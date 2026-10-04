//! README conformance: every worked example from the design doc, driven through the
//! public FlatParser interface (parse_incremental + iter).

mod common;

use common::*;
use sdave::Variant;

#[test]
fn v1_mixed_channel() {
    // README: `0%%A%%%%B%%%%C%%1` -> [0, payload A, payload B, payload C, 1]
    let buf = b"0%%A%%%%B%%%%C%%1";
    let items = collect_buf(Variant::V1, buf, &std_pairs());
    assert_eq!(
        items,
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 3, 4, Some(6)),
            Item::Ne(6, 6),
            Item::Env(6, 8, 9, Some(11)),
            Item::Ne(11, 11),
            Item::Env(11, 13, 14, Some(16)),
            Item::Ne(16, 17),
        ]
    );
    // payloads are framed verbatim
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"A");
    assert_eq!(&buf[envs[1].1..envs[1].2], b"B");
    assert_eq!(&buf[envs[2].1..envs[2].2], b"C");
}

#[test]
fn v1_trailing_potential_limiter_is_not_consumed() {
    // README: `0%%A%%%%B%%%%C%%1%` -> same as without the trailing `%`
    assert_eq!(
        collect_buf(Variant::V1, b"0%%A%%%%B%%%%C%%1%", &std_pairs()),
        collect_buf(Variant::V1, b"0%%A%%%%B%%%%C%%1", &std_pairs())
    );
}

#[test]
fn v1_long_run_pending() {
    // README: `0%%%%%%B%%%%C%%1` -> [0, pending B%%%%C%%1]
    let buf = b"0%%%%%%B%%%%C%%1";
    let items = collect_buf(Variant::V1, buf, &std_pairs());
    assert_eq!(
        items,
        vec![Item::PhantomE, Item::Ne(0, 1), Item::Env(1, 7, 16, None)]
    );
    // the pending payload region covers everything up to the buffer end
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"B%%%%C%%1");
}

#[test]
fn v1_empty_payload() {
    // README: `0^^~~^^B~~{{C}}1` -> [0, payload (empty), payload B, payload C, 1]
    let buf = b"0^^~~^^B~~{{C}}1";
    let items = collect_buf(Variant::V1, buf, &std_pairs());
    assert_eq!(
        items,
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 3, 3, Some(5)),
            Item::Ne(5, 5),
            Item::Env(5, 7, 8, Some(10)),
            Item::Ne(10, 10),
            Item::Env(10, 12, 13, Some(15)),
            Item::Ne(15, 16),
        ]
    );
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"");
    assert_eq!(&buf[envs[1].1..envs[1].2], b"B");
    assert_eq!(&buf[envs[2].1..envs[2].2], b"C");
}

#[test]
fn v1_long_limiter_run_matching_delimiter() {
    // README: `1^^^echo^^hello~~~~~2` -> [1, payload echo^^hello, ~~2]
    let buf = b"1^^^echo^^hello~~~~~2";
    let items = collect_buf(Variant::V1, buf, &std_pairs());
    assert_eq!(
        items,
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 4, 15, Some(18)),
            Item::Ne(18, 21),
        ]
    );
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"echo^^hello");
    // the delimiter slice is exactly 3 `~` (matching the limiter run), leaving `~~2`
    assert_eq!(&buf[envs[0].3.unwrap()..], b"~~2");
}

#[test]
fn v1_bad_pair_set_delimiter_is_anothers_limiter() {
    // README bad example [('^','~',2),('~','%',2)]:
    // `0^^A~~~~B%%~~C%%1` -> [0, payload A, payload B, payload C, 1]
    let pairs = vec![pair(b"^", b"~", 2), pair(b"~", b"%", 2)];
    let items = collect_buf(Variant::V1, b"0^^A~~~~B%%~~C%%1", &pairs);
    assert_eq!(
        envelops_of(&items)
            .iter()
            .map(|e| (e.1, e.2))
            .collect::<Vec<_>>(),
        vec![(3, 4), (8, 9), (13, 14)]
    );
}

#[test]
fn v2_mixed_channel() {
    // README: `0%%A%%%%B%%%%C%%1` -> [0, payload A%%, B, pending C%%1]
    let buf = b"0%%A%%%%B%%%%C%%1";
    let items = collect_buf(Variant::V2, buf, &std_pairs());
    assert_eq!(
        items,
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 3, 6, Some(8)),
            Item::Ne(8, 9),
            Item::Env(9, 13, 17, None),
        ]
    );
    let envs = envelops_of(&items);
    // payload tail collisions are absorbed into the payload under V2
    assert_eq!(&buf[envs[0].1..envs[0].2], b"A%%");
    assert_eq!(&buf[envs[1].1..envs[1].2], b"C%%1");
}

#[test]
fn v2_braces() {
    // README: `0{{A}}{{B}}??C!!1` -> [0, payload A, payload B, payload C, 1]
    let buf = b"0{{A}}{{B}}??C!!1";
    let items = collect_buf(Variant::V2, buf, &std_pairs());
    let envs = envelops_of(&items);
    assert_eq!(
        envs.iter().map(|e| (e.1, e.2)).collect::<Vec<_>>(),
        vec![(3, 4), (8, 9), (13, 14)]
    );
    assert_eq!(&buf[envs[0].1..envs[0].2], b"A");
    assert_eq!(&buf[envs[1].1..envs[1].2], b"B");
    assert_eq!(&buf[envs[2].1..envs[2].2], b"C");
}

#[test]
fn v2_confirmation_latency_and_application_level_fix() {
    // README: `0{{A}}{{B}}??C!!!` -> [0, payload A, payload B, pending C!]
    // the trailing delimiter slice can't be confirmed at end of buffer...
    let items = collect_buf(Variant::V2, b"0{{A}}{{B}}??C!!!", &std_pairs());
    assert_eq!(
        items.last(),
        Some(&Item::Env(11, 13, 15, None)),
        "delimiter slice followed by EOB stays unconfirmed"
    );
    // ...which the application solves by pushing a non-delimiter T.
    let buf = b"0{{A}}{{B}}??C!!1";
    let items = collect_buf(Variant::V2, buf, &std_pairs());
    assert_eq!(items.last(), Some(&Item::Ne(16, 17)));
    let envs = envelops_of(&items);
    assert_eq!(envs[2], (11, 13, 14, Some(16)));
    assert_eq!(&buf[envs[2].1..envs[2].2], b"C");
}

#[test]
fn v2_long_run() {
    // README: `1^^^echo^^hello~~~~~2` -> [1, payload echo^^hello~~, 2]
    //         `1^^^echo^^hello~~~~~`  -> [1, pending echo^^hello~~]
    let buf = b"1^^^echo^^hello~~~~~2";
    let items = collect_buf(Variant::V2, buf, &std_pairs());
    assert_eq!(
        items,
        vec![
            Item::PhantomE,
            Item::Ne(0, 1),
            Item::Env(1, 4, 17, Some(20)),
            Item::Ne(20, 21),
        ]
    );
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"echo^^hello~~");

    let items = collect_buf(Variant::V2, b"1^^^echo^^hello~~~~~", &std_pairs());
    assert_eq!(
        items,
        vec![Item::PhantomE, Item::Ne(0, 1), Item::Env(1, 4, 17, None)]
    );
}

#[test]
fn v2_bad_pair_set_shared_limiter_order_matters() {
    // README: the application stabilizes behavior by sorting the LimiterPairs.
    // [('^','~',2), ('^','~~',2), ('^','!',2)]: `1^^A!B!~~~~C` -> [1, payload A!B!~~, C]
    let order_a = vec![
        pair(b"^", b"~", 2),
        pair(b"^", b"~~", 2),
        pair(b"^", b"!", 2),
    ];
    let buf = b"1^^A!B!~~~~C";
    let items = collect_buf(Variant::V2, buf, &order_a);
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"A!B!~~");

    // [('^','~~',2), ('^','~',2), ('^','!',2)]: same buffer -> [1, payload A!B!, C]
    let order_b = vec![
        pair(b"^", b"~~", 2),
        pair(b"^", b"~", 2),
        pair(b"^", b"!", 2),
    ];
    let items = collect_buf(Variant::V2, buf, &order_b);
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"A!B!");
}

#[test]
fn limiter_pair_order_freedom() {
    // from the crate-level doc: sorting limiter_pairs selects how slices are framed.
    // [("ab","de",2),("ababac","dede",1)]: "ababacedede" -> limiter "abab" ++ payload "ace" ++ delimiter "dede"
    let order_a = vec![pair(b"ab", b"de", 2), pair(b"ababac", b"dede", 1)];
    let buf = b"ababacedede";
    let items = collect_buf(Variant::V1, buf, &order_a);
    assert_eq!(items, vec![Item::Env(0, 4, 7, Some(11)), Item::Ne(11, 11)]);
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"ace");

    // [("ababac","dede",1),("ab","de",2)]: same buffer -> limiter "ababac" ++ payload "e" ++ delimiter "dede"
    let order_b = vec![pair(b"ababac", b"dede", 1), pair(b"ab", b"de", 2)];
    let items = collect_buf(Variant::V1, buf, &order_b);
    assert_eq!(items, vec![Item::Env(0, 6, 7, Some(11)), Item::Ne(11, 11)]);
    let envs = envelops_of(&items);
    assert_eq!(&buf[envs[0].1..envs[0].2], b"e");
}

#[test]
fn multi_byte_units() {
    // T = u8, but units can be multi-byte (e.g. UTF-8 slices of a byte buffer).
    let pairs = vec![pair("♦".as_bytes(), "♣".as_bytes(), 1)];
    let buf = "0♦hello ♦ world♣1".as_bytes();
    let items = collect_buf(Variant::V1, buf, &pairs);
    let envs = envelops_of(&items);
    assert_eq!(envs.len(), 1);
    assert_eq!(&buf[envs[0].1..envs[0].2], "hello ♦ world".as_bytes());
}

#[test]
fn empty_buffer_and_pure_non_envelop() {
    let pairs = std_pairs();
    // empty buffer: one empty unarchived NonEnvelop at the head
    assert_eq!(collect_buf(Variant::V1, b"", &pairs), vec![Item::Ne(0, 0)]);
    // no limiter pairs match at all: one big NonEnvelop
    assert_eq!(
        collect_buf(Variant::V1, b"hello world", &pairs),
        vec![Item::Ne(0, 11)]
    );
    // pairs with least_repeat None are skipped while parsing
    let mut p = pair(b"^", b"~", 2);
    p.least_repeat = None;
    assert_eq!(
        collect_buf(Variant::V1, b"0^^A~~1", &[p]),
        vec![Item::Ne(0, 7)]
    );
}
