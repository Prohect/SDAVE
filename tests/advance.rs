//! `FlatDeserializer::advance`: dropping fully covered items, truncating a straddling
//! Envelop, re-inserting the leading phantom, shifting every offset, and counting only
//! real items.

mod common;

use common::*;
use sdave::{FlatDeserializer, Variant};

#[test]
fn advance_drops_fully_covered_items_and_shifts_offsets() {
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(
        offs(fd.archived_boundaries()),
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
            Off {
                head: 11,
                payload_head: 13,
                payload_tail: 14,
                tail: Some(16)
            },
        ]
    );

    // offset 5 sits inside the second Envelop's limiter slice.
    assert_eq!(
        adv(&mut fd, 5),
        1,
        "exactly the fully covered leading NonEnvelop is counted (the dropped phantom is not)"
    );
    assert_eq!(
        offs(fd.archived_boundaries()),
        vec![
            Off {
                head: 0,
                payload_head: 0,
                payload_tail: 0,
                tail: Some(1)
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
        ],
        "the straddling Envelop keeps its frame, with every offset clamped by 5"
    );
    assert!(
        fd.tail_non_envelop().is_none(),
        "no archived NonEnvelop behind the last cached Envelop"
    );
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::N(Hole { head: 11, tail: 12 })
    );
    assert_eq!(
        ms_of(fd.deserializer_state().next()),
        MS::N(Hole { head: 11, tail: 12 })
    );
}

#[test]
fn advance_truncates_a_straddling_envelop() {
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();

    // offset 2 sits between the two units of Envelop A's limiter slice, so the truncated
    // Envelop's `buffer[head..payload_head]` no longer matches the original limiter slice.
    assert_eq!(adv(&mut fd, 2), 1);
    assert_eq!(
        offs(fd.archived_boundaries()),
        vec![
            Off {
                head: 0,
                payload_head: 1,
                payload_tail: 2,
                tail: Some(4)
            },
            Off {
                head: 4,
                payload_head: 6,
                payload_tail: 7,
                tail: Some(9)
            },
            Off {
                head: 9,
                payload_head: 11,
                payload_tail: 12,
                tail: Some(14)
            },
        ]
    );
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::N(Hole { head: 14, tail: 15 })
    );
}

#[test]
fn advance_reinserts_the_leading_phantom_marker() {
    // V2 `0^^A~~~~B%%~~C%%1` has a NonEnvelop hole ("B%%") before its last Envelop.
    let buffer = buf("0^^A~~~~B%%~~C%%1");
    let pairs = crossing_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V2, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(
        offs(fd.archived_boundaries()),
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
                payload_tail: 6,
                tail: Some(8)
            },
            Off {
                head: 11,
                payload_head: 13,
                payload_tail: 14,
                tail: Some(16)
            },
        ]
    );

    // offset 10 leaves one unit of that hole in front of the surviving Envelop.
    assert_eq!(adv(&mut fd, 10), 2);
    assert_eq!(
        offs(fd.archived_boundaries()),
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
        ],
        "the phantom marker is re-inserted for the hole that is still in front"
    );
    assert!(fd.tail_non_envelop().is_none());
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::N(Hole { head: 6, tail: 7 })
    );
}

#[test]
fn advance_returns_real_items_only() {
    // V2 `0^^A~~~~B%%~~C%%1`: at offset 11 the NonEnvelop hole "B%%" is fully covered too.
    let buffer = buf("0^^A~~~~B%%~~C%%1");
    let pairs = crossing_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V2, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();

    assert_eq!(
        adv(&mut fd, 11),
        3,
        "two Envelops plus the non-empty hole; the phantom marker is not an item"
    );
    assert_eq!(
        offs(fd.archived_boundaries()),
        vec![Off {
            head: 0,
            payload_head: 2,
            payload_tail: 3,
            tail: Some(5)
        }]
    );
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::N(Hole { head: 5, tail: 6 })
    );
}

#[test]
fn advance_does_not_count_zero_width_holes() {
    // V1 `0^^~~^^B~~{{C}}1` has two adjacent Envelops right after the leading NonEnvelop.
    let buffer = buf("0^^~~^^B~~{{C}}1");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();

    assert_eq!(
        adv(&mut fd, 5),
        2,
        "the empty-payload Envelop and the leading NonEnvelop are items; the zero-width hole is not"
    );
    assert_eq!(
        offs(fd.archived_boundaries()),
        vec![
            Off {
                head: 0,
                payload_head: 2,
                payload_tail: 3,
                tail: Some(5)
            },
            Off {
                head: 5,
                payload_head: 7,
                payload_tail: 8,
                tail: Some(10)
            },
        ]
    );
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::N(Hole { head: 10, tail: 11 })
    );
}

#[test]
fn advance_shifts_the_trailing_non_envelop_and_the_pending_tail() {
    // V2 `0%%A%%%%B%%%%C%%1`: cached Envelop "A%%", trailing hole "B", pending "C%%1".
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V2, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(fd.tail_non_envelop().map(|t| t.get()), Some(9));

    assert_eq!(adv(&mut fd, 1), 1, "the leading NonEnvelop '0'");
    assert_eq!(
        offs(fd.archived_boundaries()),
        vec![Off {
            head: 0,
            payload_head: 2,
            payload_tail: 5,
            tail: Some(7)
        }]
    );
    assert_eq!(fd.tail_non_envelop().map(|t| t.get()), Some(8));
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::E(Off {
            head: 8,
            payload_head: 12,
            payload_tail: 16,
            tail: None
        })
    );
}

#[test]
fn advance_to_the_confirmed_tail_drops_everything() {
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();

    assert_eq!(adv(&mut fd, 7), 2, "the NonEnvelop and the Envelop");
    assert!(fd.archived_boundaries().is_empty());
    assert!(fd.tail_non_envelop().is_none());
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::N(Hole { head: 0, tail: 0 })
    );
    assert_eq!(
        ms_of(fd.deserializer_state().next()),
        MS::N(Hole { head: 0, tail: 0 })
    );
}
