//! The **capability boundary** of `sdave-codec` / `sdave-derive`, pinned by tests.
//!
//! The sibling files drive the happy paths — `primitive.rs`, `iterable.rs`,
//! `composite.rs`, `derive.rs` — and `features.rs` the feature-gated shapes. This
//! file covers the *edges*: the framing and name-policy helpers, and every
//! malformed-buffer [`Error`].
//!
//! # Supported (always available)
//!
//! These shapes are structural — they say how values are arranged, not how a leaf is
//! spelt — so no feature gates them:
//!
//! - tuples `(A, B, …)` (arities 1–12) and `()`;
//! - `[T; N]`;
//! - any struct/enum with `#[derive(Serialize, Deserialize)]`;
//! - `Box<dyn Trait>` for a `#[sdave_dyn(..)]` trait.
//!
//! **`Serialize` only** (there is no borrow-out deserializer): `str` / `&str`, `[T]`,
//! `&T`, `Box<T, A>` (only `Box<dyn Trait>` deserializes).
//!
//! # Supported (one feature each)
//!
//! Opt-in, off by default (`tests/features.rs` exercises them all under
//! `--all-features`), except the two marked *(default)*:
//!
//! - `primitive` *(default)* — the readable primitives: `u8`..`u128`, `usize`,
//!   `i8`..`i128`, `isize`, `f32`, `f64`, `bool`, `char`. The test targets pin it on,
//!   because every payload in this suite is written through it.
//!
//! | feature | shape | feature | shape |
//! | --- | --- | --- | --- |
//! | `vec` *(default)* | `Vec<T, A>` | `boxed` | `Box<T, A>` deserializer |
//! | `vec_deque` | `VecDeque<T>` | `rc` | `Rc<T>` |
//! | `linked_list` | `LinkedList<T>` | `arc` | `Arc<T>` |
//! | `btree_set` | `BTreeSet<T>` | `cow` | `Cow<'_, B>` |
//! | `option` | `Option<T>` | `cell` | `Cell<T>` |
//! | `result` | `Result<T, E>` | `unsafe_cell` | `UnsafeCell<T>` |
//! | `string` | `String` | | |
//!
//! # NOT supported
//!
//! - **Compile-time gaps** (the trait is simply not implemented): a shape whose
//!   feature is off (e.g. `u32` without `primitive`); `HashMap`/`HashSet` (they need
//!   `std`); unions; references-as-output (`&T` has no deserializer).
//! - **Recursive / self-referential owned types** do NOT derive: a field's
//!   compile-time `SLICES` is needed to size the type's own `Slices` array, so
//!   recursing through a container trips the type-level constant cycle (`E0391`);
//!   a *generic* recursive type overflows (`E0275`) on use.
//! - **A blanket `IntoIterator` impl is impossible**: `impl<T: IntoIterator>
//!   Serialize for T` overlaps every impl on a foreign type and is rejected by
//!   coherence (`E0119`).
//!
//! # Malformed buffers
//!
//! A malformed buffer never panics; it maps to a distinct [`Error`]:
//!
//! | condition | error |
//! | --- | --- |
//! | empty `limiter_pairs` set | [`Error::EmptyLimiterPairs`] |
//! | no limiter pair can frame the payload | [`Error::NoSerializableLimiterPair`] |
//! | buffer is not the expected Envelop / signature / value | [`Error::UnexpectedItem`] |
//! | a `Variant::V2` Envelop's delimiter is unconfirmed and the buffer was not declared final | [`Error::UnconfirmedDelimiter`] |
//! | a payload is not valid for the target type | [`Error::InvalidPayload`] |
//! | a well-formed decimal overflows the target integer | [`Error::ArithmeticOverflow`] |

mod common;

use std::alloc::Global;

use common::{flatten, from_text, pair, pairs, primitive_pairs};
use sdave_codec::*;

// ---------------------------------------------------------------------------
// Framing helpers.
// ---------------------------------------------------------------------------

#[test]
fn frame_region_and_frame_slice_agree() {
    let mut a = pairs();
    let mut buffer_a: Vec<u8> = Vec::new();
    let slices_a = unsafe {
        frame_slice(
            &mut buffer_a,
            b"hello",
            Variant::V1,
            Strategy::FirstMatch,
            &mut a,
        )
    }
    .unwrap();

    let mut b = pairs();
    let mut buffer_b: Vec<u8> = Vec::new();
    buffer_b.extend_from_slice(b"hello");
    let slices_b =
        unsafe { frame_region(&mut buffer_b, 0, Variant::V1, Strategy::FirstMatch, &mut b) }
            .unwrap();

    assert_eq!(buffer_a, buffer_b);
    let normalize = |slices: [Option<(Offset, Offset)>; 3]| {
        slices.map(|slice| slice.map(|(head, tail)| (head.get(), tail.get())))
    };
    assert_eq!(normalize(slices_a), normalize(slices_b));
}

#[test]
fn write_signature_requires_an_adjacent_name() {
    let mut buffer: Vec<u8> = b"name".to_vec();
    // `tail_offset` (2) != `buffer.len()` (4): the name was not appended last.
    let misplaced = Name {
        head_offset: Offset::new(0),
        tail_offset: Offset::new(2),
    };
    assert_eq!(
        write_signature(
            &mut buffer,
            Signature {
                name: misplaced,
                separator: TYPE_SEPARATOR,
                type_describer: b"u32",
            },
        )
        .err(),
        Some(Error::UnexpectedItem),
    );

    // A name whose head runs past its tail names no appended slice at all, even
    // when the tail is adjacent; it must not become a slice `head..tail` later.
    let inverted = Name {
        head_offset: Offset::new(6),
        tail_offset: Offset::new(4),
    };
    assert_eq!(
        write_signature(
            &mut buffer,
            Signature {
                name: inverted,
                separator: TYPE_SEPARATOR,
                type_describer: b"u32",
            },
        )
        .err(),
        Some(Error::UnexpectedItem),
    );

    let name = Name {
        head_offset: Offset::new(0),
        tail_offset: Offset::new(4),
    };
    let (head, tail) = write_signature(
        &mut buffer,
        Signature {
            name,
            separator: TYPE_SEPARATOR,
            type_describer: b"u32",
        },
    )
    .unwrap();
    assert_eq!((head.get(), tail.get()), (0, 9));
    assert_eq!(buffer, b"name: u32");
    assert_eq!(TYPE_SEPARATOR, b": ");
}

#[test]
fn leaf_signature_is_name_colon_shortened_type() {
    let signature = leaf_signature::<NoShortening, u32>(Name {
        head_offset: Offset::new(0),
        tail_offset: Offset::new(0),
    });
    assert_eq!(signature.separator, b": ");
    assert_eq!(signature.type_describer, b"u32");
}

#[test]
fn stack_buffer_reports_a_too_small_bound() {
    assert_eq!(format_display::<u32, 4>(&1000).unwrap().as_bytes(), b"1000");
    assert_eq!(format_display::<u32, 4>(&1000).unwrap().len(), 4);
    assert_eq!(
        format_display::<u32, 3>(&1000).err(),
        Some(Error::InvalidPayload),
    );
}

// ---------------------------------------------------------------------------
// Limiter-pair selection.
// ---------------------------------------------------------------------------

#[test]
fn select_limiter_pair_honours_the_strategy() {
    // `^` frames anything not ending in `@`; the payload `@@@x` makes pair 0's
    // delimiter collide (4 repeats) while pair 1 (`#`) never collides (1 repeat).
    let mut pairs = vec![pair(1, b"^", b"@"), pair(1, b"?", b"#")];
    let slices = [Some(&b"@@@x"[..])];

    let (index, _) =
        unsafe { select_limiter_pair(b"", &slices, Variant::V1, Strategy::FirstMatch, &mut pairs) }
            .unwrap();
    assert_eq!(index, 0);

    let (index, _) = unsafe {
        select_limiter_pair(
            b"",
            &slices,
            Variant::V1,
            Strategy::RoundRobin(1),
            &mut pairs,
        )
    }
    .unwrap();
    assert_eq!(index, 1);

    let (index, repeats) = unsafe {
        select_limiter_pair(b"", &slices, Variant::V1, Strategy::LeastRepeat, &mut pairs)
    }
    .unwrap();
    assert_eq!(index, 1);
    assert_eq!(repeats.get(), 1);
}

#[test]
fn select_limiter_pair_reports_no_candidate() {
    // An empty candidate set.
    let mut no_pairs: Vec<LimiterPair<'static, u8, Global>> = Vec::new();
    assert_eq!(
        unsafe {
            select_limiter_pair(
                b"",
                &[Some(&b"payload"[..])],
                Variant::V1,
                Strategy::FirstMatch,
                &mut no_pairs,
            )
        }
        .unwrap_err(),
        Error::NoSerializableLimiterPair,
    );

    // A V1 payload that ends with the delimiter can never be framed by it.
    let mut pairs = vec![pair(1, b"^", b"~")];
    assert_eq!(
        unsafe {
            select_limiter_pair(
                b"",
                &[Some(&b"abc~"[..])],
                Variant::V1,
                Strategy::FirstMatch,
                &mut pairs,
            )
        }
        .unwrap_err(),
        Error::NoSerializableLimiterPair,
    );
}

// ---------------------------------------------------------------------------
// Name policy.
// ---------------------------------------------------------------------------

// `Vec` is the sample container here, so these need its feature.
#[cfg(feature = "vec")]
mod short_vec {
    use super::*;

    /// A policy that shortens `Vec`'s fully qualified name only.
    pub struct ShortVec;

    impl NamePolicy for ShortVec {
        const SHORT_TYPES: &'static [ShortType] = &[ShortType::new("alloc::vec::Vec")];
    }

    pub fn render<N: NamePolicy, T: TypeName + ?Sized>() -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        T::render::<N>(&mut BufferWriter(&mut out)).unwrap();
        out
    }
}

#[cfg(feature = "vec")]
#[test]
fn policy_shortens_only_listed_types_and_per_node() {
    use short_vec::{ShortVec, render};

    // The listed node is shortened...
    assert_eq!(render::<ShortVec, Vec<u32>>(), b"Vec<u32>");
    // ...its argument is not (shortening never propagates across a level).
    assert_eq!(render::<ShortVec, Vec<Vec<u8>>>(), b"Vec<Vec<u8>>");
    // An unlisted type is untouched.
    assert_eq!(render::<ShortVec, u32>(), b"u32");
    // The default policy shortens nothing.
    assert_eq!(render::<NoShortening, Vec<u32>>(), b"alloc::vec::Vec<u32>");
}

#[test]
fn short_type_derives_the_last_path_segment() {
    assert_eq!(NoShortening::shorten("a::B"), "a::B");
    assert_eq!(ShortType::new("alloc::vec::Vec").short(), "Vec");
    assert_eq!(ShortType::new("Vec").short(), "Vec");
}

#[test]
fn signatures_are_matched_exactly() {
    assert!(render_matches::<NoShortening, u32>(b"u32"));
    assert!(!render_matches::<NoShortening, u32>(b"U32"));
    assert!(!render_matches::<NoShortening, u32>(b"u32x"));

    assert!(named_signature_matches::<NoShortening, u32>(
        b"x: u32", b"x"
    ));
    assert!(!named_signature_matches::<NoShortening, u32>(
        b"y: u32", b"x"
    ));
    // The `: ` separator is required.
    assert!(!named_signature_matches::<NoShortening, u32>(b"xu32", b"x"));
}

#[test]
#[should_panic(expected = "duplicate ShortType")]
fn assert_valid_rejects_duplicate_names() {
    assert_valid(&[ShortType::new("a::B"), ShortType::new("c::B")]);
}

// ---------------------------------------------------------------------------
// Every malformed-buffer error.
// ---------------------------------------------------------------------------

#[test]
fn empty_limiter_pairs_is_reported() {
    let empty: Vec<LimiterPair<'static, u8, Global>> = Vec::new();
    assert_eq!(
        flat_deserializer(b"anything", Variant::V1, &empty, &Global).err(),
        Some(Error::EmptyLimiterPairs),
    );
    assert_eq!(
        items_of(b"anything", Variant::V1, &empty, Global).err(),
        Some(Error::EmptyLimiterPairs),
    );
    assert_eq!(
        unsafe {
            single_item_buffer_as_envelop_payload(b"anything", Variant::V1, &empty, None, Global)
        }
        .err(),
        Some(Error::EmptyLimiterPairs),
    );
}

#[test]
fn a_non_envelop_buffer_is_unexpected_item() {
    let pairs = pairs();
    // `abc` contains no limiter, so it is a NonEnvelop, not an Envelop.
    assert_eq!(
        unsafe { single_item_buffer_as_envelop_payload(b"abc", Variant::V1, &pairs, None, Global) }
            .err(),
        Some(Error::UnexpectedItem),
    );
}

#[test]
fn an_invalid_payload_is_reported() {
    // The payload `x` is not a decimal.
    assert_eq!(deserialize_payload::<u32>(b"x"), Error::InvalidPayload);
    // A `char` payload must be exactly one scalar value.
    assert_eq!(deserialize_payload::<char>(b"xy"), Error::InvalidPayload);
}

#[test]
fn an_overflowing_payload_is_reported() {
    assert_eq!(deserialize_payload::<u8>(b"300"), Error::ArithmeticOverflow);
    assert_eq!(
        deserialize_payload::<i8>(b"-300"),
        Error::ArithmeticOverflow
    );
}

#[test]
fn v2_unconfirmed_delimiter_is_reported() {
    // A `Variant::V2` Envelop that ends the buffer has no following non-delimiter
    // to confirm its delimiter slice, so its payload is only handed out when the
    // caller declares the buffer final.
    let pairs = primitive_pairs();
    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        42u32.serialize::<NoShortening>(
            &mut buffer,
            Variant::V2,
            Strategy::FirstMatch,
            &mut working,
        )
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);

    assert_eq!(
        u32::deserialize::<NoShortening>(&flat, Variant::V2, &pairs, None, Global).unwrap_err(),
        Error::UnconfirmedDelimiter,
    );
    assert_eq!(
        u32::deserialize::<NoShortening>(
            &flat,
            Variant::V2,
            &pairs,
            Some(Offset::new(flat.len())),
            Global,
        )
        .unwrap(),
        42,
    );
}

/// Deserialize the raw bytes `text` (as a `str` payload) as `T`, returning the
/// error the codec reports.
fn deserialize_payload<T>(text: &[u8]) -> Error
where
    T: Deserialize<u8, Global, Type = T>,
{
    let text = core::str::from_utf8(text).unwrap();
    from_text::<T>(text, &primitive_pairs()).err().unwrap()
}
