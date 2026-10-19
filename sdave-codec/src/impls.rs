//! The codec's leaf impls, plus the map of the shapes that have a file of their own.
//!
//! Always available here: `str` (the raw-text leaf), `()` and the `&T`/`Box<T, A>`
//! forwarding that keeps a pointer wire-identical to its referent. The readable
//! numbers, `bool` and `char` are here as well, behind the `primitive` feature: they
//! are leaves too, so they belong with the other leaves rather than in a module of
//! their own.
//!
//! Every other shape lives in exactly one submodule, so this file stays a map rather
//! than a grab bag:
//!
//! - `macros` — the `macro_rules!` helpers: one wire shape per macro, shared by the
//!   types that spell it the same way;
//! - `channel` — everything that reads a buffer back — and `format` — the heap-free
//!   primitive formatter. Neither holds an impl, so both are re-exported here and
//!   `crate::impls::…` keeps naming them;
//! - `collections` — the feature-gated heap containers;
//! - `option` / `result`, plus `tagged`, the one wire shape those two share;
//! - `boxed`, `rc`, `arc`, `cow`, `cell` (which holds both `Cell` and `UnsafeCell`) and
//!   `string` — the feature-gated pointer, cell and owned-string wrappers.
//!
//! See the crate `README.md` for the feature table.

// Must come first: the macros it defines are invoked by the submodules below.
#[macro_use]
pub(crate) mod macros;

// The two helper groups that are not impls at all are their own files, re-exported
// here so every `crate::impls::…` path keeps working.
mod channel;
mod format;

pub use channel::*;
pub use format::*;

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

use sdave::{FlatDeserializer, LimiterPair, Offset, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    name::{NamePolicy, TypeName},
    serialize::{Name, Serialize, Strategy, frame_slice, frame_slice_typed, leaf_signature},
};

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
/// `caught` is the payload's absolute `(head, tail)` range, which an
/// [`Error::InvalidPayload`] reports so the rejected text can be located.
#[cfg(feature = "primitive")]
pub trait ParsePrimitive: Sized {
    /// Parse `text`, found at the absolute range `caught`, into `Self`.
    fn parse_primitive(text: &str, caught: (Offset, Offset)) -> Result<Self, Error>;
}

#[cfg(feature = "primitive")]
macro_rules! impl_parse_primitive {
    (integer: $($t:ty),* $(,)?) => {
        $(
            impl ParsePrimitive for $t {
                fn parse_primitive(text: &str, caught: (Offset, Offset)) -> Result<Self, Error> {
                    match text.parse::<$t>() {
                        Ok(value) => Ok(value),
                        Err(error) => match error.kind() {
                            core::num::IntErrorKind::PosOverflow
                            | core::num::IntErrorKind::NegOverflow => {
                                Err(Error::ArithmeticOverflow)
                            }
                            _ => Err(Error::InvalidPayload {
                                r#type: stringify!($t),
                                caught,
                            }),
                        },
                    }
                }
            }
        )*
    };
    (plain: $($t:ty),* $(,)?) => {
        $(
            impl ParsePrimitive for $t {
                fn parse_primitive(text: &str, caught: (Offset, Offset)) -> Result<Self, Error> {
                    text.parse::<$t>().map_err(|_| Error::InvalidPayload {
                        r#type: stringify!($t),
                        caught,
                    })
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
                    deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
                    final_buffer: Option<Offset>,
                    _allocator: A,
                ) -> Result<Self::Type, Error> {
                    let (buffer, caught) = unsafe { envelop_payload(deserializer, final_buffer)? };
                    let text = core::str::from_utf8(&buffer[caught.0.get()..caught.1.get()])
                        .map_err(|_| Error::InvalidPayload {
                            r#type: stringify!($t),
                            caught,
                        })?;
                    <Self as ParsePrimitive>::parse_primitive(text, caught)
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

// ---------------------------------------------------------------------------
// Pointer forwarding.
//
// A pointer has no wire shape of its own: `&T`, `Box<T, A>`, `Rc<T>`, `Arc<T>` and
// `Cow<'_, B>` all serialize exactly like their referent, and render the referent's name
// in a `Signature` too. The `Serialize` half below is always on; the rest of each
// pointer's impls are its own feature-gated module (`boxed`/`rc`/`arc`/`cow`).
// ---------------------------------------------------------------------------

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

// The `Serialize` half of both pointers: the wire shape is the referent's, so a
// `&T`/`Box<T, A>` field carries no marker of its own. `Box`'s `Deserialize` is the
// `boxed` feature's, in `impls::boxed`.
impl_deref_serialize!(&T, alloc::boxed::Box<T, A>);

// ---------------------------------------------------------------------------
// `()`.
//
// `()` is a structural shape: an Envelop with an empty payload — exactly the shape a
// zero-field struct derives to. Bare tuples (`(A, B, …)`) are deliberately NOT implemented:
// a tuple has no type name to identify it and no statically known slice count, so a value meant
// to travel as a positional composite is stored in a tuple struct (or a tuple enum variant)
// and derives `Serialize`/`Deserialize`. That keeps the positional wire shape statically
// analysable, so serializing it needs no in-place flatten.
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
        deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
        final_buffer: Option<Offset>,
        _allocator: A,
    ) -> Result<Self::Type, Error> {
        let (buffer, caught) = unsafe { envelop_payload(deserializer, final_buffer)? };
        if buffer[caught.0.get()..caught.1.get()].is_empty() {
            Ok(())
        } else {
            Err(Error::UnexpectedSignature {
                valid_signatures: &["()"],
                caught,
            })
        }
    }
}
