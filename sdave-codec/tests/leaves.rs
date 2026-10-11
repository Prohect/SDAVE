//! The scalar leaves: their payloads, their errors, and the fact that both entries agree.
//!
//! Every expectation is the canonical text the codec README specifies: canonical decimal integers,
//! `true`/`false`, a single scalar's UTF-8, a borrowed `&str`, byte payloads, and the standard
//! nonzero types.
// `Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

mod common;

use core::num::{NonZeroI8, NonZeroU8, NonZeroUsize};

use common::*;
use sdave_codec::{ErrorKind, NonMaxUsize, Serialize, Shape};

#[test]
fn integers_are_canonical_decimal() {
    assert_eq!(body(&0u8), "0");
    assert_eq!(body(&255u8), "255");
    assert_eq!(body(&65535u16), "65535");
    assert_eq!(body(&4294967295u32), "4294967295");
    assert_eq!(body(&18446744073709551615u64), "18446744073709551615");
    assert_eq!(
        body(&340282366920938463463374607431768211455u128),
        "340282366920938463463374607431768211455"
    );

    assert_eq!(body(&-1i8), "-1");
    assert_eq!(body(&-128i8), "-128");
    assert_eq!(body(&-32768i16), "-32768");
    assert_eq!(body(&-2147483648i32), "-2147483648");
    assert_eq!(body(&-9223372036854775808i64), "-9223372036854775808");
    assert_eq!(
        body(&-170141183460469231731687303715884105728i128),
        "-170141183460469231731687303715884105728"
    );

    // The platform's own width, canonically.
    assert_eq!(body(&1usize), "1");
    assert_eq!(body(&-1isize), "-1");
}

#[test]
fn booleans_and_characters() {
    assert_eq!(body(&true), "true");
    assert_eq!(body(&false), "false");

    assert_eq!(body(&'A'), "A");
    // A `char` payload is the scalar's UTF-8.
    assert_eq!(body(&'é'), "é");
    assert_eq!(body(&'☃'), "☃");
}

#[test]
fn floats_are_display_text_and_must_be_finite() {
    assert_eq!(body(&0.0f64), "0");
    assert_eq!(body(&1.5f64), "1.5");
    assert_eq!(body(&-0.5f32), "-0.5");
    assert_eq!(body(&0.1f64), "0.1");

    for bad in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert_eq!(*body_error(&bad).kind(), ErrorKind::NonFiniteFloat);
    }
    assert_eq!(*body_error(&f32::NAN).kind(), ErrorKind::NonFiniteFloat);
}

#[test]
fn text_and_byte_payloads_are_verbatim() {
    assert_eq!(body::<str>("hello"), "hello");
    assert_eq!(body::<str>(""), "");
    assert_eq!(
        body::<str>("^^not a limiter here~~"),
        "^^not a limiter here~~"
    );
    assert_eq!(body::<[u8]>(b"bytes"), "bytes");
}

#[test]
fn a_byte_payload_must_be_valid_text() {
    // A byte payload that is not UTF-8 cannot be carried by a text document.
    let error = body_error::<[u8]>(&[0xFF, 0xFE]);
    assert_eq!(*error.kind(), ErrorKind::InvalidUtf8);
}

#[test]
fn the_nonzero_types_are_decimal_too() {
    assert_eq!(body(&NonZeroU8::new(1).unwrap()), "1");
    assert_eq!(body(&NonZeroU8::new(255).unwrap()), "255");
    assert_eq!(body(&NonZeroUsize::new(9).unwrap()), "9");
    assert_eq!(body(&NonZeroI8::new(-8).unwrap()), "-8");
    assert_eq!(body(&NonMaxUsize::new(0)), "0");
    assert_eq!(
        body(&NonMaxUsize::new(usize::MAX - 1)),
        (usize::MAX - 1).to_string()
    );
}

#[test]
fn the_unit_value_has_an_empty_payload() {
    assert_eq!(body(&()), "");
}

#[test]
fn a_leaf_frames_nothing_of_its_own() {
    // A leaf's body is only its payload: the envelope around it belongs to its field header or
    // item position, which is why no frame marker appears in a leaf's body.
    assert_eq!(body(&7u8), "7");
    assert!(!body(&7u8).contains('<'));
    assert!(!body::<str>("hi").contains('<'));
}

#[test]
fn leaves_are_empty_tuple_shaped() {
    assert_eq!(<u8 as Serialize>::SHAPE, Shape::Tuple);
    assert_eq!(<f64 as Serialize>::SHAPE, Shape::Tuple);
    assert_eq!(<str as Serialize>::SHAPE, Shape::Tuple);
    assert_eq!(<() as Serialize>::SHAPE, Shape::Tuple);
}

#[test]
fn leaves_are_zero_deep() {
    // A leaf closes no envelope inside itself, so a record orders it before any aggregate field.
    assert_eq!(<u8 as Serialize>::DEPTH, 0);
    assert_eq!(<char as Serialize>::DEPTH, 0);
    assert_eq!(<str as Serialize>::DEPTH, 0);
    assert_eq!(<[u8] as Serialize>::DEPTH, 0);
    assert_eq!(<NonMaxUsize as Serialize>::DEPTH, 0);
    assert_eq!(<() as Serialize>::DEPTH, 0);
}
