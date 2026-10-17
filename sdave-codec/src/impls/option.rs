//! `Option<T>` as a tagged Envelop, gated behind the `option` feature.
//!
//! `None` is an Envelop whose payload is the bytes `None`; `Some(x)` is an Envelop
//! whose payload is the `Some` tag followed by `x`'s own Envelop. Both follow the
//! shared tagged-variant shape described in [`crate::impls::tagged`]: the tag is a
//! plain name NonEnvelop and the value is the Envelop right behind it, so `None` and
//! `Some` stay unambiguous.

use core::alloc::Allocator;

use alloc::vec::Vec;

use sdave::{LimiterPair, Offset, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    impls::tagged,
    name::{NamePolicy, TypeName},
    serialize::{Serialize, Strategy, frame_slice},
};

/// The wire bytes of the `None` variant.
const NONE: &[u8] = b"None";
/// The wire bytes of the `Some` variant.
const SOME: &[u8] = b"Some";

impl<T> TypeName for Option<T>
where
    T: TypeName,
{
    impl_type_name_args!("core::option::Option", <T>);
}

impl<T, A> Serialize<u8, A> for Option<T>
where
    T: Serialize<u8, A> + TypeName,
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
            None => unsafe { frame_slice(buffer, NONE, variant, strategy, limiter_pairs) },
            Some(value) => unsafe {
                tagged::frame_tagged::<N, _, A>(
                    buffer,
                    SOME,
                    value,
                    variant,
                    strategy,
                    limiter_pairs,
                )
            },
        }
    }

    generic_serialize_typed!();
}

impl<T, A> Deserialize<u8, A> for Option<T>
where
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
{
    type Type = Option<T>;

    fn deserialize<N: NamePolicy>(
        buffer: &[u8],
        variant: Variant,
        limiter_pairs: &[LimiterPair<'_, u8, A>],
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error> {
        let (tag, value) = unsafe {
            tagged::read_tagged(
                buffer,
                variant,
                limiter_pairs,
                final_buffer,
                allocator.clone(),
            )?
        };
        if tag == NONE {
            Ok(None)
        } else if tag == SOME {
            let value = value.ok_or(Error::UnexpectedItem)?;
            Ok(Some(<T as Deserialize<u8, A>>::deserialize::<N>(
                value,
                variant,
                limiter_pairs,
                Some(Offset::new(value.len())),
                allocator,
            )?))
        } else {
            Err(Error::UnexpectedItem)
        }
    }
}
