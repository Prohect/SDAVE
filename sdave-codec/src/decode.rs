//! The decode side: the `Deserialize` trait, the decoding source, and the leaf/container reads.
//!
//! Decoding is driven over the core's deserializer state machine. The core's own item sequence is
//! *flat at one level*: it yields the items directly inside the buffer it was handed, never the
//! items nested inside an envelope's payload. The codec therefore descends explicitly: it flattens
//! the document once into an arena of nodes, where each node is one item of one level and carries
//! the range of its own children (the items parsed from its payload). A level is then addressed by
//! `(its item range, its payload span)`, and descending a level is index arithmetic plus that
//! level's payload slice.
//!
//! A value's body is read by [`Deserialize`] implementations the derive generates; the source hands
//! them the framed items an aggregate holds and validates every field header's marker *before* its
//! value run is read. Leaves are borrowed verbatim from the input wherever the unit type allows it:
//! a `&str` field borrows the document's own bytes, and a **scalar unit** (a `char` buffer, which
//! has no byte view of the document) reports [`ErrorKind::NotBorrowable`] for the leaves that read
//! text.
//!
//! Like the encoding sink, [`DecodeSource`] keeps the document unit and the allocator erased, so a
//! generated entry point names neither.

use alloc::vec::Vec;
use core::alloc::Allocator;
use core::num::{
    NonZeroI128, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI8, NonZeroIsize, NonZeroU128,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU8, NonZeroUsize,
};

use sdave::{Envelop, FlatDeserializer, LimiterPair, State, Variant};

use crate::{
    BasicType, Config, Error, ErrorKind, Limit, Limits, NamePolicy, NonMaxUsize, Shape, Unit,
    units_eq_ascii,
};

/// A value the codec can read back.
///
/// `'de` is the input's borrow: leaves that can be borrowed verbatim are, so a `&'de str` field
/// borrows from the document rather than copying out of it.
pub trait Deserialize<'de>: Sized {
    /// The value's wire shape, known before any value is inspected.
    const SHAPE: Shape;

    /// Read one value whose body occupies `source`'s current frame.
    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error>;
}

/// One item of the flattened arena: the core's own item, plus the range of its children.
#[derive(Clone)]
struct Node {
    state: State,
    children_lo: usize,
    children_hi: usize,
}

/// The unit-erased view of a flattened document: the node arena and the units it was read from.
///
/// This is the codec's own bridge; a derive never names it. Keeping it a trait object is what lets
/// [`DecodeSource`] name neither the document unit nor an allocator.
trait DecodeInput<'de> {
    /// The `index`-th node of the arena.
    fn node(&self, index: usize) -> Option<Node>;

    /// The units `[start..end]` as UTF-8 text, when the unit is a byte.
    fn text(&self, start: usize, end: usize) -> Result<&'de str, Error>;

    /// The units `[start..end]` as bytes, when the unit is a byte.
    fn bytes(&self, start: usize, end: usize) -> Result<&'de [u8], Error>;

    /// Whether the units `[start..end]` equal `text` byte-exact.
    fn eq_ascii(&self, start: usize, end: usize, text: &str) -> bool;

    /// Whether the units `[start..end]` are an acceptable marker for `expected` under `policy`.
    fn accepts_marker(&self, start: usize, end: usize, expected: &str, policy: &NamePolicy) -> bool;

    /// Split a field header's span at its first `:`, giving the name span and the marker span.
    fn split_header(&self, start: usize, end: usize) -> Option<((usize, usize), (usize, usize))>;
}

/// The concrete input: the node arena and the borrowed units.
struct Input<'de, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    units: &'de [T],
    nodes: Vec<Node, A>,
}

impl<'de, T, A> DecodeInput<'de> for Input<'de, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    fn node(&self, index: usize) -> Option<Node> {
        self.nodes.get(index).cloned()
    }

    fn text(&self, start: usize, end: usize) -> Result<&'de str, Error> {
        let units: &'de [T] = self.units;
        T::as_text(&units[start..end]).ok_or_else(|| Error::new(ErrorKind::NotBorrowable))
    }

    fn bytes(&self, start: usize, end: usize) -> Result<&'de [u8], Error> {
        let units: &'de [T] = self.units;
        T::as_bytes(&units[start..end]).ok_or_else(|| Error::new(ErrorKind::NotBorrowable))
    }

    fn eq_ascii(&self, start: usize, end: usize, text: &str) -> bool {
        let units: &'de [T] = self.units;
        units_eq_ascii(&units[start..end], text)
    }

    fn accepts_marker(&self, start: usize, end: usize, expected: &str, policy: &NamePolicy) -> bool {
        let units: &'de [T] = self.units;
        policy.accepts_units(expected, &units[start..end])
    }

    fn split_header(&self, start: usize, end: usize) -> Option<((usize, usize), (usize, usize))> {
        let units: &'de [T] = self.units;
        let colon = (start..end).find(|&index| units[index].to_ascii() == Some(b':'))?;
        Some(((start, colon), (colon + 1, end)))
    }
}

/// A type-erased handle on one level of the document being read.
///
/// The source carries the naming policy and the profile's bounds dynamically; the node arena and the
/// document unit stay behind the erased core. Its payload span and its header spans are always
/// absolute offsets into the document.
pub struct DecodeSource<'de, 'a> {
    names: &'a NamePolicy,
    limits: &'a Limits,
    input: &'a dyn DecodeInput<'de>,
    depth: usize,
    base: usize,
    payload_head: usize,
    payload_tail: usize,
    lo: usize,
    hi: usize,
    pos: usize,
}

impl<'de, 'a> DecodeSource<'de, 'a> {
    /// The profile's naming policy.
    pub fn name_policy(&self) -> &NamePolicy {
        self.names
    }

    /// The profile's bounds.
    pub fn limits(&self) -> &Limits {
        self.limits
    }

    /// The current nesting depth: the document root's envelope is depth `1`.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// How many items this body holds.
    pub fn item_count(&self) -> usize {
        self.hi - self.lo
    }

    /// Whether every item of this body has been read.
    pub fn at_end(&self) -> bool {
        self.pos >= self.hi
    }

    /// Whether this body holds no item at all.
    pub fn is_body_empty(&self) -> bool {
        self.lo == self.hi
    }

    /// Whether this body's payload is empty in units.
    pub fn payload_is_empty(&self) -> bool {
        self.payload_head == self.payload_tail
    }

    /// The body's payload as UTF-8 text, when the document unit is a byte.
    pub fn payload_text(&self) -> Result<&'de str, Error> {
        self.input.text(self.payload_head, self.payload_tail)
    }

    /// The body's payload as bytes, when the document unit is a byte.
    pub fn payload_bytes(&self) -> Result<&'de [u8], Error> {
        self.input.bytes(self.payload_head, self.payload_tail)
    }

    /// Whether the body's payload equals `text` byte-exact.
    pub fn payload_eq_ascii(&self, text: &str) -> bool {
        self.input.eq_ascii(self.payload_head, self.payload_tail, text)
    }

    /// Reject a body that holds items, for a value whose body is its payload alone.
    pub fn expect_empty_body(&self) -> Result<(), Error> {
        if self.lo == self.hi {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::WrongArity {
                expected: 0,
                actual: self.hi - self.lo,
            }))
        }
    }

    /// The `index`-th node of this body, or `None` past its end.
    fn node(&self, index: usize) -> Option<Node> {
        self.input.node(index)
    }

    /// Descend into `envelop`, which this body's own parse produced.
    fn descend(
        &self,
        envelop: &Envelop,
        children_lo: usize,
        children_hi: usize,
    ) -> Result<DecodeSource<'de, 'a>, Error> {
        let depth = self.depth + 1;
        if self.limits.max_depth != 0 && depth > self.limits.max_depth {
            return Err(Error::at(
                ErrorKind::LimitExceeded {
                    limit: Limit::Depth,
                    value: depth,
                    max: self.limits.max_depth,
                },
                self.base + envelop.head_offset().get(),
            ));
        }
        let payload_head = self.base + envelop.payload_head_offset().get();
        let payload_tail = self.base + envelop.payload_tail_offset().get();
        Ok(DecodeSource {
            names: self.names,
            limits: self.limits,
            input: self.input,
            depth,
            base: payload_head,
            payload_head,
            payload_tail,
            lo: children_lo,
            hi: children_hi,
            pos: children_lo,
        })
    }

    /// Read the next item as a value envelope, and descend into its payload.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Truncated`] when the body is exhausted or the envelope is not confirmed;
    /// [`ErrorKind::Invariant`] when the next item is not an envelope (a malformed field body).
    pub fn next_envelope(&mut self) -> Result<DecodeSource<'de, 'a>, Error> {
        if self.pos >= self.hi {
            return Err(Error::new(ErrorKind::Truncated));
        }
        let node = self
            .node(self.pos)
            .ok_or_else(|| Error::new(ErrorKind::Truncated))?;
        self.pos += 1;
        match &node.state {
            State::E { envelop } => {
                if envelop.tail_offset().is_none() {
                    return Err(Error::at(
                        ErrorKind::Truncated,
                        self.base + envelop.payload_tail_offset().get(),
                    ));
                }
                let (children_lo, children_hi) = (node.children_lo, node.children_hi);
                self.descend(&envelop.clone(), children_lo, children_hi)
            }
            State::N { non_envelop } => Err(Error::at(
                ErrorKind::Invariant("a value envelope was expected here"),
                self.base + non_envelop.head_offset().get(),
            )),
        }
    }

    /// Read the next item as a value envelope and decode `V` from its body.
    pub fn next_value<V: Deserialize<'de>>(&mut self) -> Result<V, Error> {
        let mut body = self.next_envelope()?;
        V::deserialize(&mut body)
    }

    /// Read the next field header of a record body, or `None` at its end.
    ///
    /// The header never composes or compares a marker: [`FieldHeader::expect_marker`] does that, and
    /// the derive calls it before reading the value.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnknownField`] when the record body holds a bare value envelope, where a
    /// `name:marker` header belongs.
    pub fn next_field(&mut self) -> Result<Option<FieldHeader<'de, 'a>>, Error> {
        if self.pos >= self.hi {
            return Ok(None);
        }
        let node = self
            .node(self.pos)
            .ok_or_else(|| Error::new(ErrorKind::Truncated))?;
        self.pos += 1;
        match &node.state {
            State::N { non_envelop } => {
                let start = self.base + non_envelop.head_offset().get();
                let end = self.base + non_envelop.tail_offset().get();
                let (name, marker) = self.input.split_header(start, end).ok_or_else(|| {
                    Error::at(ErrorKind::Invariant("a record field run has no `:`"), start)
                })?;
                Ok(Some(FieldHeader {
                    input: self.input,
                    names: self.names,
                    name,
                    marker,
                }))
            }
            State::E { envelop } => Err(Error::at(
                ErrorKind::UnknownField,
                self.base + envelop.head_offset().get(),
            )),
        }
    }

    /// Read the value envelope that follows the header just returned by [`DecodeSource::next_field`].
    pub fn field_value<V: Deserialize<'de>>(&mut self) -> Result<V, Error> {
        self.next_value::<V>()
    }

    /// Read the value envelope that follows a `#[sdave(basic)]` field's header.
    pub fn field_basic<B: BasicType>(&mut self) -> Result<B, Error> {
        let mut body = self.next_envelope()?;
        B::read_basic(&mut body)
    }

    /// The mandatory empty header envelope of a list-shaped body.
    ///
    /// The list shape is `frame("")` followed by one item envelope per element; the codec owns that
    /// framing, so a container implementation (in `sdave-alloc`) calls this, then reads items.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::MissingListHeader`] when the body holds no header envelope;
    /// [`ErrorKind::NonEmptyListHeader`] when the header envelope is not empty.
    pub fn list_header(&mut self) -> Result<(), Error> {
        if self.pos >= self.hi {
            return Err(Error::new(ErrorKind::MissingListHeader));
        }
        let node = self
            .node(self.pos)
            .ok_or_else(|| Error::new(ErrorKind::MissingListHeader))?;
        self.pos += 1;
        match &node.state {
            State::E { envelop } => {
                if envelop.tail_offset().is_none() {
                    return Err(Error::at(
                        ErrorKind::Truncated,
                        self.base + envelop.payload_tail_offset().get(),
                    ));
                }
                if envelop.payload_head_offset().get() != envelop.payload_tail_offset().get() {
                    return Err(Error::at(
                        ErrorKind::NonEmptyListHeader,
                        self.base + envelop.payload_head_offset().get(),
                    ));
                }
                Ok(())
            }
            State::N { non_envelop } => Err(Error::at(
                ErrorKind::MissingListHeader,
                self.base + non_envelop.head_offset().get(),
            )),
        }
    }

    /// The source of a standalone document: the body of the root envelope, after validating the
    /// root's marker text against `expected`.
    fn document(
        input: &'a dyn DecodeInput<'de>,
        names: &'a NamePolicy,
        limits: &'a Limits,
        expected: &'static str,
    ) -> Result<DecodeSource<'de, 'a>, Error> {
        let marker = match input.node(0).map(|node| node.state) {
            Some(State::N { non_envelop }) => non_envelop,
            _ => return Err(Error::new(ErrorKind::Truncated)),
        };
        let start = marker.head_offset().get();
        let end = marker.tail_offset().get();
        if !input.accepts_marker(start, end, expected, names) {
            return Err(Error::at(ErrorKind::UnexpectedMarker { expected }, start));
        }
        let root = input.node(1).ok_or_else(|| Error::at(ErrorKind::Truncated, end))?;
        let envelop = match &root.state {
            State::E { envelop } => envelop.clone(),
            State::N { .. } => return Err(Error::at(ErrorKind::Truncated, end)),
        };
        if envelop.tail_offset().is_none() {
            return Err(Error::at(
                ErrorKind::Truncated,
                envelop.payload_tail_offset().get(),
            ));
        }
        let depth = 1usize;
        if limits.max_depth != 0 && depth > limits.max_depth {
            return Err(Error::at(
                ErrorKind::LimitExceeded {
                    limit: Limit::Depth,
                    value: depth,
                    max: limits.max_depth,
                },
                envelop.head_offset().get(),
            ));
        }
        let payload_head = envelop.payload_head_offset().get();
        let payload_tail = envelop.payload_tail_offset().get();
        Ok(DecodeSource {
            names,
            limits,
            input,
            depth,
            base: payload_head,
            payload_head,
            payload_tail,
            lo: root.children_lo,
            hi: root.children_hi,
            pos: root.children_lo,
        })
    }
}

/// A record field run's header: its name and its declared type's marker, as absolute spans.
///
/// The header never hands the derive a `&str` (a scalar unit has no byte view), so a declaration is
/// matched with [`FieldHeader::name_is`] and its marker validated with
/// [`FieldHeader::expect_marker`] before the value run is read.
pub struct FieldHeader<'de, 'a> {
    input: &'a dyn DecodeInput<'de>,
    names: &'a NamePolicy,
    name: (usize, usize),
    marker: (usize, usize),
}

impl<'de, 'a> FieldHeader<'de, 'a> {
    /// Whether this header names `name`.
    pub fn name_is(&self, name: &str) -> bool {
        self.input.eq_ascii(self.name.0, self.name.1, name)
    }

    /// Whether this header's marker is acceptable for the compiler spelling `expected`.
    pub fn marker_is(&self, expected: &'static str) -> bool {
        self.input
            .accepts_marker(self.marker.0, self.marker.1, expected, self.names)
    }

    /// Validate this header's marker against the compiler spelling `expected`.
    ///
    /// The comparison is the policy's own ([`NamePolicy::accepts_units`]): the expected spelling is
    /// rendered through the policy and compared, then the verbatim spelling is compared, each under
    /// the documented tuple-comma-padding equivalence and with no other normalisation.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedMarker`] when neither comparison accepts the incoming marker.
    pub fn expect_marker(&self, expected: &'static str) -> Result<(), Error> {
        if self.marker_is(expected) {
            Ok(())
        } else {
            Err(Error::at(
                ErrorKind::UnexpectedMarker { expected },
                self.marker.0,
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// flattening
// ---------------------------------------------------------------------------

/// Parse the level occupying `units[start..end]` and append it, and every level below it, to
/// `nodes`; return the range of `nodes` that holds this level's items.
///
/// The core's item sequence is flat at one level, so the codec descends explicitly, one payload
/// slice at a time. Every node's own offsets are relative to the slice its level parsed, which is
/// why a [`DecodeSource`] carries its level's `base`.
///
/// A level's items occupy a **contiguous** run of the arena — `[lo, hi)` — and each node's children
/// are a *later* contiguous run. The level is therefore written in two passes: first every one of
/// its own items, then (once `hi` is fixed) each item's payload, recursively. Appending a child
/// while its parent's level is still being written would interleave the levels and make a level
/// range pick up its descendants.
fn flatten<'l, 'v, T, A>(
    units: &[T],
    start: usize,
    end: usize,
    variant: Variant,
    limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
    allocator: &A,
    nodes: &mut Vec<Node, A>,
) -> (usize, usize)
where
    T: Unit,
    A: Allocator + Clone,
{
    let lo = nodes.len();
    let mut level: Vec<State, A> = Vec::new_in(A::clone(allocator));
    {
        let mut deserializer = FlatDeserializer::new(
            &units[start..end],
            variant,
            limiter_pairs,
            Vec::new_in(A::clone(allocator)),
            Vec::new_in(A::clone(allocator)),
        );
        for state in deserializer.iter() {
            level.push(state);
        }
    }

    // First pass: exactly this level's own items.
    let mut pending: Vec<(usize, usize, usize), A> = Vec::new_in(A::clone(allocator));
    for state in level {
        match state {
            State::E { envelop } => {
                // A phantom envelope is a structural marker for a leading NonEnvelop, not an item.
                if envelop.is_phantom() {
                    continue;
                }
                let index = nodes.len();
                nodes.push(Node {
                    state: State::E {
                        envelop: envelop.clone(),
                    },
                    children_lo: index,
                    children_hi: index,
                });
                if envelop.tail_offset().is_some() {
                    let child_start = start + envelop.payload_head_offset().get();
                    let child_end = start + envelop.payload_tail_offset().get();
                    if child_start < child_end {
                        pending.push((index, child_start, child_end));
                    }
                }
            }
            State::N { non_envelop } => {
                if non_envelop.head_offset().get() == non_envelop.tail_offset().get() {
                    continue;
                }
                let index = nodes.len();
                nodes.push(Node {
                    state: State::N { non_envelop },
                    children_lo: index,
                    children_hi: index,
                });
            }
        }
    }
    let hi = nodes.len();

    // Second pass: each item's payload, once this level's own range is closed.
    for (index, child_start, child_end) in pending {
        let (children_lo, children_hi) = flatten(
            units,
            child_start,
            child_end,
            variant,
            limiter_pairs,
            allocator,
            nodes,
        );
        nodes[index].children_lo = children_lo;
        nodes[index].children_hi = children_hi;
    }

    (lo, hi)
}

// ---------------------------------------------------------------------------
// the document entry point
// ---------------------------------------------------------------------------

impl<'l, 'v, T, A> Config<'l, 'v, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    /// Decode one standalone document from `buffer`.
    ///
    /// The document's leading marker text is validated against `D`'s own compiler spelling through
    /// the profile's naming policy, then `D`'s body is read from the root envelope's payload.
    ///
    /// `allocator` builds the one arena the flattening needs; `A` is never hidden, so the caller
    /// passes one explicitly rather than the codec picking one.
    ///
    /// A document may be followed by more content — the application drives segments itself — so
    /// only the first document is consumed.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::LimitExceeded`] for the part of [`Limits`] this decode enforces: the whole
    /// buffer against `max_len`, and each nesting level against `max_depth`. (`max_repeat` bounds a
    /// *frame*, which the serializer chooses; the deserializer's repeat bookkeeping is the core's
    /// and the codec does not re-derive it here.) Otherwise, the errors of the [`Deserialize`]
    /// family.
    pub fn deserialize<'de, D: Deserialize<'de>>(
        &self,
        buffer: &'de [T],
        allocator: &A,
    ) -> Result<D, Error> {
        let limits = *self.limits();
        if limits.max_len != 0 && buffer.len() > limits.max_len {
            return Err(Error::new(ErrorKind::LimitExceeded {
                limit: Limit::Length,
                value: buffer.len(),
                max: limits.max_len,
            }));
        }

        let mut nodes: Vec<Node, A> = Vec::new_in(A::clone(allocator));
        flatten(
            buffer,
            0,
            buffer.len(),
            self.variant(),
            self.limiter_pairs(),
            allocator,
            &mut nodes,
        );
        let input = Input {
            units: buffer,
            nodes,
        };
        let names = self.name_policy();
        let mut source =
            DecodeSource::document(&input, names, &limits, core::any::type_name::<D>())?;
        D::deserialize(&mut source)
    }
}

// ---------------------------------------------------------------------------
// leaves
// ---------------------------------------------------------------------------

/// The sign and magnitude of a canonical decimal integer's text.
///
/// An optional `-` is accepted, then one or more ASCII digits; nothing else. Leading zeros are
/// tolerated (a sender's `-001` reads back as `-1`), and overflow of `u128` is
/// [`ErrorKind::NumberOutOfRange`].
fn decimal_magnitude(text: &str) -> Result<(bool, u128), Error> {
    let bytes = text.as_bytes();
    let (negative, digits) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return Err(Error::new(ErrorKind::InvalidNumber));
    }
    let mut magnitude: u128 = 0;
    for &byte in digits {
        if !byte.is_ascii_digit() {
            return Err(Error::new(ErrorKind::InvalidNumber));
        }
        magnitude = magnitude
            .checked_mul(10)
            .and_then(|value| value.checked_add(u128::from(byte - b'0')))
            .ok_or_else(|| Error::new(ErrorKind::NumberOutOfRange))?;
    }
    Ok((negative, magnitude))
}

macro_rules! unsigned_deserialize {
    ($($ty:ty),* $(,)?) => {$(
        impl<'de> Deserialize<'de> for $ty {
            const SHAPE: Shape = Shape::Tuple;

            fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
                let (negative, magnitude) = decimal_magnitude(source.payload_text()?)?;
                if negative || magnitude > <$ty>::MAX as u128 {
                    return Err(Error::new(ErrorKind::NumberOutOfRange));
                }
                Ok(magnitude as $ty)
            }
        }
    )*};
}

macro_rules! signed_deserialize {
    ($($ty:ty),* $(,)?) => {$(
        impl<'de> Deserialize<'de> for $ty {
            const SHAPE: Shape = Shape::Tuple;

            fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
                let (negative, magnitude) = decimal_magnitude(source.payload_text()?)?;
                let max = <$ty>::MAX as u128;
                if negative {
                    let bound = max + 1;
                    if magnitude > bound {
                        return Err(Error::new(ErrorKind::NumberOutOfRange));
                    }
                    if magnitude == bound {
                        return Ok(<$ty>::MIN);
                    }
                    Ok(-(magnitude as $ty))
                } else {
                    if magnitude > max {
                        return Err(Error::new(ErrorKind::NumberOutOfRange));
                    }
                    Ok(magnitude as $ty)
                }
            }
        }
    )*};
}

macro_rules! nonzero_deserialize {
    ($($ty:ty => $inner:ty),* $(,)?) => {$(
        impl<'de> Deserialize<'de> for $ty {
            const SHAPE: Shape = Shape::Tuple;

            fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
                let value = <$inner>::deserialize(source)?;
                <$ty>::new(value).ok_or_else(|| Error::new(ErrorKind::NumberOutOfRange))
            }
        }
    )*};
}

unsigned_deserialize!(u8, u16, u32, u64, u128, usize);
signed_deserialize!(i8, i16, i32, i64, i128, isize);
nonzero_deserialize!(
    NonZeroU8 => u8,
    NonZeroU16 => u16,
    NonZeroU32 => u32,
    NonZeroU64 => u64,
    NonZeroU128 => u128,
    NonZeroUsize => usize,
    NonZeroI8 => i8,
    NonZeroI16 => i16,
    NonZeroI32 => i32,
    NonZeroI64 => i64,
    NonZeroI128 => i128,
    NonZeroIsize => isize,
);

impl<'de> Deserialize<'de> for NonMaxUsize {
    const SHAPE: Shape = Shape::Tuple;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let (negative, magnitude) = decimal_magnitude(source.payload_text()?)?;
        if negative || magnitude > usize::MAX as u128 {
            return Err(Error::new(ErrorKind::NumberOutOfRange));
        }
        // `NonMaxUsize::new` panics at `usize::MAX`, so the bound is checked *before* constructing.
        let value = magnitude as usize;
        if value == usize::MAX {
            return Err(Error::new(ErrorKind::NumberOutOfRange));
        }
        Ok(NonMaxUsize::new(value))
    }
}

impl<'de> Deserialize<'de> for bool {
    const SHAPE: Shape = Shape::Tuple;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        match source.payload_text()? {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(Error::new(ErrorKind::InvalidBoolean)),
        }
    }
}

impl<'de> Deserialize<'de> for char {
    const SHAPE: Shape = Shape::Tuple;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let mut scalar = source.payload_text()?.chars();
        match (scalar.next(), scalar.next()) {
            (Some(unit), None) => Ok(unit),
            _ => Err(Error::new(ErrorKind::InvalidCharacter)),
        }
    }
}

macro_rules! float_deserialize {
    ($($ty:ty),* $(,)?) => {$(
        impl<'de> Deserialize<'de> for $ty {
            const SHAPE: Shape = Shape::Tuple;

            fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
                let value: $ty = source
                    .payload_text()?
                    .parse()
                    .map_err(|_| Error::new(ErrorKind::InvalidNumber))?;
                if !value.is_finite() {
                    return Err(Error::new(ErrorKind::NonFiniteFloat));
                }
                Ok(value)
            }
        }
    )*};
}

float_deserialize!(f32, f64);

impl<'de: 'a, 'a> Deserialize<'de> for &'a str {
    const SHAPE: Shape = Shape::Tuple;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        source.payload_text()
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for &'a [u8] {
    const SHAPE: Shape = Shape::Tuple;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        source.payload_bytes()
    }
}

// ---------------------------------------------------------------------------
// the unit value, tuples, `Option` and `Result`
// ---------------------------------------------------------------------------

impl<'de> Deserialize<'de> for () {
    const SHAPE: Shape = Shape::Tuple;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        source.expect_empty_body()?;
        Ok(())
    }
}

macro_rules! tuple_deserialize {
    ($($ty:ident => $local:ident),+ $(,)?) => {
        impl<'de, $($ty: Deserialize<'de>),+> Deserialize<'de> for ($($ty,)+) {
            const SHAPE: Shape = Shape::Tuple;

            fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
                let arity = 0usize $(+ tuple_deserialize!(@one $ty))+;
                let found = source.item_count();
                if found != arity {
                    return Err(Error::new(ErrorKind::WrongArity {
                        expected: arity,
                        actual: found,
                    }));
                }
                let mut index = 0usize;
                $(
                    let $local = source
                        .next_value::<$ty>()
                        .map_err(|error| error.at_index(index))?;
                    index += 1;
                )+
                let _ = index;
                Ok(($($local,)+))
            }
        }
    };
    (@one $ty:ident) => { 1usize };
}

tuple_deserialize!(A => a);
tuple_deserialize!(A => a, B => b);
tuple_deserialize!(A => a, B => b, C => c);
tuple_deserialize!(A => a, B => b, C => c, D => d);
tuple_deserialize!(A => a, B => b, C => c, D => d, E => e);
tuple_deserialize!(A => a, B => b, C => c, D => d, E => e, F => f);
tuple_deserialize!(A => a, B => b, C => c, D => d, E => e, F => f, G => g);
tuple_deserialize!(A => a, B => b, C => c, D => d, E => e, F => f, G => g, H => h);
tuple_deserialize!(A => a, B => b, C => c, D => d, E => e, F => f, G => g, H => h, I => i);
tuple_deserialize!(A => a, B => b, C => c, D => d, E => e, F => f, G => g, H => h, I => i, J => j);
tuple_deserialize!(A => a, B => b, C => c, D => d, E => e, F => f, G => g, H => h, I => i, J => j, K => k);
tuple_deserialize!(A => a, B => b, C => c, D => d, E => e, F => f, G => g, H => h, I => i, J => j, K => k, L => l);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Option<T> {
    const SHAPE: Shape = Shape::Enum;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let arity = source.item_count();
        let variant = source.next_envelope()?;
        let value = if variant.payload_eq_ascii("Some") {
            if arity != 2 {
                return Err(Error::new(ErrorKind::WrongArity {
                    expected: 2,
                    actual: arity,
                }));
            }
            let mut body = source.next_envelope()?;
            Some(T::deserialize(&mut body).map_err(|error| error.in_variant("Some"))?)
        } else if variant.payload_eq_ascii("None") {
            if arity != 1 {
                return Err(Error::new(ErrorKind::WrongArity {
                    expected: 1,
                    actual: arity,
                }));
            }
            None
        } else {
            return Err(Error::new(ErrorKind::UnknownVariant));
        };
        if !source.at_end() {
            return Err(Error::new(ErrorKind::Invariant(
                "the enum body has leftover items",
            )));
        }
        Ok(value)
    }
}

impl<'de, T: Deserialize<'de>, E: Deserialize<'de>> Deserialize<'de> for Result<T, E> {
    const SHAPE: Shape = Shape::Enum;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let arity = source.item_count();
        let variant = source.next_envelope()?;
        let value = if variant.payload_eq_ascii("Ok") {
            if arity != 2 {
                return Err(Error::new(ErrorKind::WrongArity {
                    expected: 2,
                    actual: arity,
                }));
            }
            let mut body = source.next_envelope()?;
            Ok(T::deserialize(&mut body).map_err(|error| error.in_variant("Ok"))?)
        } else if variant.payload_eq_ascii("Err") {
            if arity != 2 {
                return Err(Error::new(ErrorKind::WrongArity {
                    expected: 2,
                    actual: arity,
                }));
            }
            let mut body = source.next_envelope()?;
            Err(E::deserialize(&mut body).map_err(|error| error.in_variant("Err"))?)
        } else {
            return Err(Error::new(ErrorKind::UnknownVariant));
        };
        if !source.at_end() {
            return Err(Error::new(ErrorKind::Invariant(
                "the enum body has leftover items",
            )));
        }
        Ok(value)
    }
}

// ---------------------------------------------------------------------------
// helpers the defaults hook uses
// ---------------------------------------------------------------------------

/// An absent named field's value, or its type's `Default`.
///
/// The macro's `#[sdave(default)]` provider runs only after the record is confirmed complete.
pub fn option_or_default<T: Default>(value: Option<T>) -> T {
    value.unwrap_or_default()
}

/// An absent named field's value, or the provider `#[sdave(default = path)]` names.
///
/// The provider runs at most once, and only after the record is confirmed complete.
pub fn option_or<T>(value: Option<T>, provider: fn() -> T) -> T {
    value.unwrap_or_else(provider)
}
