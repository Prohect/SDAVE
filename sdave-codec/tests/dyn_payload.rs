//! The `dyn` payload machinery: identity is delivered, and the codec never constructs the object.
//!
//! Expectations are derived from the codec README's `dyn` rules — the field header keeps the field
//! type's spelling, the value bytes carry the concrete type's marked document (`marker-text ++
//! frame(body)`), and decoding only *locates* the marker, because resolving it is the application's
//! registry.
// `Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

mod common;

use core::any::{Any, type_name};
use std::alloc::Global;

use common::*;
use sdave_codec::{
    Config, DYN_PAYLOAD_DEPTH, DynSerialize, Error, ErrorKind, FieldSlot, FlatDeserializer, Limits,
    NamePolicy, PairSelect, Serialize, Shape, ShortType, Sink, State, Variant, depth_of,
    marker_eq_ascii,
};

/// The shape an application's object trait has: the codec's supertrait, plus its own methods.
trait ToolInput: DynSerialize + core::fmt::Debug + 'static {
    fn as_any(&self) -> &dyn Any;
}

/// The first concrete implementor: a borrowed-text field.
#[derive(Debug)]
struct ReadFile {
    path: &'static str,
}

impl Serialize for ReadFile {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<&'static str as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("path", type_name::<&'static str>(), &self.path)
        })
    }
}

impl DynSerialize for ReadFile {
    fn append_dyn(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.marked_document(type_name::<Self>(), self)
    }
}

impl ToolInput for ReadFile {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The second concrete implementor: a numeric field.
#[derive(Debug)]
struct SetTimeout {
    ms: u32,
}

impl Serialize for SetTimeout {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<u32 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("ms", type_name::<u32>(), &self.ms)
        })
    }
}

impl DynSerialize for SetTimeout {
    fn append_dyn(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.marked_document(type_name::<Self>(), self)
    }
}

impl ToolInput for SetTimeout {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The one impl the codec requires per object trait, which the `#[dyn_payload]` macro will emit.
impl Serialize for dyn ToolInput {
    const SHAPE: Shape = Shape::Dynamic;
    const DEPTH: u16 = DYN_PAYLOAD_DEPTH;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        self.append_dyn(sink)
    }
}

/// A holder with one `dyn` field.
struct Holder {
    v: Box<dyn ToolInput>,
}

impl Serialize for Holder {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<Box<dyn ToolInput> as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("v", type_name::<Box<dyn ToolInput>>(), &self.v)
        })
    }
}

static ENTRIES: &[ShortType] = &[
    ShortType::of::<Holder>(),
    ShortType::of::<ReadFile>(),
    ShortType::of::<SetTimeout>(),
    ShortType::of::<Box<()>>(),
];

fn dyn_profile() -> Profile {
    Config::new(
        Variant::V1,
        pairs(STD),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
        NamePolicy::new(ENTRIES),
    )
    .expect("a valid profile")
}

/// Append a value as a document and return its units.
fn append_document(profile: &Profile, value: &impl Serialize) -> Vec<char> {
    let mut buffer = blank(512);
    let end = profile
        .append_to_slice(value, &mut buffer, 0)
        .expect("appended");
    buffer.truncate(end);
    buffer
}

/// The payload units of the first *confirmed* envelope in `units`.
fn first_envelope_payload<'u>(units: &'u [char], profile: &Profile) -> &'u [char] {
    let mut deserializer = FlatDeserializer::new(
        units,
        profile.variant(),
        profile.limiter_pairs(),
        Vec::new(),
        Vec::new(),
    );
    for state in deserializer.iter() {
        if let State::E { envelop } = state
            && !envelop.is_phantom()
            && envelop.tail_offset().is_some()
        {
            let head = envelop.payload_head_offset().get();
            let end = envelop.payload_tail_offset().get();
            return &units[head..end];
        }
    }
    &[]
}

#[test]
fn a_dyn_field_carries_the_concrete_marked_document() {
    let profile = dyn_profile();
    let value = Holder {
        v: Box::new(ReadFile { path: "README.md" }),
    };

    // Walk in: the record's payload holds the field header and the field envelope; the field's
    // payload is the dyn payload's value bytes.
    let document = append_document(&profile, &value);
    let record_payload = first_envelope_payload(&document, &profile);
    let field_value = first_envelope_payload(record_payload, &profile);

    // Identity is delivered: the concrete type's rendered spelling leads the value bytes.
    let payload = profile
        .locate_dyn_payload(field_value, &Global)
        .expect("the concrete marked document is locatable");
    assert!(marker_eq_ascii(
        payload.marker(),
        profile.name_policy().render(type_name::<ReadFile>())
    ));
    assert!(!marker_eq_ascii(
        payload.marker(),
        profile
            .name_policy()
            .render(type_name::<Box<dyn ToolInput>>())
    ));

    // The value bytes are exactly the concrete type's own marked document, which is the shape a
    // standalone root has.
    assert_eq!(
        field_value,
        append_document(&profile, &ReadFile { path: "README.md" }).as_slice()
    );
    // Marker text then one envelope, with nothing between them.
    assert_eq!(
        payload.marker().len() + payload.envelope().len(),
        field_value.len()
    );
    assert_eq!(payload.envelope(), &field_value[payload.marker().len()..]);
    // And the body is that envelope's payload: the concrete record's body, whose field run carries
    // the declared field type's spelling.
    let body_text = text_of(payload.body());
    assert!(body_text.starts_with("path:"), "body was {body_text:?}");
    assert!(
        body_text.contains(type_name::<&'static str>()),
        "body was {body_text:?}"
    );
}

#[test]
fn two_concrete_types_deliver_two_identities() {
    let profile = dyn_profile();
    let read = append_document(&profile, &ReadFile { path: "README.md" });
    let timeout = append_document(&profile, &SetTimeout { ms: 30_000 });

    let read_marker = profile
        .locate_dyn_marker(&read, &Global)
        .expect("a locatable marker");
    let timeout_marker = profile
        .locate_dyn_marker(&timeout, &Global)
        .expect("a locatable marker");
    assert!(marker_eq_ascii(
        read_marker,
        profile.name_policy().render(type_name::<ReadFile>())
    ));
    assert!(marker_eq_ascii(
        timeout_marker,
        profile.name_policy().render(type_name::<SetTimeout>())
    ));
    assert_ne!(read, timeout);
}

#[test]
fn the_field_header_keeps_the_field_types_spelling() {
    // `Box<dyn …>` and `&dyn …` are different field types, and neither is the concrete type.
    assert_ne!(
        type_name::<Box<dyn ToolInput>>(),
        type_name::<&dyn ToolInput>()
    );
    assert_ne!(type_name::<Box<dyn ToolInput>>(), type_name::<ReadFile>());

    // Shortening does not propagate across nesting: whitelisting `Box` shortens the outer
    // constructor and leaves the trait's own path verbatim.
    static BOX_ONLY: &[ShortType] = &[ShortType::of::<Box<()>>()];
    let policy = NamePolicy::new(BOX_ONLY);
    assert_eq!(
        policy.render(type_name::<Box<dyn ToolInput>>()),
        format!("Box<{}>", type_name::<dyn ToolInput>())
    );
}

#[test]
fn value_units_without_a_marker_are_truncated() {
    let profile = dyn_profile();
    let empty: [char; 0] = [];
    assert_eq!(
        *profile
            .locate_dyn_marker(&empty, &Global)
            .expect_err("no marker")
            .kind(),
        ErrorKind::Truncated
    );
    assert_eq!(
        *profile
            .locate_dyn_payload(&empty, &Global)
            .expect_err("no marker")
            .kind(),
        ErrorKind::Truncated
    );
}

#[test]
fn the_object_traits_own_method_is_exercised() {
    // The application's trait keeps its own methods; `as_any` is the one a registry downcasts
    // through, so it is exercised here rather than left as an unused declaration.
    let value = ReadFile { path: "README.md" };
    let as_trait: &dyn ToolInput = &value;
    assert!(as_trait.as_any().downcast_ref::<ReadFile>().is_some());
}
