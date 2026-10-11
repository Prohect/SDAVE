//! A demo that answers one question about type aliases: what does the codec actually see?
//!
//! Run it with:
//!
//! ```text
//! cargo run -p sdave-codec --example alias_demo
//! ```
//!
//! It prints, as real values: (1) what `core::any::type_name` reports for an alias and for its
//! underlying type; (2) the document a value with an alias-typed field really produces, plus the
//! core's framing breakdown of it; (3) what `ShortType` entries built from the alias and from the
//! underlying type look like, and what the policy makes of holding both; and (4) what the policy's
//! marker comparison does, in both directions, when a marker claims the alias spelling while the
//! receiver expects the compiler spelling.
//!
//! It is a host binary, so it may use `std` even though the library is `#![no_std]`.
// `sdave_codec::Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

use core::any::type_name;
use core::cell::OnceCell;
use core::num::{NonZeroU32, NonZeroUsize};
use std::alloc::Global;

use sdave_codec::{
    Config, FieldSlot, FlatDeserializer, LimiterPair, Limits, NamePolicy, PairSelect, Serialize,
    Shape, ShortType, Sink, State, Variant, depth_of, marker_eq_ascii, units_eq_ascii,
};

/// The alias this demo is about.
type AliasNonZeroU32 = NonZeroU32;

/// The local struct whose field is alias-typed, and an alias of that struct.
struct Holder {
    n: AliasNonZeroU32,
}

type Type1Alias = Holder;

/// What a generated body does for a named struct: announce the field with the *declared* type's
/// compiler spelling, and let the serializer and the codec do the rest.
impl Serialize for Holder {
    const SHAPE: Shape = Shape::Record;
    const DEPTH: u16 = depth_of(&[<AliasNonZeroU32 as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), sdave_codec::Error> {
        let mut slots = [FieldSlot::EMPTY; 1];
        sink.record(&mut slots, |record| {
            record.field("n", type_name::<AliasNonZeroU32>(), &self.n)
        })
    }
}

/// The types the demo profile's naming policy shortens to their last identifier.
static SHORT_TYPES: &[ShortType] = &[ShortType::of::<Holder>(), ShortType::of::<NonZeroU32>()];

/// The same, with only the underlying type whitelisted.
static SHORT_TYPES_UNDERLYING: &[ShortType] = &[ShortType::of::<NonZeroU32>()];

/// And with both the underlying type's entry and the alias's entry.
static SHORT_TYPES_BOTH: &[ShortType] = &[
    ShortType::of::<NonZeroU32>(),
    ShortType::of::<AliasNonZeroU32>(),
];

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
fn unit(text: &str) -> &'static Vec<u8> {
    Box::leak(Box::new(text.as_bytes().to_vec()))
}

fn pair(least_repeat: usize, limiter: &str, delimiter: &str) -> LimiterPair<'static, u8, Global> {
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

fn main() {
    // =========================================================================================
    // 1. What `type_name` reports for an alias.
    // =========================================================================================
    println!("== 1. type_name and aliases ==");
    println!(
        "  core::any::type_name::<AliasNonZeroU32>() = {:?}",
        type_name::<AliasNonZeroU32>()
    );
    println!(
        "  core::any::type_name::<NonZeroU32>()      = {:?}",
        type_name::<NonZeroU32>()
    );
    println!(
        "  core::any::type_name::<Type1Alias>()      = {:?}",
        type_name::<Type1Alias>()
    );
    println!(
        "  core::any::type_name::<Holder>()          = {:?}",
        type_name::<Holder>()
    );
    println!(
        "  AliasNonZeroU32 and NonZeroU32 report the same spelling: {}",
        type_name::<AliasNonZeroU32>() == type_name::<NonZeroU32>()
    );
    println!(
        "  Type1Alias and Holder report the same spelling:           {}",
        type_name::<Type1Alias>() == type_name::<Holder>()
    );

    // =========================================================================================
    // 2. The document, for a value with an alias-typed field.
    // =========================================================================================
    println!();
    println!("== 2. the profile and the document for Holder {{ n: AliasNonZeroU32 }} ==");

    // The same profile style as the type1_demo example: the README's recommendation for the
    // reserved set, bounded limits, the first-match policy and V1.
    let pairs: &'static Vec<LimiterPair<'static, u8, Global>> = Box::leak(Box::new(vec![
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
    println!(
        "  NamePolicy  = {} SHORT_TYPES entry(ies), rendering `{}` -> `{}`",
        policy.entries().len(),
        type_name::<Holder>(),
        policy.render(type_name::<Holder>())
    );
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

    let value = Holder {
        n: NonZeroU32::new(7).expect("7 is non-zero"),
    };
    println!("  value       = Holder {{ n: {} }}", value.n);

    let mut buffer = vec![0u8; 1024];
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

    // =========================================================================================
    // 2 (continued). The same document, read back as framing ranges by the core.
    // =========================================================================================
    println!();
    println!("== 2b. the same document through the core's FlatDeserializer ==");
    println!(
        "  buffer = {} unit(s), variant {}, {} limiter pair(s)",
        document.len(),
        variant_name(variant),
        pairs.len()
    );

    let mut deserializer = FlatDeserializer::new(&document, variant, pairs, Vec::new(), Vec::new());

    // The leading NonEnvelop is the document root's marker text: the units a receiver compares
    // against its own expectation. Captured here so point 4 can use the real ones.
    let mut wire_marker: Option<Vec<u8>> = None;

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
                if wire_marker.is_none() {
                    wire_marker = Some(document[head..tail].to_vec());
                }
            }
        }
    }

    // =========================================================================================
    // 3. The policy-side identity of the two entries.
    // =========================================================================================
    println!();
    println!("== 3. ShortType entries built from the alias and from the underlying type ==");
    let underlying_entry = ShortType::of::<NonZeroU32>();
    let alias_entry = ShortType::of::<AliasNonZeroU32>();
    println!("  ShortType::of::<NonZeroU32>():");
    println!("    full_name  = {:?}", underlying_entry.full_name());
    println!("    own_path   = {:?}", underlying_entry.own_path());
    println!("    short_name = {:?}", underlying_entry.short_name());
    println!("  ShortType::of::<AliasNonZeroU32>():");
    println!("    full_name  = {:?}", alias_entry.full_name());
    println!("    own_path   = {:?}", alias_entry.own_path());
    println!("    short_name = {:?}", alias_entry.short_name());
    println!(
        "  the two entries are identical in all three: {}",
        underlying_entry.full_name() == alias_entry.full_name()
            && underlying_entry.own_path() == alias_entry.own_path()
            && underlying_entry.short_name() == alias_entry.short_name()
    );

    let underlying_policy = NamePolicy::new(SHORT_TYPES_UNDERLYING);
    println!(
        "  policy with only the underlying entry: render({:?}) = {:?}",
        type_name::<NonZeroU32>(),
        underlying_policy.render(type_name::<NonZeroU32>())
    );
    println!(
        "  policy with only the underlying entry: render({:?}) = {:?}",
        "AliasNonZeroU32",
        underlying_policy.render("AliasNonZeroU32")
    );

    let both_policy = NamePolicy::new(SHORT_TYPES_BOTH);
    match both_policy.validate() {
        Ok(()) => {
            println!("  a policy holding both entries validates: the two name one constructor")
        }
        Err(error) => println!("  a policy holding both entries is rejected: {error}"),
    }

    // =========================================================================================
    // 4. The mismatch case, both directions.
    // =========================================================================================
    println!();
    println!("== 4. marker comparison when the spellings differ ==");
    let expected = type_name::<NonZeroU32>();
    let implied = type_name::<AliasNonZeroU32>();
    let alias_spelling = "AliasNonZeroU32";

    println!(
        "  expected (the receiver's contract)          = {:?}",
        expected
    );
    println!(
        "  the same type named through the alias       = {:?}",
        implied
    );
    println!(
        "  the alias spelling a document could claim   = {:?}",
        alias_spelling
    );
    println!(
        "  policy rendering of the expected spelling   = {:?}",
        underlying_policy.render(expected)
    );
    println!(
        "  policy rendering of the alias spelling      = {:?}",
        underlying_policy.render(alias_spelling)
    );

    println!("  direction A: expected = the compiler spelling, incoming = the alias spelling");
    println!(
        "    accepts(expected, incoming)                     = {}",
        underlying_policy.accepts(expected, alias_spelling)
    );
    println!(
        "    accepts_units::<u8>(expected, incoming bytes)   = {}",
        underlying_policy.accepts_units::<u8>(expected, alias_spelling.as_bytes())
    );

    println!("  direction B: expected = the alias spelling, incoming = the compiler spelling");
    println!(
        "    accepts(expected, incoming)                     = {}",
        underlying_policy.accepts(alias_spelling, expected)
    );
    println!(
        "    accepts_units::<u8>(expected, incoming bytes)   = {}",
        underlying_policy.accepts_units::<u8>(alias_spelling, expected.as_bytes())
    );

    println!("  the primitives the comparison is built on");
    println!(
        "    units_eq_ascii(incoming units, expected text)   = {}",
        units_eq_ascii::<u8>(alias_spelling.as_bytes(), expected)
    );
    println!(
        "    marker_eq_ascii(incoming units, expected text)  = {}",
        marker_eq_ascii::<u8>(alias_spelling.as_bytes(), expected)
    );

    println!("  the positive control: the marker the document above really carries");
    match &wire_marker {
        Some(units) => {
            println!(
                "    the document's leading marker units = {units:?} = {:?}",
                lossy(units)
            );
            println!(
                "    accepts_units::<u8>(type_name::<Holder>(), those units) = {}",
                profile
                    .name_policy()
                    .accepts_units::<u8>(type_name::<Holder>(), units)
            );
            match core::str::from_utf8(units) {
                Ok(text) => println!(
                    "    accepts(those units as text = expected, type_name::<Holder>() as incoming) = {}",
                    profile.name_policy().accepts(text, type_name::<Holder>())
                ),
                Err(_) => println!(
                    "    the marker units are not valid UTF-8, so the `&str` direction is not shown"
                ),
            }
        }
        None => println!("    the document had no leading NonEnvelop marker to compare"),
    }
}
