//! Streaming safety: growing a buffer one unit (or one chunk) at a time must never
//! confirm a boundary that the complete buffer does not confirm, and the settled state
//! after the last unit must be the one-shot state.

mod common;

use common::*;
use sdave::{FlatDeserializer, Variant};

// ---------------------------------------------------------------------------
// every prefix / every chunk size
// ---------------------------------------------------------------------------

#[test]
fn every_prefix_matches_the_one_shot() {
    for (text, variant, pairs) in spec_cases() {
        let buffer = buf(text);
        let n = buffer.len();
        let lens: Vec<usize> = (1..=n).collect();
        let snaps = run_prefixes(&buffer, variant, &pairs, &lens);
        let reference = one_shot_snap(&buffer, variant, &pairs);

        assert_eq!(snaps.len(), n);
        assert_eq!(
            snaps.last().unwrap(),
            &reference,
            "growing one unit at a time changed the settled result of {text:?} ({})",
            vname(variant)
        );
        assert_snaps_are_prefixes(&snaps, &reference);
    }
}

#[test]
fn every_chunk_size_matches_the_one_shot() {
    for (text, variant, pairs) in spec_cases() {
        let buffer = buf(text);
        let n = buffer.len();
        let reference = one_shot_snap(&buffer, variant, &pairs);
        for k in 1..=n {
            let mut lens: Vec<usize> = (k..=n).step_by(k).collect();
            if *lens.last().unwrap() != n {
                lens.push(n);
            }
            let snaps = run_prefixes(&buffer, variant, &pairs, &lens);
            assert_eq!(
                snaps.last().unwrap(),
                &reference,
                "chunk size {k} changed the settled result of {text:?} ({})",
                vname(variant)
            );
            assert_snaps_are_prefixes(&snaps, &reference);
        }
    }
}

#[test]
fn half_then_whole_matches_the_one_shot() {
    for (text, variant, pairs) in spec_cases() {
        let buffer = buf(text);
        let n = buffer.len();
        let reference = one_shot_snap(&buffer, variant, &pairs);
        let half = n / 2;
        let lens: Vec<usize> = if half == 0 { vec![n] } else { vec![half, n] };
        let snaps = run_prefixes(&buffer, variant, &pairs, &lens);
        assert_eq!(
            snaps.last().unwrap(),
            &reference,
            "{text:?} ({})",
            vname(variant)
        );
        assert_snaps_are_prefixes(&snaps, &reference);
    }
}

#[test]
fn whole_buffer_at_once_matches_the_one_shot() {
    for (text, variant, pairs) in spec_cases() {
        let buffer = buf(text);
        let reference = one_shot_snap(&buffer, variant, &pairs);
        let snaps = run_prefixes(&buffer, variant, &pairs, &[buffer.len()]);
        assert_eq!(snaps, vec![reference], "{text:?} ({})", vname(variant));
    }
}

// ---------------------------------------------------------------------------
// partial limiter / delimiter slices are never confirmed
// ---------------------------------------------------------------------------

#[test]
fn partial_limiter_slice_is_never_a_boundary() {
    // V1 `0%%A%%1`: the '%' limiter slice is 1..3 and the delimiter slice is 4..6.
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();
    let reference = one_shot_snap(&buffer, Variant::V1, &pairs);
    assert_eq!(reference.items, vec![PH, h("0"), e("A"), h("1")]);

    let snaps = run_prefixes(&buffer, Variant::V1, &pairs, &[1, 2, 3, 4, 5, 6, 7]);

    // Prefix "0%%" stops inside the limiter slice. Two '%' repetitions already satisfy the
    // '%' pair's `least_repeat`, so the slice under the cursor *is* a limiter slice (its
    // repeats are well defined) — only the not-repeating `T` that terminates it is missing.
    // That is exactly `InternalState::UnterminatedLimiterSlice`; nothing is a confirmed
    // boundary yet, so the cache stays empty and only the NonEnvelop "0" is reported.
    assert_eq!(snaps[2].bounds, vec![]);
    assert_eq!(snaps[2].working_on, MS::N(Hole { head: 0, tail: 1 }));
    assert_eq!(snaps[2].internal, MI::UnterminatedLimiterSlice(3));
    assert_eq!(snaps[2].items, vec![h("0")]);

    // Prefix "0%%A" has the whole limiter slice, but no delimiter slice at all.
    assert_eq!(
        snaps[3].bounds,
        vec![Off {
            head: 0,
            payload_head: 0,
            payload_tail: 0,
            tail: None
        }]
    );
    assert_eq!(snaps[3].tail_non_envelop, Some(1));
    assert_eq!(
        snaps[3].working_on,
        MS::E(Off {
            head: 1,
            payload_head: 3,
            payload_tail: 4,
            tail: None
        })
    );
    assert_eq!(snaps[3].internal, MI::Clean);

    // Prefix "0%%A~" has an unterminated *delimiter* slice: still no Envelop, and for V1
    // this is `Unknown`, not `UnterminatedLimiterSlice` — the limiter slice already ended at
    // index 3 (an Envelop is being framed), and `InternalState`'s docs make
    // `UnterminatedDeLimiterSlice` legal only when `working_on` is a V2 Envelop.
    assert_eq!(snaps[4].bounds.len(), 1);
    assert_eq!(snaps[4].internal, MI::Unknown(5));
    assert!(snaps[4].bounds.iter().all(|b| b.tail.is_none()));

    // Prefix "0%%A~~" completes the delimiter slice and only then confirms the Envelop.
    assert_eq!(
        snaps[5].bounds,
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
    assert_eq!(snaps[5].internal, MI::CleanArchived);

    // ... and the last prefix is the whole buffer.
    assert_eq!(snaps[6].items, reference.items);
}

#[test]
fn partial_delimiter_slice_is_never_a_boundary() {
    // V2 `0{{A}}{{B}}??C!!!`: the confirming non-delimiter for the third Envelop is missing.
    let buffer = buf("0{{A}}{{B}}??C!!!");
    let pairs = std_pairs();
    let reference = one_shot_snap(&buffer, Variant::V2, &pairs);
    assert_eq!(
        reference.items,
        vec![PH, h("0"), e("A"), ZH, e("B"), p("C!")]
    );

    let lens: Vec<usize> = (1..=buffer.len()).collect();
    let snaps = run_prefixes(&buffer, Variant::V2, &pairs, &lens);

    for (index, snap) in snaps.iter().enumerate() {
        let confirmed = snap.bounds.iter().filter(|b| b.tail.is_some()).count();
        assert!(
            confirmed <= 2,
            "prefix #{}: {confirmed} Envelops confirmed, the third one's delimiter slice is incomplete",
            index + 1
        );
        assert!(
            !snap.items.contains(&e("C")) && !snap.items.contains(&e("C!")),
            "prefix #{}: a partially-available delimiter slice was reported as a boundary",
            index + 1
        );
    }

    // The 16-unit prefix ends inside the '!' run: an unterminated delimiter, not a boundary.
    assert_eq!(snaps[15].bounds.len(), 3);
    assert_eq!(snaps[15].internal, MI::UnterminatedDeLimiterSlice(16));
    assert_eq!(snaps[15].items.last(), Some(&p("C")));
    assert_eq!(snaps[16].internal, MI::UnterminatedDeLimiterSlice(17));
    assert_eq!(snaps[16].items.last(), Some(&p("C!")));

    // Prefixes that stop inside the third Envelop's *limiter* slice ("??"): the first '?' is
    // one repeat short of the '?' pair's `least_repeat` of 2, so the slice is not yet a
    // limiter slice (`Unknown`); the second '?' reaches `least_repeat`, so the slice *is* a
    // live limiter slice (`UnterminatedLimiterSlice`). Both are non-archived — no boundary,
    // no third Envelop.
    assert_eq!(snaps[11].internal, MI::Unknown(12));
    assert_eq!(snaps[12].internal, MI::UnterminatedLimiterSlice(13));
    assert_eq!(snaps[11].bounds.len(), snaps[12].bounds.len());
}

// ---------------------------------------------------------------------------
// decouple / new_unchecked
// ---------------------------------------------------------------------------

#[test]
fn decouple_new_unchecked_round_trip() {
    for (text, variant, pairs) in spec_cases() {
        let buffer = buf(text);
        let reference = one_shot_snap(&buffer, variant, &pairs);

        let mut fd = FlatDeserializer::new(&buffer, variant, &pairs, Vec::new(), Vec::new());
        fd.deserialize_all();
        assert_eq!(
            snap_of(&mut fd),
            reference,
            "one-shot {text:?} ({})",
            vname(variant)
        );

        // Safety: the buffer and the pair set are unchanged, so the decoupled state matches.
        let mut rebuilt = unsafe { FlatDeserializer::new_unchecked(fd.decouple()) };
        assert_eq!(
            snap_of(&mut rebuilt),
            reference,
            "decoupled {text:?} ({})",
            vname(variant)
        );
    }
}

#[test]
fn decouple_then_append_matches_an_uninterrupted_run() {
    // V2 needs one non-delimiter behind the delimiter slice to confirm it; 'z' is that unit.
    let buffer = buf("0{{A}}{{B}}??C!!!z");
    let pairs = std_pairs();
    let whole = one_shot_snap(&buffer, Variant::V2, &pairs);
    assert_eq!(
        whole.items,
        vec![PH, h("0"), e("A"), ZH, e("B"), ZH, e("C!"), h("z")]
    );

    // Decode only the first 17 units (where the third Envelop is pending) ...
    let mut fd = FlatDeserializer::new(&buffer[..17], Variant::V2, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(
        mi_of(fd.deserializer_state().internal()),
        MI::UnterminatedDeLimiterSlice(17)
    );

    // ... then hand the deserializer the buffer with 'z' appended, keeping the state.
    let (_, bounds, tail_non_envelop, state) = fd.decouple();
    // Safety: the new buffer is the old buffer plus the appended 'z'.
    let mut grown =
        unsafe { FlatDeserializer::new_unchecked((&buffer[..], bounds, tail_non_envelop, state)) };
    grown.deserialize_all();
    assert_eq!(snap_of(&mut grown), whole);
}

// ---------------------------------------------------------------------------
// advance then replace_buffer
// ---------------------------------------------------------------------------

#[test]
fn advance_then_replace_buffer_matches_a_fresh_run() {
    let buffer = buf("0%%A%%%%B%%%%C%%1");
    let pairs = std_pairs();

    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(
        adv(&mut fd, 1),
        1,
        "only the leading NonEnvelop '0' is fully covered"
    );
    replace(&mut fd, &buffer[1..]);
    let items = items_of(&mut fd);
    let bounds = offs(fd.archived_boundaries());

    let fresh = one_shot(&buffer[1..], Variant::V1, &pairs);
    assert_eq!(items, fresh.items);
    assert_eq!(bounds, fresh.bounds);
    assert_eq!(
        items,
        vec![e("A"), ZH, e("B"), ZH, e("C"), h("1")],
        "the same channel without its leading NonEnvelop"
    );
}
