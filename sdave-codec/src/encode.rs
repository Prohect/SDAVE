//! The serialize side: the `Serialize` trait, the document target, and the sink.
//!
//! # One inner-to-outer pass
//!
//! A value is serialized by **one** traversal, bottom-up: a level's children are materialized
//! first, and then the level frames them as it closes. There is no separate size pass, no separate
//! write pass and no scratch — the bytes *are* the plan, and the document's length is a by-product
//! of the pass rather than a number computed ahead of it.
//!
//! Closing a level is an *insertion* at the end of the content written so far: the level's payload
//! is already in the target, so room for the limiter slice is made by moving that payload up, then
//! the limiter slice is written before it and the delimiter slice after it.
//!
//! # The sink and the target
//!
//! The [`Sink`] carries the naming policy and the profile's bounds *dynamically* and keeps `T` and
//! the allocator erased, which is what makes a generated `dyn` delegation object-safe: a `dyn`
//! payload's methods take no naming policy and no allocator generic, and the one config serves the
//! whole document.
//!
//! Behind the sink, the [`DocumentTarget`] is the caller's storage. The codec appends to what it
//! was handed and never grows anything itself: a target that is a fixed slice reports
//! [`crate::ErrorKind::TargetExhausted`], while a target the caller backs with its own allocator
//! grows, and that growth is the caller's capability extension, not the codec allocating.

use crate::{Error, Limits, NamePolicy, Shape};

/// A value the codec can write.
///
/// A derive sets [`Serialize::SHAPE`] and [`Serialize::DEPTH`] from the declaration and calls the
/// codec's helpers for its children; it composes no headers, compares no markers, and touches no
/// leaf bytes.
pub trait Serialize {
    /// The value's wire shape, known before any value is inspected.
    const SHAPE: Shape;

    /// The value's *structure depth*: how many envelope levels close inside it. A leaf is `0`; an
    /// aggregate is one more than its deepest child, which is what [`depth_of`] computes.
    ///
    /// A record's fields are appended in ascending depth (see [`crate::Record`]), so this is the
    /// key the serializer orders them by — it is the derive's job to supply it, and a type whose
    /// depth is unbounded (a self-referential type) declares the deepest value it can.
    const DEPTH: u16;

    /// Append the value, framing each level as it closes.
    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error>;
}

/// The deepest of `children`, or `0` when there are none.
pub const fn deepest_of(children: &[u16]) -> u16 {
    let mut deepest = 0;
    let mut index = 0;
    while index < children.len() {
        if children[index] > deepest {
            deepest = children[index];
        }
        index += 1;
    }
    deepest
}

/// The structure depth of an aggregate with `children`: one more than its deepest child.
///
/// Saturating, so an aggregate over a value that declares itself the deepest stays the deepest.
pub const fn depth_of(children: &[u16]) -> u16 {
    deepest_of(children).saturating_add(1)
}

/// The type-erased form of [`Serialize`] the record collector stores.
///
/// A record's slots must hold one type whatever the fields' types are, and the serializer appends
/// them in its own order — so the slots hold `&dyn AppendValue`. [`Serialize`]'s associated
/// constants are not available through an erased reference, which is why the erasure is this
/// const-free bridge rather than `&dyn Serialize` itself; the blanket impl makes every
/// `Serialize` type usable as one.
pub trait AppendValue {
    /// Append the value, framing each level as it closes.
    fn append_value(&self, sink: &mut Sink<'_>) -> Result<(), Error>;
}

impl<V: Serialize + ?Sized> AppendValue for V {
    fn append_value(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        self.append(sink)
    }
}

/// The unit-erased document storage a [`Sink`] appends through.
///
/// The codec's own [`crate::SliceTarget`] is fixed and reports
/// [`crate::ErrorKind::TargetExhausted`]; a caller may implement this over storage that grows
/// through its own allocator, in which case the growth is the caller's capability.
pub trait DocumentTarget {
    /// The current write position, in units, from the target's own start.
    fn cursor(&self) -> usize;

    /// The current nesting depth.
    fn depth(&self) -> usize;

    /// Append ASCII text.
    fn push_text(&mut self, text: &str) -> Result<(), Error>;

    /// Append the UTF-8 bytes of a payload.
    fn push_utf8(&mut self, bytes: &[u8]) -> Result<(), Error>;

    /// Descend one level, enforcing the profile's depth bound.
    fn enter(&mut self) -> Result<(), Error>;

    /// Ascend one level.
    fn leave(&mut self);

    /// Frame the payload written since `base`: choose a pair and repeat from it, and wrap it.
    ///
    /// This is the one place a level's framing is decided, so a target decides it from the bytes
    /// the pass has already written.
    fn frame(&mut self, base: usize) -> Result<(), Error>;

    /// Drop everything written after `len`, for a caller that wants to roll a failed pass back.
    fn truncate(&mut self, len: usize) -> Result<(), Error>;
}

/// A type-erased handle on the document being assembled.
///
/// The sink carries the naming policy and the profile's bounds dynamically, so a value's entry
/// point — and therefore a `dyn` payload's erased methods — names neither the naming policy nor an
/// allocator.
pub struct Sink<'a> {
    core: &'a mut dyn DocumentTarget,
    names: &'a NamePolicy,
    limits: &'a Limits,
}

impl<'a> Sink<'a> {
    /// Wrap a document target.
    ///
    /// This is the codec's own entry point into the sink; a derive receives an already-built sink
    /// and never constructs one.
    pub fn new(
        core: &'a mut dyn DocumentTarget,
        names: &'a NamePolicy,
        limits: &'a Limits,
    ) -> Self {
        Sink {
            core,
            names,
            limits,
        }
    }

    /// The profile's naming policy.
    pub fn name_policy(&self) -> &NamePolicy {
        self.names
    }

    /// The profile's bounds.
    pub fn limits(&self) -> &Limits {
        self.limits
    }

    /// The current nesting depth.
    pub fn depth(&self) -> usize {
        self.core.depth()
    }

    /// The current write position, in units.
    pub fn cursor(&self) -> usize {
        self.core.cursor()
    }

    /// Append ASCII text verbatim.
    pub fn text(&mut self, text: &str) -> Result<(), Error> {
        self.core.push_text(text)
    }

    /// Append the UTF-8 bytes of a payload.
    pub fn utf8(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.core.push_utf8(bytes)
    }

    /// Emit a value's type marker: the spelling rendered through the naming policy, verbatim.
    ///
    /// A standalone document root and a `dyn` payload's value bytes carry a marker; a nested value
    /// does not, because the enclosing header or the declaration already supplies its type.
    pub fn marker(&mut self, spelling: &'static str) -> Result<(), Error> {
        let rendered = self.names.render(spelling);
        self.core.push_text(rendered)
    }

    /// Emit a record field run's header: the field's name, `:`, and the field type's marker.
    pub fn field_header(
        &mut self,
        name: &'static str,
        spelling: &'static str,
    ) -> Result<(), Error> {
        self.core.push_text(name)?;
        self.core.push_text(":")?;
        let rendered = self.names.render(spelling);
        self.core.push_text(rendered)
    }

    /// Wrap everything `body` appends in one envelope.
    ///
    /// The payload is exactly what `body` appended, and the pair, the repeat and the frame's size
    /// are all decided from it.
    pub fn frame<F>(&mut self, body: F) -> Result<(), Error>
    where
        F: for<'b> FnOnce(&mut Sink<'b>) -> Result<(), Error>,
    {
        let base = self.core.cursor();
        self.core.enter()?;
        let outcome = body(&mut *self);
        self.core.leave();
        outcome?;
        self.core.frame(base)
    }

    /// Append a child value through the one append entry.
    pub fn child<V: Serialize + ?Sized>(&mut self, value: &V) -> Result<(), Error> {
        value.append(self)
    }

    /// Append a value the caller already holds type-erased (a record's announced field).
    pub(crate) fn item_erased(&mut self, value: &dyn AppendValue) -> Result<(), Error> {
        self.frame(|sink| value.append_value(sink))
    }

    /// Append a child value as one positional item: its own envelope, no header, no marker.
    pub fn item_run<V: Serialize + ?Sized>(&mut self, value: &V) -> Result<(), Error> {
        self.frame(|sink| sink.child(value))
    }

    /// Append a record field run: `name:Marker` followed by the value's envelope.
    pub fn field_run<V: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        spelling: &'static str,
        value: &V,
    ) -> Result<(), Error> {
        self.field_header(name, spelling)?;
        self.item_run(value)
    }

    /// Append a marked document: a marker, then one envelope holding the value's body.
    ///
    /// This is the shape a standalone document root has, and the shape a `dyn` payload's value
    /// bytes carry — which is why the codec composes it here rather than leaving it to a generated
    /// `DynSerialize` implementation: the naming policy stays the one naming authority and the
    /// framing stays the codec's.
    pub fn marked_document<V: Serialize + ?Sized>(
        &mut self,
        spelling: &'static str,
        value: &V,
    ) -> Result<(), Error> {
        self.marker(spelling)?;
        self.item_run(value)
    }

    /// Append a `#[sdave(basic)]` value as one positional item.
    ///
    /// A basic value contributes the enclosing envelope's *payload*: no envelope of its own, and no
    /// marker.
    pub fn item_basic<B: crate::BasicType>(&mut self, value: &B) -> Result<(), Error> {
        self.frame(|sink| value.append_basic(sink))
    }

    /// Append a `#[sdave(basic)]` record field: `name:Marker` followed by the payload.
    pub fn field_basic<B: crate::BasicType>(
        &mut self,
        name: &'static str,
        spelling: &'static str,
        value: &B,
    ) -> Result<(), Error> {
        self.field_header(name, spelling)?;
        self.item_basic(value)
    }

    /// Append a unit variant: the variant envelope alone.
    pub fn variant(&mut self, name: &'static str) -> Result<(), Error> {
        self.frame(|sink| sink.text(name))
    }

    /// Append a single-field variant: the variant envelope, then the associated-value envelope
    /// holding `value`'s payload.
    pub fn variant_with<V: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &V,
    ) -> Result<(), Error> {
        self.variant(name)?;
        self.item_run(value)
    }

    /// Append a multi-field or named-field variant: the variant envelope, then the
    /// associated-value envelope holding the body `body` appends.
    pub fn variant_fields<F>(&mut self, name: &'static str, body: F) -> Result<(), Error>
    where
        F: for<'b> FnOnce(&mut Sink<'b>) -> Result<(), Error>,
    {
        self.variant(name)?;
        self.frame(body)
    }
}
