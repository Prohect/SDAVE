//! `Cell<T>` and `UnsafeCell<T>` as transparent wrappers, gated behind the `cell`
//! and `unsafe_cell` features respectively.
//!
//! Each serializes exactly like its inner `T` (interior mutability is not part of
//! the wire format) and deserializes into a fresh cell owning the decoded value.
//!
//! # Soundness note
//!
//! `Cell<T>` reads its inner value through `Cell::get`, which needs `T: Copy`.
//!
//! `UnsafeCell<T>` has no safe way to read its contents: the impl reads through
//! `UnsafeCell::get` and therefore assumes **no `&mut` into the cell is live** for
//! the duration of the (de)serialization. Reading a cell that is concurrently
//! mutated is undefined behaviour the caller accepts — this is exactly why the
//! `unsafe_cell` impl is feature-gated and off by default.

use core::alloc::Allocator;
#[cfg(feature = "cell")]
use core::cell::Cell;
#[cfg(feature = "unsafe_cell")]
use core::cell::UnsafeCell;

use alloc::vec::Vec;

use sdave::{FlatDeserializer, LimiterPair, Offset, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    name::{NamePolicy, TypeName},
    serialize::{Name, Serialize, Strategy},
};

#[cfg(feature = "cell")]
impl<T> TypeName for Cell<T>
where
    T: TypeName,
{
    impl_type_name_forwarded!(T);
}

#[cfg(feature = "cell")]
impl<T, A> Serialize<u8, A> for Cell<T>
where
    T: Copy + Serialize<u8, A>,
    A: Allocator + Clone,
{
    const SLICES: usize = <T as Serialize<u8, A>>::SLICES;
    type Slices = <T as Serialize<u8, A>>::Slices;
    type SlicesTyped = <T as Serialize<u8, A>>::SlicesTyped;

    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8, A>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, A>],
    ) -> Result<Self::Slices, Error> {
        let value = self.get();
        unsafe {
            <T as Serialize<u8, A>>::serialize::<N>(
                &value,
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
        let value = self.get();
        unsafe {
            <T as Serialize<u8, A>>::serialize_typed::<N>(
                &value,
                buffer,
                name,
                variant,
                strategy,
                limiter_pairs,
            )
        }
    }
}

#[cfg(feature = "cell")]
impl<T, A> Deserialize<u8, A> for Cell<T>
where
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
{
    type Type = Cell<T>;

    fn deserialize<N: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error> {
        let value =
            <T as Deserialize<u8, A>>::deserialize::<N>(deserializer, final_buffer, allocator)?;
        Ok(Cell::new(value))
    }
}

#[cfg(feature = "unsafe_cell")]
impl<T> TypeName for UnsafeCell<T>
where
    T: TypeName,
{
    impl_type_name_forwarded!(T);
}

#[cfg(feature = "unsafe_cell")]
impl<T, A> Serialize<u8, A> for UnsafeCell<T>
where
    T: Serialize<u8, A>,
    A: Allocator + Clone,
{
    const SLICES: usize = <T as Serialize<u8, A>>::SLICES;
    type Slices = <T as Serialize<u8, A>>::Slices;
    type SlicesTyped = <T as Serialize<u8, A>>::SlicesTyped;

    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8, A>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, A>],
    ) -> Result<Self::Slices, Error> {
        // SAFETY: no `&mut` into this cell is live (see the module note).
        let value: &T = unsafe { &*self.get() };
        unsafe {
            <T as Serialize<u8, A>>::serialize::<N>(value, buffer, variant, strategy, limiter_pairs)
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
        // SAFETY: no `&mut` into this cell is live (see the module note).
        let value: &T = unsafe { &*self.get() };
        unsafe {
            <T as Serialize<u8, A>>::serialize_typed::<N>(
                value,
                buffer,
                name,
                variant,
                strategy,
                limiter_pairs,
            )
        }
    }
}

#[cfg(feature = "unsafe_cell")]
impl<T, A> Deserialize<u8, A> for UnsafeCell<T>
where
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
{
    type Type = UnsafeCell<T>;

    fn deserialize<N: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error> {
        let value =
            <T as Deserialize<u8, A>>::deserialize::<N>(deserializer, final_buffer, allocator)?;
        Ok(UnsafeCell::new(value))
    }
}
