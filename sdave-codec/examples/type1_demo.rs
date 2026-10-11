//! A demo of the sdave-codec append path and of the core's frame ranges, for one value.
//!
//! Run it with:
//!
//! ```text
//! cargo run -p sdave-codec --example type1_demo
//! ```
//!
//! It is a host binary, so it may use `std` even though the library is `#![no_std]`. It prints
//! (1) the exact profile it uses, (2) the document it appends, and (3) the same document read back
//! as framing ranges by the core's `FlatDeserializer`, so the marker text, the limiter slice, the
//! payload and the delimiter slice are visible as real offsets.
// `sdave_codec::Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

use core::any::type_name;
use core::cell::OnceCell;
use core::num::NonZeroUsize;
use std::alloc::Global;

use sdave_codec::{
    Config, FieldSlot, FlatDeserializer, LimiterPair, Limits, NamePolicy, PairSelect, Serialize,
    Shape, ShortType, Sink, State, Variant, depth_of,
};

/// The unit type of this demo's document: one byte per unit.
type Unit = u8;
type Pair = LimiterPair<'static, Unit, Global>;

/// The demo value.
///
/// The intended shape is `struct Type1 { x: u32, y: String }`, but `String` has no `Serialize`
/// implementation in this crate — the allocator-requiring container implementations are designated
/// for `sdave-alloc` — so `y` is the `&str` twin: the same `"42"` payload, and `&str` is a leaf this
/// crate owns. The program prints that note; it does not paper over it.
struct Type1 {
    x: u32,
    y: &'static str,
}

/// What a generated body does for a named struct: announce the fields, let the serializer order
/// them, and let the codec compose the headers, markers and envelopes.
impl Serialize for Type1 {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[
        <u32 as Serialize>::DEPTH,
        <&'static str as Serialize>::DEPTH,
    ]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 2];
        sink.record(&mut slots, |record| {
            // Markers are the compiler's own spellings, filtered by the profile's policy.
            record.field("x", type_name::<u32>(), &self.x)?;
            record.field("y", type_name::<&str>(), &self.y)
        })
    }
}

/// The types the profile's naming policy shortens to their last identifier.
static SHORT_TYPES: &[ShortType] = &[ShortType::of::<Type1>()];

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

/// A reserve string as a leaked `Vec<u8>`, so a `LimiterPair` can borrow it for `'static`.
fn unit(text: &str) -> &'static Vec<Unit> {
    Box::leak(Box::new(text.as_bytes().to_vec()))
}

fn pair(least_repeat: usize, limiter: &str, delimiter: &str) -> Pair {
    LimiterPair::new(
        Some(NonZeroUsize::new(least_repeat).expect("a non-zero repeat")),
        OnceCell::from(unit(limiter)),
        OnceCell::from(unit(delimiter)),
    )
}

fn lossy(units: &[Unit]) -> String {
    String::from_utf8_lossy(units).into_owned()
}

fn hex(units: &[Unit]) -> String {
    let mut text = String::new();
    for byte in units {
        text.push_str(&format!("{byte:02x} "));
    }
    text.trim_end().to_string()
}

fn main() {
    // -----------------------------------------------------------------------------------------
    // The note the demo must carry: `String` is not serializable here yet.
    // -----------------------------------------------------------------------------------------
    println!("== note on the value's type ==");
    println!("  The intended shape is `struct Type1 {{ x: u32, y: String }}`,");
    println!("  but `String` is not serializable by sdave-codec yet: the allocator-requiring");
    println!(
        "  container implementations (`String`, `Vec<T>`) are designated for the `sdave-alloc`"
    );
    println!("  crate, which does not exist yet. This demo therefore uses the `&str` twin,");
    println!("  `y: &str`, with the same \"42\" payload.");
    println!();

    // -----------------------------------------------------------------------------------------
    // 1. The profile, spelled out.
    // -----------------------------------------------------------------------------------------
    // A reserved set that follows the README's recommendation: every limiter differs from its
    // delimiter, and no unit is shared between any limiter and any delimiter.
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
    let policy = NamePolicy::new(SHORT_TYPES);

    let profile = Config::new(variant, pairs, limits, pair_select, policy)
        .expect("the demo profile is valid");

    println!("== 1. the exact profile ==");
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
        println!(
            "    renders `{}` -> `{}`",
            type_name::<Type1>(),
            policy.render(type_name::<Type1>())
        );
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

    // -----------------------------------------------------------------------------------------
    // 2. The document.
    // -----------------------------------------------------------------------------------------
    println!();
    println!("== 2. the document ==");
    let value = Type1 { x: 42, y: "42" };
    println!(
        "  value       = Type1 {{ x: {}, y: {:?} }}",
        value.x, value.y
    );

    // The codec appends into the target it is handed and never grows it, so the demo hands a
    // fixed slice with room to spare; `append_to_slice` returns the document's end offset.
    let mut buffer = vec![0 as Unit; 1024];
    let end = match profile.append_to_slice(&value, &mut buffer, 0) {
        Ok(end) => end,
        Err(error) => {
            println!("  append failed: {error}");
            return;
        }
    };
    let document = buffer[..end].to_vec();

    println!("  length      = {} unit(s)", document.len());
    println!("  units       = {document:?}");
    println!("  lossy text  = {:?}", lossy(&document));
    println!("  hex         = {}", hex(&document));

    // -----------------------------------------------------------------------------------------
    // 3. The same document, read back as framing ranges.
    // -----------------------------------------------------------------------------------------
    println!();
    println!("== 3. the same document through the core's FlatDeserializer ==");
    println!(
        "  buffer = {} unit(s), variant {}, {} limiter pair(s)",
        document.len(),
        variant_name(variant),
        pairs.len()
    );

    let mut deserializer = FlatDeserializer::new(&document, variant, pairs, Vec::new(), Vec::new());

    // `iter` drives the state machine and yields the item sequence, never an intermediate
    // unarchived state; the trailing item is the newest state once the deserializer settles.
    for (index, state) in deserializer.iter().enumerate() {
        match state {
            State::E { envelop } => {
                let head = envelop.head_offset().get();
                let payload_head = envelop.payload_head_offset().get();
                let payload_tail = envelop.payload_tail_offset().get();
                println!("  item {index}: Envelop (phantom {})", envelop.is_phantom());
                println!("    head            = {head}");
                println!(
                    "    limiter slice   = [{head}..{payload_head}] = {:?} (hex {})",
                    lossy(&document[head..payload_head]),
                    hex(&document[head..payload_head])
                );
                println!("    payload head    = {payload_head}");
                println!("    payload tail    = {payload_tail}");
                println!(
                    "    payload units   = [{payload_head}..{payload_tail}] = {:?} (hex {})",
                    lossy(&document[payload_head..payload_tail]),
                    hex(&document[payload_head..payload_tail])
                );
                match envelop.tail_offset() {
                    Some(tail) => {
                        let tail = tail.get();
                        println!(
                            "    delimiter slice = [{payload_tail}..{tail}] = {:?} (hex {})",
                            lossy(&document[payload_tail..tail]),
                            hex(&document[payload_tail..tail])
                        );
                        println!("    tail            = {tail}");
                    }
                    None => println!("    tail            = (not confirmed yet)"),
                }
            }
            State::N { non_envelop } => {
                let head = non_envelop.head_offset().get();
                let tail = non_envelop.tail_offset().get();
                println!(
                    "  item {index}: NonEnvelop (phantom {})",
                    non_envelop.is_phantom()
                );
                println!("    head            = {head}");
                println!("    tail            = {tail}");
                println!(
                    "    units           = [{head}..{tail}] = {:?} (hex {})",
                    lossy(&document[head..tail]),
                    hex(&document[head..tail])
                );
            }
        }
    }
}
