//! Deserialization side of the SDAVE codec.
//!
//! A [`Deserialize`] implementation reads a flat byte buffer that holds the
//! flattened serialize result of one value (a single Envelop, plus the nested
//! structure inside its payload for composite types).
//!
//! The traversal state is NOT rebuilt per value: [`Deserialize::deserialize`]
//! receives a [`FlatDeserializer`] that the caller (or the enclosing deserializer)
//! already owns, and every level hands its internal `Vec`s down to the next one
//! instead of allocating new ones (see `with_deserializer`).

use core::alloc::Allocator;

use sdave::{FlatDeserializer, Offset};

use crate::{Error, NamePolicy};

/// Deserialize a value from a SDAVE buffer.
///
/// `T` is the basic data unit of the buffer, `A` the allocator used for any
/// allocation the deserialized value itself needs (e.g. the `Vec` of a
/// `Vec<T, A>` field).
pub trait Deserialize<T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// The Rust type produced by the deserializer.
    type Type;
    /// Deserialize the value `deserializer` is positioned on into `Self::Type`.
    ///
    /// `deserializer` MUST be over exactly the bytes of one value (one Envelop),
    /// with a `variant` and `limiter_pairs` set able to deframe it, i.e. the same
    /// variant and pair set (or an equivalent one) the corresponding
    /// [`Serialize`](crate::Serialize) call was given.
    ///
    /// Implementations MUST treat `deserializer` as lent, not taken: they may
    /// descend into sub-slices with `with_deserializer` (which recycles the
    /// `Vec`s), but they MUST NOT replace its buffer with anything outside the
    /// bytes they were given, and they MUST leave it positioned after the value
    /// they consumed. Its item cache may be cleared on return.
    ///
    /// `final_buffer` lets the caller declare that `buffer` is complete and MUST
    /// not grow: `Some(offset)` asserts `offset == deserializer.buffer().len()`
    /// and accepts a [`Variant::V2`](sdave::Variant::V2) Envelop whose delimiter
    /// slice has no following non-delimiter to confirm it; `None` rejects such an
    /// Envelop with [`Error::UnconfirmedDelimiter`]. Declaring the buffer final
    /// while it actually grows afterwards is undefined behaviour the caller
    /// accepts.
    ///
    /// `N` is the [`NamePolicy`] the matching [`Serialize`](crate::Serialize)
    /// call used; it controls every field/concrete-type signature this call
    /// matches.
    fn deserialize<N: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, T, A>,
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error>;
}
