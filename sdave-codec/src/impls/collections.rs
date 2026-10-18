//! Heap-container iterables, one feature each: `vec` (`Vec`), `vec_deque`
//! (`VecDeque`), `linked_list` (`LinkedList`) and `btree_set` (`BTreeSet`).
//!
//! A container is an iterable in exactly the sense of `crate::iterable`: one outer
//! Envelop whose payload is a leading empty marker Envelop followed by one Envelop
//! per element. The slice impl (`[T]`) and the array impl (`[T; N]`) are always
//! available.
//!
//! These impls are enumerated rather than blanket, because a blanket
//! `impl<T: IntoIterator> Serialize for T` would overlap every impl on a foreign
//! type (`u32`, `Vec`, `[T; N]`, …) and is rejected by coherence.
//!
//! Only the buffers the codec allocates are allocator-parameterised: a `Vec<T, A>`
//! container is built in the caller's `A`, while `VecDeque<T>`/`LinkedList<T>`/
//! `BTreeSet<T>` are `Global`-backed by construction (`VecDeque<T, A>` would
//! require the unstable `allocator_ext` feature).

use core::alloc::Allocator;

use alloc::vec::Vec;

#[cfg(feature = "btree_set")]
use alloc::collections::BTreeSet;
#[cfg(feature = "linked_list")]
use alloc::collections::LinkedList;
#[cfg(feature = "vec_deque")]
use alloc::collections::VecDeque;

use sdave::{FlatDeserializer, LimiterPair, Offset, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    impls::visit_iterable_elements,
    iterable::{IterableSlices, serialize_iterable, serialize_iterable_typed},
    name::{NamePolicy, TypeName},
    serialize::{Name, Serialize, Strategy},
};

/// Implement `Serialize` + `Deserialize` for a heap container.
///
/// `$new` takes the caller's allocator and returns the empty container, so an
/// allocator-parameterised container can build in it while a `Global`-backed one
/// ignores it; `$push` is how an element is added (and `$extra` the extra bound it
/// needs, e.g. `Ord` for `BTreeSet`).
macro_rules! impl_container {
    ($ty:ty, $push:ident, $new:expr $(, $extra:path)?) => {
        impl<T, A> Serialize<u8, A> for $ty
        where
            T: Serialize<u8, A> + TypeName,
            <T as Serialize<u8, A>>::Slices: AsRef<[Option<(Offset, Offset)>]>,
            A: Allocator + Clone,
        {
            const SLICES: usize = 3;
            type Slices = IterableSlices;
            type SlicesTyped = IterableSlices;

            unsafe fn serialize<N: NamePolicy>(
                &self,
                buffer: &mut Vec<u8, A>,
                variant: Variant,
                strategy: Strategy,
                limiter_pairs: &mut [LimiterPair<'_, u8, A>],
            ) -> Result<Self::Slices, Error> {
                unsafe {
                    serialize_iterable::<N, T, A, _>(
                        buffer,
                        self.iter(),
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
                    serialize_iterable_typed::<N, Self, T, A, _>(
                        buffer,
                        self.iter(),
                        name,
                        variant,
                        strategy,
                        limiter_pairs,
                    )
                }
            }
        }

        impl<T, A> Deserialize<u8, A> for $ty
        where
            T: Deserialize<u8, A, Type = T> $(+ $extra)?,
            A: Allocator + Clone,
        {
            type Type = $ty;

            fn deserialize<N: NamePolicy>(
                deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
                final_buffer: Option<Offset>,
                allocator: A,
            ) -> Result<Self::Type, Error> {
                // Push every element straight into the container: decoding into an
                // intermediate `Vec` first would cost a second allocation for nothing.
                let mut out: $ty = ($new)(allocator.clone());
                unsafe {
                    visit_iterable_elements::<N, T, A, _>(
                        deserializer,
                        final_buffer,
                        &allocator,
                        |element| {
                            out.$push(element);
                            Ok(())
                        },
                    )?;
                }
                Ok(out)
            }
        }
    };
}

// Per container: a `TypeName` impl (only `Vec` carries an allocator parameter),
// then the shared `Serialize`/`Deserialize` pair.

#[cfg(feature = "vec")]
impl<T, A> TypeName for Vec<T, A>
where
    T: TypeName,
    A: Allocator,
{
    impl_type_name_args!("alloc::vec::Vec", <T>);
}

#[cfg(feature = "vec")]
impl_container!(Vec<T, A>, push, Vec::new_in);

#[cfg(feature = "vec_deque")]
impl<T> TypeName for VecDeque<T>
where
    T: TypeName,
{
    impl_type_name_args!("alloc::collections::VecDeque", <T>);
}

#[cfg(feature = "vec_deque")]
impl_container!(VecDeque<T>, push_back, |_allocator| VecDeque::new());

#[cfg(feature = "linked_list")]
impl<T> TypeName for LinkedList<T>
where
    T: TypeName,
{
    impl_type_name_args!("alloc::collections::LinkedList", <T>);
}

#[cfg(feature = "linked_list")]
impl_container!(LinkedList<T>, push_back, |_allocator| LinkedList::new());

#[cfg(feature = "btree_set")]
impl<T> TypeName for BTreeSet<T>
where
    T: TypeName,
{
    impl_type_name_args!("alloc::collections::BTreeSet", <T>);
}

#[cfg(feature = "btree_set")]
impl_container!(BTreeSet<T>, insert, |_allocator| BTreeSet::new(), Ord);
