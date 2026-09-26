//! lifecycle: incremental streaming, buffer advancing, decouple/new_unchecked
//! round-trips, and iterator entry points — all through the public interface.

mod common;

use common::*;
use sdave::*;

#[test]
fn streaming_append_only() {
    let pairs = std_pairs();

    // stage 1: a limiter slice and the first payload bytes arrive
    let buf1: &[u8] = b"0^^he";
    let mut parser = FlatParser::new(buf1, Variant::V1, pairs.clone());
    parser.parse_incremental();
    match parser.parser_state().state() {
        State::E { envelop } => {
            assert_eq!(envelop.head_offset().get(), 1);
            assert_eq!(envelop.payload_head_offset().get(), 3);
            assert_eq!(envelop.payload_tail_offset().get(), 5);
            assert_eq!(envelop.tail_offset(), None);
        }
        s => panic!("expected unarchived E, got {s:?}"),
    }
    assert_eq!(
        collect(&mut parser),
        vec![Item::PhantomE, Item::Ne(0, 1), Item::Env(1, 3, 5, None)]
    );

    // stage 2: the rest of the stream arrives; the Envelop archives
    let buf2: &[u8] = b"0^^hello~~1";
    // SAFETY: buf2 == buf1 ++ appended contents.
    unsafe { parser.replace_buffer(buf2) };
    assert_eq!(
        collect(&mut parser),
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 3, 8, Some(10)),
            Item::Ne(10, 11),
        ]
    );
}

#[test]
fn streaming_partial_limiter_slice() {
    let pairs = std_pairs();

    // a lone `^` at the tail is a potential limiter slice: parser waits
    let buf1: &[u8] = b"abc^";
    let mut parser = FlatParser::new(buf1, Variant::V1, pairs.clone());
    parser.parse_incremental();
    match parser.parser_state().state() {
        State::PartialLimiterSlice { head_offset } => assert_eq!(head_offset.get(), 3),
        s => panic!("expected PartialLimiterSlice, got {s:?}"),
    }
    // the NonEnvelop in front of it is already archived and iterable
    assert_eq!(
        collect(&mut parser),
        vec![Item::PhantomE, Item::Ne(0, 3)]
    );

    // the run completes into a limiter slice, then an Envelop archives
    let buf2: &[u8] = b"abc^^x~~";
    // SAFETY: buf2 == buf1 ++ appended contents.
    unsafe { parser.replace_buffer(buf2) };
    assert_eq!(
        collect(&mut parser),
        vec![
            Item::PhantomE,
            Item::Ne(0, 3),
            Item::Env(3, 5, 6, Some(8)),
            Item::Ne(8, 8),
        ]
    );
}

#[test]
fn advance_reclaims_consumed_prefix() {
    let pairs = std_pairs();

    let buf2: &[u8] = b"0^^hello~~1";
    let mut parser = FlatParser::new(buf2, Variant::V1, pairs.clone());
    parser.parse_incremental();

    // drop everything up to the end of the archived Envelop (offset 10)
    // SAFETY: the buffer is advanced by exactly 10.
    unsafe { parser.replace_buffer(&buf2[10..]) };
    let removed = unsafe { parser.advance(off(10)) };
    // removed: the leading NonEnvelop `0` and the Envelop
    assert_eq!(removed, 2);
    assert_eq!(parser.archived_boundaries().len(), 0);
    assert_eq!(parser.tail_non_envelop(), None);
    assert_eq!(collect(&mut parser), vec![Item::Ne(0, 1)]);

    // keep streaming on the advanced buffer
    let buf3: &[u8] = b"1^^x~~";
    // SAFETY: buf3 == advanced buffer ++ appended contents.
    unsafe { parser.replace_buffer(buf3) };
    assert_eq!(
        collect(&mut parser),
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 3, 4, Some(6)),
            Item::Ne(6, 6),
        ]
    );
}

#[test]
fn advance_mid_envelop_keeps_truncated_marker() {
    let pairs = std_pairs();

    let buf: &[u8] = b"0%%A%%1%%B%%2";
    let mut parser = FlatParser::new(buf, Variant::V1, pairs.clone());
    parser.parse_incremental();
    // items: PhantomE, Ne(0,1), E(1..6), Ne(6,7), E(7..12), Ne(12,13)

    // advance into the middle of the second Envelop
    // SAFETY: the buffer is advanced by exactly 8.
    unsafe { parser.replace_buffer(&buf[8..]) };
    let removed = unsafe { parser.advance(off(8)) };
    // removed: leading NonEnvelop, E(1..6), NonEnvelop(6,7)
    assert_eq!(removed, 3);

    // the straddling Envelop survives as a truncated marker with clamped offsets
    let items = collect(&mut parser);
    assert_eq!(items, vec![Item::Env(0, 1, 2, Some(4)), Item::Ne(4, 5)]);
    let first = &parser.archived_boundaries()[0];
    assert!(!first.is_phantom());
    assert_eq!(first.tail_offset(), Some(off(4)));
}

#[test]
fn decouple_and_rebuild() {
    let pairs = std_pairs();

    // parse a prefix, decouple, and rebuild on an extended buffer
    let buf1: &[u8] = b"0^^A";
    let mut parser = FlatParser::new(buf1, Variant::V1, pairs.clone());
    parser.parse_incremental();
    let (_, variant, pairs, boundaries, tail_ne, state) = parser.decouple();

    let buf2: &[u8] = b"0^^A~~1";
    // SAFETY: buf2 == buf1 ++ appended contents; boundaries/tail_ne/state all come from
    // decoupling a parser of buf1, untouched.
    let mut parser = unsafe { FlatParser::new_unchecked(buf2, variant, pairs, boundaries, tail_ne, state) };
    assert_eq!(
        collect(&mut parser),
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 3, 4, Some(6)),
            Item::Ne(6, 7),
        ]
    );
}

#[test]
fn decouple_roundtrip_is_transparent() {
    let pairs = std_pairs();
    let buf: &[u8] = b"0^^A~~1^^B~~2";
    let direct = collect_buf(Variant::V1, buf, &pairs);

    let mut parser = FlatParser::new(buf, Variant::V1, pairs.clone());
    parser.parse_incremental();
    let (buffer, variant, pairs, boundaries, tail_ne, state) = parser.decouple();
    // SAFETY: parts come straight from decouple, buffer untouched.
    let mut rebuilt =
        unsafe { FlatParser::new_unchecked(buffer, variant, pairs, boundaries, tail_ne, state) };
    assert_eq!(collect(&mut rebuilt), direct);
}

#[test]
fn iter_from_skips_cached_items() {
    let pairs = std_pairs();
    let buf: &[u8] = b"0^^A~~1^^B~~2";
    let mut parser = FlatParser::new(buf, Variant::V1, pairs);
    parser.parse_incremental();

    let from_start: Vec<Item> = parser.iter().map(|s| item_of(&s)).collect();
    // SAFETY: pos 2 is within the cached archived items.
    let from_two: Vec<Item> = unsafe { parser.iter_from(2) }.map(|s| item_of(&s)).collect();
    assert_eq!(from_two, &from_start[2..]);
    assert_eq!(
        from_two,
        vec![
            Item::Env(1, 3, 4, Some(6)),
            Item::Ne(6, 7),
            Item::Env(7, 9, 10, Some(12)),
            Item::Ne(12, 13),
        ]
    );
}
