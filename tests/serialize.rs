//! serializing side of the public interface: can_serialize,
//! get_all_serializable_limiter_pairs, and full write→parse round-trips.

mod common;

use common::*;
use sdave::*;

/// frame `payload` onto `buf` using `pair` with `k` repeats.
fn frame(buf: &mut Vec<u8>, pair: &LimiterPair<u8>, k: usize, payload: &[u8]) {
    for _ in 0..k {
        buf.extend_from_slice(&pair.limiter);
    }
    buf.extend_from_slice(payload);
    for _ in 0..k {
        buf.extend_from_slice(&pair.delimiter);
    }
}

#[test]
fn can_serialize_v1() {
    let percent = pair(b"%", b"%", 2);
    // simplest frame
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"A", b"1", &percent) },
        Some(nz(2))
    );
    // a delimiter run inside the payload forces a longer repeat
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"A%%B", b"1", &percent) },
        Some(nz(3))
    );
    // V1: payload tail colliding with the delimiter unit disables the pair entirely
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"A%", b"1", &percent) },
        None
    );
    // least_repeat raises the floor
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"A", b"1", &pair(b"%", b"%", 4)) },
        Some(nz(4))
    );
}

#[test]
fn can_serialize_v2() {
    let percent = pair(b"%", b"%", 2);
    assert_eq!(
        unsafe { can_serialize(Variant::V2, b"0", off(1), b"A", b"1", &percent) },
        Some(nz(2))
    );
    // V2 tolerates a payload tail colliding with the delimiter unit...
    assert_eq!(
        unsafe { can_serialize(Variant::V2, b"0", off(1), b"A%", b"1", &percent) },
        Some(nz(2))
    );
    // ...but needs a non-delimiter following to confirm the delimiter slice
    assert_eq!(
        unsafe { can_serialize(Variant::V2, b"0", off(1), b"A", b"", &percent) },
        None
    );
    assert_eq!(
        unsafe { can_serialize(Variant::V2, b"0", off(1), b"A", b"%", &percent) },
        None
    );
}

#[test]
fn can_serialize_edge_cases() {
    // least_repeat None: reserved, never serializable
    let mut p = pair(b"^", b"~", 2);
    p.least_repeat = None;
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"A", b"1", &p) },
        None
    );
    let caret = pair(b"^", b"~", 2);
    // payload starting with the limiter unit can never be framed exactly
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"^A", b"1", &caret) },
        None
    );
    // empty payload is fine when limiter != delimiter
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"", b"1", &caret) },
        Some(nz(2))
    );
    // ...but impossible when limiter == delimiter (it becomes one long limiter run)
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"", b"1", &pair(b"%", b"%", 2)) },
        None
    );
    // two contiguous V1 Envelopes can share the same limiter pair: tailing with an
    // archived delimiter slice is a clean restart boundary
    assert_eq!(
        unsafe {
            can_serialize(
                Variant::V1,
                b"0%%A%%",
                off(6),
                b"B",
                b"1",
                &pair(b"%", b"%", 2),
            )
        },
        Some(nz(2))
    );
}

#[test]
fn serialize_none_means_broken() {
    // a payload starting with the limiter unit is refused...
    let caret = pair(b"^", b"~", 2);
    assert_eq!(
        unsafe { can_serialize(Variant::V1, b"0", off(1), b"^A", b"1", &caret) },
        None
    );
    // ...and naive framing indeed misparses: the limiter run merges with the payload
    // head, the delimiter count no longer matches, and the Envelop never archives.
    let items = collect_buf(Variant::V1, b"0^^^A~~1", &std_pairs());
    assert_eq!(
        items,
        vec![Item::PhantomE, Item::Ne(0, 1), Item::Env(1, 4, 8, None)]
    );
}

#[test]
fn get_all_serializable_limiter_pairs_picks_minimal_repeats() {
    let pairs = vec![pair(b"^", b"~", 2), pair(b"%", b"%", 2)];
    let got = unsafe {
        get_all_serializable_limiter_pairs(Variant::V1, b"0", off(1), b"A~~B", b"1", &pairs)
    };
    assert_eq!(got.len(), 2);
    // `A~~B` collides with ('^','~',2) at repeat 2, so repeat 3 is required
    assert_eq!(got[0].least_repeat, Some(nz(3)));
    assert_eq!(got[1].least_repeat, Some(nz(2)));

    // nothing serializable -> empty vec
    let got = unsafe {
        get_all_serializable_limiter_pairs(Variant::V1, b"0", off(1), b"A%", b"1", &pairs)
    };
    // ('%','%',2) is disabled by the payload tail under V1; ('^','~',2) still works
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].limiter, b"^");
}

#[test]
fn round_trip_v1_contiguous_envelopes() {
    let pairs = std_pairs();
    let percent = pair(b"%", b"%", 2);
    let caret = pair(b"^", b"~", 2);

    let mut buf: Vec<u8> = b"0".to_vec();
    let mut frames: Vec<(Vec<u8>, LimiterPair<u8>, usize)> = Vec::new();
    // note: the first two frames share one limiter pair back to back
    for (payload, pair) in [
        (b"A".as_slice(), &percent),
        (b"B%%C".as_slice(), &percent),
        (b"".as_slice(), &caret),
    ] {
        // SAFETY: buf ends at the tail of an archived item (or is just `0`), so
        // buf.len() is a clean item boundary.
        let k = unsafe { can_serialize(Variant::V1, &buf, off(buf.len()), payload, b"", pair) }
            .expect("payload should be serializable");
        frame(&mut buf, pair, k.get(), payload);
        frames.push((payload.to_vec(), pair.clone(), k.get()));
    }
    buf.push(b'1');
    assert_eq!(buf, b"0%%A%%%%%B%%C%%%^^~~1");

    let mut parser = FlatParser::new(&buf, Variant::V1, pairs);
    parser.parse_incremental();
    let envs: Vec<Envelop> = parser
        .iter()
        .filter_map(|s| match s {
            State::E { envelop } if !envelop.is_phantom() => Some(envelop),
            _ => None,
        })
        .collect();
    assert_eq!(envs.len(), 3);
    for (env, (payload, pair, k)) in envs.iter().zip(frames.iter()) {
        // the payload is framed verbatim
        assert_eq!(
            &buf[env.payload_head_offset().get()..env.payload_tail_offset().get()],
            payload
        );
        // the repeat count round-trips
        assert_eq!(env.repeat(pair.limiter.len()), *k);
        // the limiter/delimiter slices have the same repeat count
        let limiter_len = env.payload_head_offset().get() - env.head_offset().get();
        let delimiter_len = env.tail_offset().unwrap().get() - env.payload_tail_offset().get();
        assert_eq!(limiter_len, delimiter_len);
        assert!(env.tail_offset().is_some());
    }
}

#[test]
fn round_trip_v2_payload_tail_collision() {
    let pairs = std_pairs();
    let percent = pair(b"%", b"%", 2);
    let caret = pair(b"^", b"~", 2);

    let mut buf: Vec<u8> = b"0".to_vec();
    let mut frames: Vec<(Vec<u8>, usize)> = Vec::new();
    // V2 frames a payload whose tail collides with the delimiter unit, as long as a
    // non-delimiter follows (here: the next Envelop's limiter slice, finally `1`).
    for (payload, pair, following) in [
        (b"A%".as_slice(), &percent, b"^".as_slice()),
        (b"B".as_slice(), &caret, b"1".as_slice()),
    ] {
        // SAFETY: buf.len() is a clean item boundary.
        let k =
            unsafe { can_serialize(Variant::V2, &buf, off(buf.len()), payload, following, pair) }
                .expect("payload should be serializable");
        frame(&mut buf, pair, k.get(), payload);
        frames.push((payload.to_vec(), k.get()));
    }
    buf.push(b'1');
    assert_eq!(buf, b"0%%A%%%^^B~~1");

    let items = collect_buf(Variant::V2, &buf, &pairs);
    assert_eq!(
        items,
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 3, 5, Some(7)),
            Item::Ne(7, 7),
            Item::Env(7, 9, 10, Some(12)),
            Item::Ne(12, 13),
        ]
    );
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"A%");
    assert_eq!(&buf[envs[1].1..envs[1].2], b"B");
}
