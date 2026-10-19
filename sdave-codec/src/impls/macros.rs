//! `macro_rules!` helpers shared by the impl modules of this crate.
//!
//! [`super`] `#[macro_use]`-includes this module *before* declaring its submodules,
//! because `macro_rules!` is scoped textually: a macro is only visible to the
//! modules declared after it.
//!
//! Every macro here describes one wire shape that several concrete types share, so
//! each shape lives in exactly one place. Two conventions keep the call sites
//! minimal:
//!
//! - the expansions spell every path in full (`crate::Serialize`,
//!   `alloc::vec::Vec`, …), so a module that invokes one needs no `use` of its own;
//! - the macros that emit *associated items* rather than whole impls are invoked
//!   inside the `impl` block the caller writes, which keeps the generics and bounds
//!   of the wrapper visible at the point of use.
//!
//! One user sits outside this subtree: `iterable` implements the same slice/array
//! shapes as the heap containers do, so it imports `impl_iterable_serialize` by
//! path (see its re-export at the end of this file).

/// The `TypeName` body of a wrapper that has no wire name of its own: it renders
/// its referent `$target` instead, so `Rc<Vec<u32>>` reads exactly like
/// `Vec<u32>`.
///
/// This MUST NOT be left to the trait's default `render`: the default shortens
/// `Self::FULL`, which would drop a composed referent's own arguments.
macro_rules! impl_type_name_forwarded {
    ($target:ty) => {
        const FULL: &'static str = <$target as crate::TypeName>::FULL;

        fn render<N: crate::NamePolicy>(f: &mut dyn ::core::fmt::Write) -> ::core::fmt::Result {
            <$target as crate::TypeName>::render::<N>(f)
        }
    };
}

/// The `TypeName` body of a container rendered as `FULL<argument, …>`, e.g.
/// `alloc::vec::Vec<u32>`.
///
/// Names are composed per node: `FULL` is shortened by the policy, and each
/// argument is rendered — through the same policy — on its own, so shortening one
/// never propagates across a level.
///
/// Gated on the features whose containers use it, so a lean build does not carry an
/// unused macro.
#[cfg(any(
    feature = "vec",
    feature = "vec_deque",
    feature = "linked_list",
    feature = "btree_set",
    feature = "option",
    feature = "result",
))]
macro_rules! impl_type_name_args {
    ($full:literal, <$($argument:ident),+>) => {
        const FULL: &'static str = $full;

        fn render<N: crate::NamePolicy>(
            f: &mut dyn ::core::fmt::Write,
        ) -> ::core::fmt::Result {
            ::core::fmt::Write::write_str(
                f,
                <N as crate::NamePolicy>::shorten(Self::FULL),
            )?;
            ::core::fmt::Write::write_str(f, "<")?;
            let mut __first = true;
            $(
                if !__first {
                    ::core::fmt::Write::write_str(f, ", ")?;
                }
                __first = false;
                <$argument as crate::TypeName>::render::<N>(f)?;
            )+
            ::core::fmt::Write::write_str(f, ">")
        }
    };
}

/// Implement `Serialize` for a pointer-like type by forwarding to its referent, so
/// a `&T`/`Box<T, A>` field serializes exactly like what it points at.
///
/// Every listed type reaches its `T` through [`Deref`](core::ops::Deref), which
/// keeps the forwarding expression identical for all of them. The `TypeName` impl
/// stays with the caller (its generics differ: `&T` and `Rc<T>` carry no allocator
/// parameter, whereas `Box<T, A>` does).
macro_rules! impl_deref_serialize {
    ($($ty:ty),* $(,)?) => {
        $(
            impl<T, A> crate::Serialize<u8, A> for $ty
            where
                T: ?Sized + crate::Serialize<u8, A>,
                A: ::core::alloc::Allocator + ::core::clone::Clone,
            {
                const SLICES: usize = <T as crate::Serialize<u8, A>>::SLICES;
                type Slices = <T as crate::Serialize<u8, A>>::Slices;
                type SlicesTyped = <T as crate::Serialize<u8, A>>::SlicesTyped;

                unsafe fn serialize<N: crate::NamePolicy>(
                    &self,
                    buffer: &mut alloc::vec::Vec<u8, A>,
                    variant: crate::Variant,
                    strategy: crate::Strategy,
                    limiter_pairs: &mut [crate::LimiterPair<'_, u8, A>],
                ) -> ::core::result::Result<Self::Slices, crate::Error> {
                    let target: &T = ::core::ops::Deref::deref(self);
                    unsafe {
                        <T as crate::Serialize<u8, A>>::serialize::<N>(
                            target,
                            buffer,
                            variant,
                            strategy,
                            limiter_pairs,
                        )
                    }
                }

                unsafe fn serialize_typed<N: crate::NamePolicy>(
                    &self,
                    buffer: &mut alloc::vec::Vec<u8, A>,
                    name: crate::Name,
                    variant: crate::Variant,
                    strategy: crate::Strategy,
                    limiter_pairs: &mut [crate::LimiterPair<'_, u8, A>],
                ) -> ::core::result::Result<Self::SlicesTyped, crate::Error> {
                    let target: &T = ::core::ops::Deref::deref(self);
                    unsafe {
                        <T as crate::Serialize<u8, A>>::serialize_typed::<N>(
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
        )*
    };
}

/// Implement `TypeName`, `Serialize` and `Deserialize` for a `Global`-backed shared
/// pointer (`Rc<T>`, `Arc<T>`): wire-identical to its referent `T`, and rebuilt
/// with `$new` on the way back.
///
/// `T` MUST be the referent parameter of `$ty`; `$new` takes the deserialized `T`
/// and returns `$ty`.
///
/// Gated on the features whose pointers use it, so a lean build does not carry an
/// unused macro.
#[cfg(any(feature = "rc", feature = "arc"))]
macro_rules! impl_shared_ptr {
    ($ty:ty => $new:path) => {
        impl<T> crate::TypeName for $ty
        where
            T: ?Sized + crate::TypeName,
        {
            impl_type_name_forwarded!(T);
        }

        impl_deref_serialize!($ty);

        impl<T, A> crate::Deserialize<u8, A> for $ty
        where
            T: crate::Deserialize<u8, A, Type = T>,
            A: ::core::alloc::Allocator + ::core::clone::Clone,
        {
            type Type = $ty;

            fn deserialize<N: crate::NamePolicy>(
                deserializer: &mut crate::FlatDeserializer<'_, '_, u8, A>,
                final_buffer: ::core::option::Option<crate::Offset>,
                allocator: A,
            ) -> ::core::result::Result<Self::Type, crate::Error> {
                let value = <T as crate::Deserialize<u8, A>>::deserialize::<N>(
                    deserializer,
                    final_buffer,
                    allocator,
                )?;
                ::core::result::Result::Ok($new(value))
            }
        }
    };
}

/// The `serialize_typed` method shared by every codec impl whose `SlicesTyped` is
/// the standard 6-slot array: write the `name: type` signature NonEnvelop,
/// serialize `self` through its own `serialize`, then wrap both in one outer
/// Envelop.
///
/// The surrounding impl MUST name its allocator parameter `A` and declare
/// `type SlicesTyped = [Option<(Offset, Offset)>; 6]`.
macro_rules! generic_serialize_typed {
    () => {
        unsafe fn serialize_typed<N: crate::NamePolicy>(
            &self,
            buffer: &mut alloc::vec::Vec<u8, A>,
            name: crate::Name,
            variant: crate::Variant,
            strategy: crate::Strategy,
            limiter_pairs: &mut [crate::LimiterPair<'_, u8, A>],
        ) -> ::core::result::Result<Self::SlicesTyped, crate::Error> {
            let mut out: Self::SlicesTyped = [::core::option::Option::None; 6];
            unsafe {
                crate::frame_typed_generic::<N, Self, A>(
                    buffer,
                    name,
                    self,
                    variant,
                    strategy,
                    limiter_pairs,
                    &mut out,
                )?;
            }
            ::core::result::Result::Ok(out)
        }
    };
}

/// Implement the `Serialize` half of an *iterable*: a container whose payload is a
/// leading empty marker Envelop followed by one Envelop per element (see
/// [`crate::iterable`]), so the whole container is still one Envelop, i.e. one
/// `[limiter, payload, delimiter]` triple.
///
/// Six types spell that shape the same way — the unsized slice `[T]`, the array
/// `[T; N]` and the four heap containers of `collections` — so they share this macro
/// and differ only in their declarations. `$policy` is the
/// [`NamePolicy`](crate::NamePolicy) parameter name, passed in because an array's
/// const generic already claims `N`.
///
/// `str` is deliberately NOT an iterable: it is the raw-text leaf shape.
macro_rules! impl_iterable_serialize {
    ($ty:ty, $policy:ident $(, const $N:ident)?) => {
        impl<T, A $(, const $N: usize)?> crate::Serialize<u8, A> for $ty
        where
            T: crate::Serialize<u8, A> + crate::TypeName,
            <T as crate::Serialize<u8, A>>::Slices: ::core::convert::AsRef<
                [::core::option::Option<(crate::Offset, crate::Offset)>],
            >,
            A: ::core::alloc::Allocator + ::core::clone::Clone,
        {
            const SLICES: usize = 3;
            type Slices = crate::IterableSlices;
            type SlicesTyped = crate::IterableSlices;

            unsafe fn serialize<$policy: crate::NamePolicy>(
                &self,
                buffer: &mut alloc::vec::Vec<u8, A>,
                variant: crate::Variant,
                strategy: crate::Strategy,
                limiter_pairs: &mut [crate::LimiterPair<'_, u8, A>],
            ) -> ::core::result::Result<Self::Slices, crate::Error> {
                unsafe {
                    crate::serialize_iterable::<$policy, T, A, _>(
                        buffer,
                        self.iter(),
                        variant,
                        strategy,
                        limiter_pairs,
                    )
                }
            }

            unsafe fn serialize_typed<$policy: crate::NamePolicy>(
                &self,
                buffer: &mut alloc::vec::Vec<u8, A>,
                name: crate::Name,
                variant: crate::Variant,
                strategy: crate::Strategy,
                limiter_pairs: &mut [crate::LimiterPair<'_, u8, A>],
            ) -> ::core::result::Result<Self::SlicesTyped, crate::Error> {
                unsafe {
                    crate::serialize_iterable_typed::<$policy, Self, T, A, _>(
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
    };
}

// Path-based, because `iterable` is a sibling of `impls` rather than a submodule of
// it: `macro_rules!` is textually scoped, and the `#[macro_use]` in `impls` only
// reaches the submodules declared below it.
pub(crate) use impl_iterable_serialize;
