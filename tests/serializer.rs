//! The encoder primitives: `can_serialize` and `get_all_serializable_limiter_pairs`,
//! including the encoder/decoder round-trip and the minimality of the repeat.

mod common;

use core::num::NonZeroUsize;

use common::*;
use sdave::{LimiterPair, Variant, can_serialize, get_all_serializable_limiter_pairs};

fn nz(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}

/// `limiter * repeat ++ payload ++ delimiter * repeat`, as `can_serialize`'s contract spells it.
fn frame(limiter: &str, delimiter: &str, repeat: usize, payload: &str) -> Vec<char> {
    let mut s = String::new();
    for _ in 0..repeat {
        s.push_str(limiter);
    }
    s.push_str(payload);
    for _ in 0..repeat {
        s.push_str(delimiter);
    }
    s.chars().collect()
}

fn chars_of(pair_i: &LimiterPair<'_, char, std::alloc::Global>) -> (String, String) {
    (
        pair_i.limiter().iter().collect(),
        pair_i.delimiter().iter().collect(),
    )
}

/// The fixture pair type of this file.
type P = LimiterPair<'static, char, std::alloc::Global>;

/// One row of the `can_serialize` repeat table.
struct Case<'a> {
    buffer: Vec<char>,
    payload: &'static str,
    variant: Variant,
    pair: &'a P,
    expect: Option<usize>,
}

/// The payload of the single Envelop a framing decodes to, or `None` when the framing does
/// not decode to exactly one Envelop.
///
/// V2's confirmation latency means a *successful* framing may still be pending; both shapes
/// count as "the Envelop frames this payload".
fn framed_payload(items: &[Item]) -> Option<&str> {
    match items {
        [Item::Env(payload)] | [Item::Pending(payload)] => Some(payload.as_str()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// the minimal working repeat
// ---------------------------------------------------------------------------

#[test]
fn can_serialize_repeat_table() {
    let pair_2 = pair(2, "^", "~");
    let pair_1 = pair(1, "^", "~");

    // (buffer, payload, variant, pair, expected repeat)
    let cases = vec![
        Case {
            buffer: Vec::new(),
            payload: "A",
            variant: Variant::V1,
            pair: &pair_2,
            expect: Some(2),
        },
        Case {
            buffer: Vec::new(),
            payload: "A",
            variant: Variant::V1,
            pair: &pair_1,
            expect: Some(1),
        },
        Case {
            buffer: Vec::new(),
            payload: "A~~B",
            variant: Variant::V1,
            pair: &pair_2,
            expect: Some(3),
        },
        Case {
            buffer: Vec::new(),
            payload: "A~~~B",
            variant: Variant::V1,
            pair: &pair_2,
            expect: Some(4),
        },
        Case {
            buffer: Vec::new(),
            payload: "~A",
            variant: Variant::V1,
            pair: &pair_2,
            expect: Some(2),
        },
        Case {
            buffer: Vec::new(),
            payload: "A~",
            variant: Variant::V1,
            pair: &pair_2,
            expect: None,
        },
        Case {
            buffer: Vec::new(),
            payload: "^A",
            variant: Variant::V1,
            pair: &pair_2,
            expect: None,
        },
        Case {
            buffer: buf("x^"),
            payload: "A",
            variant: Variant::V1,
            pair: &pair_2,
            expect: None,
        },
        Case {
            buffer: Vec::new(),
            payload: "A",
            variant: Variant::V2,
            pair: &pair_2,
            expect: Some(2),
        },
        Case {
            buffer: Vec::new(),
            payload: "A~~",
            variant: Variant::V2,
            pair: &pair_2,
            expect: Some(2),
        },
        Case {
            buffer: Vec::new(),
            payload: "A~~B",
            variant: Variant::V2,
            pair: &pair_2,
            expect: Some(3),
        },
        Case {
            buffer: Vec::new(),
            payload: "A~~~B",
            variant: Variant::V2,
            pair: &pair_2,
            expect: Some(4),
        },
    ];

    for Case {
        buffer,
        payload,
        variant,
        pair: pair_i,
        expect,
    } in cases
    {
        let actual = unsafe { can_serialize(&buffer, &buf(payload), variant, pair_i) };
        assert_eq!(
            actual,
            expect.map(nz),
            "can_serialize({buffer:?}, {payload:?}, {}, {:?})",
            vname(variant),
            chars_of(pair_i)
        );
    }
}

#[test]
fn can_serialize_rejects_degenerate_pairs() {
    let payload = buf("A");
    let empty: Vec<char> = Vec::new();

    // `least_repeat == None` is ignored by the deserializer, so nothing can be framed.
    let no_repeat = pair_opt(None, "^", "~");
    assert_eq!(
        unsafe { can_serialize(&empty, &payload, Variant::V1, &no_repeat) },
        None
    );

    let no_limiter = pair(2, "", "~");
    assert_eq!(
        unsafe { can_serialize(&empty, &payload, Variant::V1, &no_limiter) },
        None
    );

    let no_delimiter = pair(2, "^", "");
    assert_eq!(
        unsafe { can_serialize(&empty, &payload, Variant::V1, &no_delimiter) },
        None
    );

    // A payload that collides with a limiter unit cannot be framed.
    assert_eq!(
        unsafe { can_serialize(&empty, &buf("^A"), Variant::V1, &pair(2, "^", "~")) },
        None
    );
    // Nor one that collides with the delimiter at the tail (V1 only).
    assert_eq!(
        unsafe { can_serialize(&empty, &buf("A~"), Variant::V1, &pair(2, "^", "~")) },
        None
    );
    // Nor a context whose tail is a live limiter slice.
    assert_eq!(
        unsafe { can_serialize(&buf("x^"), &payload, Variant::V1, &pair(2, "^", "~")) },
        None
    );
}

#[test]
fn empty_unit_pairs_are_excluded_from_the_profile() {
    // `LimiterPair::limiter()` and `delimiter()` document that a pair with an empty unit "would
    // be ignored"; `can_serialize` enforces that by answering `None` before it ever looks at
    // the payload (`src/lib.rs:814`, `:817`) — never `Some`, never a panic.
    let buffer: Vec<char> = Vec::new();
    let payload = buf("B");
    let empty_limiter = pair(2, "", "!");
    let empty_delimiter = pair(2, "?", "");

    for variant in [Variant::V1, Variant::V2] {
        assert_eq!(
            unsafe { can_serialize(&buffer, &payload, variant, &empty_limiter) },
            None,
            "empty limiter ({})",
            vname(variant)
        );
        assert_eq!(
            unsafe { can_serialize(&buffer, &payload, variant, &empty_delimiter) },
            None,
            "empty delimiter ({})",
            vname(variant)
        );
    }

    // Pair selection drops them while keeping every valid pair, each carrying its own minimal
    // repeat: the '^','~' pair declares `least_repeat` 2, the '♦','♣' pair 1.
    let profile = vec![
        pair(2, "", "!"),
        pair(2, "^", "~"),
        pair(2, "?", ""),
        pair(1, "♦", "♣"),
    ];
    let before: Vec<_> = profile.iter().map(pair_fields).collect();
    let mut result: Pairs = Vec::new();
    unsafe {
        get_all_serializable_limiter_pairs(Variant::V1, &buffer, &payload, &profile, &mut result)
    };

    let accepted: Vec<_> = result
        .iter()
        .map(|pair_i| {
            let (l, d) = chars_of(pair_i);
            (l, d, pair_i.least_repeat().map(|r| r.get()))
        })
        .collect();
    assert_eq!(
        accepted,
        vec![
            ("^".to_string(), "~".to_string(), Some(2)),
            ("♦".to_string(), "♣".to_string(), Some(1)),
        ]
    );
    assert_eq!(
        profile.iter().map(pair_fields).collect::<Vec<_>>(),
        before,
        "the profile itself is untouched"
    );
}

#[test]
fn none_never_mutates_the_pair_or_the_profile() {
    let pair_i = pair(2, "^", "~");
    let before = pair_fields(&pair_i);
    assert_eq!(
        unsafe { can_serialize(&buf("x^"), &buf("A"), Variant::V1, &pair_i) },
        None
    );
    assert_eq!(
        unsafe { can_serialize(&buf(""), &buf("^A"), Variant::V2, &pair_i) },
        None
    );
    assert_eq!(
        pair_fields(&pair_i),
        before,
        "None is per call, never a profile change"
    );

    let pairs = std_pairs();
    let before: Vec<_> = pairs.iter().map(pair_fields).collect();
    let mut result: Pairs = vec![pair(2, "!", "!"), pair(2, "!", "!")];
    unsafe {
        get_all_serializable_limiter_pairs(
            Variant::V1,
            &buf("0%%A%%"),
            &buf("B"),
            &pairs,
            &mut result,
        )
    };
    assert_eq!(
        pairs.iter().map(pair_fields).collect::<Vec<_>>(),
        before,
        "get_all_serializable_limiter_pairs must not touch the profile"
    );
}

// ---------------------------------------------------------------------------
// get_all_serializable_limiter_pairs == exactly the accepted pairs
// ---------------------------------------------------------------------------

#[test]
fn get_all_serializable_limiter_pairs_on_an_empty_context() {
    let pairs = std_pairs();
    let mut result: Pairs = Vec::new();
    unsafe {
        get_all_serializable_limiter_pairs(Variant::V1, &buf(""), &buf("B"), &pairs, &mut result)
    };

    assert_eq!(
        result.len(),
        5,
        "every pair frames 'B' at the head of an empty buffer"
    );
    for (i, pair_i) in result.iter().enumerate() {
        let expected_repeat = if i == 4 { 1 } else { 2 };
        assert_eq!(
            pair_i.least_repeat().map(|x| x.get()),
            Some(expected_repeat)
        );
        assert_eq!(chars_of(pair_i), chars_of(&pairs[i]));
        // Each entry must be exactly what `can_serialize` answers for that pair.
        assert_eq!(
            pair_i.least_repeat(),
            unsafe { can_serialize(&buf(""), &buf("B"), Variant::V1, &pairs[i]) },
            "pair {i}"
        );
    }
}

#[test]
fn get_all_serializable_limiter_pairs_rejects_a_limiter_live_in_the_context() {
    // The buffer's tail is '%', so the ('%','%') pair cannot frame anything right here.
    let pairs = std_pairs();
    let mut result: Pairs = Vec::new();
    unsafe {
        get_all_serializable_limiter_pairs(
            Variant::V1,
            &buf("0%%A%%"),
            &buf("B"),
            &pairs,
            &mut result,
        )
    };

    let accepted: Vec<_> = result
        .iter()
        .map(|pair_i| {
            let (l, d) = chars_of(pair_i);
            (l, d, pair_i.least_repeat().map(|r| r.get()))
        })
        .collect();
    assert_eq!(
        accepted,
        vec![
            ("^".to_string(), "~".to_string(), Some(2)),
            ("?".to_string(), "!".to_string(), Some(2)),
            ("{".to_string(), "}".to_string(), Some(2)),
            ("♦".to_string(), "♣".to_string(), Some(1)),
        ]
    );
    assert_eq!(
        unsafe { can_serialize(&buf("0%%A%%"), &buf("B"), Variant::V1, &pairs[2]) },
        None
    );
}

// ---------------------------------------------------------------------------
// the encoder / decoder round trip
// ---------------------------------------------------------------------------

#[test]
fn every_serializable_pair_round_trips_v1() {
    let payload = "SDAVE";
    let empty: Vec<char> = Vec::new();

    for pair_i in std_pairs() {
        let (limiter, delimiter) = chars_of(&pair_i);
        let repeat = unsafe { can_serialize(&empty, &buf(payload), Variant::V1, &pair_i) }
            .expect("pair frames the payload")
            .get();
        let framed = frame(&limiter, &delimiter, repeat, payload);
        let shot = one_shot(&framed, Variant::V1, &vec![pair_i]);

        assert_eq!(
            shot.items,
            vec![e(payload)],
            "{limiter:?}/{delimiter:?} framing {framed:?}"
        );
        assert_eq!(shot.bounds.len(), 1);
        assert_eq!(shot.bounds[0].tail, Some(framed.len()));
        assert_eq!(shot.internal, MI::CleanArchived);
    }
}

#[test]
fn every_serializable_pair_round_trips_v2_with_a_confirmer() {
    let payload = "SDAVE";
    let empty: Vec<char> = Vec::new();

    for pair_i in std_pairs() {
        let (limiter, delimiter) = chars_of(&pair_i);
        let repeat = unsafe { can_serialize(&empty, &buf(payload), Variant::V2, &pair_i) }
            .expect("pair frames the payload")
            .get();
        let mut framed = frame(&limiter, &delimiter, repeat, payload);

        // The documented V2 confirmation latency: one non-delimiter unit behind the
        // delimiter slice is what archives the Envelop.
        let pending = one_shot(&framed, Variant::V2, &vec![pair_i.clone()]);
        assert_eq!(
            pending.items,
            vec![p(payload)],
            "{limiter:?}/{delimiter:?}: an unconfirmed framing must stay pending"
        );

        framed.push('#');
        let shot = one_shot(&framed, Variant::V2, &vec![pair_i]);
        assert_eq!(
            shot.items,
            vec![e(payload), h("#")],
            "{limiter:?}/{delimiter:?} framing {framed:?}"
        );
        assert_eq!(shot.bounds.len(), 1);
        assert_eq!(shot.bounds[0].tail, Some(framed.len() - 1));
    }
}

#[test]
fn minimal_repeat_is_the_smallest_repeat_that_frames() {
    let empty: Vec<char> = Vec::new();
    let payload = "A~~~B";

    // ---- V1 ---------------------------------------------------------------
    let pair_v1 = pair(2, "^", "~");
    let repeat = unsafe { can_serialize(&empty, &buf(payload), Variant::V1, &pair_v1) }
        .expect("pair frames the payload")
        .get();
    assert_eq!(repeat, 4, "the payload's own '~~~' run forces repeat > 3");

    let single = vec![pair_v1];
    let ok = one_shot(&frame("^", "~", repeat, payload), Variant::V1, &single);
    assert_eq!(
        framed_payload(&ok.items),
        Some(payload),
        "repeat 4 is a framing of the payload"
    );

    // One repeat less is *not* a framing of this payload: the payload's own '~~~' run is
    // taken as the delimiter slice (V1: "the every first full matched delimiter slice is
    // just the delimiter slice"), so the Envelop frames only "A" and the rest of the
    // payload stays in the channel as a NonEnvelop. This is the same shape the README's own
    // `1^^^echo^^hello~~~~~2 -> [..., payload echo^^hello, ~~2]` vector shows.
    let short = one_shot(&frame("^", "~", repeat - 1, payload), Variant::V1, &single);
    assert_ne!(framed_payload(&short.items), Some(payload));
    assert_eq!(short.items, vec![e("A"), h("B~~~")]);

    // ---- V2 ---------------------------------------------------------------
    let pair_v2 = pair(2, "^", "~");
    let repeat_v2 = unsafe { can_serialize(&empty, &buf(payload), Variant::V2, &pair_v2) }
        .expect("pair frames the payload")
        .get();
    assert_eq!(repeat_v2, 4);

    // V2's confirmation latency: with no non-delimiter behind the delimiter slice the
    // Envelop stays pending, but it still frames exactly the payload.
    let pending = one_shot(
        &frame("^", "~", repeat_v2, payload),
        Variant::V2,
        &vec![pair_v2.clone()],
    );
    assert_eq!(framed_payload(&pending.items), Some(payload));
    assert_eq!(pending.items, vec![p(payload)]);

    // One repeat less again fails to frame the payload. Here the payload's run "~~~" *is*
    // followed by a non-delimiter ('B'), so V2 confirms the run as the delimiter slice and
    // the Envelop frames "A", leaving "B~~~" as a NonEnvelop — the shape is the confirmed
    // V1 shape above, not a pending Envelop.
    let short_v2 = one_shot(
        &frame("^", "~", repeat_v2 - 1, payload),
        Variant::V2,
        &vec![pair_v2],
    );
    assert_ne!(framed_payload(&short_v2.items), Some(payload));
    assert_eq!(short_v2.items, vec![e("A"), h("B~~~")]);
}

/// Regression guard for a `can_serialize` panic that the core used to hit (fixed since: the scan
/// is now bounded by `tail` and skipped entirely when `tail < delimiter.len()`).
///
/// Historical repro: `can_serialize(&[], &['~', '~', '~'], Variant::V2, pair('^','~',2))`
/// panicked at what was then `src/lib.rs:834:29` — "slice index starts at 1 but ends at 0" —
/// because the V2 strip had already reduced `tail` to 0 while the run scan still measured its
/// bounds against `payload.len()` and evaluated `&payload[1..0]`.
///
/// The documented contract is "`Some(least_repeat)` or `None`", never a panic, and the trailing
/// run is deliberately excluded from the run scan, so the answer is the pair's own
/// `least_repeat` — 2. The second half of this test also proves the answer is a *working*
/// framing: `^^` + `~~~` + `~~` + one confirmer decodes as one Envelop whose payload is exactly
/// `~~~` (V2 hands a run's extra repeats back to the payload, so `payload_tail_offset` lands on
/// the fifth unit).
#[test]
fn can_serialize_answers_for_a_long_trailing_delimiter_run() {
    let empty: Vec<char> = Vec::new();
    let pair_i = pair(2, "^", "~");
    let payload = buf("~~~");

    let repeat = unsafe { can_serialize(&empty, &payload, Variant::V2, &pair_i) }
        .expect("an answer, not a panic")
        .get();
    assert_eq!(repeat, 2);

    let mut framed = frame("^", "~", repeat, "~~~");
    framed.push('#');
    assert_eq!(framed.iter().collect::<String>(), "^^~~~~~#");

    let shot = one_shot(&framed, Variant::V2, &vec![pair_i]);
    assert_eq!(shot.items, vec![e("~~~"), h("#")]);
    assert_eq!(
        shot.bounds,
        vec![Off {
            head: 0,
            payload_head: 2,
            payload_tail: 5,
            tail: Some(7)
        }]
    );
}

#[test]
fn can_serialize_frames_at_a_clean_boundary_of_an_existing_channel() {
    // The context is a channel with an archived Envelop; the payload is appended after it.
    let pairs = std_pairs();
    let buffer = buf("0%%A%%");
    let payload = "B";
    let pair_i = pair(2, "^", "~");
    let repeat = unsafe { can_serialize(&buffer, &buf(payload), Variant::V1, &pair_i) }
        .expect("pair frames the payload at this boundary")
        .get();
    assert_eq!(repeat, 2);

    let mut framed = buffer.clone();
    framed.extend(frame("^", "~", repeat, payload));
    assert_eq!(framed.iter().collect::<String>(), "0%%A%%^^B~~");

    let shot = one_shot(&framed, Variant::V1, &pairs);
    assert_eq!(shot.items, vec![PH, h("0"), e("A"), ZH, e("B")]);
    assert_eq!(
        shot.bounds,
        vec![
            Off {
                head: 0,
                payload_head: 0,
                payload_tail: 0,
                tail: None
            },
            Off {
                head: 1,
                payload_head: 3,
                payload_tail: 4,
                tail: Some(6)
            },
            Off {
                head: 6,
                payload_head: 8,
                payload_tail: 9,
                tail: Some(11)
            },
        ]
    );
}
