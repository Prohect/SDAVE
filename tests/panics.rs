//! Every panic the core documents.

mod common;

use common::*;
use sdave::{FlatDeserializer, FlatDeserializerState, LimiterPair, NonMaxUsize, Variant};

#[test]
#[should_panic(expected = "[SDAVE] NonMaxUsize exceeds usize::MAX")]
fn non_max_usize_new_panics_on_usize_max() {
    let _ = NonMaxUsize::new(usize::MAX);
}

#[test]
#[should_panic(expected = "[SDAVE] empty limiter_pairs")]
fn flat_deserializer_new_panics_on_empty_pairs() {
    let buffer = buf("^^A~~");
    let empty: Pairs = Vec::new();
    let _ = FlatDeserializer::new(&buffer, Variant::V1, &empty, Vec::new(), Vec::new());
}

#[test]
#[should_panic(expected = "assertion failed: offset.get() <= tail")]
fn advance_beyond_the_confirmed_tail_panics() {
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    // The settled state's semantical tail is 7 == buffer.len(); 8 is beyond it.
    assert_eq!(
        fd.deserializer_state()
            .working_on()
            .semantical_tail_offset()
            .get(),
        7
    );
    let _ = adv(&mut fd, 8);
}

#[test]
#[should_panic(expected = "assertion failed: offset.get() <= tail")]
fn state_advance_beyond_the_confirmed_tail_panics() {
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();
    let mut state = raw_fixpoint(&buffer, Variant::V1, &pairs);
    assert_eq!(state.working_on().semantical_tail_offset().get(), 7);
    unsafe { state.advance(NonMaxUsize::new(9)) };
}

#[test]
#[should_panic(expected = "assertion failed")]
fn limiter_pair_new_panics_when_a_side_is_uninited() {
    let _p: LimiterPair<'static, char, std::alloc::Global> = LimiterPair::new(
        Some(core::num::NonZeroUsize::new(2).unwrap()),
        core::cell::OnceCell::from(unit("^")),
        core::cell::OnceCell::new(),
    );
}

#[test]
fn advance_at_the_confirmed_tail_is_allowed() {
    // The documented panic is `>`, not `>=`: the boundary itself is legal.
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(adv(&mut fd, 7), 2);
    let buffer = buf("0%%A%%1");
    let pairs = std_pairs();
    let mut state = raw_fixpoint(&buffer, Variant::V1, &pairs);
    unsafe { state.advance(NonMaxUsize::new(7)) };
}

#[test]
fn flat_deserializer_new_is_fine_with_one_pair() {
    let buffer = buf("^^A~~");
    let pairs = vec![pair(2, "^", "~")];
    let mut fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    assert_eq!(items_of(&mut fd), vec![e("A")]);
    let _ = FlatDeserializerState::new(Variant::V1, &pairs, Vec::new());
}
