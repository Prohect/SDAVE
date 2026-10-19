//! Heap-container iterables, one feature each: `vec` (`Vec`), `vec_deque`
//! (`VecDeque`), `linked_list` (`LinkedList`) and `btree_set` (`BTreeSet`).
//!
//! A container is an iterable in exactly the sense of [`crate::iterable`]: one outer
//! Envelop whose payload is a leading empty marker Envelop followed by one Envelop
//! per element. The write half is the shared `impl_iterable_serialize!` of
//! [`crate::impls::macros`], which the slice `[T]` and the array `[T; N]` use too; this
//! file adds the `TypeName` each container needs and the read half, which pushes every
//! decoded element straight into the container.
//!
//! These impls are enumerated rather than blanket, because a blanket
//! `impl<T: IntoIterator> Serialize for T` would overlap every impl on a foreign
//! type (`u32`, `Vec`, `[T; N]`, …) and is rejected by coherence.
//!
//! Only the buffers the codec allocates are allocator-parameterised: a `Vec<T, A>`
//! container is built in the caller's `A`, while `VecDeque<T>`/`LinkedList<T>`/
//! `BTreeSet<T>` are `Global`-backed by construction (`VecDeque<T, A>` would
//! require the unstable `allocator_ext` feature).

// Only the `Vec` impls name the allocator, so both of these are gated on that feature:
// the other three containers are `Global`-backed by construction.
#[cfg(feature = "vec")]
use core::alloc::Allocator;

#[cfg(feature = "vec")]
use alloc::vec::Vec;

#[cfg(feature = "btree_set")]
use alloc::collections::BTreeSet;
#[cfg(feature = "linked_list")]
use alloc::collections::LinkedList;
#[cfg(feature = "vec_deque")]
use alloc::collections::VecDeque;

use crate::TypeName;

/// Implement the `Deserialize` half of an iterable container.
///
/// The elements are decoded through [`crate::visit_iterable_elements`] and pushed
/// straight into the container, so no intermediate `Vec` is built.
///
/// `$new` takes the caller's allocator and returns the empty container, so an
/// allocator-parameterised container can build in it while a `Global`-backed one
/// ignores it; `$push` is how an element is added (and `$extra` the extra bound it
/// needs, e.g. `Ord` for `BTreeSet`).
macro_rules! impl_container_deserialize {
    ($ty:ty, $push:ident, $new:expr $(, $extra:path)?) => {
        impl<T, A> crate::Deserialize<u8, A> for $ty
        where
            T: crate::Deserialize<u8, A, Type = T> $(+ $extra)?,
            A: ::core::alloc::Allocator + ::core::clone::Clone,
        {
            type Type = $ty;

            fn deserialize<N: crate::NamePolicy>(
                deserializer: &mut crate::FlatDeserializer<'_, '_, u8, A>,
                final_buffer: ::core::option::Option<crate::Offset>,
                allocator: A,
            ) -> ::core::result::Result<Self::Type, crate::Error> {
                let mut out: $ty = ($new)(allocator.clone());
                unsafe {
                    crate::visit_iterable_elements::<N, T, A, _>(
                        deserializer,
                        final_buffer,
                        &allocator,
                        |element| {
                            out.$push(element);
                            ::core::result::Result::Ok(())
                        },
                    )?;
                }
                ::core::result::Result::Ok(out)
            }
        }
    };
}

// Per container: a `TypeName` impl (only `Vec` carries an allocator parameter), the
// shared iterable `Serialize` pair, then the `Deserialize` half.

#[cfg(feature = "vec")]
impl<T, A> TypeName for Vec<T, A>
where
    T: TypeName,
    A: Allocator,
{
    impl_type_name_args!("alloc::vec::Vec", <T>);
}

#[cfg(feature = "vec")]
impl_iterable_serialize!(Vec<T, A>, N);

#[cfg(feature = "vec")]
impl_container_deserialize!(Vec<T, A>, push, Vec::new_in);

#[cfg(feature = "vec_deque")]
impl<T> TypeName for VecDeque<T>
where
    T: TypeName,
{
    impl_type_name_args!("alloc::collections::VecDeque", <T>);
}

#[cfg(feature = "vec_deque")]
impl_iterable_serialize!(VecDeque<T>, N);

#[cfg(feature = "vec_deque")]
impl_container_deserialize!(VecDeque<T>, push_back, |_allocator| VecDeque::new());

#[cfg(feature = "linked_list")]
impl<T> TypeName for LinkedList<T>
where
    T: TypeName,
{
    impl_type_name_args!("alloc::collections::LinkedList", <T>);
}

#[cfg(feature = "linked_list")]
impl_iterable_serialize!(LinkedList<T>, N);

#[cfg(feature = "linked_list")]
impl_container_deserialize!(LinkedList<T>, push_back, |_allocator| LinkedList::new());

#[cfg(feature = "btree_set")]
impl<T> TypeName for BTreeSet<T>
where
    T: TypeName,
{
    impl_type_name_args!("alloc::collections::BTreeSet", <T>);
}

#[cfg(feature = "btree_set")]
impl_iterable_serialize!(BTreeSet<T>, N);

#[cfg(feature = "btree_set")]
impl_container_deserialize!(BTreeSet<T>, insert, |_allocator| BTreeSet::new(), Ord);
