//! `Result<T, E>` as a tagged Envelop, gated behind the `result` feature.
//!
//! `Ok(x)` / `Err(e)` are Envelops whose payload is the `Ok` / `Err` tag followed
//! by the value's own Envelop — the shared tagged-variant shape of
//! [`crate::impls::tagged`], i.e. a plain name tag and the value nested directly
//! behind it.

use core::alloc::Allocator;

use alloc::vec::Vec;

use sdave::{FlatDeserializer, LimiterPair, Offset, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    impls::tagged,
    name::{NamePolicy, TypeName},
    serialize::{Serialize, Strategy},
};

/// The wire bytes of the `Ok` variant.
const OK: &[u8] = b"Ok";
/// The wire bytes of the `Err` variant.
const ERR: &[u8] = b"Err";

impl<T, E> TypeName for Result<T, E>
where
    T: TypeName,
    E: TypeName,
{
    impl_type_name_args!("core::result::Result", <T, E>);
}

impl<T, E, A> Serialize<u8, A> for Result<T, E>
where
    T: Serialize<u8, A> + TypeName,
    E: Serialize<u8, A> + TypeName,
    A: Allocator + Clone,
{
    const SLICES: usize = 3;
    type Slices = [Option<(Offset, Offset)>; 3];
    type SlicesTyped = [Option<(Offset, Offset)>; 6];

    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8, A>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, A>],
    ) -> Result<Self::Slices, Error> {
        match self {
            Ok(value) => unsafe {
                tagged::frame_tagged::<N, _, A>(buffer, OK, value, variant, strategy, limiter_pairs)
            },
            Err(error) => unsafe {
                tagged::frame_tagged::<N, _, A>(
                    buffer,
                    ERR,
                    error,
                    variant,
                    strategy,
                    limiter_pairs,
                )
            },
        }
    }

    generic_serialize_typed!();
}

impl<T, E, A> Deserialize<u8, A> for Result<T, E>
where
    T: Deserialize<u8, A, Type = T>,
    E: Deserialize<u8, A, Type = E>,
    A: Allocator + Clone,
{
    type Type = Result<T, E>;

    fn deserialize<N: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error> {
        let tagged = unsafe { tagged::read_tagged(deserializer, final_buffer)? };
        let tag = tagged.tag();
        // Both tags require their value Envelop right behind them.
        let value = tagged.value_envelop()?;
        if tag == OK {
            Ok(Ok(unsafe {
                tagged::decode_tagged_value::<N, T, _, A>(deserializer, value, allocator)?
            }))
        } else if tag == ERR {
            Ok(Err(unsafe {
                tagged::decode_tagged_value::<N, E, _, A>(deserializer, value, allocator)?
            }))
        } else {
            Err(Error::UnexpectedSignature {
                valid_signatures: &["Ok", "Err"],
                caught: tagged.tag_range,
            })
        }
    }
}
