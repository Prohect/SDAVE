//! The README's own hand-worked vectors, verbatim: both variants, the documented pair
//! set, the two "bad" pair sets, and the first-match rule.

mod common;

use common::*;
use sdave::Variant;

// ---------------------------------------------------------------------------
// Variant 1, documented pair set
// ---------------------------------------------------------------------------

#[test]
fn v1_basic_vector_1() {
    // README: `0%%A%%%%B%%%%C%%1` -> [0, payload A, payload B, payload C, 1]
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(
        shot.items,
        vec![PH, h("0"), e("A"), ZH, e("B"), ZH, e("C"), h("1")],
        "the leading phantom marks the NonEnvelop, the zero-width holes mark the \
         back-to-back Envelops (the '%' pair is its own delimiter)"
    );
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
            Off {
                head: 11,
                payload_head: 13,
                payload_tail: 14,
                tail: Some(16)
            },
        ]
    );
    assert_eq!(shot.tail_non_envelop, None);
    assert_eq!(shot.working_on, MS::N(Hole { head: 16, tail: 17 }));
    assert_eq!(shot.next, MS::N(Hole { head: 16, tail: 17 }));
    assert_eq!(shot.internal, MI::Clean);
    assert_eq!(shot.map, vec![Some(0); 5]);
    assert_eq!((shot.limiter.as_str(), shot.delimiter.as_str()), ("%", "%"));
    assert_eq!(shot.least_repeat, Some(2));
}

#[test]
fn v1_basic_vector_2_trailing_limiter() {
    // README: `0%%A%%%%B%%%%C%%1%` -> [0, payload A, payload B, payload C, 1]
    let buffer = buf("0%%A%%%%B%%%%C%%1%");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(
        shot.items,
        vec![PH, h("0"), e("A"), ZH, e("B"), ZH, e("C"), h("1")]
    );
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
            Off {
                head: 11,
                payload_head: 13,
                payload_tail: 14,
                tail: Some(16)
            },
        ]
    );
    assert_eq!(shot.tail_non_envelop, None);
    assert_eq!(shot.working_on, MS::N(Hole { head: 16, tail: 17 }));
    // The trailing '%' after "1" is a live limiter prefix, but a *single* '%' is below the
    // '%' pair's `least_repeat` of 2, so the slice is not yet a limiter slice: the buffer-end
    // scan reports `Unknown`, not `UnterminatedLimiterSlice` (one more '%' would flip it).
    assert_eq!(shot.internal, MI::Unknown(18));
}

#[test]
fn v1_basic_vector_3_pending() {
    // README: `0%%%%%%B%%%%C%%1` -> [0, pending B%%%%C%%1]
    let buffer = buf("0%%%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(shot.items, vec![PH, h("0"), p("B%%%%C%%1")]);
    assert_eq!(
        shot.bounds,
        vec![Off {
            head: 0,
            payload_head: 0,
            payload_tail: 0,
            tail: None
        }]
    );
    assert_eq!(shot.tail_non_envelop, Some(1));
    assert_eq!(
        shot.working_on,
        MS::E(Off {
            head: 1,
            payload_head: 7,
            payload_tail: 16,
            tail: None
        })
    );
    assert_eq!(shot.internal, MI::Clean);
    // The limiter slice is six '%' here, so the confirmed pair carries repeat 6.
    assert_eq!(shot.least_repeat, Some(6));
}

#[test]
fn v1_basic_vector_4_empty_payload_envelop() {
    // README: `0^^~~^^B~~{{C}}1` -> [0, payload (offset range None), payload B, payload C, 1]
    let buffer = buf("0^^~~^^B~~{{C}}1");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(
        shot.items,
        vec![PH, h("0"), e(""), ZH, e("B"), ZH, e("C"), h("1")]
    );
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
                payload_tail: 3,
                tail: Some(5)
            },
            Off {
                head: 5,
                payload_head: 7,
                payload_tail: 8,
                tail: Some(10)
            },
            Off {
                head: 10,
                payload_head: 12,
                payload_tail: 13,
                tail: Some(15)
            },
        ],
        "`^^~~` is an archived Envelop with payload_tail_offset == payload_head_offset"
    );
    assert_eq!(shot.working_on, MS::N(Hole { head: 15, tail: 16 }));
    assert_eq!(shot.internal, MI::Clean);
}

#[test]
fn v1_basic_vector_5_payload_contains_limiter_run() {
    // README: `1^^^echo^^hello~~~~~2` -> [1, payload echo^^hello, ~~2]
    let buffer = buf("1^^^echo^^hello~~~~~2");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(shot.items, vec![PH, h("1"), e("echo^^hello"), h("~~2")]);
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
                payload_head: 4,
                payload_tail: 15,
                tail: Some(18)
            },
        ]
    );
    assert_eq!(shot.working_on, MS::N(Hole { head: 18, tail: 21 }));
    assert_eq!(shot.internal, MI::Clean);
    // Three '^' were consumed as the limiter slice, so the confirmed repeat is 3.
    assert_eq!((shot.limiter.as_str(), shot.delimiter.as_str()), ("^", "~"));
    assert_eq!(shot.least_repeat, Some(3));
}

// ---------------------------------------------------------------------------
// Variant 2, documented pair set
// ---------------------------------------------------------------------------

#[test]
fn v2_basic_vector_1() {
    // README: `0{{A}}{{B}}??C!!1` -> [0, payload A, payload B, payload C, 1]
    for _repeat in 0..2 {
        // The README states this same vector twice; both readings must hold.
        let buffer = buf("0{{A}}{{B}}??C!!1");
        let pairs = std_pairs();
        let shot = one_shot(&buffer, Variant::V2, &pairs);

        assert_eq!(
            shot.items,
            vec![PH, h("0"), e("A"), ZH, e("B"), ZH, e("C"), h("1")]
        );
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
                Off {
                    head: 11,
                    payload_head: 13,
                    payload_tail: 14,
                    tail: Some(16)
                },
            ]
        );
        assert_eq!(shot.working_on, MS::N(Hole { head: 16, tail: 17 }));
        assert_eq!(shot.internal, MI::Clean);
    }
}

#[test]
fn v2_basic_vector_2_pending_delimiter() {
    // README: `0{{A}}{{B}}??C!!!` -> [0, payload A, payload B, pending C!]
    let buffer = buf("0{{A}}{{B}}??C!!!");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V2, &pairs);

    assert_eq!(shot.items, vec![PH, h("0"), e("A"), ZH, e("B"), p("C!")]);
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
    assert_eq!(
        shot.working_on,
        MS::E(Off {
            head: 11,
            payload_head: 13,
            payload_tail: 15,
            tail: None
        }),
        "the trailing '!' run is not confirmed as a delimiter slice"
    );
    assert_eq!(shot.next, shot.working_on);
    assert_eq!(shot.internal, MI::UnterminatedDeLimiterSlice(17));
}

#[test]
fn v2_basic_vector_3_echo() {
    // README: `1^^^echo^^hello~~~~~2` -> [1, payload echo^^hello~~, 2]
    let buffer = buf("1^^^echo^^hello~~~~~2");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V2, &pairs);

    assert_eq!(shot.items, vec![PH, h("1"), e("echo^^hello~~"), h("2")]);
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
                payload_head: 4,
                payload_tail: 17,
                tail: Some(20)
            },
        ],
        "V2 hands the two extra '~' of the merged run back to the payload"
    );
    assert_eq!(shot.working_on, MS::N(Hole { head: 20, tail: 21 }));
    assert_eq!(shot.internal, MI::Clean);
    assert_eq!(shot.least_repeat, Some(3));
}

#[test]
fn v2_basic_vector_4_pending_echo() {
    // README: `1^^^echo^^hello~~~~~` -> [1, pending echo^^hello~~ ]
    let buffer = buf("1^^^echo^^hello~~~~~");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V2, &pairs);

    assert_eq!(shot.items, vec![PH, h("1"), p("echo^^hello~~")]);
    assert_eq!(
        shot.working_on,
        MS::E(Off {
            head: 1,
            payload_head: 4,
            payload_tail: 17,
            tail: None
        })
    );
    assert_eq!(shot.internal, MI::UnterminatedDeLimiterSlice(20));
}

#[test]
fn v2_basic_vector_5_percent_only() {
    // README: `0%%A%%%%B%%%%C%%1` -> [0, payload A%%, B, pending C%%1]
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V2, &pairs);

    assert_eq!(
        shot.items,
        vec![PH, h("0"), e("A%%"), h("B"), p("C%%1")],
        "the fourth '%' run cannot re-frame a 4-repeat limiter, so it stays a NonEnvelop"
    );
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
                payload_tail: 6,
                tail: Some(8)
            },
        ]
    );
    assert_eq!(shot.tail_non_envelop, Some(9));
    assert_eq!(
        shot.working_on,
        MS::E(Off {
            head: 9,
            payload_head: 13,
            payload_tail: 17,
            tail: None
        })
    );
    assert_eq!(shot.internal, MI::Clean);
}

// ---------------------------------------------------------------------------
// bad pair set 1: (A.delimiter == B.limiter)
// ---------------------------------------------------------------------------

#[test]
fn crossing_pairs_v1() {
    // README: [('^','~',2), ('~','%',2)] on `0^^A~~~~B%%~~C%%1`
    //         -> [0, payload A, payload B, payload C, 1]
    let buffer = buf("0^^A~~~~B%%~~C%%1");
    let pairs = crossing_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(
        shot.items,
        vec![PH, h("0"), e("A"), ZH, e("B"), ZH, e("C"), h("1")]
    );
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
            Off {
                head: 11,
                payload_head: 13,
                payload_tail: 14,
                tail: Some(16)
            },
        ],
        "payload B wears the second pair, whose limiter slice is pair one's delimiter"
    );
    assert_eq!(shot.working_on, MS::N(Hole { head: 16, tail: 17 }));
    assert_eq!(shot.internal, MI::Clean);
}

#[test]
fn crossing_pairs_v2() {
    // README: [('^','~',2), ('~','%',2)] on `0^^A~~~~B%%~~C%%1`
    //         -> [0, payload A~~, B%%, payload C, 1]
    let buffer = buf("0^^A~~~~B%%~~C%%1");
    let pairs = crossing_pairs();
    let shot = one_shot(&buffer, Variant::V2, &pairs);

    assert_eq!(
        shot.items,
        vec![PH, h("0"), e("A~~"), h("B%%"), e("C"), h("1")]
    );
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
                payload_tail: 6,
                tail: Some(8)
            },
            Off {
                head: 11,
                payload_head: 13,
                payload_tail: 14,
                tail: Some(16)
            },
        ],
        "the stale intermediate bytes are the NonEnvelop hole between the two Envelops"
    );
    assert_eq!(shot.tail_non_envelop, None);
    assert_eq!(shot.working_on, MS::N(Hole { head: 16, tail: 17 }));
    assert_eq!(shot.internal, MI::Clean);
}

// ---------------------------------------------------------------------------
// bad pair set 2: shared limiter, and the first-match rule
// ---------------------------------------------------------------------------

#[test]
fn shared_limiter_v1() {
    // README: [('^','~',2), ('^',"~~",2), ('^','!',2)] on `1^^A!B!~~~~C`
    //         -> [1, payload A!B!, ~~C]
    let buffer = buf("1^^A!B!~~~~C");
    let pairs = shared_limiter_pairs(true);
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(shot.items, vec![PH, h("1"), e("A!B!"), h("~~C")]);
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
                payload_tail: 7,
                tail: Some(9)
            },
        ],
        "the first pair in order ('^','~') is the one confirmed"
    );
    assert_eq!((shot.limiter.as_str(), shot.delimiter.as_str()), ("^", "~"));
    assert_eq!(shot.working_on, MS::N(Hole { head: 9, tail: 12 }));
    assert_eq!(shot.internal, MI::Clean);
}

#[test]
fn shared_limiter_v2_readme_order() {
    // README: [('^','~',2), ('^',"~~",2), ('^','!',2)] on `1^^A!B!~~~~C`
    //         -> [1, payload A!B!~~, C]
    let buffer = buf("1^^A!B!~~~~C");
    let pairs = shared_limiter_pairs(true);
    let shot = one_shot(&buffer, Variant::V2, &pairs);

    assert_eq!(shot.items, vec![PH, h("1"), e("A!B!~~"), h("C")]);
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
                payload_tail: 9,
                tail: Some(11)
            },
        ]
    );
    assert_eq!(shot.working_on, MS::N(Hole { head: 11, tail: 12 }));
    assert_eq!(shot.internal, MI::Clean);
}

#[test]
fn shared_limiter_v2_reordered_pairs() {
    // README: [('^',"~~",2), ('^','~',2), ('^','!',2)] on `1^^A!B!~~~~C`
    //         -> [1, payload A!B!, C]
    let buffer = buf("1^^A!B!~~~~C");
    let pairs = shared_limiter_pairs(false);
    let shot = one_shot(&buffer, Variant::V2, &pairs);

    assert_eq!(shot.items, vec![PH, h("1"), e("A!B!"), h("C")]);
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
                payload_tail: 7,
                tail: Some(11)
            },
        ],
        "the two-unit '~~' delimiter needs only two repeats of the 4-run"
    );
    assert_eq!(
        (shot.limiter.as_str(), shot.delimiter.as_str()),
        ("^", "~~")
    );
    assert_eq!(shot.working_on, MS::N(Hole { head: 11, tail: 12 }));
    assert_eq!(shot.internal, MI::Clean);
}

#[test]
fn first_match_rule_is_order_sensitive_both_directions() {
    let buffer = buf("1^^A!B!~~~~C");
    let readme_order = one_shot(&buffer, Variant::V2, &shared_limiter_pairs(true));
    let reordered = one_shot(&buffer, Variant::V2, &shared_limiter_pairs(false));

    assert_eq!(readme_order.items[2], e("A!B!~~"));
    assert_eq!(reordered.items[2], e("A!B!"));
    assert_ne!(
        readme_order.items, reordered.items,
        "pair order is significant: same buffer, reordered pairs, different result"
    );
    // Both directions of the inequality, explicitly.
    assert!(readme_order.items != reordered.items);
    assert!(reordered.items != readme_order.items);
    assert_eq!(readme_order.bounds[1].payload_tail, 9);
    assert_eq!(reordered.bounds[1].payload_tail, 7);
}

#[test]
fn safe_pair_set_predicate() {
    // Documented: `true` means the `repeat_map_on_cursor` simplification is safe.
    assert!(!safe(&std_pairs()), "the '%' pair is its own delimiter");
    assert!(
        !safe(&crossing_pairs()),
        "pair two's limiter is pair one's delimiter"
    );
    assert!(
        !safe(&shared_limiter_pairs(true)),
        "all three pairs share the limiter"
    );
    assert!(safe(&vec![pair(2, "^", "~"), pair(2, "?", "!")]));
    assert!(safe(&multi_unit_pairs()));
}

// ---------------------------------------------------------------------------
// extension: multi-`T` limiter/delimiter units
// ---------------------------------------------------------------------------

#[test]
fn multi_unit_limiter_and_delimiter() {
    // `LimiterPair`'s limiter/delimiter are Vec<T> ("e.g. UTF-8 1B/2B/3B char in a u8 buffer"),
    // so a two-unit pair must work: `ab`*2 frames X and `cd`*2 confirms it.
    let buffer = buf("0ababXcdcd");
    let pairs = multi_unit_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(shot.items, vec![PH, h("0"), e("X")]);
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
                payload_head: 5,
                payload_tail: 6,
                tail: Some(10)
            },
        ]
    );
    assert_eq!(
        (shot.limiter.as_str(), shot.delimiter.as_str()),
        ("ab", "cd")
    );
    assert_eq!(shot.internal, MI::CleanArchived);
}

// ---------------------------------------------------------------------------
// extension: the "one Envelop only" shape, and CleanArchived
// ---------------------------------------------------------------------------

#[test]
fn single_archived_envelop_is_clean_archived() {
    let buffer = buf("^^A~~");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(
        shot.items,
        vec![e("A")],
        "no phantom: the Envelop starts at 0"
    );
    assert_eq!(
        shot.bounds,
        vec![Off {
            head: 0,
            payload_head: 2,
            payload_tail: 3,
            tail: Some(5)
        }]
    );
    assert_eq!(
        shot.working_on,
        MS::E(Off {
            head: 0,
            payload_head: 2,
            payload_tail: 3,
            tail: Some(5)
        })
    );
    assert_eq!(shot.next, MS::N(Hole { head: 5, tail: 5 }));
    assert_eq!(shot.internal, MI::CleanArchived);
}
