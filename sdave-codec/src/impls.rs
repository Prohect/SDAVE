//! The codec's always-on impls — [`Serialize`]/[`Deserialize`] for `str`, the
//! numbers, `bool`, `char`, tuples, `()` and the pointer forwarding.
//!
//! Every wire shape lives in exactly one place, so this module is a map rather than a
//! grab bag. Its submodules are:
//!
//! - `format` — the heap-free primitive formatter;
//! - `channel` — everything that reads a buffer back;
//! - the feature-gated impls: `primitive` (the readable numbers, `bool` and `char`),
//!   `collections` (the heap containers), `option`, `result`, `boxed`, `rc`, `arc`,
//!   `cow`, `cell` (which holds both `Cell` and `UnsafeCell`) and `string`, plus
//!   `tagged`, the one wire shape `option` and `result` share.
//!
//! See the crate `README.md` for the feature table.

// Must come first: the macros it defines are invoked by the submodules below.
#[macro_use]
mod macros;

#[cfg(feature = "arc")]
mod arc;
#[cfg(feature = "boxed")]
mod boxed;
#[cfg(any(feature = "cell", feature = "unsafe_cell"))]
mod cell;
#[cfg(any(
    feature = "vec",
    feature = "vec_deque",
    feature = "linked_list",
    feature = "btree_set",
))]
mod collections;
#[cfg(feature = "cow")]
mod cow;
#[cfg(feature = "option")]
mod option;
#[cfg(feature = "rc")]
mod rc;
#[cfg(feature = "result")]
mod result;
#[cfg(feature = "string")]
mod string;
#[cfg(any(feature = "option", feature = "result"))]
mod tagged;

use core::alloc::Allocator;

use alloc::vec::Vec;

use sdave::{Item, LimiterPair, Offset, Variant};

use crate::{
    BufferWriter, Error,
    deserialize::Deserialize,
    name::{NamePolicy, TypeName},
    serialize::{
        Name, Serialize, Strategy, flatten_in_place, frame_region, frame_slice, frame_slice_typed,
        leaf_signature,
    },
};

// ---------------------------------------------------------------------------
// Submodules.
//
// The two helper groups this file used to grow are their own files, re-exported here
// so every `crate::impls::…` path keeps working: `format` is the heap-free primitive
// formatter, `channel` is everything that reads a buffer back.
// ---------------------------------------------------------------------------

mod channel;
mod format;

pub use channel::*;
pub use format::*;

// ---------------------------------------------------------------------------
// Primitives.
//
// Two groups, on opposite sides of the `primitive` feature:
//
// - the numbers, `bool` and `char` are *readable*: the payload is the value's
//   `Display` text, read back with `FromStr`. That is a choice, not a given, so the
//   feature that carries it (on by default) can be turned off — an application that
//   only needs the bytes brings a leaf of its own, and a field that still names `u32`
//   then fails to compile rather than silently shipping text. Containers are
//   unaffected: `[T; N]` is structural and always on, but it carries whatever leaf it
//   is given, so `[u8; N]` follows `u8` while `[YourLeaf; N]` follows yours.
// - `str` is raw: its payload IS the UTF-8 bytes, with no formatting or parsing in
//   either direction, so it stays available whatever the feature selection.
// ---------------------------------------------------------------------------

/// Parse a scalar value from its Envelop payload text.
///
/// Implemented for the codec's readable primitives (the `primitive` feature); the
/// integer impls distinguish a well-formed but out-of-range decimal
/// ([`Error::ArithmeticOverflow`]) from a malformed one ([`Error::InvalidPayload`]).
#[cfg(feature = "primitive")]
pub trait ParsePrimitive: Sized {
    /// Parse `text` into `Self`.
    fn parse_primitive(text: &str) -> Result<Self, Error>;
}

#[cfg(feature = "primitive")]
macro_rules! impl_parse_primitive {
    (integer: $($t:ty),* $(,)?) => {
        $(
            impl ParsePrimitive for $t {
                fn parse_primitive(text: &str) -> Result<Self, Error> {
                    match text.parse::<$t>() {
                        Ok(value) => Ok(value),
                        Err(error) => match error.kind() {
                            core::num::IntErrorKind::PosOverflow
                            | core::num::IntErrorKind::NegOverflow => {
                                Err(Error::ArithmeticOverflow)
                            }
                            _ => Err(Error::InvalidPayload),
                        },
                    }
                }
            }
        )*
    };
    (plain: $($t:ty),* $(,)?) => {
        $(
            impl ParsePrimitive for $t {
                fn parse_primitive(text: &str) -> Result<Self, Error> {
                    text.parse::<$t>().map_err(|_| Error::InvalidPayload)
                }
            }
        )*
    };
}

#[cfg(feature = "primitive")]
impl_parse_primitive!(integer: u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);
#[cfg(feature = "primitive")]
impl_parse_primitive!(plain: f32, f64, bool, char);

/// Implement [`Serialize`]/[`Deserialize`] for a primitive whose payload is its
/// `Display` output and whose deserializer is `str::parse`.
///
/// Each primitive carries the longest `Display` result it can produce, so its
/// formatter buffer stays as small as the type allows.
///
/// The floats dominate because `Display` for them is positional, never
/// exponential: the longest results come from the smallest subnormals, which
/// print as `0.` followed by hundreds of zeroes. Counting the sign byte too, the
/// worst case is 48 bytes for `f32` and 327 for `f64`; the bounds below round
/// each up by one. The ignored `float_display_len_boundary` test in
/// `sdave-codec/tests/primitive.rs` measures this on the running machine.
///
/// `usize`/`isize` assume pointers no wider than 64 bits.
#[cfg(feature = "primitive")]
macro_rules! impl_primitive {
    ($($t:ty => $max_len:literal),* $(,)?) => {
        $(
            impl TypeName for $t {
                const FULL: &'static str = stringify!($t);
            }

            impl<A> Serialize<u8, A> for $t
            where
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
                    let formatted = format_display::<$t, $max_len>(self)?;
                    unsafe {
                        frame_slice(
                            buffer,
                            formatted.as_bytes(),
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
                    let formatted = format_display::<$t, $max_len>(self)?;
                    unsafe {
                        frame_slice_typed(
                            buffer,
                            leaf_signature::<N, $t>(name),
                            formatted.as_bytes(),
                            variant,
                            strategy,
                            limiter_pairs,
                        )
                    }
                }
            }

            impl<A> Deserialize<u8, A> for $t
            where
                A: Allocator + Clone,
            {
                type Type = $t;
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
                        )
                    }?;
                    let text = core::str::from_utf8(payload).map_err(|_| Error::InvalidPayload)?;
                    <Self as ParsePrimitive>::parse_primitive(text)
                }
            }
        )*
    };
}

#[cfg(feature = "primitive")]
impl_primitive! {
    u8 => 3,
    u16 => 5,
    u32 => 10,
    u64 => 20,
    u128 => 39,
    usize => 20,
    i8 => 4,
    i16 => 6,
    i32 => 11,
    i64 => 20,
    i128 => 40,
    isize => 20,
    f32 => 49,
    f64 => 328,
    bool => 5,
    char => 4,
}

impl TypeName for str {
    const FULL: &'static str = "str";
}

impl<A> Serialize<u8, A> for str
where
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
        unsafe { frame_slice(buffer, self.as_bytes(), variant, strategy, limiter_pairs) }
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
            frame_slice_typed(
                buffer,
                leaf_signature::<N, str>(name),
                self.as_bytes(),
                variant,
                strategy,
                limiter_pairs,
            )
        }
    }
}

impl_deref_serialize!(&T, alloc::boxed::Box<T, A>);

impl<T> TypeName for &T
where
    T: ?Sized + TypeName,
{
    impl_type_name_forwarded!(T);
}

impl<T, A> TypeName for alloc::boxed::Box<T, A>
where
    T: ?Sized + TypeName,
    A: Allocator + Clone,
{
    impl_type_name_forwarded!(T);
}

// ---------------------------------------------------------------------------
// Tuples and `()`.
//
// Both are structural shapes and are therefore always available. A tuple follows the
// same wire shape as a tuple struct (a per-element `(type)` signature plus the value,
// flattened in place), and `()` is an Envelop with an empty
// payload — exactly the shape a zero-field struct derives to.
// ---------------------------------------------------------------------------

impl TypeName for () {
    const FULL: &'static str = "()";
}

impl<A> Serialize<u8, A> for ()
where
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
        unsafe { frame_slice(buffer, &[], variant, strategy, limiter_pairs) }
    }

    generic_serialize_typed!();
}

impl<A> Deserialize<u8, A> for ()
where
    A: Allocator + Clone,
{
    type Type = ();

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
        if payload.is_empty() {
            Ok(())
        } else {
            Err(Error::UnexpectedItem)
        }
    }
}

/// Implement the codec traits for one tuple arity.
///
/// `$T $v $idx` are the element type parameter, its deserialization binding and
/// its positional index.
macro_rules! impl_tuple {
    ($($T:ident $v:ident $idx:tt),+ $(,)?) => {
        impl<$($T),+> TypeName for ($($T,)+)
        where
            $($T: TypeName),+
        {
            const FULL: &'static str = ::core::concat!("(", ::core::stringify!($($T),+), ")");

            fn render<N: NamePolicy>(f: &mut dyn core::fmt::Write) -> core::fmt::Result {
                let mut __first = true;
                f.write_str("(")?;
                $(
                    if !__first {
                        f.write_str(", ")?;
                    }
                    __first = false;
                    <$T as TypeName>::render::<N>(f)?;
                )+
                f.write_str(")")
            }
        }

        impl<$($T),+, A> Serialize<u8, A> for ($($T,)+)
        where
            $(
                $T: Serialize<u8, A> + TypeName,
                <$T as Serialize<u8, A>>::Slices: AsRef<[Option<(Offset, Offset)>]>,
            )+
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
                let __head = buffer.len();
                $(
                    {
                        let __region = buffer.len();
                        buffer.extend_from_slice(b"(");
                        <$T as TypeName>::render::<N>(&mut BufferWriter(&mut *buffer))
                            .map_err(|_| Error::UnexpectedItem)?;
                        buffer.extend_from_slice(b")");
                        let __signature = (Offset::new(__region), Offset::new(buffer.len()));
                        let __field = unsafe {
                            <$T as Serialize<u8, A>>::serialize::<N>(
                                &self.$idx,
                                buffer,
                                variant,
                                strategy,
                                limiter_pairs,
                            )?
                        };
                        unsafe {
                            flatten_in_place(
                                buffer,
                                __region,
                                [Some(__signature)].into_iter().chain(__field),
                            );
                        }
                    }
                )+
                unsafe { frame_region(buffer, __head, variant, strategy, limiter_pairs) }
            }

            generic_serialize_typed!();
        }

        impl<$($T),+, A> Deserialize<u8, A> for ($($T,)+)
        where
            $($T: Deserialize<u8, A, Type = $T>),+,
            A: Allocator + Clone,
        {
            type Type = ($($T,)+);

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
                        allocator.clone(),
                    )?
                };
                let items = items_of(payload, variant, limiter_pairs, allocator.clone())?;
                // Every tuple element is positional: its item pair starts at `2 * idx`.
                $(
                    let mut $v: Option<$T> = None;
                )+
                let mut __i = 0usize;
                while __i + 1 < items.len() {
                    if let (Item::N { .. }, Item::E { envelop }) =
                        (&items[__i], &items[__i + 1])
                    {
                        let __tail =
                            envelop.tail_offset.map_or(payload.len(), |tail| tail.get());
                        let __value = &payload[envelop.head_offset.get()..__tail];
                        match __i / 2 {
                            $(
                                $idx => {
                                    $v = Some(<$T as Deserialize<u8, A>>::deserialize::<N>(
                                        __value,
                                        variant,
                                        limiter_pairs,
                                        Some(Offset::new(__value.len())),
                                        allocator.clone(),
                                    )?);
                                }
                            )+
                            _ => {}
                        }
                    }
                    __i += 2;
                }
                Ok(($($v.ok_or(Error::UnexpectedItem)?,)+))
            }
        }
    };
}

impl_tuple!(T0 v0 0);
impl_tuple!(T0 v0 0, T1 v1 1);
impl_tuple!(T0 v0 0, T1 v1 1, T2 v2 2);
impl_tuple!(T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3);
impl_tuple!(T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3, T4 v4 4);
impl_tuple!(T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3, T4 v4 4, T5 v5 5);
impl_tuple!(T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3, T4 v4 4, T5 v5 5, T6 v6 6);
impl_tuple!(T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3, T4 v4 4, T5 v5 5, T6 v6 6, T7 v7 7);
impl_tuple!(
    T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3, T4 v4 4, T5 v5 5, T6 v6 6, T7 v7 7, T8 v8 8
);
impl_tuple!(
    T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3, T4 v4 4, T5 v5 5, T6 v6 6, T7 v7 7, T8 v8 8, T9 v9 9
);
impl_tuple!(
    T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3, T4 v4 4, T5 v5 5, T6 v6 6, T7 v7 7, T8 v8 8, T9 v9 9, T10
    v10 10
);
impl_tuple!(
    T0 v0 0, T1 v1 1, T2 v2 2, T3 v3 3, T4 v4 4, T5 v5 5, T6 v6 6, T7 v7 7, T8 v8 8, T9 v9 9, T10
    v10 10, T11 v11 11
);
