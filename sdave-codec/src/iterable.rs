//! `Serialize`/`Deserialize` for iterables: `Vec<T, A>`, slices `[T]` and fixed
//! arrays `[T; N]`.
//!
//! The crate `README.md` describes the wire shape of an iterable:
//!
//! ```text
//! (NonEnvelop(Signature) ++) Envelop(empty payload) ++ Envelop(element0) ++ Envelop(element1) ++ ...
//! ```
//!
//! Those Envelops together form the payload of ONE outer Envelop, so a
//! [`Serialize::serialize`] call still returns exactly one limiter/value/delimiter
//! triple; the type marker is NOT duplicated for each element. The leading empty
//! marker Envelop distinguishes the real empty `[]` from the semantic empty
//! `[""]`.
//!
//! The per-element logical slices cannot be kept in one fixed-size array, so they
//! are stacked on native stack frames with [`Stack`] (no heap) and flattened in
//! place once every element has been serialized.

use core::{alloc::Allocator, cell::RefCell};

use alloc::vec::Vec;

use sdave::{FlatDeserializer, LimiterPair, Offset, Stack, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    impls::deserialize_iterable_elements,
    name::{NamePolicy, TypeName},
    serialize::{
        Name, Serialize, Strategy, flatten_in_place, frame_region, frame_slice,
        write_type_signature,
    },
};

/// Every iterable serialization is one outer Envelop:
/// `[limiter, payload, delimiter]`.
pub type IterableSlices = [Option<(Offset, Offset)>; 3];

impl<T> TypeName for [T]
where
    T: TypeName,
{
    const FULL: &'static str = "[T]";

    fn render<N: NamePolicy>(f: &mut dyn core::fmt::Write) -> core::fmt::Result {
        f.write_str("[")?;
        <T as TypeName>::render::<N>(f)?;
        f.write_str("]")
    }
}

impl<T, const N: usize> TypeName for [T; N]
where
    T: TypeName,
{
    const FULL: &'static str = "[T; N]";

    // `P`, not `N`: the policy parameter must not shadow the array's length.
    fn render<P: NamePolicy>(f: &mut dyn core::fmt::Write) -> core::fmt::Result {
        f.write_str("[")?;
        <T as TypeName>::render::<P>(f)?;
        f.write_str("; ")?;
        write!(f, "{}", N)?;
        f.write_str("]")
    }
}

/// Frame `elements` as one Envelop whose payload is
/// `buffer[payload_head..] ++ Envelop(empty payload) ++ Envelop(element0) ++ ...`.
///
/// `buffer[payload_head..]` is the already serialized `Signature` (possibly
/// empty) and becomes part of the payload, e.g. the `name: type` NonEnvelop of a
/// [`Serialize::serialize_typed`] call. This is the primitive underlying
/// [`serialize_iterable`] and [`serialize_iterable_typed`].
///
/// # Safety
/// Same contract as [`sdave::can_serialize`]: the tail of `buffer[..payload_head]`
/// MUST be a clean item boundary.
pub unsafe fn frame_iterable<'e, N, E, A, I>(
    buffer: &mut Vec<u8, A>,
    payload_head: usize,
    elements: I,
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, u8, A>],
) -> Result<IterableSlices, Error>
where
    N: NamePolicy,
    E: Serialize<u8, A> + 'e,
    <E as Serialize<u8, A>>::Slices: AsRef<[Option<(Offset, Offset)>]>,
    A: Allocator + Clone,
    I: IntoIterator<Item = &'e E>,
{
    // The leading empty marker Envelop: `[]` serializes to just this Envelop,
    // while `[""]` serializes to this Envelop followed by one empty element
    // Envelop. An empty payload is already in logical order, so no flattening is
    // needed for it.
    unsafe { frame_slice(buffer, &[], variant, strategy, limiter_pairs)? };
    // Head of the (still unflattened) elements region.
    let unflattened_start = buffer.len();

    /// `produce`/`consume` state shared across the [`Stack::with`] recursion.
    ///
    /// Every element is serialized unflattened through `produce`; once the stack
    /// is complete, `consume` flattens the whole elements region in place with a
    /// single memmove. Both closures need `buffer`, and [`Stack::with`] holds them
    /// at the same time, hence the `RefCell`.
    struct Ctx<'v, 'l, A>
    where
        A: Allocator + Clone,
    {
        buffer: &'v mut Vec<u8, A>,
        limiter_pairs: &'v mut [LimiterPair<'l, u8, A>],
        variant: Variant,
        strategy: Strategy,
        unflattened_start: usize,
        error: Option<Error>,
    }

    impl<A> Ctx<'_, '_, A>
    where
        A: Allocator + Clone,
    {
        /// Serialize the next element (unflattened) and hand its logical slices
        /// back to the [`Stack`].
        ///
        /// Returning `None` ends the stack: either the elements are exhausted, or
        /// this call just failed and recorded the `error` (the recursion stops
        /// there, so a later call never sees a set `error`).
        fn produce<'e, N, E, I>(
            &mut self,
            elements: &mut I,
        ) -> Option<<E as Serialize<u8, A>>::Slices>
        where
            N: NamePolicy,
            E: Serialize<u8, A> + 'e,
            <E as Serialize<u8, A>>::Slices: AsRef<[Option<(Offset, Offset)>]>,
            I: Iterator<Item = &'e E>,
        {
            let element = elements.next()?;
            match unsafe {
                <E as Serialize<u8, A>>::serialize::<N>(
                    element,
                    self.buffer,
                    self.variant,
                    self.strategy,
                    self.limiter_pairs,
                )
            } {
                Ok(slices) => Some(slices),
                Err(error) => {
                    self.error = Some(error);
                    None
                }
            }
        }

        /// Flatten the whole elements region in place: concatenate every element's
        /// logical slices (in producer order) down to `unflattened_start` and drop
        /// the unflattened region.
        fn consume<E>(&mut self, stack: &Stack<<E as Serialize<u8, A>>::Slices>)
        where
            E: Serialize<u8, A>,
            <E as Serialize<u8, A>>::Slices: AsRef<[Option<(Offset, Offset)>]>,
        {
            if self.error.is_some() {
                // A half-serialized region MUST NOT be flattened: leave the buffer
                // alone and let the caller report the error.
                return;
            }
            let unflattened_start = self.unflattened_start;
            unsafe {
                flatten_in_place(
                    self.buffer,
                    unflattened_start,
                    stack
                        .iter()
                        .flat_map(|slices| slices.as_ref().iter().copied()),
                );
            }
        }
    }

    let ctx = RefCell::new(Ctx {
        buffer: &mut *buffer,
        limiter_pairs: &mut *limiter_pairs,
        variant,
        strategy,
        unflattened_start,
        error: None,
    });
    unsafe {
        Stack::with(
            elements.into_iter(),
            |elements| ctx.borrow_mut().produce::<N, E, _>(elements),
            |stack: &Stack<<E as Serialize<u8, A>>::Slices>, _: &mut ()| {
                ctx.borrow_mut().consume::<E>(stack);
            },
            // The consumer has no state of its own: both closures share `ctx`.
            &mut (),
        );
    }
    if let Some(error) = ctx.into_inner().error {
        return Err(error);
    }

    // The trailing marker+element Envelops, now flattened into one region, are
    // `buffer[payload_head..]`; frame that region as the outer Envelop.
    unsafe { frame_region(buffer, payload_head, variant, strategy, limiter_pairs) }
}

/// Frame `elements` as one iterable Envelop written to the end of `buffer`.
///
/// This is the shared body of the `Vec<T, A>` / `[T]` / `[T; N]` `serialize`
/// impls, exposed for hand-written iterable-like impls.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`]: the tail of `buffer` MUST be a clean
/// item boundary.
pub unsafe fn serialize_iterable<'e, N, E, A, I>(
    buffer: &mut Vec<u8, A>,
    elements: I,
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, u8, A>],
) -> Result<IterableSlices, Error>
where
    N: NamePolicy,
    E: Serialize<u8, A> + 'e,
    <E as Serialize<u8, A>>::Slices: AsRef<[Option<(Offset, Offset)>]>,
    A: Allocator + Clone,
    I: IntoIterator<Item = &'e E>,
{
    let head = buffer.len();
    unsafe {
        frame_iterable::<N, E, A, I>(buffer, head, elements, variant, strategy, limiter_pairs)
    }
}

/// Frame `elements` as one typed iterable Envelop: write the `name: type`
/// signature (rendering `S`), then the iterable Envelop.
///
/// This is the shared body of the `Vec<T, A>` / `[T]` / `[T; N]`
/// `serialize_typed` impls, exposed for hand-written iterable-like impls.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`]: the tail of `buffer` MUST be a clean
/// item boundary.
pub unsafe fn serialize_iterable_typed<'e, N, S, E, A, I>(
    buffer: &mut Vec<u8, A>,
    elements: I,
    name: Name,
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, u8, A>],
) -> Result<IterableSlices, Error>
where
    N: NamePolicy,
    S: TypeName + ?Sized,
    E: Serialize<u8, A> + 'e,
    <E as Serialize<u8, A>>::Slices: AsRef<[Option<(Offset, Offset)>]>,
    A: Allocator + Clone,
    I: IntoIterator<Item = &'e E>,
{
    let (signature_head, _) = write_type_signature::<N, S, _>(buffer, name)?;
    unsafe {
        frame_iterable::<N, E, A, I>(
            buffer,
            signature_head.get(),
            elements,
            variant,
            strategy,
            limiter_pairs,
        )
    }
}

/// Implement [`Serialize`] for an iterable: the unsized slice `[T]` (so `&[T]` and
/// `Box<[T], A>` reach it through the codec's `&T`/`Box<T, A>` forwarding) and the
/// fixed-size array `[T; N]`.
///
/// An array carries the same wire shape as a slice — it is `[T]` with a statically
/// known count — so the two impls differ only in their generic parameters, which is
/// why `$policy` (the [`NamePolicy`] parameter name) is passed in: an array's const
/// generic already claims `N`.
/// `str` is deliberately NOT an iterable: it is the raw-text leaf shape.
macro_rules! impl_iterable_serialize {
    ($ty:ty, $policy:ident $(, const $N:ident)?) => {
        impl<T, A $(, const $N: usize)?> Serialize<u8, A> for $ty
        where
            T: Serialize<u8, A> + TypeName,
            <T as Serialize<u8, A>>::Slices: AsRef<[Option<(Offset, Offset)>]>,
            A: Allocator + Clone,
        {
            const SLICES: usize = 3;
            type Slices = IterableSlices;
            type SlicesTyped = IterableSlices;

            unsafe fn serialize<$policy: NamePolicy>(
                &self,
                buffer: &mut Vec<u8, A>,
                variant: Variant,
                strategy: Strategy,
                limiter_pairs: &mut [LimiterPair<'_, u8, A>],
            ) -> Result<Self::Slices, Error> {
                unsafe {
                    serialize_iterable::<$policy, T, A, _>(
                        buffer,
                        self.iter(),
                        variant,
                        strategy,
                        limiter_pairs,
                    )
                }
            }

            unsafe fn serialize_typed<$policy: NamePolicy>(
                &self,
                buffer: &mut Vec<u8, A>,
                name: Name,
                variant: Variant,
                strategy: Strategy,
                limiter_pairs: &mut [LimiterPair<'_, u8, A>],
            ) -> Result<Self::SlicesTyped, Error> {
                unsafe {
                    serialize_iterable_typed::<$policy, Self, T, A, _>(
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

impl_iterable_serialize!([T], N);
impl_iterable_serialize!([T; N], P, const N);

impl<T, A, const N: usize> Deserialize<u8, A> for [T; N]
where
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
{
    type Type = [T; N];

    // `P`, not `N`: the policy parameter must not shadow the array's length.
    fn deserialize<P: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error> {
        let elements = unsafe {
            deserialize_iterable_elements::<P, T, A>(deserializer, final_buffer, allocator)?
        };
        if elements.len() != N {
            return Err(Error::UnexpectedItem);
        }
        // The length was checked above, so `next` is never `None` within `N` calls.
        let mut elements = elements.into_iter();
        Ok(core::array::from_fn(|_| {
            elements.next().expect("array length checked")
        }))
    }
}
