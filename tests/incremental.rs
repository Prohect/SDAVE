//! The one-step contract, `deserialize_all` as its fixpoint, and what `iter()` may expose.

mod common;

use common::*;
use sdave::{FlatDeserializer, FlatDeserializerState, Variant, deserialize_incremental};

// ---------------------------------------------------------------------------
// one item per step
// ---------------------------------------------------------------------------

#[test]
fn one_step_advances_exactly_one_item() {
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();
    let steps = raw_run(&buffer, Variant::V1, &pairs);

    // Three real items (NonEnvelop, Envelop, NonEnvelop) + the no-op fixpoint step.
    assert_eq!(steps.len(), 4);

    assert_eq!(steps[0].working_on, MS::N(Hole { head: 0, tail: 1 }));
    assert_eq!(steps[0].internal, MI::Archived);
    assert_eq!(
        steps[1].working_on,
        MS::E(Off {
            head: 1,
            payload_head: 3,
            payload_tail: 4,
            tail: Some(6)
        })
    );
    assert_eq!(steps[1].internal, MI::Archived);
    assert_eq!(steps[2].working_on, MS::N(Hole { head: 6, tail: 7 }));
    assert_eq!(steps[2].internal, MI::Clean);

    assert!(steps[0].changed && steps[1].changed && steps[2].changed);
    assert!(!steps[3].changed, "the fourth step claims nothing new");
    assert_eq!(steps[3].working_on, steps[2].working_on);
    assert_eq!(steps[3].internal, steps[2].internal);

    // Each productive step claims one item whose head is the previous item's semantical tail.
    let mut cursor = 0usize;
    for step in steps.iter().filter(|s| s.changed) {
        assert_eq!(ms_head(step.working_on), cursor, "items must be contiguous");
        cursor = ms_tail(step.working_on);
    }
    assert_eq!(cursor, buffer.len());
}

#[test]
fn one_step_advances_exactly_one_item_on_a_v2_shared_channel() {
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let steps = raw_run(&buffer, Variant::V2, &pairs);

    // NonEnvelop "0", Envelop "A%%", NonEnvelop "B", pending Envelop "C%%1", then fixpoint.
    assert_eq!(steps.len(), 5);
    assert_eq!(
        steps.iter().map(|s| s.working_on).collect::<Vec<_>>(),
        vec![
            MS::N(Hole { head: 0, tail: 1 }),
            MS::E(Off {
                head: 1,
                payload_head: 3,
                payload_tail: 6,
                tail: Some(8)
            }),
            MS::N(Hole { head: 8, tail: 9 }),
            MS::E(Off {
                head: 9,
                payload_head: 13,
                payload_tail: 17,
                tail: None
            }),
            MS::E(Off {
                head: 9,
                payload_head: 13,
                payload_tail: 17,
                tail: None
            }),
        ]
    );
    assert_eq!(
        steps.iter().map(|s| s.internal).collect::<Vec<_>>(),
        vec![
            MI::Archived,
            MI::Archived,
            MI::Archived,
            MI::Clean,
            MI::Clean
        ]
    );
    assert!(!steps[4].changed);
    // Step 4 ends in the delimiter seek's `from == buffer.len()` path (the pending Envelop's
    // payload reached the end of the buffer), hence `Clean` — not `UnterminatedDeLimiterSlice`,
    // which needs the delimiter slice itself to be live at the buffer end.
}

#[test]
fn internal_states_are_the_documented_ones_for_v1() {
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let steps = raw_run(&buffer, Variant::V1, &pairs);

    // Four archived items, then the settled trailing NonEnvelop.
    assert_eq!(
        steps.iter().map(|s| s.internal).collect::<Vec<_>>(),
        vec![
            MI::Archived,
            MI::Archived,
            MI::Archived,
            MI::Archived,
            MI::Clean,
            MI::Clean
        ]
    );
    assert_eq!(steps.len(), 6);
    assert_eq!(steps[4].working_on, MS::N(Hole { head: 16, tail: 17 }));
    assert!(!steps[5].changed);
    // Step 5 scans the trailing NonEnvelop all the way to the buffer end, which is the
    // `from == buffer.len()` path, so the settled state is `Clean`. `UnterminatedLimiterSlice`
    // is reserved for a scan that stops while a limiter pair is still live; see
    // `unterminated_limiter_slice_is_its_own_state_and_transitions`.
}

#[test]
fn unterminated_limiter_slice_is_its_own_state_and_transitions() {
    // V1 `0%%A%%1`, starting from its 3-unit prefix and growing one unit at a time. The '%'
    // pair is its own delimiter, so this one Envelop's whole progress follows from the
    // variant-1 rule plus `InternalState`'s own documentation.
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();

    // Three units: two '%'. The '%' pair's `least_repeat` is 2, so the slice under the cursor
    // is a limiter slice whose repeats are well defined — only the not-repeating `T` that
    // terminates it ('A') is missing from the buffer. That is exactly
    // `UnterminatedLimiterSlice`, and nothing has been claimed: the cache is empty and only
    // the NonEnvelop "0" is reported.
    let mut fd = FlatDeserializer::new(&buffer[..3], Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(
        mi_of(fd.deserializer_state().internal()),
        MI::UnterminatedLimiterSlice(3)
    );
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::N(Hole { head: 0, tail: 1 })
    );
    // The documented reading of `repeat_map_on_cursor`: the repeats of each pair inside
    // `buffer[next.semantical_tail .. tail_offset]` == `buffer[1..3]` == "%%" — two for the
    // '%' pair, a mismatch for every other pair.
    assert_eq!(
        map_of(fd.deserializer_state()),
        vec![None, None, Some(2), None, None]
    );
    assert!(fd.archived_boundaries().is_empty());
    assert_eq!(items_of(&mut fd), vec![h("0")]);

    // The same three units under V2: the limiter scan does not depend on the variant.
    let mut fd_v2 =
        FlatDeserializer::new(&buffer[..3], Variant::V2, &pairs, Vec::new(), Vec::new());
    fd_v2.deserialize_all();
    assert_eq!(
        mi_of(fd_v2.deserializer_state().internal()),
        MI::UnterminatedLimiterSlice(3)
    );

    // The terminating non-repeating `T` ('A') arrives at unit 4. The limiter slice is now well
    // defined, so the Envelop is claimed (its confirmed `least_repeat` is 2 and its
    // `payload_head_offset` is 3) and the NonEnvelop "0" is archived. The Envelop stays
    // *pending*, because no delimiter slice has been seen; the buffer is fully scanned, which
    // is `Clean`.
    let mut fd = grow(&buffer, 4, fd);
    fd.deserialize_all();
    assert_eq!(mi_of(fd.deserializer_state().internal()), MI::Clean);
    assert_eq!(
        ms_of(fd.deserializer_state().working_on()),
        MS::E(Off {
            head: 1,
            payload_head: 3,
            payload_tail: 4,
            tail: None
        })
    );
    assert_eq!(
        pair_fields(fd.deserializer_state().limiter_pair()),
        (Some(2), "%".to_string(), "%".to_string())
    );
    assert_eq!(
        offs(fd.archived_boundaries()),
        vec![Off {
            head: 0,
            payload_head: 0,
            payload_tail: 0,
            tail: None
        }]
    );
    assert_eq!(fd.tail_non_envelop().map(|t| t.get()), Some(1));

    // Unit 5 starts the delimiter slice and stops inside it. V1 has no "unterminated
    // delimiter" state — `UnterminatedDeLimiterSlice` is documented as legal only for a V2
    // Envelop — so this is `Unknown`, with the pending Envelop still the tail item.
    let mut fd = grow(&buffer, 5, fd);
    fd.deserialize_all();
    assert_eq!(mi_of(fd.deserializer_state().internal()), MI::Unknown(5));
    assert_eq!(items_of(&mut fd), vec![PH, h("0"), p("A")]);

    // Unit 6 completes the delimiter slice: the Envelop is archived, the state is promoted to
    // `CleanArchived`, and the tail must not be reported a second time.
    let mut fd = grow(&buffer, 6, fd);
    fd.deserialize_all();
    assert_eq!(mi_of(fd.deserializer_state().internal()), MI::CleanArchived);
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
        ]
    );
    assert_eq!(items_of(&mut fd), vec![PH, h("0"), e("A")]);
}

#[test]
fn empty_unit_pairs_are_ignored_by_the_deserializer() {
    // `LimiterPair::limiter()` and `delimiter()` document that a pair with an empty unit
    // "would be ignored", and `least_repeat()` uses the same word for `None`. Being ignored is
    // the *point* here, not an accident: an empty limiter trivially "matches" at every
    // position, so honouring it would turn every byte into a limiter slice.
    let buffer = buf("0^^A~~1");

    // The same channel without junk: the '^' pair frames "A" as 1..6, the '{' pair never
    // matches. A list containing junk must not change this.
    let reference = one_shot(
        &buffer,
        Variant::V1,
        &vec![pair(2, "^", "~"), pair(2, "{", "}")],
    );
    assert_eq!(reference.items, vec![PH, h("0"), e("A"), h("1")]);
    assert_eq!(
        reference.bounds,
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
        ]
    );

    // (name, pair list, expected `repeat_map_on_cursor` of the settled state)
    let cases: Vec<(&str, Pairs, Vec<Option<usize>>)> = vec![
        (
            "empty limiter first",
            vec![pair(2, "", "!"), pair(2, "^", "~"), pair(2, "{", "}")],
            vec![None, Some(0), Some(0)],
        ),
        (
            "empty limiter in the middle",
            vec![pair(2, "^", "~"), pair(2, "", "!"), pair(2, "{", "}")],
            vec![Some(0), None, Some(0)],
        ),
        (
            "empty delimiter first",
            vec![pair(2, "?", ""), pair(2, "^", "~"), pair(2, "{", "}")],
            vec![None, Some(0), Some(0)],
        ),
        (
            "empty delimiter in the middle",
            vec![pair(2, "^", "~"), pair(2, "?", ""), pair(2, "{", "}")],
            vec![Some(0), None, Some(0)],
        ),
        (
            "both kinds",
            vec![pair(2, "", "!"), pair(2, "?", ""), pair(2, "^", "~")],
            vec![None, None, Some(0)],
        ),
    ];

    for (name, pairs, expected_map) in cases {
        let shot = one_shot(&buffer, Variant::V1, &pairs);

        // Non-interference: identical items, identical archived boundaries, identical tail.
        assert_eq!(shot.items, reference.items, "{name}");
        assert_eq!(shot.bounds, reference.bounds, "{name}");
        assert_eq!(shot.tail_non_envelop, reference.tail_non_envelop, "{name}");
        // The confirmed pair is the valid one, never the ignored pair — that also covers the
        // case where the junk pair is first in order and so holds the initial
        // `limiter_pair()` value before anything is confirmed.
        assert_eq!(
            (shot.limiter.as_str(), shot.delimiter.as_str()),
            ("^", "~"),
            "{name}"
        );
        assert_eq!(shot.least_repeat, Some(2), "{name}");
        // Exactly one Envelop reached the cache, and it is the one the valid pair framed:
        // the ignored pair contributed none.
        assert_eq!(
            shot.bounds.iter().filter(|b| b.tail.is_some()).count(),
            1,
            "{name}"
        );
        // `repeat_map_on_cursor`'s doc gives an entry two readings: `Some(repeats)` in the
        // slice under the cursor, or `None` for a mismatch. An ignored pair can never have
        // repeats, so it can only be `None` — the map is refilled with `Some(0)` (on each
        // cursor advance) only for the pairs that can actually match.
        assert_eq!(shot.map, expected_map, "{name}");
    }

    // Mid-scan the eligible pair reports live repeats while the ignored pair stays a mismatch:
    // at the 3-unit prefix `0^^` the '^' pair has matched twice (which is the live
    // limiter-slice state), the ignored pair has no entry, and the non-matching '{' pair has
    // been dropped to `None` as well.
    let with_junk = vec![pair(2, "^", "~"), pair(2, "", "!"), pair(2, "{", "}")];
    let mut fd = FlatDeserializer::new(
        &buffer[..3],
        Variant::V1,
        &with_junk,
        Vec::new(),
        Vec::new(),
    );
    fd.deserialize_all();
    assert_eq!(map_of(fd.deserializer_state()), vec![Some(2), None, None]);
    assert_eq!(
        mi_of(fd.deserializer_state().internal()),
        MI::UnterminatedLimiterSlice(3)
    );
    assert!(fd.archived_boundaries().is_empty());
}

#[test]
fn an_all_inert_pair_list_confirms_nothing_but_still_runs() {
    // Every pair ignored, but the list is *non-empty*, so this is not the documented
    // `"[SDAVE] empty limiter_pairs"` panic that `panics.rs` covers for an empty list. With no
    // pair able to match, the non-matching path advances the NonEnvelop by one unit per
    // iteration until it reaches the buffer end, where the limiter scan's `from ==
    // buffer.len()` branch settles as `Clean`: the whole channel is one NonEnvelop hole.
    let buffer = buf("0^^A~~1");
    let cases: Vec<(&str, Pairs, Vec<Option<usize>>)> = vec![
        ("empty limiter only", vec![pair(2, "", "!")], vec![None]),
        ("empty delimiter only", vec![pair(2, "?", "")], vec![None]),
        (
            "one of each",
            vec![pair(2, "", "!"), pair(2, "?", "")],
            vec![None, None],
        ),
    ];

    for (name, pairs, expected_map) in cases {
        let shot = one_shot(&buffer, Variant::V1, &pairs);
        assert_eq!(shot.bounds, vec![], "{name}: no boundary may be confirmed");
        assert_eq!(shot.tail_non_envelop, None, "{name}");
        assert_eq!(shot.internal, MI::Clean, "{name}");
        // `limiter_pair()` is deliberately not asserted: the docs make it meaningful only when
        // `working_on` is an Envelop, and here the whole channel settles as a NonEnvelop.
        assert_eq!(shot.working_on, MS::N(Hole { head: 0, tail: 7 }), "{name}");
        assert_eq!(shot.items, vec![h("0^^A~~1")], "{name}");
        assert_eq!(shot.map, expected_map, "{name}");
    }
}

// ---------------------------------------------------------------------------
// deserialize_all == fixpoint loop
// ---------------------------------------------------------------------------

#[test]
fn deserialize_all_is_the_fixpoint_loop() {
    for (text, variant, pairs) in spec_cases() {
        let buffer = buf(text);
        let fixpoint = raw_fixpoint(&buffer, variant, &pairs);

        let mut fd = FlatDeserializer::new(&buffer, variant, &pairs, Vec::new(), Vec::new());
        fd.deserialize_all();
        assert!(
            state_eq(
                fd.deserializer_state(),
                &fixpoint,
                fixpoint.repeat_map_on_cursor().unwrap()
            ),
            "deserialize_all diverged from the fixpoint loop for {text:?} ({})",
            vname(variant)
        );
    }
}

#[test]
fn a_step_that_changes_nothing_is_detectable() {
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();

    let mut settled = raw_fixpoint(&buffer, Variant::V1, &pairs);

    let old_repeat_map_on_cursor = settled.repeat_map_on_cursor().unwrap().clone();
    let again = unsafe { deserialize_incremental(&buffer, &mut settled) };
    assert!(
        state_eq(&again, &settled, &old_repeat_map_on_cursor),
        "a no-op step must be recognisable by state equality"
    );

    let mut fresh = FlatDeserializerState::new(Variant::V1, &pairs, Vec::new());

    let old_repeat_map_on_cursor = fresh.repeat_map_on_cursor().unwrap().clone();
    let stepped = unsafe { deserialize_incremental(&buffer, &mut fresh) };
    assert!(
        !state_eq(&stepped, &fresh, &old_repeat_map_on_cursor),
        "the first step on a fresh state must claim an item"
    );
}

// ---------------------------------------------------------------------------
// iter() observability
// ---------------------------------------------------------------------------

#[test]
fn iter_never_exposes_an_intermediate_state() {
    for (text, variant, pairs) in spec_cases() {
        let buffer = buf(text);
        let mut fd = FlatDeserializer::new(&buffer, variant, &pairs, Vec::new(), Vec::new());
        fd.deserialize_all();
        let items = items_of(&mut fd);
        // Read the settled state through the views: `iter()` may have driven further no-op
        // steps, and its trailing item is exactly this `working_on`.
        let internal = mi_of(fd.deserializer_state().internal());

        for (i, item) in items.iter().enumerate() {
            if i + 1 < items.len() {
                assert!(
                    !matches!(item, Item::Pending(_)),
                    "{text:?} ({}): item {i} of {:?} is unarchived before the tail",
                    vname(variant),
                    items
                );
            }
        }

        // Every archived Envelop is reported exactly once (no double-reporting of a
        // CleanArchived tail, no skipped Envelop).
        let archived = fd
            .archived_boundaries()
            .iter()
            .filter(|b| b.tail_offset().is_some())
            .count();
        let reported = items.iter().filter(|i| matches!(i, Item::Env(_))).count();
        assert_eq!(
            reported,
            archived,
            "{text:?} ({}): {reported} Envelop items for {archived} archived Envelops",
            vname(variant)
        );

        // Only a settled tail may appear: outside `CleanArchived` the tail item is exactly
        // `working_on` (and a CleanArchived tail is already in the cache, so it is reported
        // as the last cached Envelop instead of being appended a second time).
        if internal == MI::CleanArchived {
            assert!(items.iter().all(|i| !matches!(i, Item::Pending(_))));
        } else {
            assert_eq!(
                items.last(),
                Some(&item_of(fd.deserializer_state().working_on(), &buffer)),
                "{text:?} ({}): the trailing item must be the settled tail",
                vname(variant)
            );
        }
    }
}

#[test]
fn clean_archived_tail_is_not_double_reported() {
    let buffer = buf("^^A~~");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(mi_of(fd.deserializer_state().internal()), MI::CleanArchived);
    assert_eq!(items_of(&mut fd), vec![e("A")]);

    // The same on a multi-unit pair: the Envelop ending exactly at the buffer end.
    let buffer = buf("0ababXcdcd");
    let pairs = multi_unit_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(mi_of(fd.deserializer_state().internal()), MI::CleanArchived);
    assert_eq!(items_of(&mut fd), vec![PH, h("0"), e("X")]);
}

#[test]
fn phantom_entries_appear_exactly_where_documented() {
    // A leading NonEnvelop is expressed by a zero-width phantom Envelop with tail_offset None.
    // Two adjacent archived Envelops are expressed by a zero-width NonEnvelop hole.
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let shot = one_shot(&buffer, Variant::V1, &pairs);

    assert_eq!(
        shot.bounds[0],
        Off {
            head: 0,
            payload_head: 0,
            payload_tail: 0,
            tail: None
        },
        "phantom Envelop"
    );
    assert!(shot.bounds[0].tail.is_none() && shot.bounds[0].head == 0);
    assert_eq!(shot.items[0], PH);

    assert_eq!(shot.bounds[1].tail, Some(6));
    assert_eq!(shot.bounds[2].head, 6);
    assert_eq!(shot.items[3], ZH, "hole between Envelop A and Envelop B");
    assert_eq!(shot.bounds[2].tail, Some(11));
    assert_eq!(shot.bounds[3].head, 11);
    assert_eq!(shot.items[5], ZH, "hole between Envelop B and Envelop C");

    // When the first item IS an Envelop there is no phantom.
    let shot = one_shot(&buf("^^A~~"), Variant::V1, &std_pairs());
    assert_eq!(shot.bounds.len(), 1);
    assert_eq!(shot.items, vec![e("A")]);
}

#[test]
fn semantical_tail_offset_of_a_pending_envelop_is_its_payload_tail() {
    let buffer = buf("0%%%%%%B%%%%C%%1");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();

    let working_on = fd.deserializer_state().working_on();
    assert_eq!(
        ms_of(working_on),
        MS::E(Off {
            head: 1,
            payload_head: 7,
            payload_tail: 16,
            tail: None
        })
    );
    assert_eq!(working_on.semantical_tail_offset().get(), 16);
    assert_eq!(items_of(&mut fd).last(), Some(&p("B%%%%C%%1")));

    // An archived Envelop reports its tail_offset instead.
    let buffer = buf("^^A~~");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(
        fd.deserializer_state()
            .working_on()
            .semantical_tail_offset()
            .get(),
        5
    );
}
