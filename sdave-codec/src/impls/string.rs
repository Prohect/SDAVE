//! `String` as the owned counterpart of `str`, gated behind the `string` feature.
//!
//! A `String` is wire-identical to a `str` (and to `&str` / `Cow<'_, str>` through
//! their transparent forwarding): the Envelop payload IS the UTF-8 bytes. That
//! makes `String` the deserializable side of the `str` shape — a document written
//! from a `&str` field deserializes into a `String` field, and vice versa.
//!
//! The `string` feature is off by default, so an application that never handles
//! owned strings does not pull the impl in. Deserialization allocates the `String`
//! with `Global` (the type carries no allocator parameter); the surrounding buffer
//! still uses the caller's `A`.

use core::alloc::Allocator;

use alloc::{string::String, vec::Vec};

use sdave::{LimiterPair, Offset, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    impls::single_item_buffer_as_envelop_payload,
    name::{NamePolicy, TypeName},
    serialize::{Name, Serialize, Strategy},
};

impl TypeName for String {
    impl_type_name_forwarded!(str);
}

/// A `String` IS its `str`, so both halves forward: the wire shape (including its
/// signature) is the `str` one, which is what keeps the two in lockstep.
impl<A> Serialize<u8, A> for String
where
    A: Allocator + Clone,
{
    const SLICES: usize = <str as Serialize<u8, A>>::SLICES;
    type Slices = <str as Serialize<u8, A>>::Slices;
    type SlicesTyped = <str as Serialize<u8, A>>::SlicesTyped;

    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8, A>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, A>],
    ) -> Result<Self::Slices, Error> {
        unsafe {
            <str as Serialize<u8, A>>::serialize::<N>(
                self.as_str(),
                buffer,
                variant,
                strategy,
                limiter_pairs,
            )
        }
    }

    unsafe fn serialize_typed<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8, A>,
        name: Name,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, A>],
    ) -> Result<Self::SlicesTyped, Error> {
        unsafe {
            <str as Serialize<u8, A>>::serialize_typed::<N>(
                self.as_str(),
                buffer,
                name,
                variant,
                strategy,
                limiter_pairs,
            )
        }
    }
}

impl<A> Deserialize<u8, A> for String
where
    A: Allocator + Clone,
{
    type Type = String;

    fn deserialize<N: NamePolicy>(
        buffer: &[u8],
        variant: Variant,
        limiter_pairs: &[LimiterPair<'_, u8, A>],
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error> {
        let payload = unsafe {
            single_item_buffer_as_envelop_payload(
                buffer,
                variant,
                limiter_pairs,
                final_buffer,
                allocator,
            )?
        };
        let text = core::str::from_utf8(payload).map_err(|_| Error::InvalidPayload)?;
        Ok(String::from(text))
    }
}
