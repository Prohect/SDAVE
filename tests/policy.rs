//! The 0.3.0 limiter-pair select policy: `FirstMatch` (historical) versus
//! `LeastRepeat` and `SmallestFrame`. Everything here runs over a test-local
//! profile; the shipped default pair set is never touched.
//!
//! Two proofs:
//!
//! 1. A hand-computable unit test: a payload with known internal runs per unit
//!    and a pair set that also contains a pair whose units do not occur at all.
//!    Each policy's choice is asserted exactly, so the chosen pair and its
//!    repeat are both asserted and printed.
//! 2. A nested-document measurement that reports both policies' byte counts and
//!    max delimiter runs. The payoff is bounded by the pair set (only an
//!    application-supplied collision-free pair collapses the repeat), so the
//!    assertions here are a non-regression bound, not a `depth * run` claim.

#![cfg(feature = "derive")]

use sdave::*;
use std::num::NonZeroUsize;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Schema {
    /// A string leaf whose value collides with the first pair's delimiter.
    text: String,
    /// Recursive, and last.
    inner: Option<Box<Schema>>,
}

fn pair(limiter: u8, delimiter: u8, repeat: usize) -> LimiterPair<u8> {
    LimiterPair {
        least_repeat: NonZeroUsize::new(repeat),
        limiter: vec![limiter],
        delimiter: vec![delimiter],
    }
}

/// Test-local profile: the first pair's delimiter (`~`) collides with the
/// document content; the second pair (`#`/`$`) is collision-free. Limits are
/// widened so a deliberately deep document fits.
fn base_config() -> Config {
    Config::new(Variant::V2, vec![pair(b'^', b'~', 1), pair(b'#', b'$', 1)])
        .unwrap()
        .with_limits(Limits {
            max_depth: 4096,
            max_input_bytes: usize::MAX,
            max_output_bytes: usize::MAX,
            max_elements: usize::MAX,
            max_delimiter_work: usize::MAX,
            max_repeat: 1 << 20,
        })
        .unwrap()
}

fn config(policy: PairSelect) -> Config {
    base_config().with_pair_select(policy)
}

const RUN: usize = 200;
const DEPTH: usize = 64;

/// `DEPTH + 1` levels; every level repeats the colliding delimiter `RUN` times.
fn nested() -> Schema {
    let mut node = Schema {
        text: "~".repeat(RUN),
        inner: None,
    };
    for _ in 0..DEPTH {
        node = Schema {
            text: "~".repeat(RUN),
            inner: Some(Box::new(node)),
        };
    }
    node
}

fn encode(policy: PairSelect, value: &Schema) -> Vec<u8> {
    Serializer::<DefaultNames>::new(config(policy))
        .unwrap()
        .to_vec(value)
        .unwrap()
}

fn decode(policy: PairSelect, input: &[u8]) -> Result<Schema> {
    Deserializer::<DefaultNames>::new(config(policy))
        .unwrap()
        .from_slice(input)
}

fn max_run(bytes: &[u8], unit: u8) -> usize {
    let (mut best, mut current) = (0usize, 0usize);
    for &byte in bytes {
        if byte == unit {
            current += 1;
            best = best.max(current);
        } else {
            current = 0;
        }
    }
    best
}

fn envelope_of(frame: &[u8], config: &Config) -> Envelop {
    let mut parser = FlatParser::new(frame, config.variant(), config.limiter_pairs().to_vec());
    parser.parse_incremental();
    parser
        .iter()
        .find_map(|state| match state {
            State::E { envelop } if !envelop.is_phantom() => Some(envelop.clone()),
            _ => None,
        })
        .expect("one confirmed envelope")
}

/// `(limiter slice, repeat, payload)` of the single envelope in `frame`.
fn describe(frame: &[u8], config: &Config) -> (String, usize, Vec<u8>) {
    let env = envelope_of(frame, config);
    let head = env.head_offset().get();
    let payload_head = env.payload_head_offset().get();
    let limiter = std::str::from_utf8(&frame[head..payload_head]).unwrap().to_owned();
    let repeat = limiter.chars().count();
    let payload = frame[env.payload_head_offset().get()..env.payload_tail_offset().get()].to_vec();
    (limiter, repeat, payload)
}

// ---------------------------------------------------------------------------
// 1. hand-computable policy selection
// ---------------------------------------------------------------------------

#[test]
fn policy_selects_the_hand_computable_pair() {
    // Payload with known internal runs: five `~`, two `$`, and none of the units
    // of pairs P2/P3. It does not start with any limiter, so no pair is skipped.
    let payload: &[u8] = b"x~~~~~yy$$z";
    // P0 ('^','~') needs 5+1 = 6 repeats; P1 ('#','$') needs 2+1 = 3;
    // P2 ('♦','♣') and P3 ('%','&') occur nowhere, so each needs its 1 minimum.
    let pairs = vec![
        pair(b'^', b'~', 1),
        pair(b'#', b'$', 1),
        LimiterPair {
            least_repeat: NonZeroUsize::new(1),
            limiter: "♦".as_bytes().to_vec(),
            delimiter: "♣".as_bytes().to_vec(),
        },
        pair(b'%', b'&', 1),
    ];
    let cfg = |policy: PairSelect| {
        Config::new(Variant::V1, pairs.clone())
            .unwrap()
            .with_pair_select(policy)
    };
    let frame = |policy: PairSelect| {
        Serializer::<DefaultNames>::new(cfg(policy))
            .unwrap()
            .frame(payload)
            .unwrap()
    };

    let first = frame(PairSelect::FirstMatch);
    let least = frame(PairSelect::LeastRepeat);
    let smallest = frame(PairSelect::SmallestFrame);

    // Hand-computed envelope bytes: limiter*repeat ++ payload ++ delimiter*repeat.
    let expected = |limiter: &[u8], delimiter: &[u8], repeat: usize| {
        let mut bytes = limiter.repeat(repeat);
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&delimiter.repeat(repeat));
        bytes
    };
    // FirstMatch -> P0, repeat 6 (12 slice bytes + 11 payload = 23).
    assert_eq!(first, expected(b"^", b"~", 6));
    // LeastRepeat -> P2, repeat 1 (earliest of the two repeat-1 pairs; 6+11 = 17).
    assert_eq!(least, expected("♦".as_bytes(), "♣".as_bytes(), 1));
    // SmallestFrame -> P3, repeat 1 (narrowest frame; 2+11 = 13).
    assert_eq!(smallest, expected(b"%", b"&", 1));
    assert_eq!((first.len(), least.len(), smallest.len()), (23, 17, 13));

    for (policy, frame, chosen) in [
        (PairSelect::FirstMatch, &first, "P0 ('^','~')"),
        (PairSelect::LeastRepeat, &least, "P2 ('♦','♣')"),
        (PairSelect::SmallestFrame, &smallest, "P3 ('%','&')"),
    ] {
        let (limiter, repeat, decoded) = describe(frame, &cfg(policy));
        eprintln!(
            "{policy:?}: chose {chosen} -> limiter {limiter:?} x{repeat}, frame {} bytes",
            frame.len()
        );
        assert_eq!(decoded, payload.to_vec(), "payload must round-trip verbatim");
    }
}

// ---------------------------------------------------------------------------
// 2. nested-document measurement
// ---------------------------------------------------------------------------

#[test]
fn nested_document_measurement() {
    let value = nested();
    let first = encode(PairSelect::FirstMatch, &value);
    let lowest = encode(PairSelect::LeastRepeat, &value);
    let first_run = max_run(&first, b'~');
    let lowest_run = max_run(&lowest, b'~');

    let (first_limiter, first_repeat, _) = describe(&first, &config(PairSelect::FirstMatch));
    let (lowest_limiter, lowest_repeat, _) = describe(&lowest, &config(PairSelect::LeastRepeat));
    eprintln!(
        "nested depth={DEPTH} run={RUN}: FirstMatch {} bytes, top pair {:?} x{}, max '~' run {first_run}; \
         LeastRepeat {} bytes, top pair {:?} x{}, max '~' run {lowest_run}",
        first.len(),
        first_limiter,
        first_repeat,
        lowest.len(),
        lowest_limiter,
        lowest_repeat,
    );

    // Both policies round-trip through the same (parser-order) profile.
    assert_eq!(decode(PairSelect::FirstMatch, &first).unwrap(), value);
    assert_eq!(decode(PairSelect::LeastRepeat, &lowest).unwrap(), value);

    // The run is driven by the payload's own content (~RUN), not by depth, and
    // the byte-size policy is a non-regression over the historical one.
    assert!(first_run >= RUN && lowest_run >= RUN);
    assert!(lowest_run <= first_run, "{lowest_run} vs {first_run}");
    assert!(lowest.len() <= first.len(), "{} vs {}", lowest.len(), first.len());
}

// ---------------------------------------------------------------------------
// supporting policy checks
// ---------------------------------------------------------------------------

#[test]
fn default_policy_is_the_historical_first_match() {
    assert_eq!(PairSelect::default(), PairSelect::FirstMatch);
    assert_eq!(base_config().pair_select(), PairSelect::FirstMatch);

    // A profile that never mentions the policy frames identically to FirstMatch.
    let value = nested();
    let historical = Serializer::<DefaultNames>::new(base_config())
        .unwrap()
        .to_vec(&value)
        .unwrap();
    assert_eq!(historical, encode(PairSelect::FirstMatch, &value));
}

#[test]
fn fingerprint_covers_pair_select() {
    assert_eq!(
        config(PairSelect::FirstMatch).fingerprint(),
        config(PairSelect::FirstMatch).fingerprint()
    );
    assert_ne!(
        config(PairSelect::FirstMatch).fingerprint(),
        config(PairSelect::LeastRepeat).fingerprint()
    );
    assert_ne!(
        config(PairSelect::LeastRepeat).fingerprint(),
        config(PairSelect::SmallestFrame).fingerprint()
    );
}

#[test]
fn smallest_frame_prefers_narrower_units() {
    // Pair A: wide 3-byte units, needs 2 repeats. Pair B: narrow 1-byte units,
    // needs 3 repeats because the payload collides with its delimiter. So
    // `LeastRepeat` keeps A while `SmallestFrame` switches to the shorter B.
    let pairs = vec![
        LimiterPair {
            least_repeat: NonZeroUsize::new(2),
            limiter: "♦".as_bytes().to_vec(),
            delimiter: "♣".as_bytes().to_vec(),
        },
        pair(b'^', b'~', 1),
    ];
    let cfg = |policy: PairSelect| {
        Config::new(Variant::V1, pairs.clone())
            .unwrap()
            .with_pair_select(policy)
    };
    let frame = |policy: PairSelect| {
        Serializer::<DefaultNames>::new(cfg(policy))
            .unwrap()
            .frame(b"xx~~yy")
            .unwrap()
    };

    let first = frame(PairSelect::FirstMatch);
    let lowest = frame(PairSelect::LeastRepeat);
    let smallest = frame(PairSelect::SmallestFrame);

    // A: 2*(3+3) bytes of slices. B: 3*(1+1) bytes of slices. Same 6-byte payload.
    assert_eq!(first.len(), 18);
    assert_eq!(lowest.len(), 18);
    assert_eq!(smallest.len(), 12);
    assert!(smallest.len() < lowest.len());

    for policy in [
        PairSelect::FirstMatch,
        PairSelect::LeastRepeat,
        PairSelect::SmallestFrame,
    ] {
        let bytes = frame(policy);
        assert_eq!(
            describe(&bytes, &cfg(policy)).2,
            b"xx~~yy".to_vec(),
            "payload must round-trip verbatim"
        );
    }
}
