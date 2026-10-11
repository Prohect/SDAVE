//! A demo of the `dyn` payload machinery, with what the macros will later generate written by hand.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p sdave-codec --example dyn_demo
//! ```
//!
//! It prints (1) the spellings involved and the policy's rendering of each; (2) the real document for
//! a holder of each concrete type; (3) the framing breakdown of both, walked down through the
//! record's payload, the field's payload (the marked document), the concrete frame and the concrete
//! body, so the field header, the concrete marker and the concrete frame appear as real ranges;
//! (4) the profile, field by field; and (5) the line about what reading a `dyn` payload back needs.
//!
//! It is a host binary, so it may use `std` even though the library is `#![no_std]`.
// `sdave_codec::Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

use core::any::{Any, type_name};
use core::cell::OnceCell;
use core::num::NonZeroUsize;
use std::alloc::Global;

use sdave_codec::{
    Config, DYN_PAYLOAD_DEPTH, DynSerialize, FieldSlot, FlatDeserializer, LimiterPair, Limits,
    NamePolicy, PairSelect, Serialize, Shape, ShortType, Sink, State, Variant, depth_of,
};

// =============================================================================================
// What `#[dyn_payload]` and `#[derive(DynPayload)]` will later generate, written out by hand.
// =============================================================================================

/// The application's object trait. Taking the codec's `DynSerialize` as a supertrait is what keeps
/// this trait object-safe: the supertrait's method is type-erased, and the sink it uses carries the
/// naming policy dynamically, so no fixed inner policy exists.
trait ToolInput: DynSerialize + core::fmt::Debug + 'static {
    fn as_any(&self) -> &dyn Any;
}

/// The first concrete implementor: a borrowed-text field.
#[derive(Debug)]
struct ReadFileInput {
    path: &'static str,
}

impl Serialize for ReadFileInput {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<&'static str as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("path", type_name::<&'static str>(), &self.path)
        })
    }
}

impl DynSerialize for ReadFileInput {
    fn append_dyn(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        // The marked document: the concrete type's marker, then one envelope holding its body.
        sink.marked_document(type_name::<Self>(), self)
    }
}

impl ToolInput for ReadFileInput {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The second concrete implementor: a numeric field.
#[derive(Debug)]
struct SetTimeoutInput {
    ms: u32,
}

impl Serialize for SetTimeoutInput {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<u32 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("ms", type_name::<u32>(), &self.ms)
        })
    }
}

impl DynSerialize for SetTimeoutInput {
    fn append_dyn(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        sink.marked_document(type_name::<Self>(), self)
    }
}

impl ToolInput for SetTimeoutInput {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The one impl the codec requires per object trait: `#[dyn_payload]` emits exactly this, and it is
/// what lets `Box<dyn ToolInput>` and `&dyn ToolInput` be written at all.
impl Serialize for dyn ToolInput {
    const SHAPE: Shape = Shape::Dynamic;
    const DEPTH: u16 = DYN_PAYLOAD_DEPTH;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        self.append_dyn(sink)
    }
}

/// A holder with one `dyn` field: the header carries the field type's spelling, the value bytes the
/// concrete type's marked document.
struct Holder {
    v: Box<dyn ToolInput>,
}

impl Serialize for Holder {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<Box<dyn ToolInput> as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("v", type_name::<Box<dyn ToolInput>>(), &self.v)
        })
    }
}

/// The types the profile's naming policy shortens to their last identifier.
static SHORT_TYPES: &[ShortType] = &[
    ShortType::of::<Holder>(),
    ShortType::of::<ReadFileInput>(),
    ShortType::of::<SetTimeoutInput>(),
    ShortType::of::<Box<()>>(),
];

type Pair = LimiterPair<'static, u8, Global>;
type Profile = Config<'static, 'static, u8, Global>;

fn variant_name(variant: Variant) -> &'static str {
    match variant {
        Variant::V1 => "V1",
        Variant::V2 => "V2",
    }
}

fn pair_select_name(pair_select: PairSelect) -> &'static str {
    match pair_select {
        PairSelect::FirstMatch => "FirstMatch",
        PairSelect::LeastRepeat => "LeastRepeat",
        PairSelect::SmallestFrame => "SmallestFrame",
    }
}

fn unit(text: &str) -> &'static Vec<u8> {
    Box::leak(Box::new(text.as_bytes().to_vec()))
}

fn pair(least_repeat: usize, limiter: &str, delimiter: &str) -> Pair {
    LimiterPair::new(
        Some(NonZeroUsize::new(least_repeat).expect("a non-zero repeat")),
        OnceCell::from(unit(limiter)),
        OnceCell::from(unit(delimiter)),
    )
}

fn lossy(units: &[u8]) -> String {
    String::from_utf8_lossy(units).into_owned()
}

fn hex(units: &[u8]) -> String {
    let mut text = String::new();
    for byte in units {
        text.push_str(&format!("{byte:02x} "));
    }
    text.trim_end().to_string()
}

/// Append a value as a document.
fn append_document(profile: &Profile, value: &impl Serialize, spelling: &str) -> Vec<u8> {
    let mut buffer = vec![0u8; 1024];
    let end = profile
        .append_to_slice(value, &mut buffer, 0)
        .expect("the demo document is appendable");
    println!("  {spelling}:");
    println!("    length     = {} unit(s)", end);
    println!("    units      = {:?}", &buffer[..end]);
    println!("    lossy text = {:?}", lossy(&buffer[..end]));
    println!("    hex        = {}", hex(&buffer[..end]));
    buffer[..end].to_vec()
}

/// Print the framing breakdown of `units`, labelling offsets with `base` as their document offset,
/// and return the document offsets of the confirmed envelopes' payloads so a caller can walk in.
fn show(
    units: &[u8],
    base: usize,
    label: &str,
    variant: Variant,
    pairs: &Vec<Pair>,
) -> Vec<(usize, usize)> {
    println!("    -- {label} --");
    let mut deserializer = FlatDeserializer::new(units, variant, pairs, Vec::new(), Vec::new());
    let mut payloads = Vec::new();
    for (index, state) in deserializer.iter().enumerate() {
        match state {
            State::E { envelop } => {
                let head = envelop.head_offset().get();
                let payload_head = envelop.payload_head_offset().get();
                let payload_tail = envelop.payload_tail_offset().get();
                println!(
                    "      item {index}: Envelop (phantom {})",
                    envelop.is_phantom()
                );
                println!("        head            = {}", base + head);
                println!(
                    "        limiter slice   = [{}..{}] = {:?} (hex {})",
                    base + head,
                    base + payload_head,
                    lossy(&units[head..payload_head]),
                    hex(&units[head..payload_head])
                );
                println!("        payload head    = {}", base + payload_head);
                println!("        payload tail    = {}", base + payload_tail);
                println!(
                    "        payload units   = [{}..{}] = {:?} (hex {})",
                    base + payload_head,
                    base + payload_tail,
                    lossy(&units[payload_head..payload_tail]),
                    hex(&units[payload_head..payload_tail])
                );
                match envelop.tail_offset() {
                    Some(tail) => {
                        let tail = tail.get();
                        println!(
                            "        delimiter slice = [{}..{}] = {:?} (hex {})",
                            base + payload_tail,
                            base + tail,
                            lossy(&units[payload_tail..tail]),
                            hex(&units[payload_tail..tail])
                        );
                        println!("        tail            = {}", base + tail);
                        if !envelop.is_phantom() {
                            payloads.push((base + payload_head, base + payload_tail));
                        }
                    }
                    None => println!("        tail            = (not confirmed yet)"),
                }
            }
            State::N { non_envelop } => {
                let head = non_envelop.head_offset().get();
                let tail = non_envelop.tail_offset().get();
                println!(
                    "      item {index}: NonEnvelop (phantom {})",
                    non_envelop.is_phantom()
                );
                println!("        head            = {}", base + head);
                println!("        tail            = {}", base + tail);
                println!(
                    "        units           = [{}..{}] = {:?} (hex {})",
                    base + head,
                    base + tail,
                    lossy(&units[head..tail]),
                    hex(&units[head..tail])
                );
            }
        }
    }
    payloads
}

/// Walk one document down: root, record payload, field payload (the marked document), the concrete
/// frame, and the concrete body — plus the codec's own locator on the marked document.
fn walk(document: &[u8], profile: &Profile, spelling: &str) {
    let variant = profile.variant();
    let pairs = profile.limiter_pairs();

    println!("  {spelling}:");
    let root = show(
        document,
        0,
        "the whole document: the root marker, then the record envelope",
        variant,
        pairs,
    );
    let Some((record_start, record_end)) = root.first().copied() else {
        println!("    (no record envelope to walk into)");
        return;
    };
    let record = show(
        &document[record_start..record_end],
        record_start,
        "the record's payload: the field header as text, then the field envelope",
        variant,
        pairs,
    );
    let Some((field_start, field_end)) = record.first().copied() else {
        println!("    (no field envelope to walk into)");
        return;
    };
    let field = show(
        &document[field_start..field_end],
        field_start,
        "the field's payload: the concrete marker as text, then the concrete frame",
        variant,
        pairs,
    );
    let marked = &document[field_start..field_end];
    match profile.locate_dyn_payload(marked, &Global) {
        Ok(payload) => println!(
            "    its locator on those units: marker = {:?} ({} unit(s)), body = {:?} ({} unit(s))",
            lossy(payload.marker()),
            payload.marker().len(),
            lossy(payload.body()),
            payload.body().len()
        ),
        Err(error) => println!("    its locator on those units reported: {error}"),
    }
    let Some((frame_start, frame_end)) = field.first().copied() else {
        println!("    (no concrete frame to walk into)");
        return;
    };
    show(
        &document[frame_start..frame_end],
        frame_start,
        "the concrete frame's payload: the concrete type's own body",
        variant,
        pairs,
    );
}

fn main() {
    // =========================================================================================
    // 1. The spellings involved, and the policy's rendering of each.
    // =========================================================================================
    println!("== 1. type_name and the policy's rendering ==");
    for (label, spelling) in [
        ("Box<dyn ToolInput>", type_name::<Box<dyn ToolInput>>()),
        ("&dyn ToolInput", type_name::<&dyn ToolInput>()),
        ("dyn ToolInput", type_name::<dyn ToolInput>()),
        ("ReadFileInput", type_name::<ReadFileInput>()),
        ("SetTimeoutInput", type_name::<SetTimeoutInput>()),
    ] {
        println!("  {label:<18} type_name = {:?}", spelling);
    }
    // The policy the documents below use renders them like this; `Box<dyn …>` shortens its outer
    // constructor and keeps the trait's own path verbatim, because shortening does not propagate.
    let policy = NamePolicy::new(SHORT_TYPES);
    println!("  with the demo policy:");
    for spelling in [
        type_name::<Box<dyn ToolInput>>(),
        type_name::<&dyn ToolInput>(),
        type_name::<dyn ToolInput>(),
        type_name::<ReadFileInput>(),
        type_name::<SetTimeoutInput>(),
        type_name::<Holder>(),
    ] {
        println!("    {:?} -> {:?}", spelling, policy.render(spelling));
    }

    // =========================================================================================
    // 2. The real documents.
    // =========================================================================================
    println!();
    println!("== 2. the documents ==");
    // The profile style of the other demos: the README's recommendation for the reserved set,
    // bounded limits, first-match selection and V1.
    let pairs: &'static Vec<Pair> = Box::leak(Box::new(vec![
        pair(2, "^", "~"),
        pair(2, "?", "!"),
        pair(2, "{", "}"),
        pair(2, "%", "+"),
        pair(1, "([", "])"),
    ]));
    let limits = Limits::new(16, 4096, 16);
    let pair_select = PairSelect::FirstMatch;
    let variant = Variant::V1;
    let profile = Config::new(variant, pairs, limits, pair_select, policy)
        .expect("the demo profile is valid");

    let first = Holder {
        v: Box::new(ReadFileInput { path: "README.md" }),
    };
    let second = Holder {
        v: Box::new(SetTimeoutInput { ms: 30_000 }),
    };
    println!("  value 1 = Holder {{ v: {:?} }}", first.v);
    let first_document = append_document(
        &profile,
        &first,
        "Holder { v: Box::new(ReadFileInput { .. }) }",
    );
    println!("  value 2 = Holder {{ v: {:?} }}", second.v);
    let second_document = append_document(
        &profile,
        &second,
        "Holder { v: Box::new(SetTimeoutInput { .. }) }",
    );

    // =========================================================================================
    // 3. The framing breakdown of both.
    // =========================================================================================
    println!();
    println!("== 3. the framing breakdown ==");
    println!("  (offsets are absolute in each document)");
    walk(&first_document, &profile, "document 1");
    walk(&second_document, &profile, "document 2");

    // =========================================================================================
    // 4. The profile, field by field.
    // =========================================================================================
    println!();
    println!("== 4. the exact profile ==");
    println!("  Variant     = {}", variant_name(profile.variant()));
    println!(
        "  PairSelect  = {}",
        pair_select_name(profile.pair_select())
    );
    println!(
        "  Limits      = max_depth {}, max_len {}, max_repeat {}",
        profile.limits().max_depth,
        profile.limits().max_len,
        profile.limits().max_repeat
    );
    if policy.is_verbatim() {
        println!("  NamePolicy  = verbatim (no SHORT_TYPES)");
    } else {
        println!(
            "  NamePolicy  = {} SHORT_TYPES entry(ies)",
            policy.entries().len()
        );
        for (index, entry) in policy.entries().iter().enumerate() {
            println!(
                "    SHORT_TYPES[{index}] = ShortType::of::<{}>(): own constructor path `{}`, emits `{}`",
                entry.full_name(),
                entry.own_path(),
                entry.short_name()
            );
        }
    }
    println!("  limiter pairs, in profile order:");
    for (index, limiter_pair) in pairs.iter().enumerate() {
        println!(
            "    [{index}] limiter {:?} ({} unit(s), hex {}), delimiter {:?} ({} unit(s), hex {}), least_repeat {:?}",
            lossy(limiter_pair.limiter()),
            limiter_pair.limiter().len(),
            hex(limiter_pair.limiter()),
            lossy(limiter_pair.delimiter()),
            limiter_pair.delimiter().len(),
            hex(limiter_pair.delimiter()),
            limiter_pair.least_repeat()
        );
    }

    // =========================================================================================
    // 5. What reading a `dyn` payload back needs.
    // =========================================================================================
    println!();
    println!("== 5. reading a dyn payload back ==");
    println!(
        "  The codec locates the concrete marker and the concrete body of a dyn payload — the"
    );
    println!("  locator runs printed in section 3 are that call — and it stops there. Turning the");
    println!(
        "  located marker back into a `Box<dyn ToolInput>` (mapping it to the concrete type and"
    );
    println!(
        "  decoding the body with it) requires the application's registry, because a trait object"
    );
    println!(
        "  cannot be constructed by the codec. The object's own half is what a registry keys on:"
    );
    println!(
        "    value 1: as_any().downcast_ref::<ReadFileInput>().map(|input| input.path) = {:?}",
        first
            .v
            .as_any()
            .downcast_ref::<ReadFileInput>()
            .map(|input| input.path)
    );
    println!(
        "    value 2: as_any().downcast_ref::<ReadFileInput>().map(|input| input.path) = {:?}",
        second
            .v
            .as_any()
            .downcast_ref::<ReadFileInput>()
            .map(|input| input.path)
    );
}
