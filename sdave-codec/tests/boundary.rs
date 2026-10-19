//! The **capability boundary** of `sdave-codec` / `sdave-derive`, pinned by tests.
//!
//! The sibling files drive the happy paths — `primitive.rs`, `iterable.rs`,
//! `composite.rs`, `derive.rs` — and `features.rs` the feature-gated shapes. This
//! file covers the *edges*: the framing and name-policy helpers, and every
//! malformed-buffer [`Error`].
//!
//! Which shapes the codec supports, and which feature gates each one, is stated once in
//! the crate `README.md` (*Capability boundary*); this file only pins the runtime half
//! of that statement. The compile-time gaps are pinned differently — they are the
//! absence of an impl (or a macro rejection), which no test can observe at run time.
//!
//! # Malformed buffers
//!
//! A malformed buffer never panics; it maps to a distinct [`Error`]:
//!
//! | condition | error |
//! | --- | --- |
//! | no limiter pair can frame the payload | [`Error::NoSerializableLimiterPair`] |
//! | a required signature is absent / not accepted | [`Error::UnexpectedSignature`] |
//! | an Envelop that must be there is missing | [`Error::MissingEnvelop`] |
//! | a `Variant::V2` Envelop's delimiter is unconfirmed and the buffer was not declared final | [`Error::UnconfirmedDelimiter`] |
//! | a payload is not valid for the target type | [`Error::InvalidPayload`] |
//! | a well-formed decimal overflows the target integer | [`Error::ArithmeticOverflow`] |
//!
//! Every error carries the absolute `(head, tail)` offsets of what it caught, so the
//! assertions below pin the location as well as the kind, and [`fmt_error`] renders one
//! against the buffer those offsets index (see `errors_render_against_the_buffer_they_came_from`).
//!
//! An *empty* `limiter_pairs` set is not a document fault: building a `FlatDeserializer` with
//! one panics (core's `FlatDeserializer::new` contract), so the codec has no `Error` variant
//! for it.

mod common;

use std::alloc::Global;

use common::{
    deserialize_one, flat_deserializer, flatten, from_text, iterable_element_ranges, pair, pairs,
    primitive_pairs, single_item_buffer_as_envelop_payload,
};
// Only the name-policy test renders a type, and it is `vec`-gated.
#[cfg(feature = "vec")]
use common::render;
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
        // The name was not appended last, so the report points at the range it named.
        Some(Error::UnexpectedSignature {
            valid_signatures: &[],
            caught: (Offset::new(0), Offset::new(2)),
        }),
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
        // `head > tail`: the range names no slice at all, and is reported as given.
        Some(Error::UnexpectedSignature {
            valid_signatures: &[],
            caught: (Offset::new(6), Offset::new(4)),
        }),
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
        // No payload was written yet: the report names the type and the slot reserved
        // for its text.
        Some(Error::InvalidPayload {
            r#type: "u32",
            caught: (Offset::new(0), Offset::new(3)),
        }),
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
        Error::NoSerializableLimiterPair {
            len: Offset::new(0),
        },
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
        Error::NoSerializableLimiterPair {
            len: Offset::new(1),
        },
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
}

#[cfg(feature = "vec")]
#[test]
fn policy_shortens_only_listed_types_and_per_node() {
    use short_vec::ShortVec;

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
// Every malformed-buffer error, plus the one caller-side panic.
// ---------------------------------------------------------------------------

#[test]
#[should_panic(expected = "empty limiter_pairs")]
fn an_empty_limiter_pair_set_panics_while_building() {
    // Building a deserializer is the caller's step, and an empty pair set is a caller bug
    // rather than a malformed document: `FlatDeserializer::new` panics on one, so there is no
    // `Error` variant for it.
    let empty: Vec<LimiterPair<'static, u8, Global>> = Vec::new();
    let _ = flat_deserializer(b"anything", Variant::V1, &empty, &Global);
}

#[test]
fn a_non_envelop_buffer_is_a_missing_envelop() {
    let pairs = pairs();
    // `abc` contains no limiter, so it is a NonEnvelop: the Envelop `deserialize` was fed
    // is simply not there, and the report says so (`required_by` is `None`).
    assert_eq!(
        unsafe { single_item_buffer_as_envelop_payload(b"abc", Variant::V1, &pairs, None, Global) }
            .err(),
        Some(Error::MissingEnvelop {
            required_by: None,
            caught: (Offset::new(0), Offset::new(3)),
        }),
    );
}

#[test]
fn an_invalid_payload_is_reported() {
    // The payload `x` is not a decimal. `from_text` frames it as `^^x~~`, so the payload
    // is the absolute range `[2, 3)`.
    assert_eq!(
        deserialize_payload::<u32>(b"x"),
        Error::InvalidPayload {
            r#type: "u32",
            caught: (Offset::new(2), Offset::new(3)),
        },
    );
    // A `char` payload must be exactly one scalar value.
    assert_eq!(
        deserialize_payload::<char>(b"xy"),
        Error::InvalidPayload {
            r#type: "char",
            caught: (Offset::new(2), Offset::new(4)),
        },
    );
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
        deserialize_one::<NoShortening, u32, _>(&flat, Variant::V2, &pairs, None, Global)
            .unwrap_err(),
        // The flat document is `^^42~~`: the limiter is `[0, 2)`, the payload settles at
        // `[2, 4)`, and the delimiter run `[4, 6)` is what nothing confirmed.
        Error::UnconfirmedDelimiter {
            limiter_slice: (Offset::new(0), Offset::new(2)),
            unconfirmed: (Offset::new(4), Offset::new(6)),
        },
    );
    assert_eq!(
        deserialize_one::<NoShortening, u32, _>(
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

// ---------------------------------------------------------------------------
// Rendering an error against its buffer.
// ---------------------------------------------------------------------------

/// Frame `payload` as one document and return it together with the error `T`'s
/// deserializer raises on it.
fn document_and_error<T>(payload: &[u8]) -> (Vec<u8>, Error)
where
    T: Deserialize<u8, Global, Type = T>,
{
    let pairs = primitive_pairs();
    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        frame_slice(
            &mut buffer,
            payload,
            Variant::V1,
            Strategy::FirstMatch,
            &mut working,
        )
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);
    let error = deserialize_one::<NoShortening, T, _>(&flat, Variant::V1, &pairs, None, Global)
        .err()
        .unwrap();
    (flat, error)
}

/// [`fmt_error`] is the diagnostic path: it quotes the bytes an [`Error`] points at, so it
/// has to cope with a payload that is not UTF-8 (the wire form is only text when the
/// readable primitives wrote it) and with a buffer that no longer holds the offsets.
///
/// Every rendering is printed — run this with `--nocapture` to read them — and the
/// collected order is what the assertion pins.
#[test]
fn errors_render_against_the_buffer_they_came_from() {
    let mut rendered: Vec<String> = Vec::new();
    let mut render =
        |error: Error, buffer: &[u8]| rendered.push(format!("{}", fmt_error(error, buffer)));

    // A payload that is not a decimal...
    let (flat, error) = document_and_error::<u32>(b"x");
    render(error, &flat);

    // ...one that is not even UTF-8, so the dump has to escape it...
    let (flat, error) = document_and_error::<u32>(b"\xff\x02");
    render(error, &flat);

    // ...and one that is a well-formed decimal, just too wide for the target.
    let (flat, error) = document_and_error::<u8>(b"300");
    render(error, &flat);

    // An item where an Envelop was expected: nothing invalid, the Envelop is just missing.
    let pairs = pairs();
    let error =
        unsafe { single_item_buffer_as_envelop_payload(b"abc", Variant::V1, &pairs, None, Global) }
            .err()
            .unwrap();
    render(error, b"abc");

    // A `Variant::V2` Envelop whose delimiter nothing confirmed.
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
    let error = deserialize_one::<NoShortening, u32, _>(&flat, Variant::V2, &pairs, None, Global)
        .err()
        .unwrap();
    render(error, &flat);

    // The one error that carries no range at all.
    let mut none: Vec<LimiterPair<'static, u8, Global>> = Vec::new();
    let error = unsafe {
        select_limiter_pair(
            b"",
            &[Some(&b"payload"[..])],
            Variant::V1,
            Strategy::FirstMatch,
            &mut none,
        )
    }
    .err()
    .unwrap();
    render(error, b"");

    // The ranges are absolute, so a buffer that was advanced (or a shorter one handed in by
    // mistake) no longer holds them: that is reported, never a panic.
    let (flat, error) = document_and_error::<u32>(b"x");
    render(error, &flat[..1]);

    // An iterable element slot that holds a signature: `^^~~` is the marker Envelop, then
    // `sig` is a `NonEnvelop` where an element Envelop belongs.
    let error = iterable_element_ranges(b"^^~~sig^^7~~", Variant::V1, &primitive_pairs(), Global)
        .err()
        .unwrap();
    render(error, b"^^~~sig^^7~~");

    // A typed iterable payload whose signature has no marker Envelop behind it.
    let error = iterable_element_ranges(b"sig", Variant::V1, &primitive_pairs(), Global)
        .err()
        .unwrap();
    render(error, b"sig");

    for line in &rendered {
        eprintln!("{line}");
    }

    assert_eq!(
        rendered,
        [
            r#"InvalidPayload { type: u32, caught: [2..3) "x" }"#,
            r#"InvalidPayload { type: u32, caught: [2..4) "\xff\x02" }"#,
            "ArithmeticOverflow",
            r#"MissingEnvelop { required_by: None, caught: [0..3) "abc" }"#,
            r#"UnconfirmedDelimiter { limiter_slice: [0..2) "^^", payload: [2..4) "42", unconfirmed: [4..6) "~~" }"#,
            "NoSerializableLimiterPair { len: 0 }",
            r#"InvalidPayload { type: u32, caught: [2..3) <outside the buffer> }"#,
            r#"UnexpectedSignature { valid_signatures: [], caught: [4..7) "sig" }"#,
            r#"MissingEnvelop { required_by: [0..3) "sig", caught: [3..3) "" }"#,
        ],
    );
}
