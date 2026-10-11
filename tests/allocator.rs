//! The app-supplied allocator: the same scenarios must run with `Global`, with a fixed
//! bump arena, and with a counting allocator that exposes every byte the deserializer holds.

mod common;

use core::mem::{align_of, size_of};

use common::*;
use sdave::{Envelop, FlatDeserializer, LimiterPair, NonMaxUsize, Variant};

type Spec = &'static [(&'static str, &'static str, usize)];

fn alloc_cases() -> Vec<(&'static str, Variant, Spec)> {
    vec![
        ("0%%A%%%%B%%%%C%%1", Variant::V1, &STD_SPECS[..]),
        ("0%%A%%%%B%%%%C%%1%", Variant::V1, &STD_SPECS[..]),
        ("0%%%%%%B%%%%C%%1", Variant::V1, &STD_SPECS[..]),
        ("1^^^echo^^hello~~~~~2", Variant::V1, &STD_SPECS[..]),
        ("0{{A}}{{B}}??C!!!", Variant::V2, &STD_SPECS[..]),
        ("0%%A%%%%B%%%%C%%1", Variant::V2, &STD_SPECS[..]),
        ("0^^A~~~~B%%~~C%%1", Variant::V1, &CROSSING_SPECS[..]),
        ("0^^A~~~~B%%~~C%%1", Variant::V2, &CROSSING_SPECS[..]),
        ("1^^A!B!~~~~C", Variant::V1, &SHARED_SPECS_README[..]),
        ("1^^A!B!~~~~C", Variant::V2, &SHARED_SPECS_README[..]),
        ("1^^A!B!~~~~C", Variant::V2, &SHARED_SPECS_REORDERED[..]),
        ("0ababXcdcd", Variant::V1, &MULTI_SPECS[..]),
    ]
}

#[test]
fn a_fixed_arena_budget_yields_the_same_result_as_global() {
    for (text, variant, specs) in alloc_cases() {
        let buffer = buf(text);
        let arena = Arena::new(1 << 16);
        let (shot, requests) = run_scenario(arena.clone(), &buffer, variant, specs);
        let global = one_shot(&buffer, variant, &specs_pairs(specs));

        assert_eq!(shot, global, "{text:?} ({})", vname(variant));
        assert!(!requests.is_empty(), "the arena was never used");
        assert!(arena.used() > 0 && arena.used() <= 1 << 16);
    }
}

#[test]
fn custom_allocators_see_an_identical_request_sequence() {
    let buffer = buf("0%%A%%1");
    let (shot_counting, requests_counting) =
        run_scenario(Counting::new(), &buffer, Variant::V1, &STD_SPECS);
    let (shot_arena, requests_arena) =
        run_scenario(Arena::new(1 << 16), &buffer, Variant::V1, &STD_SPECS);

    assert_eq!(shot_counting, shot_arena);
    assert_eq!(
        requests_counting, requests_arena,
        "the deserializer must ask every allocator for exactly the same layouts"
    );
    assert_eq!(shot_counting, one_shot(&buffer, Variant::V1, &std_pairs()));
}

#[test]
fn the_application_allocator_owns_every_live_byte() {
    let alloc = Counting::new();
    let mut store: Vec<Vec<char, Counting>, Counting> = Vec::new_in(alloc.clone());
    fill_store(&alloc, &mut store, &STD_SPECS);
    let pairs = pairs_from(&store, alloc.clone(), &STD_SPECS);

    let buffer = buf("0%%A%%1");
    let mut fd = FlatDeserializer::new(
        &buffer,
        Variant::V1,
        &pairs,
        Vec::new_in(alloc.clone()),
        Vec::with_capacity_in(pairs.len(), alloc.clone()),
    );
    let after_new = alloc.requests().len();
    fd.deserialize_all();
    let after_all = alloc.requests().len();
    assert!(
        after_all > after_new,
        "deserialize_all must allocate the state and the cache through the application allocator"
    );

    // Every byte handed out is still reachable from a vector this test owns, so nothing
    // allocated behind the application allocator's back.
    let expected = store.capacity() * size_of::<Vec<char, Counting>>()
        + store
            .iter()
            .map(|v| v.capacity() * size_of::<char>())
            .sum::<usize>()
        + pairs.capacity() * size_of::<LimiterPair<'static, char, Counting>>()
        + fd.archived_boundaries().capacity() * size_of::<Envelop>()
        + fd.deserializer_state()
            .repeat_map_on_cursor()
            .unwrap()
            .capacity()
            * size_of::<Option<NonMaxUsize>>();

    assert_eq!(
        alloc.live_bytes(),
        expected as i64,
        "live bytes handed out by the application allocator ({} allocations, {} deallocations)",
        alloc.alloc_count(),
        alloc.dealloc_count()
    );
    assert!(
        alloc.dealloc_count() > 0,
        "grow/free cycles went back through A"
    );
}

#[test]
fn pre_sizing_the_state_vectors_makes_construction_allocation_free() {
    let alloc = Counting::new();
    let mut store: Vec<Vec<char, Counting>, Counting> = Vec::new_in(alloc.clone());
    fill_store(&alloc, &mut store, &STD_SPECS);
    let pairs = pairs_from(&store, alloc.clone(), &STD_SPECS);
    let buffer = buf("0%%A%%1");

    // Sized exactly: `FlatDeserializer::new` has nothing to allocate.
    let bounds: Vec<Envelop, Counting> = Vec::new_in(alloc.clone());
    let repeat_map: Vec<Option<NonMaxUsize>, Counting> =
        Vec::with_capacity_in(pairs.len(), alloc.clone());
    let before = alloc.requests().len();
    let fd = FlatDeserializer::new(&buffer, Variant::V1, &pairs, bounds, repeat_map);
    assert_eq!(
        alloc.requests().len(),
        before,
        "constructing with pre-sized vectors must allocate nothing"
    );
    drop(fd);

    // Unsized: the state's repeat map comes from the application allocator.
    let before = alloc.requests().len();
    let unsized_map: Vec<Option<NonMaxUsize>, Counting> = Vec::new_in(alloc.clone());
    let fd = FlatDeserializer::new(
        &buffer,
        Variant::V1,
        &pairs,
        Vec::new_in(alloc.clone()),
        unsized_map,
    );
    let requests = alloc.requests();
    assert!(requests.len() > before, "the repeat map came from A");
    let first = requests[before];
    assert_eq!(
        first.1,
        align_of::<Option<NonMaxUsize>>(),
        "layout of the first request"
    );
    assert!(first.0 >= size_of::<Option<NonMaxUsize>>());
    drop(fd);

    // A `deserialize_incremental` step clones the state through A, then frees the old one.
    let mut fd = FlatDeserializer::new(
        &buffer,
        Variant::V1,
        &pairs,
        Vec::new_in(alloc.clone()),
        Vec::with_capacity_in(pairs.len(), alloc.clone()),
    );
    let before = alloc.requests().len();
    assert!(alloc.live_bytes() > 0);
    fd.deserialize_incremental();
    assert!(alloc.requests().len() > before);
}
