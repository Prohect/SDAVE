//! The typed deserializer: round-trips, the decode rules, and the error cases.
//!
//! Every document below is hand-derived from the codec README's grammar plus `can_serialize`'s
//! documented contract: a standalone document is `marker-text ++ frame(body)`, a record body is the
//! alternation `name:marker` / `frame(body(field))`, a tuple body is `frame(body(item))*`, an enum
//! body is `frame(variant-name)` plus, when the variant has fields, `frame(body(value))`, and a
//! list body is the mandatory empty `frame("")` followed by `frame(body(item))*`. Round-trips go
//! through the codec's own append entry, so the two halves are checked against each other; the
//! error cases use documents written out by hand.
// `Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

mod common;

use core::any::type_name;
use std::alloc::Global;

use common::{Profile8, pairs8, profile8};
use sdave_codec::{
    Config, DecodeSource, Deserialize, Error, ErrorKind, FieldSlot, Limit, Limits, NamePolicy,
    NonMaxUsize, PairSelect, Serialize, Shape, Sink, Step, Variant, depth_of,
};

// ---------------------------------------------------------------------------
// profiles
// ---------------------------------------------------------------------------

/// A minimal byte pair set: `^`/`~` and `?`/`!`, one repeat each, so a hand-written document is
/// short and its frames are easy to check.
const MINIMAL: &[(&str, &str, usize)] = &[("^", "~", 1), ("?", "!", 1)];

/// The round-trip pair set: three disjoint pairs at two repeats minimum.
const WIDE: &[(&str, &str, usize)] = &[("^", "~", 2), ("?", "!", 2), ("{", "}", 2)];

fn minimal() -> Profile8 {
    profile8(Variant::V1, pairs8(MINIMAL), Limits::UNBOUNDED)
}

fn wide() -> Profile8 {
    profile8(Variant::V1, pairs8(WIDE), Limits::UNBOUNDED)
}

/// Encode a value with `profile`, then decode it back and compare.
///
/// No `Debug` bound: the core's public types deliberately have none, so the comparison goes through
/// `PartialEq` alone.
fn roundtrip<V>(profile: &Profile8, value: V)
where
    V: Serialize + for<'de> Deserialize<'de> + PartialEq,
{
    let mut buffer = vec![0u8; 1024];
    let end = profile
        .append_to_slice(&value, &mut buffer, 0)
        .expect("appended");
    let decoded = profile
        .deserialize::<V>(&buffer[..end], &Global)
        .expect("decoded");
    assert!(decoded == value, "the value must round-trip");
}

/// Decode a hand-written document.
fn decode<V>(profile: &Profile8, document: &[u8]) -> Result<V, Error>
where
    V: for<'de> Deserialize<'de>,
{
    profile.deserialize::<V>(document, &Global)
}

/// The error decoding a hand-written document produces.
///
/// Matched rather than `expect_err`'d: `expect_err` would demand `V: Debug`, which the core's public
/// types deliberately do not have.
fn decode_error<V>(profile: &Profile8, document: &[u8]) -> Error
where
    V: for<'de> Deserialize<'de>,
{
    match decode::<V>(profile, document) {
        Ok(_) => panic!("the document must not decode"),
        Err(error) => error,
    }
}

// ---------------------------------------------------------------------------
// the declared types
// ---------------------------------------------------------------------------

/// A one-field record.
#[derive(Debug, PartialEq)]
struct Rec {
    a: u8,
}

impl Serialize for Rec {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<u8 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("a", type_name::<u8>(), &self.a)
        })
    }
}

impl<'de> Deserialize<'de> for Rec {
    const SHAPE: Shape = Shape::Record;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let mut a: Option<u8> = None;
        while let Some(header) = source.next_field()? {
            if header.name_is("a") {
                header
                    .expect_marker(type_name::<u8>())
                    .map_err(|error| error.in_field("a"))?;
                if a.is_some() {
                    return Err(Error::new(ErrorKind::DuplicateField).in_field("a"));
                }
                a = Some(source.field_value::<u8>().map_err(|e| e.in_field("a"))?);
            } else {
                return Err(Error::new(ErrorKind::UnknownField));
            }
        }
        let a = a.ok_or_else(|| Error::new(ErrorKind::MissingField { name: "a" }).in_field("a"))?;
        Ok(Rec { a })
    }
}

/// A two-field record, so that a missing field has something to be missing.
#[derive(Debug, PartialEq)]
struct Pair2 {
    a: u8,
    b: u8,
}

impl Serialize for Pair2 {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<u8 as Serialize>::DEPTH, <u8 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 2];
        sink.record(&mut slots, |record| {
            record.field("a", type_name::<u8>(), &self.a)?;
            record.field("b", type_name::<u8>(), &self.b)
        })
    }
}

impl<'de> Deserialize<'de> for Pair2 {
    const SHAPE: Shape = Shape::Record;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let mut a: Option<u8> = None;
        let mut b: Option<u8> = None;
        while let Some(header) = source.next_field()? {
            if header.name_is("a") {
                header
                    .expect_marker(type_name::<u8>())
                    .map_err(|error| error.in_field("a"))?;
                if a.is_some() {
                    return Err(Error::new(ErrorKind::DuplicateField).in_field("a"));
                }
                a = Some(source.field_value::<u8>().map_err(|e| e.in_field("a"))?);
            } else if header.name_is("b") {
                header
                    .expect_marker(type_name::<u8>())
                    .map_err(|error| error.in_field("b"))?;
                if b.is_some() {
                    return Err(Error::new(ErrorKind::DuplicateField).in_field("b"));
                }
                b = Some(source.field_value::<u8>().map_err(|e| e.in_field("b"))?);
            } else {
                return Err(Error::new(ErrorKind::UnknownField));
            }
        }
        let a = a.ok_or_else(|| Error::new(ErrorKind::MissingField { name: "a" }).in_field("a"))?;
        let b = b.ok_or_else(|| Error::new(ErrorKind::MissingField { name: "b" }).in_field("b"))?;
        Ok(Pair2 { a, b })
    }
}

/// A record with mixed-depth leaves, for the nested round-trip.
#[derive(Debug, PartialEq)]
struct Point {
    x: u32,
    y: i32,
}

impl Serialize for Point {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<u32 as Serialize>::DEPTH, <i32 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 2];
        sink.record(&mut slots, |record| {
            record.field("x", type_name::<u32>(), &self.x)?;
            record.field("y", type_name::<i32>(), &self.y)
        })
    }
}

impl<'de> Deserialize<'de> for Point {
    const SHAPE: Shape = Shape::Record;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let mut x: Option<u32> = None;
        let mut y: Option<i32> = None;
        while let Some(header) = source.next_field()? {
            if header.name_is("x") {
                header
                    .expect_marker(type_name::<u32>())
                    .map_err(|error| error.in_field("x"))?;
                x = Some(source.field_value::<u32>().map_err(|e| e.in_field("x"))?);
            } else if header.name_is("y") {
                header
                    .expect_marker(type_name::<i32>())
                    .map_err(|error| error.in_field("y"))?;
                y = Some(source.field_value::<i32>().map_err(|e| e.in_field("y"))?);
            } else {
                return Err(Error::new(ErrorKind::UnknownField));
            }
        }
        let x = x.ok_or_else(|| Error::new(ErrorKind::MissingField { name: "x" }).in_field("x"))?;
        let y = y.ok_or_else(|| Error::new(ErrorKind::MissingField { name: "y" }).in_field("y"))?;
        Ok(Point { x, y })
    }
}

/// A newtype: one envelope holding one item envelope.
#[derive(Debug, PartialEq)]
struct Newtype(u16);

impl Serialize for Newtype {
    const SHAPE: Shape = Shape::Tuple;
    const DEPTH: u16 = depth_of(&[<u16 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.item_run(&self.0)
    }
}

impl<'de> Deserialize<'de> for Newtype {
    const SHAPE: Shape = Shape::Tuple;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let arity = source.item_count();
        if arity != 1 {
            return Err(Error::new(ErrorKind::WrongArity {
                expected: 1,
                actual: arity,
            }));
        }
        Ok(Newtype(
            source.next_value::<u16>().map_err(|e| e.at_index(0))?,
        ))
    }
}

/// A unit struct: one empty record envelope.
#[derive(Debug, PartialEq)]
struct Unit;

impl Serialize for Unit {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = 0;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots: [FieldSlot<'static>; 0] = [];
        sink.record(&mut slots, |_| Ok(()))
    }
}

impl<'de> Deserialize<'de> for Unit {
    const SHAPE: Shape = Shape::Record;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        if source.next_field()?.is_some() {
            return Err(Error::new(ErrorKind::UnknownField));
        }
        Ok(Unit)
    }
}

/// An enum with a unit variant and a single-field variant.
#[derive(Debug, PartialEq)]
enum En {
    A,
    B(u8),
}

impl Serialize for En {
    const SHAPE: Shape = Shape::Enum;
    const DEPTH: u16 = depth_of(&[<u8 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        match self {
            En::A => sink.variant("A"),
            En::B(value) => sink.variant_with("B", value),
        }
    }
}

impl<'de> Deserialize<'de> for En {
    const SHAPE: Shape = Shape::Enum;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let arity = source.item_count();
        let variant = source.next_envelope()?;
        if variant.payload_eq_ascii("A") {
            if arity != 1 {
                return Err(Error::new(ErrorKind::WrongArity {
                    expected: 1,
                    actual: arity,
                })
                .in_variant("A"));
            }
            Ok(En::A)
        } else if variant.payload_eq_ascii("B") {
            if arity != 2 {
                return Err(Error::new(ErrorKind::WrongArity {
                    expected: 2,
                    actual: arity,
                })
                .in_variant("B"));
            }
            let mut body = source.next_envelope()?;
            let value = u8::deserialize(&mut body).map_err(|error| error.in_variant("B"))?;
            Ok(En::B(value))
        } else {
            Err(Error::new(ErrorKind::UnknownVariant))
        }
    }
}

/// A borrowed-text record: the decoded `&str` must borrow the input, not copy out of it.
#[derive(Debug, PartialEq)]
struct Text<'a> {
    s: &'a str,
}

impl Serialize for Text<'_> {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<&'static str as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("s", type_name::<&str>(), &self.s)
        })
    }
}

impl<'de> Deserialize<'de> for Text<'de> {
    const SHAPE: Shape = Shape::Record;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let mut s: Option<&'de str> = None;
        while let Some(header) = source.next_field()? {
            if header.name_is("s") {
                header
                    .expect_marker(type_name::<&str>())
                    .map_err(|error| error.in_field("s"))?;
                s = Some(source.field_value::<&'de str>().map_err(|e| e.in_field("s"))?);
            } else {
                return Err(Error::new(ErrorKind::UnknownField));
            }
        }
        let s = s.ok_or_else(|| Error::new(ErrorKind::MissingField { name: "s" }).in_field("s"))?;
        Ok(Text { s })
    }
}

/// A stand-in for `Vec<T>`: the mandatory empty header envelope, then one item envelope per
/// element. Its real impls are `sdave-alloc`'s; this exercises the codec's list framing.
#[derive(Debug, PartialEq)]
struct List(Vec<u8>);

impl<'de> Deserialize<'de> for List {
    const SHAPE: Shape = Shape::Tuple;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        source.list_header()?;
        let mut items = Vec::new();
        let mut index = 0usize;
        while !source.at_end() {
            items.push(
                source
                    .next_value::<u8>()
                    .map_err(|error| error.at_index(index))?,
            );
            index += 1;
        }
        Ok(List(items))
    }
}

/// A nested aggregate: a record holding a newtype and a tuple.
#[derive(Debug, PartialEq)]
struct Nested {
    inner: Newtype,
    pair: (u8, bool),
}

impl Serialize for Nested {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[
        <Newtype as Serialize>::DEPTH,
        <(u8, bool) as Serialize>::DEPTH,
    ]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 2];
        sink.record(&mut slots, |record| {
            record.field("inner", type_name::<Newtype>(), &self.inner)?;
            record.field("pair", type_name::<(u8, bool)>(), &self.pair)
        })
    }
}

impl<'de> Deserialize<'de> for Nested {
    const SHAPE: Shape = Shape::Record;

    fn deserialize(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        let mut inner: Option<Newtype> = None;
        let mut pair: Option<(u8, bool)> = None;
        while let Some(header) = source.next_field()? {
            if header.name_is("inner") {
                header
                    .expect_marker(type_name::<Newtype>())
                    .map_err(|error| error.in_field("inner"))?;
                inner = Some(source.field_value::<Newtype>().map_err(|e| e.in_field("inner"))?);
            } else if header.name_is("pair") {
                header
                    .expect_marker(type_name::<(u8, bool)>())
                    .map_err(|error| error.in_field("pair"))?;
                pair = Some(
                    source
                        .field_value::<(u8, bool)>()
                        .map_err(|e| e.in_field("pair"))?,
                );
            } else {
                return Err(Error::new(ErrorKind::UnknownField));
            }
        }
        let inner = inner
            .ok_or_else(|| Error::new(ErrorKind::MissingField { name: "inner" }).in_field("inner"))?;
        let pair = pair
            .ok_or_else(|| Error::new(ErrorKind::MissingField { name: "pair" }).in_field("pair"))?;
        Ok(Nested { inner, pair })
    }
}

// ---------------------------------------------------------------------------
// round-trips
// ---------------------------------------------------------------------------

#[test]
fn scalar_leaves_round_trip() {
    let profile = wide();
    roundtrip(&profile, 7u8);
    roundtrip(&profile, -7i8);
    roundtrip(&profile, 70_000u32);
    roundtrip(&profile, -70_000i64);
    roundtrip(&profile, 3.5f64);
    roundtrip(&profile, -0.25f32);
    roundtrip(&profile, true);
    roundtrip(&profile, false);
    roundtrip(&profile, '☃');
    roundtrip(&profile, ());
}

#[test]
fn nonzero_leaves_round_trip() {
    let profile = wide();
    roundtrip(&profile, core::num::NonZeroU32::new(7).expect("non-zero"));
    roundtrip(&profile, core::num::NonZeroI16::new(-3).expect("non-zero"));
}

#[test]
fn a_non_max_usize_round_trips() {
    let profile = wide();
    roundtrip(&profile, NonMaxUsize::new(0));
    roundtrip(&profile, NonMaxUsize::new(41));
}

#[test]
fn tuples_round_trip() {
    let profile = wide();
    roundtrip(&profile, (1u8, -2i16, true));
    roundtrip(&profile, (1u8,));
    roundtrip(&profile, (7u8, (8u8, 9u8)));
}

#[test]
fn option_and_result_round_trip() {
    let profile = wide();
    roundtrip(&profile, Some(5u8));
    roundtrip(&profile, None::<u8>);
    roundtrip(&profile, Ok::<u8, u8>(1));
    roundtrip(&profile, Err::<u8, u8>(2));
}

#[test]
fn records_round_trip() {
    let profile = wide();
    roundtrip(&profile, Rec { a: 7 });
    roundtrip(&profile, Pair2 { a: 1, b: 2 });
    roundtrip(&profile, Point { x: 42, y: -1 });
}

#[test]
fn a_unit_struct_round_trips() {
    roundtrip(&wide(), Unit);
    roundtrip(&wide(), Newtype(9));
}

#[test]
fn enums_round_trip() {
    let profile = wide();
    roundtrip(&profile, En::A);
    roundtrip(&profile, En::B(9));
}

#[test]
fn nested_aggregates_round_trip() {
    let profile = wide();
    roundtrip(
        &profile,
        Nested {
            inner: Newtype(3),
            pair: (4, true),
        },
    );
}

#[test]
fn a_record_decodes_its_fields_in_any_order() {
    // The wire order is `b` then `a` — the opposite of the declaration — and decoding still fills
    // both fields, because named fields are matched by name.
    let profile = minimal();
    let marker = type_name::<Pair2>();
    let document = format!("{marker}?b:u8^2~a:u8^1~!").into_bytes();
    let decoded = decode::<Pair2>(&profile, &document).expect("decoded");
    assert_eq!(decoded, Pair2 { a: 1, b: 2 });
}

#[test]
fn canonical_decimals_are_accepted_on_decode() {
    let profile = minimal();
    // Leading zeros are tolerated: `-001` reads back as `-1`, and `07` as `7`.
    assert_eq!(decode::<i32>(&profile, b"i32^-001~").unwrap(), -1);
    assert_eq!(decode::<i32>(&profile, b"i32^-0~").unwrap(), 0);
    assert_eq!(decode::<u8>(&profile, b"u8^007~").unwrap(), 7);
}

#[test]
fn a_second_document_after_the_first_is_ignored() {
    let profile = minimal();
    let decoded = decode::<u8>(&profile, b"u8^7~u8^9~").expect("decoded");
    assert_eq!(decoded, 7);
}

// ---------------------------------------------------------------------------
// leaves borrow from the input
// ---------------------------------------------------------------------------

#[test]
fn a_decoded_str_borrows_from_the_input() {
    let profile = wide();
    let value = Text { s: "hi" };
    let mut buffer = vec![0u8; 256];
    let end = profile
        .append_to_slice(&value, &mut buffer, 0)
        .expect("appended");
    let document = &buffer[..end];

    let decoded: Text<'_> = profile.deserialize(document, &Global).expect("decoded");
    assert_eq!(decoded, value);

    // The borrowed text is a subslice of the input, not a copy out of it.
    let base = document.as_ptr() as usize;
    let text = decoded.s.as_ptr() as usize;
    assert!(text >= base, "the text starts inside the input");
    assert!(
        text + decoded.s.len() <= base + document.len(),
        "the text ends inside the input"
    );
}

#[test]
fn a_decoded_byte_slice_borrows_from_the_input() {
    let profile = minimal();
    let document = b"&[u8]^hi~".to_vec();
    let decoded: &[u8] = profile.deserialize(&document, &Global).expect("decoded");
    assert_eq!(decoded, b"hi");

    let base = document.as_ptr() as usize;
    let bytes = decoded.as_ptr() as usize;
    assert!(bytes >= base && bytes + decoded.len() <= base + document.len());
}

#[test]
fn a_decoded_str_at_the_root_borrows_from_the_input() {
    let profile = minimal();
    let document = b"&str^hi~".to_vec();
    let decoded: &str = profile.deserialize(&document, &Global).expect("decoded");
    assert_eq!(decoded, "hi");

    let base = document.as_ptr() as usize;
    let text = decoded.as_ptr() as usize;
    assert!(text >= base && text + decoded.len() <= base + document.len());
}

// ---------------------------------------------------------------------------
// marker validation
// ---------------------------------------------------------------------------

#[test]
fn a_wrong_root_marker_is_rejected() {
    let error = decode_error::<u16>(&minimal(), b"u8^7~");
    assert_eq!(
        *error.kind(),
        ErrorKind::UnexpectedMarker {
            expected: type_name::<u16>()
        }
    );
}

#[test]
fn a_wrong_field_marker_is_rejected_before_its_value_is_read() {
    let profile = minimal();
    let marker = type_name::<Rec>();
    // The field's own value is well formed; only the header's marker disagrees.
    let document = format!("{marker}?a:u16^1~!").into_bytes();
    let error = decode_error::<Rec>(&profile, &document);
    assert_eq!(
        *error.kind(),
        ErrorKind::UnexpectedMarker {
            expected: type_name::<u8>()
        }
    );
}

#[test]
fn an_error_carries_the_outer_inner_field_path() {
    let profile = minimal();
    let marker = type_name::<Pair2>();
    // `b` comes first and carries the wrong marker, so the error's path names `b`.
    let document = format!("{marker}?b:u16^1~a:u8^2~!").into_bytes();
    let error = decode_error::<Pair2>(&profile, &document);
    assert_eq!(
        error.path().steps().next(),
        Some(Step::Field("b")),
        "path was `{}`",
        error.path()
    );
}

// ---------------------------------------------------------------------------
// record errors
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_field_is_rejected() {
    let profile = minimal();
    let marker = type_name::<Rec>();
    let document = format!("{marker}?z:u8^1~!").into_bytes();
    let error = decode_error::<Rec>(&profile, &document);
    assert_eq!(*error.kind(), ErrorKind::UnknownField);
}

#[test]
fn a_duplicate_field_is_rejected() {
    let profile = minimal();
    let marker = type_name::<Rec>();
    let document = format!("{marker}?a:u8^1~a:u8^2~!").into_bytes();
    let error = decode_error::<Rec>(&profile, &document);
    assert_eq!(*error.kind(), ErrorKind::DuplicateField);
}

#[test]
fn a_missing_field_is_rejected() {
    let profile = minimal();
    let marker = type_name::<Pair2>();
    let document = format!("{marker}?a:u8^1~!").into_bytes();
    let error = decode_error::<Pair2>(&profile, &document);
    assert_eq!(*error.kind(), ErrorKind::MissingField { name: "b" });
}

// ---------------------------------------------------------------------------
// variant errors
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_variant_is_rejected() {
    let profile = minimal();
    let marker = type_name::<En>();
    let document = format!("{marker}?^C~!").into_bytes();
    let error = decode_error::<En>(&profile, &document);
    assert_eq!(*error.kind(), ErrorKind::UnknownVariant);
}

#[test]
fn a_variant_with_the_wrong_arity_is_rejected() {
    let profile = minimal();
    let marker = type_name::<En>();

    // `B` declares one associated value; the document supplies none.
    let short = format!("{marker}?^B~!").into_bytes();
    assert_eq!(
        *decode_error::<En>(&profile, &short).kind(),
        ErrorKind::WrongArity {
            expected: 2,
            actual: 1
        }
    );

    // `A` declares none; the document supplies one.
    let long = format!("{marker}?^A~^7~!").into_bytes();
    assert_eq!(
        *decode_error::<En>(&profile, &long).kind(),
        ErrorKind::WrongArity {
            expected: 1,
            actual: 2
        }
    );
}

// ---------------------------------------------------------------------------
// list framing
// ---------------------------------------------------------------------------

#[test]
fn a_list_decodes_its_mandatory_header_then_its_items() {
    let profile = minimal();
    let marker = type_name::<List>();

    let empty = format!("{marker}?^~!").into_bytes();
    assert_eq!(decode::<List>(&profile, &empty).unwrap(), List(Vec::new()));

    let one = format!("{marker}?^~^7~!").into_bytes();
    assert_eq!(decode::<List>(&profile, &one).unwrap(), List(vec![7]));
}

#[test]
fn a_missing_list_header_is_rejected() {
    let profile = minimal();
    let marker = type_name::<List>();
    let document = format!("{marker}^~").into_bytes();
    let error = decode_error::<List>(&profile, &document);
    assert_eq!(*error.kind(), ErrorKind::MissingListHeader);
}

#[test]
fn a_nonempty_list_header_is_rejected() {
    let profile = minimal();
    let marker = type_name::<List>();
    let document = format!("{marker}?^x~!").into_bytes();
    let error = decode_error::<List>(&profile, &document);
    assert_eq!(*error.kind(), ErrorKind::NonEmptyListHeader);
}

// ---------------------------------------------------------------------------
// leaf errors and the bounds
// ---------------------------------------------------------------------------

#[test]
fn a_truncated_frame_is_rejected() {
    // The root envelope never finds its delimiter, so it is not confirmed.
    let error = decode_error::<u8>(&minimal(), b"u8^7");
    assert_eq!(*error.kind(), ErrorKind::Truncated);
}

#[test]
fn a_non_finite_float_is_rejected() {
    let profile = minimal();
    assert_eq!(
        *decode_error::<f64>(&profile, b"f64^NaN~").kind(),
        ErrorKind::NonFiniteFloat
    );
    assert_eq!(
        *decode_error::<f64>(&profile, b"f64^inf~").kind(),
        ErrorKind::NonFiniteFloat
    );
}

#[test]
fn an_invalid_boolean_is_rejected() {
    let error = decode_error::<bool>(&minimal(), b"bool^yes~");
    assert_eq!(*error.kind(), ErrorKind::InvalidBoolean);
}

#[test]
fn an_invalid_character_is_rejected() {
    let profile = minimal();
    assert_eq!(
        *decode_error::<char>(&profile, b"char^ab~").kind(),
        ErrorKind::InvalidCharacter
    );
    assert_eq!(
        *decode_error::<char>(&profile, b"char^~").kind(),
        ErrorKind::InvalidCharacter
    );
}

#[test]
fn an_out_of_range_number_is_rejected() {
    let profile = minimal();
    assert_eq!(
        *decode_error::<u8>(&profile, b"u8^256~").kind(),
        ErrorKind::NumberOutOfRange
    );
    assert_eq!(
        *decode_error::<i8>(&profile, b"i8^-129~").kind(),
        ErrorKind::NumberOutOfRange
    );
    // A `-` is not a value an unsigned type can carry.
    assert_eq!(
        *decode_error::<u8>(&profile, b"u8^-1~").kind(),
        ErrorKind::NumberOutOfRange
    );
    // A malformed spelled number.
    assert_eq!(
        *decode_error::<u8>(&profile, b"u8^x2~").kind(),
        ErrorKind::InvalidNumber
    );
}

#[test]
fn a_non_max_usize_at_usize_max_is_out_of_range_not_a_panic() {
    let profile = minimal();
    let marker = type_name::<NonMaxUsize>();
    let document = format!("{marker}^{}~", usize::MAX).into_bytes();
    assert_eq!(
        *decode_error::<NonMaxUsize>(&profile, &document).kind(),
        ErrorKind::NumberOutOfRange
    );
}

#[test]
fn the_depth_bound_is_enforced_on_decode() {
    // The root's envelope is depth 1 and each field's envelope is depth 2, so a profile that allows
    // only one level rejects the record's fields.
    let profile = profile8(Variant::V1, pairs8(MINIMAL), Limits::new(1, 0, 0));
    let marker = type_name::<Rec>();
    let document = format!("{marker}?a:u8^1~!").into_bytes();
    let error = decode_error::<Rec>(&profile, &document);
    assert_eq!(
        *error.kind(),
        ErrorKind::LimitExceeded {
            limit: Limit::Depth,
            value: 2,
            max: 1,
        }
    );
}

#[test]
fn the_length_bound_is_enforced_on_decode() {
    let profile = profile8(Variant::V1, pairs8(MINIMAL), Limits::new(0, 4, 0));
    let error = decode_error::<u8>(&profile, b"u8^7~");
    assert_eq!(
        *error.kind(),
        ErrorKind::LimitExceeded {
            limit: Limit::Length,
            value: 5,
            max: 4,
        }
    );
}

// ---------------------------------------------------------------------------
// the profile the tests never touch directly
// ---------------------------------------------------------------------------

/// `Config::new`/`NamePolicy` are exercised elsewhere; this keeps the imports honest.
#[test]
fn a_verbatim_profile_is_what_these_tests_use() {
    let profile = Config::new(
        Variant::V1,
        pairs8(MINIMAL),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
        NamePolicy::VERBATIM,
    )
    .expect("a valid profile");
    assert!(profile.name_policy().is_verbatim());
    assert_eq!(decode::<u8>(&profile, b"u8^7~").unwrap(), 7);
}
