//! The containers and wrappers: their framing and their structure depth, in the codec README's
//! grammar.
//!
//! A frame is recorded as `<payload>`, so a body reads directly as its envelope structure: a tuple
//! is one envelope per element, an enum variant is a variant envelope plus (when it has fields) one
//! associated-value envelope, and `Box`/`&` add nothing.
// `Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

mod common;

use common::*;
use sdave_codec::{
    Basic, BasicType, DecodeSource, Error, ErrorKind, FieldSlot, Limits, NamePolicy, Serialize,
    Shape, Sink, depth_of,
};

// ---------------------------------------------------------------------------
// tuples
// ---------------------------------------------------------------------------

#[test]
fn a_tuple_is_one_envelope_per_element() {
    assert_eq!(body(&(1u8, 2u8)), "<1><2>");
    assert_eq!(body(&(1u8, 2u8, 3u8)), "<1><2><3>");
    assert_eq!(body(&(1u8,)), "<1>");
    assert_eq!(body(&("a", 2u8)), "<a><2>");
}

#[test]
fn tuple_arity_is_capped_at_twelve() {
    let twelve = (
        1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8, 9u8, 10u8, 11u8, 12u8,
    );
    let written = body(&twelve);
    assert_eq!(written.matches('<').count(), 12);
    assert_eq!(written, "<1><2><3><4><5><6><7><8><9><10><11><12>");
    assert_eq!(<(u8, u8) as Serialize>::SHAPE, Shape::Tuple);
}

#[test]
fn a_tuples_depth_is_one_more_than_its_deepest_element() {
    assert_eq!(<(u8, u8) as Serialize>::DEPTH, 1);
    assert_eq!(<(u8, (u8, u8)) as Serialize>::DEPTH, 2);
    assert_eq!(<(Option<u8>, u8) as Serialize>::DEPTH, 2);
    assert_eq!(<(u8,) as Serialize>::DEPTH, 1);
}

// ---------------------------------------------------------------------------
// enums
// ---------------------------------------------------------------------------

#[test]
fn option_is_a_variant_plus_an_associated_value() {
    assert_eq!(body(&Some(5u8)), "<Some><5>");
    assert_eq!(body(&None::<u8>), "<None>");
    // The associated value is itself framed, so a nested enum nests: `Some(None)` is the outer
    // variant envelope, then one associated-value envelope holding the inner enum's body (which is
    // the inner variant envelope alone), then the inner associated-value envelope for `Some(1)`.
    assert_eq!(body(&Some(None::<u8>)), "<Some><<None>>");
    assert_eq!(body(&Some(Some(1u8))), "<Some><<Some><1>>");
    assert_eq!(<Option<u8> as Serialize>::SHAPE, Shape::Enum);
    assert_eq!(<Option<u8> as Serialize>::DEPTH, 1);
    assert_eq!(<Option<Option<u8>> as Serialize>::DEPTH, 2);
    assert_eq!(<Option<u8> as Serialize>::DEPTH + 1, 2);
}

#[test]
fn result_is_a_variant_plus_an_associated_value() {
    assert_eq!(body(&Ok::<u8, bool>(1)), "<Ok><1>");
    assert_eq!(body(&Err::<u8, bool>(true)), "<Err><true>");
    assert_eq!(<Result<u8, bool> as Serialize>::SHAPE, Shape::Enum);
    assert_eq!(<Result<u8, bool> as Serialize>::DEPTH, 1);
    assert_eq!(<Result<u8, Option<u8>> as Serialize>::DEPTH, 2);
}

/// A multi-field variant: the variant envelope, then one associated-value envelope holding the
/// positional tuple body.
struct Both {
    a: u8,
    b: u8,
}

impl Serialize for Both {
    const SHAPE: Shape = Shape::Enum;
    const DEPTH: u16 = depth_of(&[<u8 as Serialize>::DEPTH, <u8 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.variant_fields("Both", |sink| {
            sink.item_run(&self.a)?;
            sink.item_run(&self.b)
        })
    }
}

#[test]
fn a_multi_field_variant_holds_a_positional_body() {
    assert_eq!(body(&Both { a: 1, b: 2 }), "<Both><<1><2>>");
    assert_eq!(Both::SHAPE, Shape::Enum);
    assert_eq!(Both::DEPTH, 1);
}

/// A unit variant stand-in: only the variant envelope.
struct Nothing;

impl Serialize for Nothing {
    const SHAPE: Shape = Shape::Enum;
    const DEPTH: u16 = 0;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.variant("Nothing")
    }
}

#[test]
fn a_unit_variant_is_the_variant_envelope_alone() {
    assert_eq!(body(&Nothing), "<Nothing>");
}

/// A zero-field tuple variant stand-in: the variant envelope plus an *empty* associated-value
/// envelope, which is what keeps it distinct from a unit variant.
struct Empty;

impl Serialize for Empty {
    const SHAPE: Shape = Shape::Enum;
    const DEPTH: u16 = 0;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.variant_fields("Empty", |_| Ok(()))
    }
}

#[test]
fn a_zero_field_tuple_variant_is_not_a_unit_variant() {
    assert_eq!(body(&Empty), "<Empty><>");
    assert_ne!(body(&Empty), body(&Nothing));
}

// ---------------------------------------------------------------------------
// wrappers
// ---------------------------------------------------------------------------

#[test]
fn box_and_reference_are_transparent() {
    assert_eq!(body(&Box::new(7u8)), "7");
    assert_eq!(body(&Box::new("hi")), "hi");
    assert_eq!(body(&7u8), body(&&7u8));
    assert_eq!(body(&Some(1u8)), body(&Box::new(Some(1u8))));
    // A wrapper keeps the shape and the depth of what it points at.
    assert_eq!(<Box<u8> as Serialize>::SHAPE, Shape::Tuple);
    assert_eq!(<Box<Option<u8>> as Serialize>::SHAPE, Shape::Enum);
    assert_eq!(<Box<u8> as Serialize>::DEPTH, 0);
    assert_eq!(<Box<Option<u8>> as Serialize>::DEPTH, 1);
}

// ---------------------------------------------------------------------------
// the basic escape hatch
// ---------------------------------------------------------------------------

/// An opaque type: its whole representation is hand-written, and it adds no envelope or marker.
struct Raw;

impl Serialize for Raw {
    const SHAPE: Shape = Shape::Tuple;
    const DEPTH: u16 = 0;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.item_basic(self)
    }
}

impl BasicType for Raw {
    fn append_basic(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.text("raw!")
    }

    fn read_basic<'de>(_source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        // The read side of the escape hatch is exercised with the decoding source.
        Err(Error::new(ErrorKind::NotBorrowable))
    }
}

#[test]
fn a_basic_value_contributes_only_its_envelope_payload() {
    // One envelope, and no marker of its own.
    assert_eq!(body(&Raw), "<raw!>");
    assert_eq!(<Raw as Serialize>::DEPTH, 0);

    // As a record field: the header carries the declared spelling, the value adds no envelope.
    let limits = Limits::UNBOUNDED;
    let mut recording = Recording::new();
    {
        let mut sink = Sink::new(&mut recording, &NamePolicy::VERBATIM, &limits);
        sink.field_basic("r", "cont::Raw", &Raw).unwrap();
    }
    assert_eq!(recording.text(), "r:cont::Raw<raw!>");
}

/// A record with a basic field: the adapter is announced like any other field, so the basic type
/// needs no `Serialize` implementation of its own beyond the adapter.
struct WithBasic {
    label: u8,
    raw: Raw,
}

impl Serialize for WithBasic {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<u8 as Serialize>::DEPTH, <Basic<&Raw> as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        // The values must outlive the `record` call: the serializer appends them after the body
        // has announced them all.
        let basic = Basic(&self.raw);
        let mut slots = [FieldSlot::EMPTY; 2];
        sink.record(&mut slots, |record| {
            record.field("label", "u8", &self.label)?;
            record.field("raw", "cont::Raw", &basic)
        })
    }
}

#[test]
fn a_record_field_can_be_a_basic_value() {
    let value = WithBasic { label: 7, raw: Raw };
    assert_eq!(body(&value), "label:u8<7>raw:cont::Raw<raw!>");
    // Both fields are announced at depth 0, so the declaration order is kept.
    assert_eq!(WithBasic::DEPTH, 1);
}

/// A record whose only field's declared type is unsized: the value is announced through a *sized*
/// reference to it, which is what the transparent `&T` implementation is for.
struct WithBorrowedText {
    text: &'static str,
}

impl Serialize for WithBorrowedText {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<&'static str as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            // `self.text` is `&str`: a sized value whose referent is `str`, and `&str: Serialize`
            // (transparent), so erasing it coerces `&&str` — never an unsized referent.
            record.field("text", "&str", &self.text)
        })
    }
}

#[test]
fn a_field_whose_referent_is_unsized_is_announced_through_its_reference() {
    assert_eq!(body(&WithBorrowedText { text: "hi" }), "text:&str<hi>");
    assert_eq!(WithBorrowedText::DEPTH, 1);
}
