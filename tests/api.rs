//! surface-level API contract tests: NonMaxUsize, accessors, FlatParserState,
//! the free parse_incremental function, and panic behavior.

mod common;

use common::*;
use sdave::*;
use std::num::NonZeroUsize;

#[test]
fn non_max_usize_roundtrip() {
    for x in [0, 1, 2, 42, usize::MAX - 1] {
        // SAFETY: x < usize::MAX
        let o = unsafe { NonMaxUsize::new(x) };
        assert_eq!(o.get(), x);
    }
}

#[test]
#[should_panic]
fn non_max_usize_rejects_max() {
    // SAFETY: none — this must panic.
    unsafe { NonMaxUsize::new(usize::MAX) };
}

#[test]
fn non_max_usize_offset_by() {
    // SAFETY: 7 < usize::MAX, 7 >= 4
    let mut o = unsafe { NonMaxUsize::new(7) };
    unsafe { o.offset_by(4) };
    assert_eq!(o.get(), 3);
}

#[test]
#[should_panic]
fn non_max_usize_offset_by_underflow() {
    // SAFETY: none for `new`; `offset_by` underflows and must panic.
    let mut o = unsafe { NonMaxUsize::new(3) };
    unsafe { o.offset_by(4) };
}

#[test]
#[should_panic]
fn flat_parser_rejects_empty_limiter_pairs() {
    let _ = FlatParser::new(b"buf".as_slice(), Variant::V1, vec![]);
}

#[test]
fn flat_parser_accessors() {
    let pairs = std_pairs();
    let buf: &[u8] = b"0^^A~~1";
    let mut parser = FlatParser::new(buf, Variant::V1, pairs.clone());
    assert_eq!(parser.buffer(), buf);
    assert_eq!(parser.variant(), Variant::V1);
    assert_eq!(parser.limiter_pairs().len(), 4);
    assert!(matches!(parser.parser_state().state(), State::Unknown));

    parser.parse_incremental();
    // [PhantomE, E] archived; trailing `1` is the live NonEnvelop
    assert_eq!(parser.archived_boundaries().len(), 2);
    assert!(parser.archived_boundaries()[0].is_phantom());
    assert!(!parser.archived_boundaries()[1].is_phantom());
    assert_eq!(parser.tail_non_envelop(), None);
    assert!(matches!(
        parser.parser_state().state(),
        State::NonEnvelop { .. }
    ));
}

#[test]
fn envelop_getters() {
    let buf: &[u8] = b"0^^^hello~~~~~1";
    let mut parser = FlatParser::new(buf, Variant::V1, std_pairs());
    parser.parse_incremental();
    let env = &parser.archived_boundaries()[1];
    assert_eq!(env.head_offset().get(), 1);
    assert_eq!(env.payload_head_offset().get(), 4);
    assert_eq!(env.payload_tail_offset().get(), 9);
    // V1 consumes exactly `repeat` delimiter units; the remaining `~~1` is NonEnvelop
    assert_eq!(env.tail_offset().map(|t| t.get()), Some(12));
    assert_eq!(&buf[4..9], b"hello");
    // repeat = limiter run length in units
    assert_eq!(env.repeat(1), 3);
}

#[test]
fn flat_parser_state_accessors() {
    let p = pair(b"^", b"~", 2);
    let st = FlatParserState::default(p.clone());
    assert!(matches!(st.state(), State::Unknown));
    assert_eq!(st.limiter().limiter, b"^");

    let st = FlatParserState::new(
        State::NonEnvelop {
            head_offset: off(2),
            tail_offset: off(5),
        },
        p,
    );
    match st.state() {
        State::NonEnvelop {
            head_offset,
            tail_offset,
        } => {
            assert_eq!(head_offset.get(), 2);
            assert_eq!(tail_offset.get(), 5);
        }
        s => panic!("expected NonEnvelop, got {s:?}"),
    }
}

#[test]
fn free_parse_incremental_stepwise() {
    let pairs = std_pairs();
    let buf: &[u8] = b"0^^A~~1";

    // start fresh with FlatParserState::default
    let st = FlatParserState::default(pairs[0].clone());
    // SAFETY: default state matches any buffer at offset 0.
    let st = unsafe { parse_incremental(Variant::V1, buf, &pairs, st) };
    // the leading NonEnvelop [0,1) is confirmed archived by the head change
    match st.state() {
        State::E { envelop } => {
            assert_eq!(envelop.head_offset().get(), 1);
            assert_eq!(envelop.tail_offset(), None);
        }
        s => panic!("expected unarchived E, got {s:?}"),
    }
    // SAFETY: passing back the last returned state for the same buffer.
    let st = unsafe { parse_incremental(Variant::V1, buf, &pairs, st) };
    match st.state() {
        State::E { envelop } => {
            assert_eq!(envelop.payload_tail_offset().get(), 4);
            assert_eq!(envelop.tail_offset().map(|t| t.get()), Some(6));
        }
        s => panic!("expected archived E, got {s:?}"),
    }
    // SAFETY: same as above.
    let st = unsafe { parse_incremental(Variant::V1, buf, &pairs, st) };
    match st.state() {
        State::NonEnvelop {
            head_offset,
            tail_offset,
        } => {
            assert_eq!(head_offset.get(), 6);
            assert_eq!(tail_offset.get(), 7);
        }
        s => panic!("expected NonEnvelop, got {s:?}"),
    }
    // converged: no more progress without more buffer
    // SAFETY: same as above.
    let st2 = unsafe { parse_incremental(Variant::V1, buf, &pairs, st.clone()) };
    assert_eq!(st2.state(), st.state());
}

#[test]
fn limiter_pair_fields_are_public() {
    let p = LimiterPair {
        least_repeat: NonZeroUsize::new(2),
        limiter: b"^".to_vec(),
        delimiter: b"~".to_vec(),
    };
    assert_eq!(p.least_repeat, NonZeroUsize::new(2));
    assert_eq!(p.limiter, b"^");
    assert_eq!(p.delimiter, b"~");
}
