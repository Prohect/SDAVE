//! The derive-facing append surface: the sink's marker and header emission, the one append entry
//! per type, the record collector's ordering, and the shapes.
//!
//! The expectations are the codec README's wire rules written out: a field run is `name:marker`
//! followed by the value's envelope, a marker is the policy-rendered compiler spelling, and a
//! record's field order is the serializer's.
// `Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

mod common;

use common::*;
use sdave_codec::{
    FieldSlot, Limits, NamePolicy, Serialize, Shape, ShortType, Sink, deepest_of, depth_of,
};

#[test]
fn a_verbatim_policy_emits_the_compiler_spelling() {
    let limits = Limits::UNBOUNDED;
    let mut recording = Recording::new();
    {
        let mut sink = Sink::new(&mut recording, &NamePolicy::VERBATIM, &limits);
        assert_eq!(sink.cursor(), 0);
        assert_eq!(sink.depth(), 0);
        assert!(sink.name_policy().is_verbatim());
        assert_eq!(sink.limits(), &Limits::UNBOUNDED);
        sink.field_header("timeout_ms", "core::option::Option<u64>")
            .unwrap();
    }
    assert_eq!(recording.text(), "timeout_ms:core::option::Option<u64>");
}

#[test]
fn a_whitelisted_constructor_is_emitted_short() {
    static ENTRIES: &[ShortType] = &[
        ShortType::of::<Vec<()>>(),
        ShortType::of::<core::option::Option<u8>>(),
    ];
    let policy = NamePolicy::new(ENTRIES);
    let limits = Limits::UNBOUNDED;

    let mut recording = Recording::new();
    {
        let mut sink = Sink::new(&mut recording, &policy, &limits);
        sink.marker("alloc::vec::Vec<u8>").unwrap();
        sink.text(" ").unwrap();
        sink.field_header("timeout_ms", "core::option::Option<u64>")
            .unwrap();
        sink.utf8("é".as_bytes()).unwrap();
    }
    // A whitelisted constructor contributes its last identifier and keeps whatever follows that
    // path — here the arguments — verbatim.
    assert_eq!(recording.text(), "Vec<u8> timeout_ms:Option<u64>é");
}

// ---------------------------------------------------------------------------
// the one append entry
// ---------------------------------------------------------------------------

/// A leaf stand-in: its payload is its own text, and it frames nothing itself.
struct Count(u32);

impl Serialize for Count {
    const SHAPE: Shape = Shape::Tuple;
    const DEPTH: u16 = 0;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        sink.text(&self.0.to_string())
    }
}

/// A record stand-in whose fields tie on depth, so the serializer keeps declaration order.
struct TwoFields {
    left: Count,
    right: Count,
}

impl Serialize for TwoFields {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[Count::DEPTH, Count::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 2];
        sink.record(&mut slots, |record| {
            record.field("left", "sink::Count", &self.left)?;
            record.field("right", "sink::Count", &self.right)
        })
    }
}

#[test]
fn a_record_body_announces_its_fields_and_the_serializer_appends_them() {
    let value = TwoFields {
        left: Count(1),
        right: Count(23),
    };

    // Each field run is its header followed by the value's own envelope.
    assert_eq!(body(&value), "left:sink::Count<1>right:sink::Count<23>");
    assert_eq!(TwoFields::DEPTH, 1);
    assert_eq!(Count::DEPTH, 0);
}

/// A record with one nested field: `Some(Count)` is one envelope deeper than `Count`.
struct Ordered {
    nested: Option<Count>,
    flat: Count,
}

impl Serialize for Ordered {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<Option<Count> as Serialize>::DEPTH, Count::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 2];
        // Announced nested-first, the opposite of the order the serializer must use.
        sink.record(&mut slots, |record| {
            record.field("nested", "core::option::Option<sink::Count>", &self.nested)?;
            record.field("flat", "sink::Count", &self.flat)
        })
    }
}

/// The same fields, announced flat-first: the wire body must come out identical.
struct OrderedReversed {
    nested: Option<Count>,
    flat: Count,
}

impl Serialize for OrderedReversed {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<Option<Count> as Serialize>::DEPTH, Count::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 2];
        sink.record(&mut slots, |record| {
            record.field("flat", "sink::Count", &self.flat)?;
            record.field("nested", "core::option::Option<sink::Count>", &self.nested)
        })
    }
}

#[test]
fn the_serializer_appends_a_records_fields_shallowest_first() {
    // `Some(Count)` has depth 1, `Count` depth 0, so the flat field goes first whatever order the
    // body announces them in.
    let expected = "flat:sink::Count<2>nested:core::option::Option<sink::Count><<Some><1>>";

    assert_eq!(
        body(&Ordered {
            nested: Some(Count(1)),
            flat: Count(2),
        }),
        expected
    );
    assert_eq!(
        body(&OrderedReversed {
            nested: Some(Count(1)),
            flat: Count(2),
        }),
        expected
    );
    assert_eq!(Ordered::DEPTH, 2);
}

// ---------------------------------------------------------------------------
// shapes
// ---------------------------------------------------------------------------

#[test]
fn shapes_name_the_framing_and_flag_the_dyn_case() {
    assert_eq!(Shape::Record.name(), "Record");
    assert_eq!(Shape::Tuple.name(), "Tuple");
    assert_eq!(Shape::Enum.name(), "Enum");
    assert_eq!(Shape::Dynamic.name(), "Dynamic");

    assert!(Shape::Record.is_aggregate());
    assert!(Shape::Tuple.is_aggregate());
    assert!(!Shape::Enum.is_aggregate());
    assert!(!Shape::Dynamic.is_aggregate());

    assert!(Shape::Record.is_declared());
    assert!(Shape::Enum.is_declared());
    // Only a `dyn` payload is not statically known.
    assert!(!Shape::Dynamic.is_declared());

    assert_eq!(Count::SHAPE, Shape::Tuple);
    assert_eq!(TwoFields::SHAPE, Shape::Record);
}

#[test]
fn the_structure_depth_is_one_more_than_the_deepest_child() {
    assert_eq!(deepest_of(&[]), 0);
    assert_eq!(deepest_of(&[0, 3, 1]), 3);
    assert_eq!(depth_of(&[]), 1);
    assert_eq!(depth_of(&[0, 0]), 1);
    assert_eq!(depth_of(&[1, 0]), 2);
    // Saturating: an aggregate over a value that declares itself the deepest stays the deepest.
    assert_eq!(depth_of(&[u16::MAX]), u16::MAX);
}
