//! The append entry over a real `[T]` target: the exact bytes, the fixed-target contract, and the
//! rollback.
//!
//! Every document below is hand-derived from the codec README's grammar plus `can_serialize`'s
//! documented contract: the returned repeat is the *minimal* one that frames the payload, `None`
//! means the pair cannot frame it here (V1 forbids a payload whose tail collides with the frame's
//! delimiter; a payload that opens with the limiter cannot use that pair at all), and a successful
//! framing is guaranteed to round-trip.
// `Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

mod common;

use core::any::type_name;

use common::*;
use sdave_codec::{
    Config, ErrorKind, FieldSlot, Limit, Limits, NamePolicy, PairSelect, Serialize, Shape,
    ShortType, Sink, SliceTarget, Variant,
};

/// One pair, `['^', '~']`, with a least repeat of 2.
const CARETS: &[(&str, &str, usize)] = &[("^", "~", 2)];

/// A named struct stand-in: one record envelope of `name:marker` field runs.
struct Point {
    x: u8,
    y: u8,
}

impl Serialize for Point {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = sdave_codec::depth_of(&[<u8 as Serialize>::DEPTH, <u8 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 2];
        sink.record(&mut slots, |record| {
            record.field("x", "u8", &self.x)?;
            record.field("y", "u8", &self.y)
        })
    }
}

/// The document `Point { x: 1, y: 2 }` appends under the standard profile: the marker, then the
/// record envelope whose payload is the two field runs.
fn point_document() -> String {
    format!(
        "{marker}??{body}!!",
        marker = type_name::<Point>(),
        // Both fields are leaves at depth 0, so the serializer keeps the declaration order. Each
        // field value is framed by the first pair, which the content before it allows; the record
        // frame then cannot use that pair, because its payload's tail is that pair's delimiter —
        // which V1 forbids — so the '?' pair frames it.
        body = "x:u8^^1~~y:u8^^2~~"
    )
}

fn point_required() -> usize {
    type_name::<Point>().len() + 22
}

// ---------------------------------------------------------------------------
// exact bytes
// ---------------------------------------------------------------------------

#[test]
fn a_leaf_root_is_its_marker_then_its_envelope() {
    let profile = profile_of(Variant::V1, pairs(CARETS), Limits::UNBOUNDED);
    let mut buffer = blank(16);
    // "u8" (2 units) then the frame: two repeats of the limiter and of the delimiter around "7".
    let end = profile
        .append_to_slice(&7u8, &mut buffer, 0)
        .expect("appended");

    assert_eq!(end, 7);
    assert_eq!(text_of(&buffer[..end]), "u8^^7~~");
    // Everything past the document is untouched.
    assert_eq!(text_of(&buffer[end..]), filler(16 - end));
}

#[test]
fn a_nested_record_frames_each_field_and_uses_a_pair_its_payload_allows() {
    let profile = demo_profile();
    let value = Point { x: 1, y: 2 };
    let required = point_required();

    let mut buffer = blank(required + 4);
    let end = profile
        .append_to_slice(&value, &mut buffer, 0)
        .expect("appended");

    assert_eq!(end, required);
    assert_eq!(text_of(&buffer[..end]), point_document());
    assert_eq!(text_of(&buffer[end..]), filler(4));
}

#[test]
fn a_document_is_appended_after_the_context() {
    let profile = demo_profile();
    let value = Point { x: 1, y: 2 };
    let required = point_required();

    let mut buffer = blank(2 + required + 1);
    buffer[0] = 'A';
    buffer[1] = 'B';
    let end = profile
        .append_to_slice(&value, &mut buffer, 2)
        .expect("appended");

    assert_eq!(end, 2 + required);
    assert_eq!(text_of(&buffer[..end]), format!("AB{}", point_document()));
    // Nothing is appended past the document.
    assert_eq!(text_of(&buffer[end..]), filler(1));
}

#[test]
fn the_variant_decides_which_pair_frames_a_delimiter_tail() {
    let value = "A~";
    let marker = type_name::<&str>();
    assert_eq!(marker, "&str");
    let required = marker.len() + 6;

    // V1 refuses a payload whose tail collides with the frame's delimiter, so the '^' pair is out
    // and the '?' pair frames it in two repeats.
    let v1 = profile_of(Variant::V1, pairs(STD), Limits::UNBOUNDED);
    let mut buffer = blank(required);
    v1.append_to_slice(&value, &mut buffer, 0)
        .expect("appended under V1");
    assert_eq!(text_of(&buffer), format!("{marker}??A~!!"));

    // V2 tolerates the delimiter tail, so the profile's first pair frames it.
    let v2 = profile_of(Variant::V2, pairs(STD), Limits::UNBOUNDED);
    let mut buffer = blank(required);
    v2.append_to_slice(&value, &mut buffer, 0)
        .expect("appended under V2");
    assert_eq!(text_of(&buffer), format!("{marker}^^A~~~"));
}

#[test]
fn the_root_marker_is_rendered_through_the_policy() {
    static ENTRIES: &[ShortType] = &[ShortType::of::<core::option::Option<u8>>()];
    let profile = Config::new(
        Variant::V1,
        pairs(STD),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
        NamePolicy::new(ENTRIES),
    )
    .expect("a valid profile");

    // The marker is the policy's rendering of the root's own spelling: `Option<u8>` — the
    // constructor's last identifier, with its arguments kept verbatim.
    let marker = "Option<u8>";
    // Body(Enum) is the variant envelope, then the associated-value envelope. The variant's frame
    // sees only the marker before it, which does not end in a limiter, so the first pair frames it
    // in two repeats. The associated-value frame's context ends in `~~`, the first pair's
    // *delimiter*, which `can_serialize` does not object to, so it is framed by the first pair too.
    let body = "^^Some~~^^1~~";
    // The root frame then cannot use the first pair: its payload opens with the child's limiter
    // slice. The '?' pair frames it, again in two repeats.
    let required = marker.len() + 2 + body.len() + 2;
    assert_eq!(required, 27);

    let mut buffer = blank(required);
    let end = profile
        .append_to_slice(&Some(1u8), &mut buffer, 0)
        .expect("appended");

    assert_eq!(end, required);
    assert_eq!(text_of(&buffer), format!("{marker}??{body}!!"));
}

#[test]
fn a_char_unit_carries_one_scalar_however_long_its_utf8_is() {
    let profile = profile_of(Variant::V1, pairs(CARETS), Limits::UNBOUNDED);
    let marker = type_name::<char>();
    assert_eq!(marker, "char");
    // 4 marker units, then two limiter units, the single scalar, and two delimiter units.
    let required = marker.len() + 5;
    let mut buffer = blank(required);
    let end = profile
        .append_to_slice(&'☃', &mut buffer, 0)
        .expect("appended");

    assert_eq!(end, required);
    assert_eq!(text_of(&buffer), "char^^☃~~");
}

#[test]
fn a_byte_document_frames_the_same_way() {
    let profile = profile8(Variant::V1, pairs8(CARETS), Limits::UNBOUNDED);
    let value = "hi";
    let required = type_name::<&str>().len() + 6;
    let mut buffer = vec![0u8; required];
    let end = profile
        .append_to_slice(&value, &mut buffer, 0)
        .expect("appended");

    assert_eq!(end, required);
    assert_eq!(&buffer[..end], b"&str^^hi~~");
}

// ---------------------------------------------------------------------------
// the fixed-target contract, the rollback, and the bounds
// ---------------------------------------------------------------------------

#[test]
fn a_fixed_target_reports_the_room_it_had_and_how_far_the_pass_reached() {
    let profile = demo_profile();
    let value = Point { x: 1, y: 2 };

    // Ten units of room: the root's marker alone needs twelve, so the pass stops on the eleventh
    // unit of the document.
    let mut buffer = blank(10);
    let error = profile
        .append_to_slice(&value, &mut buffer, 0)
        .expect_err("the target is too short");
    assert_eq!(
        *error.kind(),
        ErrorKind::TargetExhausted {
            available: 10,
            reached: 11,
        }
    );

    // One unit short of the finished document: `reached` is then the document's exact length,
    // because that is the unit the pass stopped trying to append.
    let required = point_required();
    let mut buffer = blank(required - 1);
    let error = profile
        .append_to_slice(&value, &mut buffer, 0)
        .expect_err("one unit short");
    assert_eq!(
        *error.kind(),
        ErrorKind::TargetExhausted {
            available: required - 1,
            reached: required,
        }
    );
}

#[test]
fn a_failed_pass_leaves_the_callers_content_untouched() {
    let profile = demo_profile();
    let value = Point { x: 1, y: 2 };

    let mut region = blank(12);
    region[0] = 'A';
    region[1] = 'B';
    {
        let mut target = SliceTarget::new(&mut region, 2, &profile);
        let error = profile
            .append(&value, &mut target)
            .expect_err("the target is too short");
        assert_eq!(
            *error.kind(),
            ErrorKind::TargetExhausted {
                available: 10,
                reached: 11,
            }
        );
        // The pass rolled the target back to where it started.
        assert_eq!(target.end(), 2);
        assert_eq!(target.context_len(), 2);
    }
    // The caller's own content is where it was; everything the pass wrote lay beyond it.
    assert_eq!(text_of(&region[..2]), "AB");
}

#[test]
fn a_failed_pass_leaves_a_growable_target_exactly_as_it_was() {
    // The mock's framing is its own (a frame is `<payload>`), so pair selection cannot fail here;
    // what this drives is a *payload* failure, after the tuple's first element has been appended,
    // so the rollback has something to undo.
    let profile = profile_of(Variant::V1, pairs(CARETS), Limits::UNBOUNDED);
    let value = (1u8, f64::NAN);

    let mut recording = Recording::new();
    let error = profile
        .append(&value, &mut recording)
        .expect_err("a NaN payload cannot be written");
    assert_eq!(*error.kind(), ErrorKind::NonFiniteFloat);
    assert_eq!(recording.text(), "");
}

#[test]
fn a_retry_with_more_room_produces_the_same_document() {
    let profile = demo_profile();
    let value = Point { x: 1, y: 2 };
    let required = point_required();

    let mut small = blank(10);
    profile
        .append_to_slice(&value, &mut small, 0)
        .expect_err("the first attempt is short");

    let mut roomy = blank(required);
    let end = profile
        .append_to_slice(&value, &mut roomy, 0)
        .expect("the retry fits");
    assert_eq!(end, required);
    assert_eq!(text_of(&roomy), point_document());
}

#[test]
fn the_profile_bounds_are_enforced_while_appending() {
    let value = Point { x: 1, y: 2 };
    let required = point_required();

    // The root record frame is depth 1, each field's envelope depth 2.
    let deep = profile_of(Variant::V1, pairs(STD), Limits::new(1, 0, 0));
    let mut buffer = blank(required);
    let error = deep
        .append_to_slice(&value, &mut buffer, 0)
        .expect_err("too deep");
    assert_eq!(
        *error.kind(),
        ErrorKind::LimitExceeded {
            limit: Limit::Depth,
            value: 2,
            max: 1,
        }
    );

    // The first field's frame needs two repeats of its pair.
    let short_repeat = profile_of(Variant::V1, pairs(STD), Limits::new(0, 0, 1));
    let mut buffer = blank(required);
    let error = short_repeat
        .append_to_slice(&value, &mut buffer, 0)
        .expect_err("too many repeats");
    assert_eq!(
        *error.kind(),
        ErrorKind::LimitExceeded {
            limit: Limit::Repeat,
            value: 2,
            max: 1,
        }
    );

    // The length bound is a running one: a single pass knows the length only as it produces it,
    // so the bound is reported as soon as the document passes it.
    let short_document = profile_of(Variant::V1, pairs(STD), Limits::new(0, 10, 0));
    let mut buffer = blank(required);
    let error = short_document
        .append_to_slice(&value, &mut buffer, 0)
        .expect_err("too long");
    assert_eq!(
        *error.kind(),
        ErrorKind::LimitExceeded {
            limit: Limit::Length,
            value: 11,
            max: 10,
        }
    );
}
