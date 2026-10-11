//! The derives and the `dyn` pair, exercised end to end against the codec.
//!
//! Every expectation is hand-derived: the round-trips go through the codec's own append entry, and
//! the hand-written documents below are built from the codec README's grammar (a standalone document
//! is `marker-text ++ frame(body)`, a record body is the `name:marker` / `frame(body(field))`
//! alternation), with the pair set the test file declares.
// `sdave_codec::Error` carries its fixed-capacity path by value on purpose; see its crate root's
// note. The derives emit the matching allowance themselves, but this file's hand-written impls
// return the same `Result`.
#![allow(clippy::result_large_err)]

use core::any::type_name;
use core::cell::OnceCell;
use core::num::NonZeroUsize;
use std::alloc::Global;

use sdave_codec::Deserialize as DeserializeTrait;
use sdave_codec::Serialize as SerializeTrait;
use sdave_codec::{
    BasicType, Config, DecodeSource, Error, ErrorKind, LimiterPair, Limits, NamePolicy, PairSelect,
    Shape, Sink, Variant, depth_of, marker_eq_ascii,
};
use sdave_macro::{Deserialize, DynPayload, Serialize, dyn_payload};

// ---------------------------------------------------------------------------
// profiles
// ---------------------------------------------------------------------------

type Pair = LimiterPair<'static, u8, Global>;
type Profile = Config<'static, 'static, u8, Global>;

/// A leaked byte vector, so a `LimiterPair` can borrow it for `'static`.
fn leaked(text: &str) -> &'static Vec<u8> {
    Box::leak(Box::new(text.as_bytes().to_vec()))
}

fn pair(least_repeat: usize, limiter: &str, delimiter: &str) -> Pair {
    LimiterPair::new(
        Some(NonZeroUsize::new(least_repeat).expect("a non-zero repeat")),
        OnceCell::from(leaked(limiter)),
        OnceCell::from(leaked(delimiter)),
    )
}

fn profile_of(specs: &[(&str, &str, usize)]) -> Profile {
    let pairs: &'static Vec<Pair> = Box::leak(Box::new(
        specs
            .iter()
            .map(|(limiter, delimiter, repeat)| pair(*repeat, limiter, delimiter))
            .collect::<Vec<Pair>>(),
    ));
    Config::new(
        Variant::V1,
        pairs,
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
        NamePolicy::VERBATIM,
    )
    .expect("a valid profile")
}

/// The round-trip pair set: three disjoint pairs at two repeats minimum.
fn profile() -> Profile {
    profile_of(&[("^", "~", 2), ("?", "!", 2), ("{", "}", 2)])
}

/// A minimal pair set, for the hand-written documents below.
fn minimal() -> Profile {
    profile_of(&[("^", "~", 1), ("?", "!", 1)])
}

fn document_of<V: SerializeTrait + ?Sized>(profile: &Profile, value: &V) -> Vec<u8> {
    let mut buffer = vec![0u8; 1024];
    let end = profile
        .append_to_slice(value, &mut buffer, 0)
        .expect("appended");
    buffer.truncate(end);
    buffer
}

fn roundtrip<V>(value: V)
where
    V: SerializeTrait + for<'de> DeserializeTrait<'de> + PartialEq + core::fmt::Debug,
{
    let profile = profile();
    let mut buffer = vec![0u8; 1024];
    let end = profile
        .append_to_slice(&value, &mut buffer, 0)
        .expect("appended");
    let decoded = profile
        .deserialize::<V>(&buffer[..end], &Global)
        .expect("decoded");
    assert_eq!(decoded, value);
}

// ---------------------------------------------------------------------------
// the declared types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Point {
    x: u32,
    y: i32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Newtype(u16);

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Unit;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Nothing();

#[derive(Serialize, Deserialize, Debug, PartialEq)]
enum En {
    A,
    B(u8),
    C(u8, bool),
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
enum Named {
    D { a: u8, b: u8 },
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Wrapper<T> {
    inner: T,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Two<A, B> {
    first: A,
    second: B,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Text<'a> {
    s: &'a str,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct WithDefaults {
    a: u8,
    #[sdave(default)]
    b: u8,
    #[sdave(default = default_flag)]
    flag: bool,
}

fn default_flag() -> bool {
    true
}

/// A `#[sdave(basic)]` type: opaque to the codec, contributing only the enclosing payload.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[sdave(basic)]
struct Raw;

impl BasicType for Raw {
    fn append_basic(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.text("raw!")
    }

    fn read_basic<'de>(source: &mut DecodeSource<'de, '_>) -> Result<Self, Error> {
        if source.payload_text()? == "raw!" {
            Ok(Raw)
        } else {
            Err(Error::new(ErrorKind::InvalidNumber))
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct WithRaw {
    label: u8,
    raw: Raw,
}

/// A recursive type. Only `Serialize` is derived: decoding a `Box` needs an allocator and therefore
/// belongs to `sdave-alloc`.
#[derive(Serialize)]
struct Node {
    value: u8,
    next: Option<Box<Node>>,
}

// ---------------------------------------------------------------------------
// round-trips
// ---------------------------------------------------------------------------

#[test]
fn a_named_struct_round_trips() {
    roundtrip(Point { x: 42, y: -1 });
}

#[test]
fn a_tuple_struct_round_trips() {
    roundtrip(Newtype(9));
}

#[test]
fn the_unit_and_zero_field_tuple_structs_round_trip() {
    roundtrip(Unit);
    roundtrip(Nothing());
}

#[test]
fn every_variant_shape_round_trips() {
    roundtrip(En::A);
    roundtrip(En::B(9));
    roundtrip(En::C(1, true));
    roundtrip(Named::D { a: 1, b: 2 });
}

#[test]
fn generic_types_round_trip() {
    roundtrip(Wrapper { inner: 5u8 });
    roundtrip(Two {
        first: 1u8,
        second: -2i16,
    });
    roundtrip(Wrapper {
        inner: Point { x: 1, y: 2 },
    });
}

#[test]
fn a_borrowed_text_field_round_trips_through_the_input() {
    let profile = profile();
    let value = Text { s: "hi" };
    let mut buffer = vec![0u8; 256];
    let end = profile
        .append_to_slice(&value, &mut buffer, 0)
        .expect("appended");
    let document = &buffer[..end];
    let decoded: Text<'_> = profile.deserialize(document, &Global).expect("decoded");
    assert_eq!(decoded, value);
    // The text is borrowed from the document, not copied out of it.
    let base = document.as_ptr() as usize;
    let text = decoded.s.as_ptr() as usize;
    assert!(text >= base && text + decoded.s.len() <= base + document.len());
}

#[test]
fn a_basic_type_contributes_only_its_payload() {
    roundtrip(WithRaw {
        label: 1,
        raw: Raw,
    });

    // The basic type adds no marker of its own: the field run carries the declared spelling and the
    // payload directly.
    let document = document_of(
        &profile(),
        &WithRaw {
            label: 1,
            raw: Raw,
        },
    );
    let text = String::from_utf8_lossy(&document).into_owned();
    assert!(
        text.contains(&format!("raw:{}", type_name::<Raw>())),
        "document was {text:?}"
    );
    assert!(text.contains("raw!"), "document was {text:?}");
}

#[test]
fn a_recursive_type_encodes_and_saturates_its_depth() {
    let value = Node {
        value: 1,
        next: Some(Box::new(Node {
            value: 2,
            next: None,
        })),
    };
    let document = document_of(&profile(), &value);
    assert!(!document.is_empty());
    // A self-referential field contributes the deepest value, and the computation saturates.
    assert_eq!(<Node as SerializeTrait>::DEPTH, u16::MAX);
}

// ---------------------------------------------------------------------------
// shape and depth
// ---------------------------------------------------------------------------

#[test]
fn the_derive_sets_the_shape_and_the_depth() {
    assert_eq!(<Point as SerializeTrait>::SHAPE, Shape::Record);
    assert_eq!(<Point as SerializeTrait>::DEPTH, 1);
    assert_eq!(<Newtype as SerializeTrait>::SHAPE, Shape::Tuple);
    assert_eq!(<Unit as SerializeTrait>::SHAPE, Shape::Record);
    assert_eq!(<Unit as SerializeTrait>::DEPTH, 1);
    assert_eq!(<Nothing as SerializeTrait>::SHAPE, Shape::Tuple);
    assert_eq!(<Nothing as SerializeTrait>::DEPTH, 1);
    assert_eq!(<En as SerializeTrait>::SHAPE, Shape::Enum);
    assert_eq!(<En as SerializeTrait>::DEPTH, 1);
    assert_eq!(<Named as SerializeTrait>::SHAPE, Shape::Enum);
    assert_eq!(<Wrapper<u8> as SerializeTrait>::DEPTH, 1);
    assert_eq!(<Raw as SerializeTrait>::DEPTH, 0);
    // The depth is `depth_of` over the fields' own depths.
    assert_eq!(
        <Newtype as SerializeTrait>::DEPTH,
        depth_of(&[<u16 as SerializeTrait>::DEPTH])
    );
    assert_eq!(
        <Two<u8, i16> as SerializeTrait>::DEPTH,
        depth_of(&[<u8 as SerializeTrait>::DEPTH, <i16 as SerializeTrait>::DEPTH])
    );
}

// ---------------------------------------------------------------------------
// the deserialize rules
// ---------------------------------------------------------------------------

#[test]
fn an_absent_named_field_uses_its_provider() {
    let profile = minimal();
    let marker = type_name::<WithDefaults>();
    let document = format!("{marker}?a:u8^1~!").into_bytes();
    let decoded = profile
        .deserialize::<WithDefaults>(&document, &Global)
        .expect("decoded");
    assert_eq!(
        decoded,
        WithDefaults {
            a: 1,
            b: 0,
            flag: true,
        }
    );
}

#[test]
fn a_present_named_field_never_runs_its_provider() {
    let profile = minimal();
    let marker = type_name::<WithDefaults>();

    // `b` supplied, `flag` absent -> only `flag` falls back.
    let document = format!("{marker}?a:u8^1~b:u8^9~!").into_bytes();
    let decoded = profile
        .deserialize::<WithDefaults>(&document, &Global)
        .expect("decoded");
    assert_eq!(
        decoded,
        WithDefaults {
            a: 1,
            b: 9,
            flag: true,
        }
    );

    // `flag` supplied as `false` — a provider that ran would be observable.
    let document = format!("{marker}?a:u8^1~flag:bool^false~!").into_bytes();
    let decoded = profile
        .deserialize::<WithDefaults>(&document, &Global)
        .expect("decoded");
    assert_eq!(
        decoded,
        WithDefaults {
            a: 1,
            b: 0,
            flag: false,
        }
    );
}

#[test]
fn a_record_decodes_its_fields_in_any_order() {
    // The wire order is `y` then `x`, the opposite of the declaration; decoding still fills both
    // fields, because named fields are matched by name.
    let profile = minimal();
    let marker = type_name::<Point>();
    let document = format!("{marker}?y:i32^-1~x:u32^42~!").into_bytes();
    let decoded = profile
        .deserialize::<Point>(&document, &Global)
        .expect("decoded");
    assert_eq!(decoded, Point { x: 42, y: -1 });
}

#[test]
fn a_missing_required_field_is_an_error() {
    let profile = minimal();
    let marker = type_name::<WithDefaults>();
    let document = format!("{marker}?b:u8^1~flag:bool^true~!").into_bytes();
    let error = profile
        .deserialize::<WithDefaults>(&document, &Global)
        .expect_err("`a` is required");
    assert_eq!(*error.kind(), ErrorKind::MissingField { name: "a" });
}

#[test]
fn a_duplicate_field_is_an_error() {
    let profile = minimal();
    let marker = type_name::<WithDefaults>();
    let document = format!("{marker}?a:u8^1~a:u8^2~!").into_bytes();
    let error = profile
        .deserialize::<WithDefaults>(&document, &Global)
        .expect_err("duplicate");
    assert_eq!(*error.kind(), ErrorKind::DuplicateField);
}

#[test]
fn an_unknown_field_is_an_error() {
    let profile = minimal();
    let marker = type_name::<WithDefaults>();
    let document = format!("{marker}?z:u8^1~!").into_bytes();
    let error = profile
        .deserialize::<WithDefaults>(&document, &Global)
        .expect_err("unknown");
    assert_eq!(*error.kind(), ErrorKind::UnknownField);
}

#[test]
fn a_wrong_field_marker_is_an_error() {
    let profile = minimal();
    let marker = type_name::<WithDefaults>();
    let document = format!("{marker}?a:u16^1~!").into_bytes();
    let error = profile
        .deserialize::<WithDefaults>(&document, &Global)
        .expect_err("bad marker");
    assert_eq!(
        *error.kind(),
        ErrorKind::UnexpectedMarker {
            expected: type_name::<u8>()
        }
    );
}

#[test]
fn an_unknown_variant_is_an_error() {
    let profile = minimal();
    let marker = type_name::<En>();
    let document = format!("{marker}?^Z~!").into_bytes();
    let error = profile
        .deserialize::<En>(&document, &Global)
        .expect_err("unknown variant");
    assert_eq!(*error.kind(), ErrorKind::UnknownVariant);
}

#[test]
fn a_variant_of_the_wrong_arity_is_an_error() {
    let profile = minimal();
    let marker = type_name::<En>();
    // `C` declares two associated values; the associated-value envelope below holds none.
    let document = format!("{marker}?^C~^~!").into_bytes();
    let error = profile
        .deserialize::<En>(&document, &Global)
        .expect_err("wrong arity");
    assert_eq!(
        *error.kind(),
        ErrorKind::WrongArity {
            expected: 2,
            actual: 0
        }
    );
}

// ---------------------------------------------------------------------------
// the `dyn` pair
// ---------------------------------------------------------------------------

#[dyn_payload]
trait ToolInput: sdave_codec::DynSerialize + core::fmt::Debug + 'static {
    fn as_any(&self) -> &dyn core::any::Any;
    fn eq_dyn(&self, other: &dyn ToolInput) -> bool;
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[derive(DynPayload)]
#[dyn_payload_impl(dyn_trait = ToolInput)]
struct ReadFile {
    path: u8,
}

struct Holder {
    v: Box<dyn ToolInput>,
}

impl SerializeTrait for Holder {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<Box<dyn ToolInput> as SerializeTrait>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut slots = [sdave_codec::FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("v", type_name::<Box<dyn ToolInput>>(), &self.v)
        })
    }
}

#[test]
fn a_dyn_payload_carries_the_concrete_marked_document() {
    let profile = profile();
    let concrete = ReadFile { path: 7 };
    let document = document_of(&profile, &concrete);

    // The value bytes lead with the concrete type's own rendered marker.
    let marker = profile
        .locate_dyn_marker(&document, &Global)
        .expect("a locatable marker");
    assert!(marker_eq_ascii(
        marker,
        profile.name_policy().render(type_name::<ReadFile>())
    ));
    assert!(!marker_eq_ascii(marker, type_name::<Box<dyn ToolInput>>()));
}

#[test]
fn a_dyn_field_keeps_the_field_types_spelling() {
    // `Box<dyn …>` and `&dyn …` are different field types, and neither is the concrete type.
    assert_ne!(
        type_name::<Box<dyn ToolInput>>(),
        type_name::<&dyn ToolInput>()
    );
    assert_ne!(type_name::<Box<dyn ToolInput>>(), type_name::<ReadFile>());
}

#[test]
fn a_boxed_trait_object_is_serializable() {
    let profile = profile();
    let holder = Holder {
        v: Box::new(ReadFile { path: 3 }),
    };
    let document = document_of(&profile, &holder);
    let text = String::from_utf8_lossy(&document).into_owned();
    assert!(
        text.contains(type_name::<ReadFile>()),
        "document was {text:?}"
    );
    // The field header keeps the *field type's* spelling.
    assert!(
        text.contains(&format!("v:{}", type_name::<Box<dyn ToolInput>>())),
        "document was {text:?}"
    );
}

#[test]
fn the_generated_object_methods_are_sound() {
    let one = ReadFile { path: 1 };
    let two = ReadFile { path: 2 };
    let as_trait: &dyn ToolInput = &one;

    assert!(as_trait.as_any().downcast_ref::<ReadFile>().is_some());
    assert!(as_trait.eq_dyn(&one));
    assert!(!as_trait.eq_dyn(&two));
}
