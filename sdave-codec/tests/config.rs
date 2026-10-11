//! The profile: `Config`'s components, its pair-select policy, and its fingerprint.
//!
//! Every expectation is derived from the codec README (profile identity, the three policies) and
//! from `can_serialize`'s documented minimal-repeat contract.

use core::cell::OnceCell;
use core::num::NonZeroUsize;
use std::alloc::Global;

use sdave_codec::{
    Config, ErrorKind, Fingerprint, LimiterPair, Limits, NamePolicy, PairSelect, ShortType, Variant,
};

type Pair = LimiterPair<'static, char, Global>;
type Profile = Config<'static, 'static, char, Global>;

/// A constructor whose last path segment collides with `core::option::Option`'s.
mod shadow {
    pub struct Option;
}

/// The README's pair set: `[('^', '~', 2), ('?', '!', 2), ('%', '%', 2), ('{', '}', 2), ('♦', '♣', 1)]`.
const STD: &[(&str, &str, usize)] = &[
    ("^", "~", 2),
    ("?", "!", 2),
    ("%", "%", 2),
    ("{", "}", 2),
    ("♦", "♣", 1),
];

fn unit(text: &str) -> &'static Vec<char> {
    Box::leak(Box::new(text.chars().collect::<Vec<char>>()))
}

fn pair(least_repeat: usize, limiter: &str, delimiter: &str) -> Pair {
    LimiterPair::new(
        Some(NonZeroUsize::new(least_repeat).unwrap()),
        OnceCell::from(unit(limiter)),
        OnceCell::from(unit(delimiter)),
    )
}

fn ignored_pair(limiter: &str, delimiter: &str) -> Pair {
    LimiterPair::new(
        None,
        OnceCell::from(unit(limiter)),
        OnceCell::from(unit(delimiter)),
    )
}

fn pairs(specs: &[(&str, &str, usize)]) -> &'static Vec<Pair> {
    Box::leak(Box::new(
        specs
            .iter()
            .map(|(limiter, delimiter, repeat)| pair(*repeat, limiter, delimiter))
            .collect::<Vec<Pair>>(),
    ))
}

fn profile_of(
    variant: Variant,
    pairs: &'static Vec<Pair>,
    limits: Limits,
    pair_select: PairSelect,
) -> Profile {
    Config::new(variant, pairs, limits, pair_select, NamePolicy::VERBATIM)
        .expect("valid profile")
}

fn demo_profile() -> Profile {
    profile_of(
        Variant::V1,
        pairs(STD),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    )
}

fn text(units: &[char]) -> String {
    units.iter().collect()
}

// ---------------------------------------------------------------------------
// the profile's components
// ---------------------------------------------------------------------------

#[test]
fn a_profile_exposes_what_it_was_built_from() {
    let profile = demo_profile();
    assert!(profile.variant() == Variant::V1);
    assert!(profile.pair_select() == PairSelect::FirstMatch);
    assert_eq!(profile.limits(), &Limits::UNBOUNDED);
    assert_eq!(profile.limiter_pairs().len(), STD.len());
    assert!(profile.name_policy().is_verbatim());
    assert_eq!(profile.limiter_pairs()[0].limiter().len(), 1);
    assert_eq!(text(profile.limiter_pairs()[4].delimiter()), "♣");
}

#[test]
fn limits_are_three_optional_bounds() {
    assert_eq!(Limits::default(), Limits::UNBOUNDED);
    assert_eq!(Limits::UNBOUNDED, Limits::new(0, 0, 0));
    assert_ne!(Limits::new(4, 0, 0), Limits::UNBOUNDED);
    assert_ne!(Limits::new(0, 4096, 0), Limits::UNBOUNDED);
    assert_ne!(Limits::new(0, 0, 8), Limits::UNBOUNDED);
}

#[test]
fn first_match_is_the_default_policy() {
    assert_eq!(PairSelect::default(), PairSelect::FirstMatch);
}

// ---------------------------------------------------------------------------
// the fingerprint
// ---------------------------------------------------------------------------

#[test]
fn the_digest_primitive_is_fnv1a_64() {
    // The published FNV-1a 64-bit vectors, independent of this crate.
    assert_eq!(Fingerprint::of_bytes(b"").get(), 0xcbf2_9ce4_8422_2325);
    assert_eq!(Fingerprint::of_bytes(b"a").get(), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(
        Fingerprint::of_bytes(b"foobar").get(),
        0x8594_4171_f739_67e8
    );
    assert_eq!(Fingerprint::ENCODING_VERSION, 1);
}

#[test]
fn a_fingerprint_is_a_version_tagged_byte_encoding() {
    let fingerprint = demo_profile().fingerprint();
    assert_eq!(fingerprint.to_string().len(), 16);
    assert_eq!(
        Fingerprint::new(u64::from_le_bytes(fingerprint.to_le_bytes())),
        fingerprint
    );
}

#[test]
fn the_fingerprint_is_stable_for_the_same_profile() {
    // Two profiles with equal components but separately built pair sets agree.
    let left = demo_profile();
    let right = profile_of(
        Variant::V1,
        pairs(STD),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    assert_eq!(left.fingerprint(), right.fingerprint());
}

#[test]
fn the_fingerprint_covers_every_documented_component() {
    let base = demo_profile().fingerprint();

    let variant = profile_of(
        Variant::V2,
        pairs(STD),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    let pair_list = profile_of(
        Variant::V1,
        pairs(&[("^", "~", 2)]),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    let least_repeat = profile_of(
        Variant::V1,
        pairs(&[("^", "~", 3)]),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    let limits = profile_of(
        Variant::V1,
        pairs(STD),
        Limits::new(4, 0, 0),
        PairSelect::FirstMatch,
    );
    let policy = profile_of(
        Variant::V1,
        pairs(STD),
        Limits::UNBOUNDED,
        PairSelect::LeastRepeat,
    );

    let digests = [
        variant.fingerprint().get(),
        pair_list.fingerprint().get(),
        least_repeat.fingerprint().get(),
        limits.fingerprint().get(),
        policy.fingerprint().get(),
    ];
    for digest in digests {
        assert_ne!(
            digest,
            base.get(),
            "a component change must change the digest"
        );
    }
    for (index, digest) in digests.iter().enumerate() {
        assert!(
            !digests[index + 1..].contains(digest),
            "distinct components must give distinct digests"
        );
    }
}

// ---------------------------------------------------------------------------
// pair selection
// ---------------------------------------------------------------------------

#[test]
fn first_match_takes_the_first_pair_that_can_frame() {
    let profile = demo_profile();
    let chosen = profile.choose(&[], &['A']).expect("framed");
    assert_eq!(chosen.pair_index, 0);
    assert_eq!(chosen.repeat, NonZeroUsize::new(2).unwrap());
    assert_eq!(text(chosen.limiter), "^");
    assert_eq!(text(chosen.delimiter), "~");
    // The frame's own size excludes the payload: 2 repeats of one limiter unit and one delimiter
    // unit each.
    assert_eq!(chosen.frame_len(), 4);

    // Order is significant: with the pairs reversed, the first match is the other pair.
    let reversed = profile_of(
        Variant::V1,
        pairs(&[("♦", "♣", 1), ("^", "~", 2)]),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    assert_eq!(reversed.choose(&[], &['A']).unwrap().pair_index, 0);
    assert_eq!(text(reversed.choose(&[], &['A']).unwrap().limiter), "♦");
}

#[test]
fn the_pair_select_policy_is_a_choice_over_serializable_pairs() {
    // The `^^^`/`~~~` pair frames in one repeat but costs six units; the `?`/`!` pair needs two
    // repeats and costs four.
    const SPECS: &[(&str, &str, usize)] = &[("^^^", "~~~", 1), ("?", "!", 2)];
    let context: [char; 0] = [];
    let payload = ['A'];

    let first = profile_of(
        Variant::V1,
        pairs(SPECS),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    assert_eq!(first.choose(&context, &payload).unwrap().pair_index, 0);

    let least_repeat = profile_of(
        Variant::V1,
        pairs(SPECS),
        Limits::UNBOUNDED,
        PairSelect::LeastRepeat,
    );
    let chosen = least_repeat.choose(&context, &payload).unwrap();
    assert_eq!(chosen.pair_index, 0);
    assert_eq!(chosen.repeat, NonZeroUsize::new(1).unwrap());

    let smallest_frame = profile_of(
        Variant::V1,
        pairs(SPECS),
        Limits::UNBOUNDED,
        PairSelect::SmallestFrame,
    );
    let chosen = smallest_frame.choose(&context, &payload).unwrap();
    assert_eq!(chosen.pair_index, 1);
    assert_eq!(chosen.repeat, NonZeroUsize::new(2).unwrap());
    assert_eq!(chosen.frame_len(), 4);
}

#[test]
fn the_variant_decides_whether_a_delimiter_tail_can_be_framed() {
    let payload = ['A', '~'];
    let pair_list = pairs(&[("^", "~", 2)]);

    // V1: the tail of the payload must not collide with the delimiter.
    let v1 = profile_of(
        Variant::V1,
        pair_list,
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    let error = v1.choose(&[], &payload).expect_err("unframable under V1");
    assert_eq!(*error.kind(), ErrorKind::Unframable);

    // V2 accepts it, with the delimiter's own run excluded from the repeat.
    let v2 = profile_of(
        Variant::V2,
        pair_list,
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    let chosen = v2.choose(&[], &payload).expect("framed under V2");
    assert_eq!(chosen.repeat, NonZeroUsize::new(2).unwrap());
}

#[test]
fn a_context_that_ends_in_the_limiter_disables_that_pair() {
    let profile = profile_of(
        Variant::V1,
        pairs(&[("^", "~", 2)]),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
    );
    let error = profile
        .choose(&['x', '^'], &['A'])
        .expect_err("a live limiter slice precedes");
    assert_eq!(*error.kind(), ErrorKind::Unframable);

    // A payload that opens with the limiter cannot be framed either.
    let error = profile
        .choose(&[], &['^', 'A'])
        .expect_err("the payload opens with the limiter");
    assert_eq!(*error.kind(), ErrorKind::Unframable);
}

#[test]
fn an_ignored_pair_never_frames_anything() {
    // `None` repeats and empty units make a pair inert, in both directions.
    let none_repeat = Box::leak(Box::new(vec![ignored_pair("^", "~")]));
    let empty_limiter = Box::leak(Box::new(vec![pair(2, "", "~")]));
    let empty_delimiter = Box::leak(Box::new(vec![pair(2, "^", "")]));

    for pair_list in [none_repeat, empty_limiter, empty_delimiter] {
        let profile = profile_of(
            Variant::V1,
            pair_list,
            Limits::UNBOUNDED,
            PairSelect::FirstMatch,
        );
        let error = profile
            .choose(&[], &['A'])
            .expect_err("an inert pair must not frame");
        assert_eq!(*error.kind(), ErrorKind::Unframable);
    }
}

// ---------------------------------------------------------------------------
// construction
// ---------------------------------------------------------------------------

#[test]
fn building_a_profile_validates_the_naming_policy() {
    static COLLIDING: &[ShortType] = &[
        ShortType::of::<core::option::Option<u8>>(),
        ShortType::of::<shadow::Option>(),
    ];

    let error = Config::new(
        Variant::V1,
        pairs(STD),
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
        NamePolicy::new(COLLIDING),
    )
    .err()
    .expect("an ambiguous policy must be rejected");
    assert_eq!(*error.kind(), ErrorKind::InvalidNamePolicy);
}

#[test]
fn a_profile_needs_at_least_one_pair() {
    let empty = Box::leak(Box::new(Vec::<Pair>::new()));
    let error = Config::new(
        Variant::V1,
        empty,
        Limits::UNBOUNDED,
        PairSelect::FirstMatch,
        NamePolicy::VERBATIM,
    )
    .err()
    .expect("an empty pair list is degenerate");
    assert_eq!(
        *error.kind(),
        ErrorKind::Invariant("a profile needs at least one limiter pair")
    );
}
