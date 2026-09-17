//! Tests: every example of `README.md`, plus incremental (streaming)
//! equivalence and `offset()` behaviour.

use std::num::NonZeroUsize;

use sdave::{
    Envelop, LimiterPair, NonEnvelop, State, Variant, limiter_pairs_recommended, parse,
    parse_incremental, parse_incremental_unchecked, parse_unchecked,
};

fn pair(
    limiter: &'static [u8],
    delimiter: &'static [u8],
    least_repeat: usize,
) -> LimiterPair<'static, u8> {
    LimiterPair {
        least_repeat: NonZeroUsize::new(least_repeat).unwrap(),
        limiter,
        delimiter,
    }
}

/// `[('^', '~', 2), ('?', '!', 2), ('%', '%', 2), ('{', '}', 2)]`
fn default_pairs() -> [LimiterPair<'static, u8>; 4] {
    [
        pair(b"^", b"~", 2),
        pair(b"?", b"!", 2),
        pair(b"%", b"%", 2),
        pair(b"{", b"}", 2),
    ]
}

/// Flat, human-readable view of a parsed segment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    /// Archived `NonEnvelop` (`ok == true`).
    Content(String),
    /// Pending `NonEnvelop`.
    ContentPending(String),
    /// Archived `Envelop` with non-empty payload.
    Payload(String),
    /// Pending `Envelop`; the string is the committed payload so far.
    PayloadPending(String),
    /// Archived `Envelop` with empty payload (both payload offsets `None`).
    EmptyPayload,
}

fn seg(s: &[u8], st: &State) -> Seg {
    match st {
        State::Empty => panic!("no segment behind the buffer"),
        State::N(n) => {
            let t = String::from_utf8(s[n.head_offset..n.tail_offset].to_vec()).unwrap();
            if n.ok {
                Seg::Content(t)
            } else {
                Seg::ContentPending(t)
            }
        }
        State::Y(e) => match (e.payload_head_offset, e.payload_tail_offset, e.tail_offset) {
            (None, None, Some(_)) => Seg::EmptyPayload,
            (Some(h), Some(t), Some(_)) => {
                Seg::Payload(String::from_utf8(s[h.get()..t.get()].to_vec()).unwrap())
            }
            (Some(h), t, None) => Seg::PayloadPending(match t {
                Some(t) => String::from_utf8(s[h.get()..t.get()].to_vec()).unwrap(),
                None => String::new(),
            }),
            _ => panic!("invalid Envelop offsets: {e:?}"),
        },
    }
}

fn xparse(uc: bool, v: Variant, s: &[u8], pairs: &[LimiterPair<u8>]) -> State {
    unsafe {
        if uc {
            parse_unchecked(v, s, pairs)
        } else {
            parse(v, s, pairs)
        }
    }
}

fn xparse_inc(uc: bool, v: Variant, s: &[u8], pairs: &[LimiterPair<u8>], st: &State) -> State {
    unsafe {
        if uc {
            parse_incremental_unchecked(v, s, pairs, st)
        } else {
            parse_incremental(v, s, pairs, st)
        }
    }
}

/// Batch driver: tells every segment of `s` apart in one shot.
fn run(v: Variant, s: &[u8], pairs: &[LimiterPair<u8>]) -> Vec<Seg> {
    run_uc(v, s, pairs, false)
}

fn run_uc(v: Variant, s: &[u8], pairs: &[LimiterPair<u8>], uc: bool) -> Vec<Seg> {
    let mut out = vec![];
    let mut st = xparse(uc, v, s, pairs);
    loop {
        if matches!(st, State::Empty) {
            break;
        }
        let archived = st.is_archived();
        out.push(seg(s, &st));
        if !archived {
            break;
        }
        st = xparse_inc(uc, v, s, pairs, &st);
    }
    out
}

/// Streaming driver: feeds `s` in `chunk`-sized pieces; must yield the same
/// segments as the batch driver. Also asserts offset monotonicity of pending
/// states across updates.
fn run_incremental(v: Variant, s: &[u8], pairs: &[LimiterPair<u8>], chunk: usize) -> Vec<Seg> {
    run_incremental_uc(v, s, pairs, chunk, false)
}

fn run_incremental_uc(
    v: Variant,
    s: &[u8],
    pairs: &[LimiterPair<u8>],
    chunk: usize,
    uc: bool,
) -> Vec<Seg> {
    let mut out = vec![];
    let mut st = State::Empty;
    let mut n = 0;
    loop {
        loop {
            let buf = &s[..n];
            let next = xparse_inc(uc, v, buf, pairs, &st);
            // Nothing behind the buffer yet: keep the (archived) state —
            // never re-pass `State::Empty` over non-empty contents.
            if matches!(next, State::Empty) || next == st {
                break;
            }
            assert_monotonic(&st, &next);
            // A differing head_offset confirms the previous pending state;
            // the confirmed content extends to the new state's head.
            if next.head_offset() != st.head_offset()
                && let State::N(ne) = &st
                && !ne.ok
            {
                out.push(Seg::Content(
                    String::from_utf8(buf[ne.head_offset..next.head_offset()].to_vec()).unwrap(),
                ));
            }
            st = next;
            if st.is_archived() {
                out.push(seg(buf, &st));
            } else {
                break;
            }
        }
        if n == s.len() {
            break;
        }
        n = (n + chunk.max(1)).min(s.len());
    }
    if !matches!(st, State::Empty) && !st.is_archived() {
        out.push(seg(s, &st));
    }
    out
}

/// Streaming driver with buffer compaction: drains archived prefixes and
/// shifts the pending state via `offset()`; must yield the same segments.
fn run_compaction(v: Variant, s: &[u8], pairs: &[LimiterPair<u8>], chunk: usize) -> Vec<Seg> {
    run_compaction_uc(v, s, pairs, chunk, false)
}

fn run_compaction_uc(
    v: Variant,
    s: &[u8],
    pairs: &[LimiterPair<u8>],
    chunk: usize,
    uc: bool,
) -> Vec<Seg> {
    let mut out = vec![];
    let mut buf: Vec<u8> = vec![];
    let mut st = State::Empty;
    let mut fed = 0;
    loop {
        loop {
            let next = xparse_inc(uc, v, &buf, pairs, &st);
            if matches!(next, State::Empty) || next == st {
                break;
            }
            assert_monotonic(&st, &next);
            if next.head_offset() != st.head_offset()
                && let State::N(ne) = &st
                && !ne.ok
            {
                out.push(Seg::Content(
                    String::from_utf8(buf[ne.head_offset..next.head_offset()].to_vec()).unwrap(),
                ));
            }
            st = next;
            if st.is_archived() {
                out.push(seg(&buf, &st));
                // Consume the archived segment: drop its bytes; the fresh
                // remainder re-parses from 0 (an archived state MUST NOT be
                // shifted by its own tail).
                buf.drain(..st.tail_offset().unwrap());
                st = State::Empty;
            } else {
                // Drop already-emitted content behind the pending state.
                let head = st.head_offset();
                if head > 0 {
                    buf.drain(..head);
                    unsafe { st.offset(head) };
                }
                break;
            }
        }
        if fed == s.len() {
            break;
        }
        let n = (fed + chunk.max(1)).min(s.len());
        buf.extend_from_slice(&s[fed..n]);
        fed = n;
    }
    if !matches!(st, State::Empty) && !st.is_archived() {
        out.push(seg(&buf, &st));
    }
    out
}

/// Pending states MUST grow monotonically across `parse_incremental` updates.
fn assert_monotonic(old: &State, new: &State) {
    match (old, new) {
        (State::N(a), State::N(b)) if a.head_offset == b.head_offset => {
            assert!(b.tail_offset >= a.tail_offset, "{a:?} -> {b:?}")
        }
        (State::Y(a), State::Y(b)) if a.head_offset == b.head_offset => {
            // Archiving with an empty payload resets both payload offsets to
            // `None`; otherwise they are stable / grow monotonically.
            if b.payload_head_offset.is_some() {
                assert_eq!(
                    a.payload_head_offset, b.payload_head_offset,
                    "{a:?} -> {b:?}"
                );
            }
            if let (Some(ta), Some(tb)) = (a.payload_tail_offset, b.payload_tail_offset) {
                assert!(tb >= ta, "{a:?} -> {b:?}");
            }
        }
        _ => {}
    }
}

/// Asserts batch result and streaming equivalence for several chunk sizes.
fn check(v: Variant, s: &[u8], pairs: &[LimiterPair<u8>], expected: Vec<Seg>) {
    assert_eq!(
        run(v, s, pairs),
        expected,
        "batch parse of {:?}",
        String::from_utf8_lossy(s)
    );
    for chunk in [1, 2, 3, 4, s.len().max(1)] {
        assert_eq!(
            run_incremental(v, s, pairs, chunk),
            expected,
            "incremental parse of {:?} with chunk size {chunk}",
            String::from_utf8_lossy(s)
        );
        assert_eq!(
            run_compaction(v, s, pairs, chunk),
            expected,
            "compacting parse of {:?} with chunk size {chunk}",
            String::from_utf8_lossy(s)
        );
    }
}

use Variant::{V1, V2};

// ---------------------------------------------------------------- Variant1

#[test]
fn variant1_percent() {
    // `0%%A%%%%B%%%%C%%1` -> [0, payload A, payload B, payload C, 1]
    let expected = vec![
        Seg::Content("0".into()),
        Seg::Payload("A".into()),
        Seg::Payload("B".into()),
        Seg::Payload("C".into()),
        Seg::ContentPending("1".into()),
    ];
    check(V1, b"0%%A%%%%B%%%%C%%1", &default_pairs(), expected.clone());
    // `0%%A%%%%B%%%%C%%1%` -> [0, payload A, payload B, payload C, 1]
    // (the trailing `%` is a held back potential limiter slice)
    check(V1, b"0%%A%%%%B%%%%C%%1%", &default_pairs(), expected);
}

#[test]
fn variant1_long_limiter_slice() {
    // `0%%%%%%B%%%%C%%1` -> [0, pending B%%%%C%%1]
    // limiter == delimiter: SDAVE always considers the slice as long limiter
    // slice; an empty-payload-Envelop is impossible.
    check(
        V1,
        b"0%%%%%%B%%%%C%%1",
        &default_pairs(),
        vec![
            Seg::Content("0".into()),
            Seg::PayloadPending("B%%%%C%%1".into()),
        ],
    );
}

#[test]
fn variant1_empty_payload() {
    // `0^^~~^^B~~{{C}}1` -> [0, payload (offset range None), payload B, payload C, 1]
    check(
        V1,
        b"0^^~~^^B~~{{C}}1",
        &default_pairs(),
        vec![
            Seg::Content("0".into()),
            Seg::EmptyPayload,
            Seg::Payload("B".into()),
            Seg::Payload("C".into()),
            Seg::ContentPending("1".into()),
        ],
    );
}

#[test]
fn variant1_first_match_wins() {
    // `1^^^echo^^hello~~~~~2` -> [1, payload echo^^hello, ~~2]
    check(
        V1,
        b"1^^^echo^^hello~~~~~2",
        &default_pairs(),
        vec![
            Seg::Content("1".into()),
            Seg::Payload("echo^^hello".into()),
            Seg::ContentPending("~~2".into()),
        ],
    );
}

#[test]
fn variant1_bad_pair_set_delimiter_eq_limiter() {
    // A bad example of a limiter pair set containing A and B with
    // (A.delimiter == B.limiter): [('^', '~', 2), ('~', '%', 2)].
    // `0^^A~~~~B%%~~C%%1` -> [0, payload A, payload B, payload C, 1]
    let pairs = [pair(b"^", b"~", 2), pair(b"~", b"%", 2)];
    check(
        V1,
        b"0^^A~~~~B%%~~C%%1",
        &pairs,
        vec![
            Seg::Content("0".into()),
            Seg::Payload("A".into()),
            Seg::Payload("B".into()),
            Seg::Payload("C".into()),
            Seg::ContentPending("1".into()),
        ],
    );
}

#[test]
fn variant1_bad_pair_set_shared_limiter() {
    // A bad example of a shared limiter:
    // [('^', '~', 2), ('^', "~~", 2), ('^', '!', 2)].
    // `1^^A!B!~~~~C` -> [1, payload A!B!, ~~C]
    let pairs = [
        pair(b"^", b"~", 2),
        pair(b"^", b"~~", 2),
        pair(b"^", b"!", 2),
    ];
    check(
        V1,
        b"1^^A!B!~~~~C",
        &pairs,
        vec![
            Seg::Content("1".into()),
            Seg::Payload("A!B!".into()),
            Seg::ContentPending("~~C".into()),
        ],
    );
}

// ---------------------------------------------------------------- Variant2

#[test]
fn variant2_percent() {
    // `0%%A%%%%B%%%%C%%1` -> [0, payload A%%, B, pending C%%1]
    check(
        V2,
        b"0%%A%%%%B%%%%C%%1",
        &default_pairs(),
        vec![
            Seg::Content("0".into()),
            Seg::Payload("A%%".into()),
            Seg::Content("B".into()),
            Seg::PayloadPending("C%%1".into()),
        ],
    );
}

#[test]
fn variant2_contiguous_envelops() {
    // `0{{A}}{{B}}??C!!1` -> [0, payload A, payload B, payload C, 1]
    check(
        V2,
        b"0{{A}}{{B}}??C!!1",
        &default_pairs(),
        vec![
            Seg::Content("0".into()),
            Seg::Payload("A".into()),
            Seg::Payload("B".into()),
            Seg::Payload("C".into()),
            Seg::ContentPending("1".into()),
        ],
    );
}

#[test]
fn variant2_confirmation_latency() {
    // `0{{A}}{{B}}??C!!!` -> [0, payload A, payload B, pending C!]
    check(
        V2,
        b"0{{A}}{{B}}??C!!!",
        &default_pairs(),
        vec![
            Seg::Content("0".into()),
            Seg::Payload("A".into()),
            Seg::Payload("B".into()),
            Seg::PayloadPending("C!".into()),
        ],
    );
}

#[test]
fn variant2_payload_tail_not_reserved() {
    // `1^^^echo^^hello~~~~~2` -> [1, payload echo^^hello~~, 2]
    check(
        V2,
        b"1^^^echo^^hello~~~~~2",
        &default_pairs(),
        vec![
            Seg::Content("1".into()),
            Seg::Payload("echo^^hello~~".into()),
            Seg::ContentPending("2".into()),
        ],
    );
    // `1^^^echo^^hello~~~~~` -> [1, pending echo^^hello~~ ]
    check(
        V2,
        b"1^^^echo^^hello~~~~~",
        &default_pairs(),
        vec![
            Seg::Content("1".into()),
            Seg::PayloadPending("echo^^hello~~".into()),
        ],
    );
}

#[test]
fn variant2_bad_pair_set_delimiter_eq_limiter() {
    // [('^', '~', 2), ('~', '%', 2)].
    // `0^^A~~~~B%%~~C%%1` -> [0, payload A~~, B%%, payload C, 1]
    let pairs = [pair(b"^", b"~", 2), pair(b"~", b"%", 2)];
    check(
        V2,
        b"0^^A~~~~B%%~~C%%1",
        &pairs,
        vec![
            Seg::Content("0".into()),
            Seg::Payload("A~~".into()),
            Seg::Content("B%%".into()),
            Seg::Payload("C".into()),
            Seg::ContentPending("1".into()),
        ],
    );
}

#[test]
fn variant2_bad_pair_set_shared_limiter() {
    // [('^', '~', 2), ('^', "~~", 2), ('^', '!', 2)].
    // `1^^A!B!~~~~C` -> [1, payload A!B!~~, C]
    let pairs = [
        pair(b"^", b"~", 2),
        pair(b"^", b"~~", 2),
        pair(b"^", b"!", 2),
    ];
    check(
        V2,
        b"1^^A!B!~~~~C",
        &pairs,
        vec![
            Seg::Content("1".into()),
            Seg::Payload("A!B!~~".into()),
            Seg::ContentPending("C".into()),
        ],
    );

    // [('^', "~~", 2), ('^', '~', 2), ('^', '!', 2)].
    // `1^^A!B!~~~~C` -> [1, payload A!B!, C]
    let pairs = [
        pair(b"^", b"~~", 2),
        pair(b"^", b"~", 2),
        pair(b"^", b"!", 2),
    ];
    check(
        V2,
        b"1^^A!B!~~~~C",
        &pairs,
        vec![
            Seg::Content("1".into()),
            Seg::Payload("A!B!".into()),
            Seg::ContentPending("C".into()),
        ],
    );
}

// ------------------------------------------------------------------ basics

#[test]
fn empty_buffer_and_potential_limiter_slice() {
    let pairs = default_pairs();
    for v in [V1, V2] {
        assert_eq!(run(v, b"", &pairs), vec![]);
        // `s[..]` MUST be a limiter slice, repeats not well defined yet.
        assert_eq!(run(v, b"^", &pairs), vec![]);
        assert_eq!(run(v, b"^^^", &pairs), vec![]);
    }
}

#[test]
fn non_envelop_only() {
    check(
        V1,
        b"hello",
        &default_pairs(),
        vec![Seg::ContentPending("hello".into())],
    );
    // A lone limiter repeat (`< least_repeat`) is plain content...
    check(
        V1,
        b"a^b",
        &default_pairs(),
        vec![Seg::ContentPending("a^b".into())],
    );
    // ...but a potential limiter slice is held back from the tail.
    check(
        V1,
        b"ab^",
        &default_pairs(),
        vec![Seg::ContentPending("ab".into())],
    );
}

#[test]
fn empty_envelop_is_not_non_envelop() {
    let pairs = default_pairs();
    // `^^~~`: archived Envelop with empty payload; both payload offsets None.
    for (v, s, expected) in [
        // Variant1: no delimiter slice confirmation latency.
        (V1, &b"^^~~"[..], vec![Seg::EmptyPayload]),
        // Variant2: waits for a non-delimiter following the delimiter slice.
        (V2, &b"^^~~"[..], vec![Seg::PayloadPending(String::new())]),
        (
            V2,
            &b"^^~~x"[..],
            vec![Seg::EmptyPayload, Seg::ContentPending("x".into())],
        ),
    ] {
        check(v, s, &pairs, expected.clone());
        if expected == [Seg::EmptyPayload] {
            let st = unsafe { parse(v, s, &pairs) };
            assert_eq!(
                st,
                State::Y(Envelop {
                    head_offset: 0,
                    payload_head_offset: None,
                    payload_tail_offset: None,
                    tail_offset: NonZeroUsize::new(4),
                })
            );
        }
    }
}

#[test]
fn pending_envelop_offsets() {
    let pairs = default_pairs();
    // Limiter slice well defined, payload streaming.
    let st = unsafe { parse(V1, b"^^ab", &pairs) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 0,
            payload_head_offset: NonZeroUsize::new(2),
            payload_tail_offset: NonZeroUsize::new(4),
            tail_offset: None,
        })
    );
    // A potential delimiter slice is held back from the committed payload.
    let st = unsafe { parse(V1, b"^^a~", &pairs) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 0,
            payload_head_offset: NonZeroUsize::new(2),
            payload_tail_offset: NonZeroUsize::new(3),
            tail_offset: None,
        })
    );
    // The whole tail is a potential delimiter slice: nothing committed yet.
    let st = unsafe { parse(V1, b"^^~", &pairs) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 0,
            payload_head_offset: NonZeroUsize::new(2),
            payload_tail_offset: None,
            tail_offset: None,
        })
    );
    // The envelope archives in place once the delimiter slice completes.
    let st = unsafe { parse_incremental(V1, b"^^a~~", &pairs, &st) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 0,
            payload_head_offset: NonZeroUsize::new(2),
            payload_tail_offset: NonZeroUsize::new(3),
            tail_offset: NonZeroUsize::new(5),
        })
    );
}

#[test]
fn utf8_multibyte_limiter_pair() {
    // Best practice for u8 buffers: reserve UTF-8 only chars; ('♦', '♣', 1).
    let pairs = [pair("♦".as_bytes(), "♣".as_bytes(), 1)];
    let s = "x♦♦hi♣♣y".as_bytes();
    check(
        V1,
        s,
        &pairs,
        vec![
            Seg::Content("x".into()),
            Seg::Payload("hi".into()),
            Seg::ContentPending("y".into()),
        ],
    );
}

#[test]
fn verbatim_payload() {
    // The payload decodes byte-identical to what was written, escapes and all.
    let payload = b"r#\"this is A\nrust raw string\n\"# go go go\\\\r\\n\t1.!";
    let mut s = b"^^".to_vec();
    s.extend_from_slice(payload);
    s.extend_from_slice(b"~~");
    check(
        V1,
        &s,
        &default_pairs(),
        vec![Seg::Payload(String::from_utf8(payload.to_vec()).unwrap())],
    );
}

// ------------------------------------------------------------------ offset

#[test]
fn offset_shifts_states() {
    let mut e = Envelop {
        head_offset: 6,
        payload_head_offset: NonZeroUsize::new(8),
        payload_tail_offset: NonZeroUsize::new(9),
        tail_offset: NonZeroUsize::new(11),
    };
    unsafe { e.offset(6) };
    assert_eq!(
        e,
        Envelop {
            head_offset: 0,
            payload_head_offset: NonZeroUsize::new(2),
            payload_tail_offset: NonZeroUsize::new(3),
            tail_offset: NonZeroUsize::new(5),
        }
    );

    let mut n = NonEnvelop {
        head_offset: 8,
        tail_offset: 11,
        ok: true,
    };
    unsafe { n.offset(6) };
    assert_eq!(
        n,
        NonEnvelop {
            head_offset: 2,
            tail_offset: 5,
            ok: true,
        }
    );

    // Empty holds no offsets.
    let mut st = State::Empty;
    unsafe { st.offset(6) };
    assert_eq!(st, State::Empty);
}

#[test]
fn offset_supports_buffer_compaction() {
    // Parse, archive, drop the archived prefix, shift the pending state, and
    // keep streaming on the compacted buffer.
    let pairs = default_pairs();
    let mut s = b"0%%A%%xy".to_vec();

    // `0` content (ok), `%%A%%` envelop, `xy` pending content.
    let st = unsafe { parse(V1, &s, &pairs) };
    assert!(st.is_archived());
    let st = unsafe { parse_incremental(V1, &s, &pairs, &st) };
    assert!(st.is_archived());
    let mut st = unsafe { parse_incremental(V1, &s, &pairs, &st) };
    assert_eq!(
        st,
        State::N(NonEnvelop {
            head_offset: 6,
            tail_offset: 8,
            ok: false,
        })
    );

    // Compact the buffer: drop the archived prefix and shift the state.
    s.drain(..6);
    unsafe { st.offset(6) };
    assert_eq!(
        st,
        State::N(NonEnvelop {
            head_offset: 0,
            tail_offset: 2,
            ok: false,
        })
    );

    // Keep streaming on the compacted buffer.
    s.extend_from_slice(b"^^z~~");
    let st = unsafe { parse_incremental(V1, &s, &pairs, &st) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 2,
            payload_head_offset: NonZeroUsize::new(4),
            payload_tail_offset: NonZeroUsize::new(5),
            tail_offset: NonZeroUsize::new(7),
        })
    );
}

#[test]
fn offset_supports_pending_envelop_compaction() {
    // Compacting with a *pending Envelop* exercises `re_derive_limiter` on
    // shifted offsets — the most delicate resume path.
    let pairs = default_pairs();
    let mut s = b"0%%A%".to_vec();

    let st = unsafe { parse(V1, &s, &pairs) }; // `0` content (ok)
    let mut st = unsafe { parse_incremental(V1, &s, &pairs, &st) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 1,
            payload_head_offset: NonZeroUsize::new(3),
            payload_tail_offset: NonZeroUsize::new(4),
            tail_offset: None,
        })
    );

    // Drop the archived `0` and shift the pending Envelop.
    s.drain(..1);
    unsafe { st.offset(1) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 0,
            payload_head_offset: NonZeroUsize::new(2),
            payload_tail_offset: NonZeroUsize::new(3),
            tail_offset: None,
        })
    );

    // Keep streaming: the limiter slice re-derives on the compacted buffer.
    s.extend_from_slice(b"%");
    let st = unsafe { parse_incremental(V1, &s, &pairs, &st) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 0,
            payload_head_offset: NonZeroUsize::new(2),
            payload_tail_offset: NonZeroUsize::new(3),
            tail_offset: NonZeroUsize::new(5),
        })
    );
}

// ------------------------------------------------------------- regressions

#[test]
fn held_back_potential_limiter_dies() {
    // A held-back potential limiter slice (`^`) dies when `x` arrives; the
    // confirmed NonEnvelop extends up to the next limiter slice (`^^`) —
    // beyond the last committed tail.
    let pairs = [pair(b"^", b"~", 2)];
    check(
        V1,
        b"abc^x^^y~~",
        &pairs,
        vec![Seg::Content("abc^x".into()), Seg::Payload("y".into())],
    );
    check(
        V2,
        b"abc^x^^y~~z",
        &pairs,
        vec![
            Seg::Content("abc^x".into()),
            Seg::Payload("y".into()),
            Seg::ContentPending("z".into()),
        ],
    );
}

#[test]
fn partial_multibyte_limiter_and_delimiter() {
    let pairs = [pair(b"ab", b"cd", 1)];
    // A partial multi-byte limiter at buffer end is a potential limiter slice.
    check(V1, b"xa", &pairs, vec![Seg::ContentPending("x".into())]);
    // 1-byte chunks exercise every partial-limiter/partial-delimiter boundary.
    check(
        V1,
        b"xabcdy",
        &pairs,
        vec![
            Seg::Content("x".into()),
            Seg::EmptyPayload,
            Seg::ContentPending("y".into()),
        ],
    );
}

#[test]
fn least_repeat_three() {
    let pairs = [pair(b"^", b"~", 3)];
    // `^^` < least_repeat: plain content.
    check(V1, b"^^a", &pairs, vec![Seg::ContentPending("^^a".into())]);
    check(
        V1,
        b"0^^^a~~~1",
        &pairs,
        vec![
            Seg::Content("0".into()),
            Seg::Payload("a".into()),
            Seg::ContentPending("1".into()),
        ],
    );
    // A run longer than least_repeat: the delimiter repeats match the run.
    check(V1, b"^^^^a~~~~", &pairs, vec![Seg::Payload("a".into())]);
}

#[test]
fn overlapping_limiter_run() {
    // Overlapping repeats are not stacked: `^^^~` scans as plain content.
    let pairs = [pair(b"^^", b"~~", 2)];
    check(
        V1,
        b"x^^^~y",
        &pairs,
        vec![Seg::ContentPending("x^^^~y".into())],
    );
    // `^^^^` is a limiter slice of 2 repeats; the delimiter slice is then
    // `~~` repeated the same 2 times.
    check(
        V1,
        b"x^^^^~~~~y",
        &pairs,
        vec![
            Seg::Content("x".into()),
            Seg::EmptyPayload,
            Seg::ContentPending("y".into()),
        ],
    );
    check(
        V1,
        b"x^^^^~~y",
        &pairs,
        vec![Seg::Content("x".into()), Seg::PayloadPending("~~y".into())],
    );
}

#[test]
fn degenerate_limiter_pairs() {
    // Empty limiter never matches.
    let pairs = [pair(b"", b"~", 1)];
    check(V1, b"a^b", &pairs, vec![Seg::ContentPending("a^b".into())]);
    // Empty delimiter: an Envelop archives right behind its limiter slice.
    let pairs = [pair(b"^", b"", 1)];
    check(
        V1,
        b"^x",
        &pairs,
        vec![Seg::EmptyPayload, Seg::ContentPending("x".into())],
    );
    // No pairs: everything is content.
    check(V1, b"a^b", &[], vec![Seg::ContentPending("a^b".into())]);
    // Duplicate pairs: the first one wins.
    let pairs = [pair(b"^", b"~", 1), pair(b"^", b"~", 1)];
    check(V1, b"^a~", &pairs, vec![Seg::Payload("a".into())]);
}

#[test]
fn non_u8_unit() {
    // `T` is generic; smoke-test with `char` units.
    let pairs = [LimiterPair {
        least_repeat: NonZeroUsize::new(1).unwrap(),
        limiter: &['\u{2666}'][..],
        delimiter: &['\u{2663}'][..],
    }];
    let s: Vec<char> = "x\u{2666}\u{2666}hi\u{2663}\u{2663}y".chars().collect();
    let st = unsafe { parse(V1, &s, &pairs) };
    assert_eq!(
        st,
        State::N(NonEnvelop {
            head_offset: 0,
            tail_offset: 1,
            ok: true,
        })
    );
    let st = unsafe { parse_incremental(V1, &s, &pairs, &st) };
    assert_eq!(
        st,
        State::Y(Envelop {
            head_offset: 1,
            payload_head_offset: NonZeroUsize::new(3),
            payload_tail_offset: NonZeroUsize::new(5),
            tail_offset: NonZeroUsize::new(7),
        })
    );
    let e = match st {
        State::Y(e) => e,
        _ => unreachable!(),
    };
    assert_eq!(e.payload_range(), Some(3..5));
}

// --------------------------------------------------------------------- fuzz

struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

#[test]
fn fuzz_drivers_agree() {
    const CASES: usize = 400;
    let mut rng = Lcg(0x5DA0_E5DA_0E5D_A0E5);
    for case in 0..CASES {
        // Random pair set: 1..=3 pairs over a tiny alphabet (collisions
        // encouraged), limiter/delimiter of length 1..=3, least_repeat 1..=3.
        let alphabet: &[u8] = b"^~%";
        let n_pairs = 1 + rng.below(3);
        let mut storage: Vec<Vec<u8>> = Vec::new();
        let mut leasts = vec![];
        for _ in 0..n_pairs {
            for _ in 0..2 {
                let len = 1 + rng.below(3);
                storage.push(
                    (0..len)
                        .map(|_| alphabet[rng.below(alphabet.len())])
                        .collect(),
                );
            }
            leasts.push(1 + rng.below(3));
        }
        let pairs: Vec<LimiterPair<u8>> = (0..n_pairs)
            .map(|i| LimiterPair {
                least_repeat: NonZeroUsize::new(leasts[i]).unwrap(),
                limiter: &storage[2 * i],
                delimiter: &storage[2 * i + 1],
            })
            .collect();
        // Random buffer.
        let buf_alphabet: &[u8] = b"^~%ab01";
        let len = rng.below(41);
        let s: Vec<u8> = (0..len)
            .map(|_| buf_alphabet[rng.below(buf_alphabet.len())])
            .collect();
        let v = if case % 2 == 0 { V1 } else { V2 };
        let expected = run(v, &s, &pairs);
        for chunk in [1, 2, 3, 7, len.max(1)] {
            let ctx = format!(
                "case {case} {v:?} chunk {chunk} s={:?} pairs={pairs:?}",
                String::from_utf8_lossy(&s)
            );
            assert_eq!(
                run_incremental(v, &s, &pairs, chunk),
                expected,
                "incremental: {ctx}"
            );
            assert_eq!(
                run_compaction(v, &s, &pairs, chunk),
                expected,
                "compaction: {ctx}"
            );
        }
    }
}

// ---------------------------------------------------------- *_unchecked fns

/// Like `check`, but additionally asserts the `_unchecked` fns yield the
/// same segments (the pair set MUST satisfy the unchecked contract).
fn check_unchecked(v: Variant, s: &[u8], pairs: &[LimiterPair<u8>], expected: Vec<Seg>) {
    assert!(
        limiter_pairs_recommended(pairs),
        "test pair set MUST satisfy the unchecked contract: {pairs:?}"
    );
    check(v, s, pairs, expected.clone());
    assert_eq!(
        run_uc(v, s, pairs, true),
        expected,
        "unchecked batch parse of {:?}",
        String::from_utf8_lossy(s)
    );
    for chunk in [1, 2, 3, 4, s.len().max(1)] {
        assert_eq!(
            run_incremental_uc(v, s, pairs, chunk, true),
            expected,
            "unchecked incremental parse of {:?} with chunk size {chunk}",
            String::from_utf8_lossy(s)
        );
        assert_eq!(
            run_compaction_uc(v, s, pairs, chunk, true),
            expected,
            "unchecked compacting parse of {:?} with chunk size {chunk}",
            String::from_utf8_lossy(s)
        );
    }
}

/// `[('^', '~', 2), ('?', '!', 2), ('{', '}', 2)]` — satisfies the
/// unchecked contract.
fn recommended_pairs() -> [LimiterPair<'static, u8>; 3] {
    [
        pair(b"^", b"~", 2),
        pair(b"?", b"!", 2),
        pair(b"{", b"}", 2),
    ]
}

#[test]
fn recommended_limiter_pair_sets() {
    // The default set contains ('%', '%', 2): limiter == delimiter.
    assert!(!limiter_pairs_recommended(&default_pairs()));
    assert!(limiter_pairs_recommended(&recommended_pairs()));
    // Single pair; self-periodic (de)limiters are allowed (only relations
    // between distinct (de)limiters are forbidden).
    assert!(limiter_pairs_recommended(&[pair(b"^^", b"~~", 2)]));
    assert!(limiter_pairs_recommended(&[pair(b"ab", b"cd", 1)]));
    assert!(limiter_pairs_recommended(&[pair(
        " ".as_bytes(),
        "".as_bytes(),
        1
    )]));
    // Shared limiter.
    assert!(!limiter_pairs_recommended(&[
        pair(b"^", b"~", 1),
        pair(b"^", b"!", 1)
    ]));
    // Shared delimiter.
    assert!(!limiter_pairs_recommended(&[
        pair(b"^", b"~", 1),
        pair(b"?", b"~", 1)
    ]));
    // limiter == another pair's delimiter.
    assert!(!limiter_pairs_recommended(&[
        pair(b"^", b"~", 1),
        pair(b"~", b"%", 1)
    ]));
    // limiter == delimiter within a pair.
    assert!(!limiter_pairs_recommended(&[pair(b"%", b"%", 1)]));
    // A (de)limiter being a sequence of another (de)limiter.
    assert!(!limiter_pairs_recommended(&[
        pair(b"^^", b"~", 1),
        pair(b"^", b"!", 1)
    ]));
    assert!(!limiter_pairs_recommended(&[
        pair(b"^", b"~~", 1),
        pair(b"?", b"~", 1)
    ]));
    // limiter being a sequence of its own delimiter (README.md L50).
    assert!(!limiter_pairs_recommended(&[pair(b"~~", b"~", 1)]));
    // Empty (de)limiter.
    assert!(!limiter_pairs_recommended(&[pair(b"", b"~", 1)]));
    assert!(!limiter_pairs_recommended(&[pair(b"^", b"", 1)]));
    // No pairs at all: trivially recommended.
    assert!(limiter_pairs_recommended::<u8>(&[]));
}

#[test]
fn recommended_limiter_pair_sets_interference() {
    // Regression for review finding D1: these sets pass the repetition
    // checks but let limiters interfere, so the unchecked shortcuts would
    // diverge from the checked paths.
    // Prefix-comparable limiters (first-hit-owns / re-derive ambiguity).
    assert!(!limiter_pairs_recommended(&[
        pair(b"ab", b"~", 2),
        pair(b"a", b"!", 1)
    ]));
    assert!(!limiter_pairs_recommended(&[
        pair(b"aa", b"~", 2),
        pair(b"aab", b"!", 1)
    ]));
    // A limiter occurring inside another limiter's rotation.
    assert!(!limiter_pairs_recommended(&[
        pair(b"ab", b"~", 2),
        pair(b"b", b"!", 1)
    ]));
    assert!(!limiter_pairs_recommended(&[
        pair(b"ab", b"~", 2),
        pair(b"bx", b"!", 1)
    ]));
    // Self-bordering limiter: `bab` occurs at offset 2 of `bab`+`bab...`.
    assert!(!limiter_pairs_recommended(&[pair(b"bab", b"!", 2)]));
    // ...but pure self-repetition / periodic self-overlap stays harmless...
    assert!(limiter_pairs_recommended(&[pair(b"^^", b"~~", 2)]));
    assert!(limiter_pairs_recommended(&[pair(b"abab", b"~", 2)]));
    // ...and rotation-clean multi-char sets stay recommended.
    assert!(limiter_pairs_recommended(&[
        pair(b"ab", b"cd", 2),
        pair(b"ef", b"gh", 2)
    ]));
    assert!(limiter_pairs_recommended(&[
        pair(b"ab", b"!", 2),
        pair(b"acb", b"~", 1)
    ]));
}

#[test]
fn prefix_comparable_limiters_defer_confirmation() {
    // Regression for review finding D2: an earlier pair's partial limiter at
    // buffer end defers a later pair's complete limiter slice, so streaming
    // never commits to a pair the batch parse would not choose.
    let pairs = [pair(b"baa", b"!", 1), pair(b"b", b"~", 1)];
    check(V1, b"baaX!", &pairs, vec![Seg::Payload("X".into())]);
    check(V1, b"baxb~", &pairs, vec![Seg::Payload("axb".into())]);
    // `ba` alone: a potential limiter slice of the earlier pair.
    check(V1, b"ba", &pairs, vec![]);
    check(V1, b"b", &pairs, vec![]);
    // The later pair still wins once the earlier pair's partial dies.
    check(V1, b"baxb~", &pairs, vec![Seg::Payload("axb".into())]);
}

#[test]
fn unchecked_matches_checked_on_recommended_sets() {
    let pairs = recommended_pairs();
    check_unchecked(
        V1,
        b"0^^~~^^B~~{{C}}1",
        &pairs,
        vec![
            Seg::Content("0".into()),
            Seg::EmptyPayload,
            Seg::Payload("B".into()),
            Seg::Payload("C".into()),
            Seg::ContentPending("1".into()),
        ],
    );
    check_unchecked(
        V1,
        b"1^^^echo^^hello~~~~~2",
        &pairs,
        vec![
            Seg::Content("1".into()),
            Seg::Payload("echo^^hello".into()),
            Seg::ContentPending("~~2".into()),
        ],
    );
    check_unchecked(
        V2,
        b"0{{A}}{{B}}??C!!1",
        &pairs,
        vec![
            Seg::Content("0".into()),
            Seg::Payload("A".into()),
            Seg::Payload("B".into()),
            Seg::Payload("C".into()),
            Seg::ContentPending("1".into()),
        ],
    );
    check_unchecked(
        V2,
        b"0{{A}}{{B}}??C!!!",
        &pairs,
        vec![
            Seg::Content("0".into()),
            Seg::Payload("A".into()),
            Seg::Payload("B".into()),
            Seg::PayloadPending("C!".into()),
        ],
    );
    check_unchecked(
        V2,
        b"1^^^echo^^hello~~~~~2",
        &pairs,
        vec![
            Seg::Content("1".into()),
            Seg::Payload("echo^^hello~~".into()),
            Seg::ContentPending("2".into()),
        ],
    );
    check_unchecked(
        V2,
        b"1^^^echo^^hello~~~~~",
        &pairs,
        vec![
            Seg::Content("1".into()),
            Seg::PayloadPending("echo^^hello~~".into()),
        ],
    );
    // Short runs below `least_repeat` (the skipped-wholesale path) mixed
    // with potential slices dying and reviving.
    check_unchecked(
        V1,
        b"a^b^c^{{d}}e",
        &pairs,
        vec![
            Seg::Content("a^b^c^".into()),
            Seg::Payload("d".into()),
            Seg::ContentPending("e".into()),
        ],
    );
    // Single-pair sets, including self-periodic (de)limiters.
    check_unchecked(
        V1,
        b"abc^x^^y~~",
        &[pair(b"^", b"~", 2)],
        vec![Seg::Content("abc^x".into()), Seg::Payload("y".into())],
    );
    check_unchecked(
        V1,
        b"x^^^^~~~~y",
        &[pair(b"^^", b"~~", 2)],
        vec![
            Seg::Content("x".into()),
            Seg::EmptyPayload,
            Seg::ContentPending("y".into()),
        ],
    );
    check_unchecked(
        V1,
        b"xabcdy",
        &[pair(b"ab", b"cd", 1)],
        vec![
            Seg::Content("x".into()),
            Seg::EmptyPayload,
            Seg::ContentPending("y".into()),
        ],
    );
}

#[test]
fn checked_unchecked_state_interop() {
    // States produced by the checked and unchecked fns are interoperable.
    let pairs = recommended_pairs();
    let mut s = b"0^^a".to_vec();
    let st = xparse(false, V1, &s, &pairs); // `0` content (ok)
    let st_checked = xparse_inc(false, V1, &s, &pairs, &st);
    let st_unchecked = xparse_inc(true, V1, &s, &pairs, &st);
    assert_eq!(st_checked, st_unchecked);
    // Grow the buffer; continue each state with the other mode.
    s.extend_from_slice(b"~~");
    let a = xparse_inc(true, V1, &s, &pairs, &st_checked);
    let b = xparse_inc(false, V1, &s, &pairs, &st_unchecked);
    assert_eq!(a, b);
    assert!(a.is_archived());
}

#[test]
fn fuzz_unchecked_matches_checked_multichar() {
    // Like `fuzz_unchecked_matches_checked`, but with multi-char pair sets
    // filtered through `limiter_pairs_recommended`, exercising the
    // first-hit-owns and short-run-skip paths for real.
    const CASES: usize = 400;
    let mut rng = Lcg(0xBEEF_5DA0_E5DA_0001);
    let limiter_alphabet: &[u8] = b"ab";
    let delimiter_alphabet: &[u8] = b"!~";
    let mut accepted = 0;
    while accepted < CASES {
        let n_pairs = 1 + rng.below(2);
        let mut storage: Vec<Vec<u8>> = Vec::new();
        let mut leasts = vec![];
        for _ in 0..n_pairs {
            let ll = 1 + rng.below(3);
            storage.push(
                (0..ll)
                    .map(|_| limiter_alphabet[rng.below(limiter_alphabet.len())])
                    .collect(),
            );
            let dl = 1 + rng.below(2);
            storage.push(
                (0..dl)
                    .map(|_| delimiter_alphabet[rng.below(delimiter_alphabet.len())])
                    .collect(),
            );
            leasts.push(1 + rng.below(3));
        }
        let pairs: Vec<LimiterPair<u8>> = (0..n_pairs)
            .map(|i| LimiterPair {
                least_repeat: NonZeroUsize::new(leasts[i]).unwrap(),
                limiter: &storage[2 * i],
                delimiter: &storage[2 * i + 1],
            })
            .collect();
        if !limiter_pairs_recommended(&pairs) {
            continue;
        }
        accepted += 1;
        let buf_alphabet: &[u8] = b"ab!~xy01";
        let len = rng.below(61);
        let s: Vec<u8> = (0..len)
            .map(|_| buf_alphabet[rng.below(buf_alphabet.len())])
            .collect();
        let v = if accepted % 2 == 0 { V1 } else { V2 };
        let expected = run(v, &s, &pairs);
        let ctx = format!(
            "case {accepted} {v:?} s={:?} pairs={pairs:?}",
            String::from_utf8_lossy(&s)
        );
        assert_eq!(
            run_uc(v, &s, &pairs, true),
            expected,
            "unchecked batch: {ctx}"
        );
        for chunk in [1, 3, len.max(1)] {
            assert_eq!(
                run_incremental_uc(v, &s, &pairs, chunk, true),
                expected,
                "unchecked incremental chunk {chunk}: {ctx}"
            );
            assert_eq!(
                run_compaction_uc(v, &s, &pairs, chunk, true),
                expected,
                "unchecked compaction chunk {chunk}: {ctx}"
            );
        }
    }
}

#[test]
fn fuzz_unchecked_matches_checked() {
    const CASES: usize = 400;
    let mut rng = Lcg(0xC0FF_EEC0_FFEE_C0FF);
    let limiter_chars: &[u8] = b"^?{([<";
    let delimiter_chars: &[u8] = b"~!})/>";
    for case in 0..CASES {
        // Single-char pair sets with distinct limiter/delimiter chars always
        // satisfy the unchecked contract.
        let n_pairs = 1 + rng.below(3);
        let mut ls = limiter_chars.to_vec();
        let mut ds = delimiter_chars.to_vec();
        for i in 0..ls.len() {
            let j = i + rng.below(ls.len() - i);
            ls.swap(i, j);
        }
        for i in 0..ds.len() {
            let j = i + rng.below(ds.len() - i);
            ds.swap(i, j);
        }
        let pairs: Vec<LimiterPair<u8>> = (0..n_pairs)
            .map(|i| LimiterPair {
                least_repeat: NonZeroUsize::new(1 + rng.below(3)).unwrap(),
                limiter: &ls[i..i + 1],
                delimiter: &ds[i..i + 1],
            })
            .collect();
        assert!(limiter_pairs_recommended(&pairs));
        let buf_alphabet: &[u8] = b"^?{~!}ab01";
        let len = rng.below(61);
        let s: Vec<u8> = (0..len)
            .map(|_| buf_alphabet[rng.below(buf_alphabet.len())])
            .collect();
        let v = if case % 2 == 0 { V1 } else { V2 };
        let expected = run(v, &s, &pairs);
        let ctx = format!(
            "case {case} {v:?} s={:?} pairs={pairs:?}",
            String::from_utf8_lossy(&s)
        );
        assert_eq!(
            run_uc(v, &s, &pairs, true),
            expected,
            "unchecked batch: {ctx}"
        );
        for chunk in [1, 3, len.max(1)] {
            assert_eq!(
                run_incremental_uc(v, &s, &pairs, chunk, true),
                expected,
                "unchecked incremental chunk {chunk}: {ctx}"
            );
            assert_eq!(
                run_compaction_uc(v, &s, &pairs, chunk, true),
                expected,
                "unchecked compaction chunk {chunk}: {ctx}"
            );
        }
    }
}
