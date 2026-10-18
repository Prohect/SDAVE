//! Integration tests for SDAVE.
//!
//! These tests drive the public API from an external crate (with `std`
//! available), exercising framing/deframing, state transitions, serializer
//! helpers and the documented README examples for both `Variant::V1` and
//! `Variant::V2`.

use std::{alloc::Global, iter::repeat_n, num::NonZeroUsize};

use sdave::{
    Cursor2D, Envelop, FlatDeserializer, FlatDeserializerState, InternalState, Item, LimiterPair,
    NonEnvelop, Offset, Slices, Stack, Variant, can_serialize, deserialize_incremental,
    get_all_serializable_limiter_pairs,
};

/// A `LimiterPair` whose `limiter`/`delimiter` storage is leaked to `'static`,
/// keeping every test free of borrow-lifetime plumbing.
type LP = LimiterPair<'static, u8, Global>;

/// Build a `LimiterPair` over byte slices.
///
/// The slice storage is leaked so the pair can live for `'static`.
fn pair(least_repeat: usize, limiter: &[u8], delimiter: &[u8]) -> LP {
    let limiter: &'static Vec<u8> = &*Box::leak(Box::new(limiter.to_vec()));
    let delimiter: &'static Vec<u8> = &*Box::leak(Box::new(delimiter.to_vec()));
    LimiterPair::new(NonZeroUsize::new(least_repeat), limiter, delimiter)
}

/// Render an `Item` against the buffer it indexes into.
fn render(buffer: &[u8], item: &Item) -> String {
    match item {
        Item::E { envelop } if envelop.is_phantom() => "E[]".to_string(),
        Item::E { envelop } => format!(
            "E[{}]",
            String::from_utf8_lossy(
                &buffer[envelop.payload_head_offset.get()..envelop.payload_tail_offset.get()]
            )
        ),
        Item::N { non_envelop } if non_envelop.is_phantom() => "N[]".to_string(),
        Item::N { non_envelop } => format!(
            "N[{}]",
            String::from_utf8_lossy(
                &buffer[non_envelop.head_offset.get()..non_envelop.tail_offset.get()]
            )
        ),
    }
}

/// Fully deserialize `buffer` via [`FlatDeserializer::iter`], rendering every
/// item (including phantom markers) as a string.
fn collect_all(buffer: &[u8], variant: Variant, pairs: Vec<LP>) -> Vec<String> {
    let mut fd = FlatDeserializer::new(buffer, variant, pairs, Vec::new(), Vec::new());
    fd.iter().map(|item| render(buffer, &item)).collect()
}

/// Like [`collect_all`], but phantom markers are skipped, yielding only the
/// logical item stream.
fn collect_logical(buffer: &[u8], variant: Variant, pairs: Vec<LP>) -> Vec<String> {
    let mut fd = FlatDeserializer::new(buffer, variant, pairs, Vec::new(), Vec::new());
    let mut out = Vec::new();
    for item in fd.iter() {
        let phantom = match &item {
            Item::E { envelop } => envelop.is_phantom(),
            Item::N { non_envelop } => non_envelop.is_phantom(),
        };
        if !phantom {
            out.push(render(buffer, &item));
        }
    }
    out
}

/// Build a `Vec<String>` from a list of `&str`, for compact `assert_eq!`s.
fn strs(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

/// Frame a single payload with `limiter`/`delimiter` repeated `n` times each.
fn frame(limiter: u8, delimiter: u8, n: usize, payload: Option<&[u8]>) -> Vec<u8> {
    let mut buf = Vec::new();
    for _ in 0..n {
        buf.push(limiter);
    }
    buf.extend_from_slice(payload.unwrap());
    for _ in 0..n {
        buf.push(delimiter);
    }
    buf
}

// ---------------------------------------------------------------------------
// Offset / Offset
// ---------------------------------------------------------------------------

#[test]
fn non_max_usize_roundtrip() {
    assert_eq!(Offset::new(0).get(), 0);
    assert_eq!(Offset::new(1).get(), 1);
    assert_eq!(Offset::new(usize::MAX - 1).get(), usize::MAX - 1);
}

#[test]
#[should_panic(expected = "[SDAVE] NonMaxUsize exceeds usize::MAX")]
fn non_max_usize_rejects_max() {
    let _ = Offset::new(usize::MAX);
}

#[test]
fn non_max_usize_decouple() {
    let x = Offset::new(5);
    assert_eq!(x.decouple().get(), usize::MAX - 5);

    // `decouple` then `new_unchecked` is an identity on the held value.
    let y = unsafe { Offset::new_unchecked(Offset::new(5).decouple()) };
    assert_eq!(y.get(), 5);
}

#[test]
fn non_max_usize_ordering() {
    assert!(Offset::new(1) < Offset::new(2));
    assert!(Offset::new(2) > Offset::new(1));
    assert!(Offset::new(0) < Offset::new(usize::MAX - 1));
}

// ---------------------------------------------------------------------------
// Item / Envelop / NonEnvelop
// ---------------------------------------------------------------------------

#[test]
fn phantom_markers() {
    let offset = Offset::new(4);

    let pe = unsafe { Envelop::phantom(offset) };
    assert!(pe.is_phantom());
    assert!(!pe.is_archived());

    let pn = unsafe { NonEnvelop::phantom(offset) };
    assert!(pn.is_phantom());
}

#[test]
fn item_offsets() {
    let envelop = unsafe {
        Envelop::new_unchecked(
            Offset::new(1),
            Offset::new(3),
            Offset::new(5),
            Some(Offset::new(7)),
        )
    };
    let item = Item::E { envelop };
    assert_eq!(item.head_offset().get(), 1);
    // Archived: semantical tail is the envelope's tail offset.
    assert_eq!(item.semantical_tail_offset().get(), 7);

    let envelop =
        unsafe { Envelop::new_unchecked(Offset::new(1), Offset::new(3), Offset::new(5), None) };
    let item = Item::E { envelop };
    assert_eq!(item.head_offset().get(), 1);
    // Unarchived: semantical tail falls back to payload tail offset.
    assert_eq!(item.semantical_tail_offset().get(), 5);

    let non_envelop = Item::N {
        non_envelop: unsafe { NonEnvelop::new_unchecked(Offset::new(2), Offset::new(9)) },
    };
    assert_eq!(non_envelop.head_offset().get(), 2);
    assert_eq!(non_envelop.semantical_tail_offset().get(), 9);
}

// ---------------------------------------------------------------------------
// Deserialization: V1 (documented README examples + basic shapes)
// ---------------------------------------------------------------------------

#[test]
fn v1_basic() {
    // `abc` is a NonEnvelop, `^^XYZ~~` an Envelop, `def` a trailing NonEnvelop.
    let buf = b"abc^^XYZ~~def";
    let logical = collect_logical(buf, Variant::V1, vec![pair(2, b"^", b"~")]);
    assert_eq!(
        logical,
        strs(&["N[abc]", "E[XYZ]", "N[def]"]),
        "payload: {}",
        String::from_utf8_lossy(buf)
    );
}

#[test]
fn v1_iter_full_sequence_includes_phantoms() {
    let buf = b"abc^^XYZ~~def";
    let full = collect_all(buf, Variant::V1, vec![pair(2, b"^", b"~")]);
    assert_eq!(full, strs(&["E[]", "N[abc]", "E[XYZ]", "N[def]"]));
}

#[test]
fn v1_multiple_pairs() {
    let buf = b"x^^A~~y{{B}}z";
    let pairs = vec![pair(2, b"^", b"~"), pair(2, b"{", b"}")];
    let logical = collect_logical(buf, Variant::V1, pairs);
    assert_eq!(logical, strs(&["N[x]", "E[A]", "N[y]", "E[B]", "N[z]"]));
}

#[test]
fn v1_readme_percent_example() {
    // `('%', '%', 2)` — limiter == delimiter is legal in SDAVE.
    let buf = b"0%%A%%%%B%%%%C%%1";
    let logical = collect_logical(buf, Variant::V1, vec![pair(2, b"%", b"%")]);
    assert_eq!(logical, strs(&["N[0]", "E[A]", "E[B]", "E[C]", "N[1]"]));
}

#[test]
fn v1_readme_caret_example() {
    // `('^', '~', 2)`; the three leading carets are absorbed as the limiter
    // slice, the run of five tildes splits into `~~~` delimiter + `~~2` tail.
    let buf = b"1^^^echo^^hello~~~~~2";
    let logical = collect_logical(buf, Variant::V1, vec![pair(2, b"^", b"~")]);
    assert_eq!(logical, strs(&["N[1]", "E[echo^^hello]", "N[~~2]"]));
}

// ---------------------------------------------------------------------------
// Deserialization: V2 (documented README examples)
// ---------------------------------------------------------------------------

#[test]
fn v2_readme_brace_example() {
    let buf = b"0{{A}}{{B}}??C!!1";
    let pairs = vec![pair(2, b"{", b"}"), pair(2, b"?", b"!")];
    let logical = collect_logical(buf, Variant::V2, pairs);
    assert_eq!(logical, strs(&["N[0]", "E[A]", "E[B]", "E[C]", "N[1]"]));
}

#[test]
fn v2_readme_brace_pending() {
    // The trailing `!!!` leaves C's delimiter unconfirmed: the extra `!` is
    // absorbed into the pending payload.
    let buf = b"0{{A}}{{B}}??C!!!";
    let pairs = vec![pair(2, b"{", b"}"), pair(2, b"?", b"!")];
    let logical = collect_logical(buf, Variant::V2, pairs);
    assert_eq!(logical, strs(&["N[0]", "E[A]", "E[B]", "E[C!]"]));
}

#[test]
fn v2_readme_caret_example() {
    let buf = b"1^^^echo^^hello~~~~~2";
    let logical = collect_logical(buf, Variant::V2, vec![pair(2, b"^", b"~")]);
    assert_eq!(logical, strs(&["N[1]", "E[echo^^hello~~]", "N[2]"]));
}

#[test]
fn v2_readme_percent_pending() {
    // V2 absorbs the first `%%` after `A` into A's payload, so `B` becomes a
    // NonEnvelop and `C%%1` stays pending.
    let buf = b"0%%A%%%%B%%%%C%%1";
    let logical = collect_logical(buf, Variant::V2, vec![pair(2, b"%", b"%")]);
    assert_eq!(logical, strs(&["N[0]", "E[A%%]", "N[B]", "E[C%%1]"]));
}

// ---------------------------------------------------------------------------
// FlatDeserializerState / InternalState transitions
// ---------------------------------------------------------------------------

#[test]
fn state_new_starts_archived() {
    let state = FlatDeserializerState::new(Variant::V1, vec![pair(2, b"^", b"~")], Vec::new());
    assert!(state.internal().is_archived());
    assert!(!state.internal().is_exhausted());
    match state.working_on() {
        Item::N { non_envelop } => assert!(non_envelop.is_phantom()),
        _ => panic!("fresh state must start from a phantom NonEnvelop"),
    }
    // A single valid pair seeds one repeat-map entry reset to zero.
    let repeat_map = unsafe { state.repeat_map() }.unwrap();
    assert_eq!(repeat_map.len(), 1);
    assert_eq!(repeat_map[0].unwrap().get(), 0);
}

#[test]
#[should_panic(expected = "[SDAVE] empty limiter_pairs")]
fn flat_deserializer_state_new_empty_pairs_panics() {
    let pairs: Vec<LimiterPair<'static, u8, Global>> = Vec::new();
    let repeat_map: Vec<Option<Offset>, Global> = Vec::new();
    let _ = FlatDeserializerState::new(Variant::V1, pairs, repeat_map);
}

#[test]
fn state_clean_archived_v1() {
    let buf = b"^^A~~";
    let mut fd = FlatDeserializer::new(
        buf,
        Variant::V1,
        vec![pair(2, b"^", b"~")],
        Vec::new(),
        Vec::new(),
    );
    fd.deserialize_all();
    let st = fd.deserializer_state();
    assert!(st.internal().is_clean());
    assert!(st.internal().is_archived());
    assert!(st.internal().is_exhausted());
    assert!(matches!(st.internal(), InternalState::CleanArchived));
    match st.working_on() {
        Item::E { envelop } => {
            assert!(envelop.is_archived());
            assert_eq!(
                &buf[envelop.payload_head_offset.get()..envelop.payload_tail_offset.get()],
                b"A"
            );
        }
        _ => panic!("expected an archived Envelop"),
    }
}

#[test]
fn state_clean_non_envelop() {
    let buf = b"plain";
    let mut fd = FlatDeserializer::new(
        buf,
        Variant::V1,
        vec![pair(2, b"^", b"~")],
        Vec::new(),
        Vec::new(),
    );
    fd.deserialize_all();
    let st = fd.deserializer_state();
    assert!(st.internal().is_clean());
    assert!(!st.internal().is_archived());
    assert!(st.internal().is_exhausted());
    assert!(matches!(st.internal(), InternalState::Clean));
    match st.working_on() {
        Item::N { non_envelop } => {
            assert_eq!(non_envelop.head_offset.get(), 0);
            assert_eq!(non_envelop.tail_offset.get(), 5);
        }
        _ => panic!("expected a NonEnvelop"),
    }
}

#[test]
fn state_unarchived_envelop_v1() {
    // Limiter + payload, but no delimiter yet: the Envelop stays unarchived.
    let buf = b"^^A";
    let mut fd = FlatDeserializer::new(
        buf,
        Variant::V1,
        vec![pair(2, b"^", b"~")],
        Vec::new(),
        Vec::new(),
    );
    fd.deserialize_all();
    let st = fd.deserializer_state();
    assert!(st.internal().is_clean());
    assert!(!st.internal().is_archived());
    assert!(st.internal().is_exhausted());
    match st.working_on() {
        Item::E { envelop } => {
            assert!(!envelop.is_archived());
            assert_eq!(envelop.payload_head_offset.get(), 2);
            assert_eq!(envelop.payload_tail_offset.get(), 3);
        }
        _ => panic!("expected an unarchived Envelop"),
    }
}

#[test]
fn deserialize_incremental_step() {
    let buf = b"^^A~~";
    let mut state = FlatDeserializerState::new(Variant::V1, vec![pair(2, b"^", b"~")], Vec::new());
    let new_state = unsafe { deserialize_incremental(buf, &mut state) };
    assert!(new_state.internal().is_archived());
    assert!(matches!(new_state.internal(), InternalState::CleanArchived));
    match new_state.working_on() {
        Item::E { envelop } => assert_eq!(
            &buf[envelop.payload_head_offset.get()..envelop.payload_tail_offset.get()],
            b"A"
        ),
        _ => panic!("expected an archived Envelop"),
    }
}

#[test]
fn deserialize_incremental_idempotent_when_exhausted() {
    let buf = b"^^A~~";
    let mut state = FlatDeserializerState::new(Variant::V1, vec![pair(2, b"^", b"~")], Vec::new());
    let s1 = unsafe { deserialize_incremental(buf, &mut state) };
    let mut s1 = s1;
    let s2 = unsafe { deserialize_incremental(buf, &mut s1) };
    assert!(s1.internal().is_exhausted());
    assert!(s2.internal().is_exhausted());
    assert!(matches!(s2.internal(), InternalState::CleanArchived));
}

// ---------------------------------------------------------------------------
// FlatDeserializer::advance
// ---------------------------------------------------------------------------

#[test]
fn advance_zero_is_noop() {
    let buf = b"a^^XYZ~~bcdef";
    let mut fd = FlatDeserializer::new(
        buf,
        Variant::V1,
        vec![pair(2, b"^", b"~")],
        Vec::new(),
        Vec::new(),
    );
    fd.deserialize_all();
    let removed = unsafe { fd.advance(Offset::new(0)) };
    assert_eq!(removed, 0);
    // phantom marker + archived Envelop.
    assert_eq!(unsafe { fd.archived_boundaries() }.unwrap().len(), 2);
}

#[test]
fn advance_drops_and_shifts() {
    let buf = b"abc^^XYZ~~def";
    let mut fd = FlatDeserializer::new(
        buf,
        Variant::V1,
        vec![pair(2, b"^", b"~")],
        Vec::new(),
        Vec::new(),
    );
    fd.deserialize_all();

    // Drop the leading NonEnvelop `abc` (plus its phantom marker).
    let removed = unsafe { fd.advance(Offset::new(3)) };
    assert_eq!(removed, 1);

    let boundaries = unsafe { fd.archived_boundaries() }.unwrap();
    assert_eq!(boundaries.len(), 1);
    let envelop = &boundaries[0];
    assert_eq!(envelop.head_offset.get(), 0);
    assert_eq!(envelop.payload_head_offset.get(), 2);
    assert_eq!(envelop.payload_tail_offset.get(), 5);
    assert_eq!(envelop.tail_offset.unwrap().get(), 7);
    assert!(fd.tail_non_envelop().is_none());

    // The shifted offsets remain consistent with an actually-advanced buffer.
    let advanced = &buf[3..];
    assert_eq!(
        &advanced[envelop.payload_head_offset.get()..envelop.payload_tail_offset.get()],
        b"XYZ"
    );
}

// ---------------------------------------------------------------------------
// Serializer helpers: can_serialize / get_all_serializable_limiter_pairs
// ---------------------------------------------------------------------------

#[test]
fn can_serialize_minimal() {
    let payload: Option<&[u8]> = Some(b"XYZ");
    let mut lp = pair(2, b"^", b"~");
    let repeat = unsafe { can_serialize(b"abc", &[payload], Variant::V1, &mut lp) };
    assert_eq!(repeat.map(|n| n.get()), Some(2));
}

#[test]
fn can_serialize_rejects_buffer_ending_with_limiter() {
    let payload: Option<&[u8]> = Some(b"XYZ");
    let mut lp = pair(2, b"^", b"~");
    assert!(unsafe { can_serialize(b"abc^", &[payload], Variant::V1, &mut lp) }.is_none());
}

#[test]
fn can_serialize_rejects_payload_starting_with_limiter() {
    let payload: Option<&[u8]> = Some(b"^XYZ");
    let mut lp = pair(2, b"^", b"~");
    assert!(unsafe { can_serialize(b"abc", &[payload], Variant::V1, &mut lp) }.is_none());
}

#[test]
fn can_serialize_v1_rejects_trailing_delimiter_v2_accepts() {
    let payload: Option<&[u8]> = Some(b"XYZ~");
    let mut lp = pair(2, b"^", b"~");
    assert!(unsafe { can_serialize(b"abc", &[payload], Variant::V1, &mut lp) }.is_none());
    // V2 strips a trailing delimiter run before measuring collisions.
    assert_eq!(
        unsafe { can_serialize(b"abc", &[payload], Variant::V2, &mut lp) }.map(|n| n.get()),
        Some(2)
    );
}

#[test]
fn can_serialize_delimiter_run_increases_repeat() {
    let payload: Option<&[u8]> = Some(b"a~~b");
    let mut lp = pair(2, b"^", b"~");
    // A payload containing `~~` forces at least three delimiter repeats.
    assert_eq!(
        unsafe { can_serialize(b"abc", &[payload], Variant::V1, &mut lp) }.map(|n| n.get()),
        Some(3)
    );
    assert_eq!(
        unsafe { can_serialize(b"abc", &[payload], Variant::V2, &mut lp) }.map(|n| n.get()),
        Some(3)
    );
}

#[test]
fn can_serialize_empty_limiter_or_delimiter_rejected() {
    let payload: Option<&[u8]> = Some(b"XYZ");
    let mut lp = pair(2, b"", b"~");
    assert!(unsafe { can_serialize(b"abc", &[payload], Variant::V1, &mut lp) }.is_none());

    let mut lp = pair(2, b"^", b"");
    assert!(unsafe { can_serialize(b"abc", &[payload], Variant::V1, &mut lp) }.is_none());
}

#[test]
fn get_all_serializable_limiter_pairs_collects() {
    let payload: Option<&[u8]> = Some(b"X~~}}Y!!Z");
    let mut pairs = vec![
        pair(2, b"^", b"~"),
        pair(2, b"{", b"}"),
        pair(2, b"?", b"!"),
    ];
    let mut result = Vec::new();
    unsafe {
        get_all_serializable_limiter_pairs(b"abc", &[payload], Variant::V1, &mut pairs, &mut result)
    };
    assert_eq!(result.len(), 3);
    for p in &result {
        assert!(p.least_repeat().unwrap().get() >= 3);
        assert!(!p.limiter().is_empty());
        assert!(!p.delimiter().is_empty());
    }
}

#[test]
fn get_all_serializable_limiter_pairs_filters() {
    // The buffer tail `^` collides with the first pair's limiter, so only the
    // `{}` pair remains serializable.
    let payload: Option<&[u8]> = Some(b"XYZ");
    let mut pairs = vec![pair(2, b"^", b"~"), pair(2, b"{", b"}")];
    let mut result = Vec::new();
    unsafe {
        get_all_serializable_limiter_pairs(b"ab^", &[payload], Variant::V1, &mut pairs, &mut result)
    };
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].limiter().as_slice(), &b"{"[..]);
    assert_eq!(result[0].delimiter().as_slice(), &b"}"[..]);
}

#[test]
fn is_limiter_pairs_valid() {
    // Distinct limiter/delimiter slices across pairs are valid.
    let p1 = pair(2, b"^", b"~");
    let p2 = pair(2, b"{", b"}");
    assert!(FlatDeserializerState::is_limiter_pairs_valid(&vec![p1, p2]));

    // limiter == delimiter is rejected.
    let bad = pair(2, b"^", b"^");
    assert!(!FlatDeserializerState::is_limiter_pairs_valid(&vec![bad]));

    // A slice shared across pairs (delimiter of A == limiter of B) is rejected.
    let a = pair(2, b"^", b"~");
    let b = pair(2, b"~", b"%");
    assert!(!FlatDeserializerState::is_limiter_pairs_valid(&vec![a, b]));
}

// ---------------------------------------------------------------------------
// Round trips: frame with can_serialize, then deframe.
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_v1_simple() {
    let payload: Option<&[u8]> = Some(b"hello world");
    let mut lp = pair(2, b"^", b"~");
    let n = unsafe { can_serialize(b"", &[payload], Variant::V1, &mut lp) }
        .unwrap()
        .get();
    assert_eq!(n, 2);

    let buf = frame(b'^', b'~', n, payload);
    let logical = collect_logical(&buf, Variant::V1, vec![pair(2, b"^", b"~")]);
    assert_eq!(logical, strs(&["E[hello world]"]));
}

#[test]
fn roundtrip_v1_collision() {
    // `a~~b` collides with a two-repeat `~` delimiter, so the repeat grows.
    let payload: Option<&[u8]> = Some(b"a~~b");
    let mut lp = pair(2, b"^", b"~");
    let n = unsafe { can_serialize(b"", &[payload], Variant::V1, &mut lp) }
        .unwrap()
        .get();
    assert_eq!(n, 3);

    let buf = frame(b'^', b'~', n, payload);
    let logical = collect_logical(&buf, Variant::V1, vec![pair(2, b"^", b"~")]);
    assert_eq!(logical, strs(&["E[a~~b]"]));
}

#[test]
fn roundtrip_v1_multi_slice_payload() {
    let parts: [Option<&[u8]>; 3] = [Some(b"hello "), Some(b"beautiful "), Some(b"world")];
    let payload = &parts;
    let mut lp = pair(2, b"^", b"~");
    let n = unsafe { can_serialize(b"", payload, Variant::V1, &mut lp) }
        .unwrap()
        .get();
    assert_eq!(n, 2);

    let mut buf = Vec::new();
    buf.extend(repeat_n(b'^', n));
    for part in payload {
        buf.extend_from_slice(part.unwrap());
    }
    buf.extend(repeat_n(b'~', n));

    let logical = collect_logical(&buf, Variant::V1, vec![pair(2, b"^", b"~")]);
    assert_eq!(logical, strs(&["E[hello beautiful world]"]));
}

#[test]
fn roundtrip_v2_simple() {
    let payload: Option<&[u8]> = Some(b"hello world");
    let mut lp = pair(2, b"^", b"~");
    let n = unsafe { can_serialize(b"", &[payload], Variant::V2, &mut lp) }
        .unwrap()
        .get();
    assert_eq!(n, 2);
    let mut buf = frame(b'^', b'~', n, payload);

    let mut fd = FlatDeserializer::new(
        &buf,
        Variant::V2,
        vec![pair(2, b"^", b"~")],
        Vec::new(),
        Vec::new(),
    );
    let items: Vec<Item> = fd.iter().collect();
    let item = items.last().unwrap();
    assert!(matches!(item, Item::E { envelop } if !envelop.is_archived()));

    let logical = collect_logical(&buf, Variant::V2, vec![pair(2, b"^", b"~")]);
    assert_eq!(logical, strs(&["E[hello world]"]));

    // V2 needs a following non-delimiter to confirm the delimiter slice so that it can archive one Envelop.
    buf.push(b'X');

    let logical = collect_logical(&buf, Variant::V2, vec![pair(2, b"^", b"~")]);
    assert_eq!(logical, strs(&["E[hello world]", "N[X]"]));
}

#[test]
fn serialize_onto_existing_tail() {
    let tail = b"abc";
    let payload: &[u8] = b"XYZ";
    let mut lp = pair(2, b"^", b"~");
    let n = unsafe { can_serialize(tail, &[Some(payload)], Variant::V1, &mut lp) }
        .unwrap()
        .get();
    assert_eq!(n, 2);

    let mut buf = tail.to_vec();
    buf.extend(repeat_n(b'^', n));
    buf.extend_from_slice(payload);
    buf.extend(repeat_n(b'~', n));

    let logical = collect_logical(&buf, Variant::V1, vec![pair(2, b"^", b"~")]);
    assert_eq!(logical, strs(&["N[abc]", "E[XYZ]"]));
}

// ---------------------------------------------------------------------------
// Iterator / edge cases
// ---------------------------------------------------------------------------

#[test]
fn iter_from_skips_cached_items() {
    let buf = b"abc^^XYZ~~def";
    let mut fd = FlatDeserializer::new(
        buf,
        Variant::V1,
        vec![pair(2, b"^", b"~")],
        Vec::new(),
        Vec::new(),
    );
    let items: Vec<String> = fd.iter_from(2).map(|item| render(buf, &item)).collect();
    // Skipping the phantom marker (pos 0) and the NonEnvelop `abc` (pos 1).
    assert_eq!(items, strs(&["E[XYZ]", "N[def]"]));
}

#[test]
fn empty_buffer_yields_no_logical_items() {
    let buf: &[u8] = b"";
    assert!(collect_logical(buf, Variant::V1, vec![pair(2, b"^", b"~")]).is_empty());
}

#[test]
fn test_stack() {
    let mut sum = 0usize;
    unsafe {
        Stack::with(
            core::iter::once(69).chain(0..6),
            |values| values.next().map(Offset::new),
            // `mutatable` carries the side effect out of the deepest frame; the consumer itself captures nothing.
            |stack, sum: &mut usize| {
                for data in stack.iter() {
                    *sum += data.get();
                }
            },
            &mut sum,
        );
    }
    assert_eq!(sum, 84);
}

#[test]
fn test_v2_cache() {
    let buffer = b"^^^^^^^e: derive::EqualWidths^^A^^(u32)^^1~~~~~~tail: u32^^7~~~~~~~~~";
    let expected_payload = b"e: derive::EqualWidths^^A^^(u32)^^1~~~~~~tail: u32^^7~~";
    let mut fd = FlatDeserializer::new(
        buffer,
        Variant::V2,
        vec![pair(2, b"^", b"~")],
        Vec::new(),
        Vec::new(),
    );
    if let Item::E { envelop } = fd.iter().next().unwrap() {
        assert!(
            &buffer[envelop.payload_head_offset.get()..envelop.payload_tail_offset.get()]
                == expected_payload
        );
        let internal_state = fd.deserializer_state().internal().clone();
        assert!(matches!(
            internal_state,
            InternalState::UnterminatedDeLimiterSlice { .. }
        ))
    } else {
        panic!("expect envelop")
    }
}

/// A `None` entry and an empty `Some` entry carry no `T`, so a cursor steps over both.
#[test]
fn forward_cursor_steps_over_entries_that_carry_no_t() {
    let first = [1u8, 2];
    let empty: [u8; 0] = [];
    let second = [3u8];
    let payload = [Some(&first[..]), None, Some(&empty[..]), Some(&second[..])];
    let view = Slices(&payload);
    let mut cursor = view.head();
    assert_eq!(cursor, unsafe { Cursor2D::new_unchecked(0, 0) });
    view.forward_cursor(&mut cursor, 2);
    assert_eq!(cursor, unsafe { Cursor2D::new_unchecked(3, 0) });
    // Past the end it parks on the end of the last entry that carries a `T`.
    view.forward_cursor(&mut cursor, 1);
    assert_eq!(cursor, unsafe { Cursor2D::new_unchecked(3, 1) });
    assert_eq!(view.len_till(&cursor), 3);
    assert_eq!(view.len_until(&cursor), 0);
}

#[test]
fn head_and_tail_skip_padding() {
    let first = [1u8];
    let second = [2u8, 3];
    let payload = [None, Some(&first[..]), None, Some(&second[..]), None];
    let view = Slices(&payload);
    assert_eq!(view.head(), unsafe { Cursor2D::new_unchecked(1, 0) });
    assert_eq!(view.tail(), unsafe { Cursor2D::new_unchecked(3, 2) });
    assert_eq!(view.len(), 3);
    assert!(!view.is_empty());
}

#[test]
fn readers_see_the_slices_flattened() {
    let first = [1u8, 2];
    let second = [3u8, 4];
    let payload = [Some(&first[..]), None, Some(&second[..])];
    let view = Slices(&payload);
    let mut cursor = unsafe { Cursor2D::new_unchecked(0, 1) };
    assert!(view.starts_with(&cursor, &[2, 3]));
    assert!(!view.starts_with(&cursor, &[2, 4]));
    assert_eq!(view.len_till(&cursor), 1);
    assert_eq!(view.len_until(&cursor), 3);
    view.forward_cursor(&mut cursor, 2);
    assert_eq!(cursor, unsafe { Cursor2D::new_unchecked(2, 1) });
    assert_eq!(view.len_till(&cursor), 3);

    let tail = view.tail();
    assert!(view.ends_with(&tail, &[3, 4]));
    assert!(view.ends_with(&tail, &[1, 2, 3, 4]));
    assert!(view.ends_with(&tail, &[2, 3, 4]));
    assert!(!view.ends_with(&tail, &[1, 3, 4]));
    // The end-of-entry position is a cursor `ends_with` accepts, and it reaches into the entries
    // before it.
    assert!(view.ends_with(&unsafe { Cursor2D::new_unchecked(2, 0) }, &[1, 2]));
    assert!(view.ends_with(&unsafe { Cursor2D::new_unchecked(2, 0) }, &[2]));
    assert!(view.ends_with(&unsafe { Cursor2D::new_unchecked(2, 1) }, &[2, 3]));
}

#[test]
fn backward_cursor_walks_into_the_entry_before() {
    let first = [1u8, 2];
    let second = [3u8, 4];
    let payload = [Some(&first[..]), Some(&second[..])];
    let view = Slices(&payload);
    let mut cursor = view.tail();
    assert_eq!(cursor, unsafe { Cursor2D::new_unchecked(1, 2) });
    view.backward_cursor(&mut cursor, 1);
    assert_eq!(cursor, unsafe { Cursor2D::new_unchecked(1, 1) });
    view.backward_cursor(&mut cursor, 2);
    assert_eq!(cursor, unsafe { Cursor2D::new_unchecked(0, 1) });
    view.backward_cursor(&mut cursor, 1);
    assert_eq!(cursor, unsafe { Cursor2D::new_unchecked(0, 0) });
}
