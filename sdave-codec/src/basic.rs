//! The `#[sdave(basic)]` escape hatch.
//!
//! A basic type is treated as opaque: it contributes the *payload* of the envelope its field or
//! item occupies — no envelope of its own, and no marker of its own. Its implementation is ordinary
//! fallible code that must not panic and must not allocate on the codec's behalf; the sink and the
//! decoding source are the only things it needs.

use crate::{DecodeSource, Error, Serialize, Shape, Sink};

/// A type written and read as the enclosing envelope's payload.
///
/// The derive turns `#[sdave(basic)]` into [`Sink::field_basic`] / [`Sink::item_basic`] on the
/// append side, and into [`BasicType::read_basic`] on the read side, so a basic type never adds an
/// envelope or a marker and never needs to know how the codec frames.
pub trait BasicType: Sized {
    /// Append the enclosing envelope's payload.
    fn append_basic(&self, sink: &mut Sink<'_>) -> Result<(), Error>;

    /// Read the payload the enclosing envelope holds.
    fn read_basic<'de>(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error>;
}

/// [`BasicType`] as a value that can be announced to a record, or appended as a positional item.
///
/// A basic value's payload is opaque to the codec, so a record announces it through this adapter —
/// `Basic(&self.raw)` — which contributes the payload the *enclosing* envelope holds (the field
/// run's or the item's), adds no envelope of its own, and declares the depth of a leaf, since
/// nothing about the payload's own nesting is knowable here. [`Sink::item_basic`] is the
/// self-framing form, for a body that supplies the envelope itself.
pub struct Basic<T>(pub T);

impl<B: BasicType> Serialize for Basic<&B> {
    const SHAPE: Shape = Shape::Tuple;
    const DEPTH: u16 = 0;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        self.0.append_basic(sink)
    }
}
