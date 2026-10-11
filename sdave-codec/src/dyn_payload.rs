//! The `dyn` payload machinery.
//!
//! A `dyn` field **delivers identity instead of erasing it**: the field header keeps the compiler
//! spelling of the *field type* (`Box<dyn …>` and `&dyn …` differ on the wire), and the value bytes
//! carry the *concrete* type's **marked document** — `marker-text ++ frame(body)` — which is the
//! same shape a standalone document root has.
//!
//! # Object-safety drives the design
//!
//! An application's object trait takes [`DynSerialize`] as a supertrait, which keeps *that* trait
//! object-safe: the method here is type-erased — no naming policy, no allocator generic — and the
//! [`Sink`] it is handed carries the naming policy *dynamically*, so there is no fixed inner policy
//! and the one config serves the whole document.
//!
//! # The codec never constructs the trait object
//!
//! On the read side the codec only **locates** the concrete marker ([`locate_dyn_marker`],
//! [`locate_dyn_payload`]); resolving that marker and constructing the value is the application's
//! registry. There is deliberately no per-type read method on [`DynSerialize`]: locating is
//! type-independent, and a trait object cannot be built here anyway.
//!
//! # What the codec composes
//!
//! A concrete type's [`DynSerialize`] implementation does not compose the marked document itself —
//! [`Sink::marked_document`] does, so the marker text and the envelope come from the codec and the
//! naming policy stays the single naming authority:
//!
//! ```text
//! impl DynSerialize for ReadFileInput {
//!     fn append_dyn(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
//!         sink.marked_document(type_name::<Self>(), self)
//!     }
//! }
//!
//! // The application's object trait keeps its own methods; the codec needs one impl of its own:
//! impl Serialize for dyn ToolInput {
//!     const SHAPE: Shape = Shape::Dynamic;
//!     const DEPTH: u16 = DYN_PAYLOAD_DEPTH;
//!     fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
//!         DynSerialize::append_dyn(self, sink)
//!     }
//! }
//! ```

use alloc::vec::Vec;
use core::alloc::Allocator;

use sdave::{Envelop, FlatDeserializer, LimiterPair, State, Variant};

use crate::{Config, Error, ErrorKind, Sink, Unit};

/// The structure depth a `dyn` payload declares.
///
/// A marked document is one envelope deep, and the concrete body's own nesting is not knowable
/// through a trait object, so a `dyn` field is ordered as if its concrete body were a leaf.
pub const DYN_PAYLOAD_DEPTH: u16 = 1;

/// The type-erased half of a `dyn` payload: the supertrait an application's object trait takes.
///
/// Its single method names neither the naming policy nor an allocator, which is what keeps the
/// application's trait object-safe; [`Sink`] carries the naming policy dynamically.
pub trait DynSerialize {
    /// Append the concrete value's marked document: the concrete type's marker, rendered through
    /// the sink's naming policy, then one envelope holding the concrete body.
    fn append_dyn(&self, sink: &mut Sink<'_>) -> Result<(), Error>;
}

/// A `dyn` payload's concrete identity, located in a document.
///
/// The three ranges are slices of the value units that were handed to the locator: the marker units
/// as they appear on the wire, the concrete envelope around them, and that envelope's payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DynPayload<'u, T> {
    marker: &'u [T],
    envelope: &'u [T],
    body: &'u [T],
}

impl<'u, T> DynPayload<'u, T> {
    /// The concrete type's marker units, as the document carries them.
    pub fn marker(&self) -> &'u [T] {
        self.marker
    }

    /// The concrete type's whole envelope: its limiter slice, its payload and its delimiter slice.
    pub fn envelope(&self) -> &'u [T] {
        self.envelope
    }

    /// The concrete type's body units: the payload of that envelope.
    pub fn body(&self) -> &'u [T] {
        self.body
    }
}

/// Locate the concrete marker that a `dyn` payload's value units carry.
///
/// The value units have the shape a document root has — marker text, then one envelope — so this is
/// a plain deserialization of them. Only the envelope's *head* is needed, so a marker is locatable
/// even when the envelope after it is still unconfirmed (V2's delimiter-slice latency).
///
/// `allocator` builds the two scratch buffers the core's deserializer needs: `A` is never hidden, so
/// the caller passes one explicitly rather than the codec picking one.
///
/// # Errors
///
/// [`ErrorKind::Truncated`] when the units carry no marker text at all.
pub fn locate_dyn_marker<'u, 'l, 'v, T, A>(
    units: &'u [T],
    variant: Variant,
    limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
    allocator: &A,
) -> Result<&'u [T], Error>
where
    T: Unit,
    A: Allocator + Clone,
{
    match scan(units, variant, limiter_pairs, allocator).0 {
        Some(marker) => Ok(marker),
        None => Err(Error::new(ErrorKind::Truncated)),
    }
}

/// Locate the concrete marker, the concrete envelope and the concrete body of a `dyn` payload.
///
/// `allocator` builds the two scratch buffers the core's deserializer needs: `A` is never hidden, so
/// the caller passes one explicitly rather than the codec picking one.
///
/// # Errors
///
/// [`ErrorKind::Truncated`] when the units carry no marker, or when the concrete envelope is not yet
/// confirmed: under V2 the last envelope of a buffer needs a following non-delimiter to confirm its
/// delimiter slice, so a caller that isolates the value units either appends one such unit or asks
/// only for the marker with [`locate_dyn_marker`].
pub fn locate_dyn_payload<'u, 'l, 'v, T, A>(
    units: &'u [T],
    variant: Variant,
    limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
    allocator: &A,
) -> Result<DynPayload<'u, T>, Error>
where
    T: Unit,
    A: Allocator + Clone,
{
    let (marker, envelop) = scan(units, variant, limiter_pairs, allocator);
    let marker = marker.ok_or(Error::new(ErrorKind::Truncated))?;
    let envelop = envelop.ok_or(Error::new(ErrorKind::Truncated))?;
    let tail = envelop
        .tail_offset()
        .ok_or(Error::new(ErrorKind::Truncated))?;
    let head = envelop.head_offset().get();
    let payload_head = envelop.payload_head_offset().get();
    let payload_tail = envelop.payload_tail_offset().get();
    Ok(DynPayload {
        marker,
        envelope: &units[head..tail.get()],
        body: &units[payload_head..payload_tail],
    })
}

impl<'l, 'v, T, A> Config<'l, 'v, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    /// [`locate_dyn_marker`] for this profile's variant and pair list.
    ///
    /// `allocator` is passed on to the core's deserializer, which needs somewhere to put its two
    /// scratch buffers.
    pub fn locate_dyn_marker<'u>(&self, units: &'u [T], allocator: &A) -> Result<&'u [T], Error> {
        locate_dyn_marker(units, self.variant(), self.limiter_pairs(), allocator)
    }

    /// [`locate_dyn_payload`] for this profile's variant and pair list.
    ///
    /// `allocator` is passed on to the core's deserializer, which needs somewhere to put its two
    /// scratch buffers.
    pub fn locate_dyn_payload<'u>(
        &self,
        units: &'u [T],
        allocator: &A,
    ) -> Result<DynPayload<'u, T>, Error> {
        locate_dyn_payload(units, self.variant(), self.limiter_pairs(), allocator)
    }
}

/// The leading marker and the first confirmed envelope of a marked document.
///
/// `None` for the envelope when there is none, or when the one there is has not been confirmed.
fn scan<'u, 'l, 'v, T, A>(
    units: &'u [T],
    variant: Variant,
    limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
    allocator: &A,
) -> (Option<&'u [T]>, Option<Envelop>)
where
    T: Unit,
    A: Allocator + Clone,
{
    let mut deserializer = FlatDeserializer::new(
        units,
        variant,
        limiter_pairs,
        Vec::new_in(A::clone(allocator)),
        Vec::new_in(A::clone(allocator)),
    );
    let mut marker = None;
    let mut envelop = None;
    for state in deserializer.iter() {
        match state {
            State::E { envelop: item } => {
                if marker.is_some() && item.tail_offset().is_some() {
                    envelop = Some(item);
                    break;
                }
            }
            State::N { non_envelop } => {
                if marker.is_none() {
                    let head = non_envelop.head_offset().get();
                    let tail = non_envelop.tail_offset().get();
                    if tail > head {
                        marker = Some(&units[head..tail]);
                    }
                }
            }
        }
    }
    (marker, envelop)
}
