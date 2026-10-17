//! `Cow<'_, B>` as a transparent wrapper, gated behind the `cow` feature.
//!
//! `Serialize` forwards to the borrowed form `B`, so a `Cow` is wire-identical to
//! `B`. `Deserialize` always produces the owned form, so it needs
//! `B::Owned: Deserialize`: `Cow<'_, str>` needs the `string` feature (for
//! `String`) and `Cow<'_, [u8]>` needs the `vec` feature (for `Vec<u8>`).

use core::{alloc::Allocator, ops::Deref};

use alloc::{
    borrow::{Cow, ToOwned},
    vec::Vec,
};

use sdave::{LimiterPair, Offset, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    name::{NamePolicy, TypeName},
    serialize::{Name, Serialize, Strategy},
};

impl<B> TypeName for Cow<'_, B>
where
    B: ToOwned + ?Sized + TypeName,
{
    impl_type_name_forwarded!(B);
}

impl<B, A> Serialize<u8, A> for Cow<'_, B>
where
    B: ToOwned + ?Sized + Serialize<u8, A>,
    A: Allocator + Clone,
{
    const SLICES: usize = <B as Serialize<u8, A>>::SLICES;
    type Slices = <B as Serialize<u8, A>>::Slices;
    type SlicesTyped = <B as Serialize<u8, A>>::SlicesTyped;

    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8, A>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, A>],
    ) -> Result<Self::Slices, Error> {
        let target: &B = Deref::deref(self);
        unsafe {
            <B as Serialize<u8, A>>::serialize::<N>(
                target,
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
        let target: &B = Deref::deref(self);
        unsafe {
            <B as Serialize<u8, A>>::serialize_typed::<N>(
                target,
                buffer,
                name,
                variant,
                strategy,
                limiter_pairs,
            )
        }
    }
}

impl<'a, B, A> Deserialize<u8, A> for Cow<'a, B>
where
    B: ToOwned + ?Sized,
    <B as ToOwned>::Owned: Deserialize<u8, A, Type = <B as ToOwned>::Owned>,
    A: Allocator + Clone,
{
    type Type = Cow<'a, B>;

    fn deserialize<N: NamePolicy>(
        buffer: &[u8],
        variant: Variant,
        limiter_pairs: &[LimiterPair<'_, u8, A>],
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error> {
        let owned = <<B as ToOwned>::Owned as Deserialize<u8, A>>::deserialize::<N>(
            buffer,
            variant,
            limiter_pairs,
            final_buffer,
            allocator,
        )?;
        Ok(Cow::Owned(owned))
    }
}
