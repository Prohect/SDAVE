//! Deserialization side of the SDAVE codec.
//!
//! A [`Deserialize`] implementation reads a flat byte buffer that holds the
//! flattened serialize result of one value (a single Envelop, plus the nested
//! structure inside its payload for composite types).

use core::alloc::Allocator;

use sdave::{LimiterPair, Offset, Variant};

use crate::{Error, NamePolicy};

/// Deserialize a value from a SDAVE buffer.
///
/// `T` is the basic data unit of the buffer, `A` the allocator used for any
/// intermediate allocation.
pub trait Deserialize<T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// The Rust type produced by the deserializer.
    type Type;
    /// Deserialize `buffer` into `Self::Type`.
    ///
    /// `variant` and `limiter_pairs` MUST be able to deframe `buffer`, i.e. they
    /// are the same variant and pair set (or an equivalent one) the
    /// corresponding [`Serialize`](crate::Serialize) call was given.
    ///
    /// `final_buffer` lets the caller declare that `buffer` is complete and MUST
    /// not grow: `Some(offset)` asserts `offset == buffer.len()` and accepts a
    /// [`Variant::V2`] Envelop whose delimiter slice has no following
    /// non-delimiter to confirm it; `None` rejects such an Envelop with
    /// [`Error::UnconfirmedDelimiter`]. Declaring `buffer` final while it actually
    /// grows afterwards is undefined behaviour the caller accepts.
    ///
    /// `N` is the [`NamePolicy`] the matching [`Serialize`](crate::Serialize)
    /// call used; it controls every field/concrete-type signature this call
    /// matches.
    fn deserialize<N: NamePolicy>(
        buffer: &[T],
        variant: Variant,
        limiter_pairs: &[LimiterPair<'_, T, A>],
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error>;
}
