//! Shared fixtures and views for the SDAVE integration tests.
//!
//! Every expectation used by the test files is derived from `SDAVE/README.md` (the
//! protocol spec) plus the public API of `sdave`; nothing here is derived from observed
//! output. The types in this module only *view* public `sdave` types through `Debug`
//! impls so failures print the offsets that were hand-computed.

#![allow(dead_code)]
#![allow(clippy::ptr_arg)]

use core::alloc::{AllocError, Allocator, Layout};
use core::cell::OnceCell;
use core::num::NonZeroUsize;
use core::ptr::NonNull;
// `Global` lives in `alloc`/`std`, not in `core`.
use std::alloc::Global;
use std::sync::{Arc, Mutex};

use sdave::{
    Envelop, FlatDeserializer, FlatDeserializerState, InternalState, LimiterPair, NonEnvelop,
    NonMaxUsize, State, Variant, deserialize_incremental,
};

// ---------------------------------------------------------------------------
// limiter pairs
// ---------------------------------------------------------------------------

/// The pair set every README vector is written against:
/// `[('^', '~', 2), ('?', '!', 2), ('%', '%', 2), ('{', '}', 2), ('♦', '♣', 1)]`.
pub const STD_SPECS: [(&str, &str, usize); 5] = [
    ("^", "~", 2),
    ("?", "!", 2),
    ("%", "%", 2),
    ("{", "}", 2),
    ("♦", "♣", 1),
];

pub type Pairs = Vec<LimiterPair<'static, char, Global>>;

pub fn unit(s: &str) -> &'static Vec<char> {
    Box::leak(Box::new(s.chars().collect::<Vec<char>>()))
}

pub fn pair_opt(
    least_repeat: Option<usize>,
    limiter: &str,
    delimiter: &str,
) -> LimiterPair<'static, char, Global> {
    LimiterPair::new(
        least_repeat.map(|r| NonZeroUsize::new(r).unwrap()),
        OnceCell::from(unit(limiter)),
        OnceCell::from(unit(delimiter)),
    )
}

pub fn pair(
    least_repeat: usize,
    limiter: &str,
    delimiter: &str,
) -> LimiterPair<'static, char, Global> {
    pair_opt(Some(least_repeat), limiter, delimiter)
}

pub fn std_pairs() -> Pairs {
    specs_pairs(&STD_SPECS)
}

/// README: "a bad example of a limiter pair set containing A and B with (A.delimiter == B.limiter)".
pub const CROSSING_SPECS: [(&str, &str, usize); 2] = [("^", "~", 2), ("~", "%", 2)];

pub fn crossing_pairs() -> Pairs {
    specs_pairs(&CROSSING_SPECS)
}

/// README: "a bad example of a shared limiter". `readme_order == true` keeps the README's own order.
pub const SHARED_SPECS_README: [(&str, &str, usize); 3] =
    [("^", "~", 2), ("^", "~~", 2), ("^", "!", 2)];
pub const SHARED_SPECS_REORDERED: [(&str, &str, usize); 3] =
    [("^", "~~", 2), ("^", "~", 2), ("^", "!", 2)];

pub fn shared_limiter_pairs(readme_order: bool) -> Pairs {
    if readme_order {
        specs_pairs(&SHARED_SPECS_README)
    } else {
        specs_pairs(&SHARED_SPECS_REORDERED)
    }
}

/// A pair whose limiter/delimiter are more than one `T` wide.
pub const MULTI_SPECS: [(&str, &str, usize); 1] = [("ab", "cd", 2)];

pub fn multi_unit_pairs() -> Pairs {
    specs_pairs(&MULTI_SPECS)
}

/// Build a `Global` pair set from a spec slice.
pub fn specs_pairs(specs: &[(&str, &str, usize)]) -> Pairs {
    specs.iter().map(|(l, d, r)| pair(*r, l, d)).collect()
}

pub fn buf(s: &str) -> Vec<char> {
    s.chars().collect()
}

/// `Variant` has no `Debug` impl, so failures name it through this.
pub fn vname(v: Variant) -> &'static str {
    match v {
        Variant::V1 => "V1",
        Variant::V2 => "V2",
    }
}

/// Every buffer/variant/pair-set combination that the test files exercise.
pub fn spec_cases() -> Vec<(&'static str, Variant, Pairs)> {
    vec![
        ("0%%A%%%%B%%%%C%%1", Variant::V1, std_pairs()),
        ("0%%A%%%%B%%%%C%%1%", Variant::V1, std_pairs()),
        ("0%%%%%%B%%%%C%%1", Variant::V1, std_pairs()),
        ("0^^~~^^B~~{{C}}1", Variant::V1, std_pairs()),
        ("1^^^echo^^hello~~~~~2", Variant::V1, std_pairs()),
        ("0{{A}}{{B}}??C!!1", Variant::V2, std_pairs()),
        ("0{{A}}{{B}}??C!!!", Variant::V2, std_pairs()),
        ("1^^^echo^^hello~~~~~2", Variant::V2, std_pairs()),
        ("1^^^echo^^hello~~~~~", Variant::V2, std_pairs()),
        ("0%%A%%%%B%%%%C%%1", Variant::V2, std_pairs()),
        ("0^^A~~~~B%%~~C%%1", Variant::V1, crossing_pairs()),
        ("0^^A~~~~B%%~~C%%1", Variant::V2, crossing_pairs()),
        ("1^^A!B!~~~~C", Variant::V1, shared_limiter_pairs(true)),
        ("1^^A!B!~~~~C", Variant::V2, shared_limiter_pairs(true)),
        ("1^^A!B!~~~~C", Variant::V2, shared_limiter_pairs(false)),
        ("^^A~~", Variant::V1, std_pairs()),
        ("0%%A%%1", Variant::V1, std_pairs()),
        ("0ababXcdcd", Variant::V1, multi_unit_pairs()),
    ]
}

// ---------------------------------------------------------------------------
// Debuggable views of the public state types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Off {
    pub head: usize,
    pub payload_head: usize,
    pub payload_tail: usize,
    pub tail: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hole {
    pub head: usize,
    pub tail: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MS {
    E(Off),
    N(Hole),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MI {
    Unknown(usize),
    UnterminatedLimiterSlice(usize),
    UnterminatedDeLimiterSlice(usize),
    Clean,
    Archived,
    CleanArchived,
}

pub fn off_of(e: &Envelop) -> Off {
    Off {
        head: e.head_offset().get(),
        payload_head: e.payload_head_offset().get(),
        payload_tail: e.payload_tail_offset().get(),
        tail: e.tail_offset().map(|t| t.get()),
    }
}

pub fn hole_of(n: &NonEnvelop) -> Hole {
    Hole {
        head: n.head_offset().get(),
        tail: n.tail_offset().get(),
    }
}

pub fn ms_of(s: &State) -> MS {
    match s {
        State::E { envelop } => MS::E(off_of(envelop)),
        State::N { non_envelop } => MS::N(hole_of(non_envelop)),
    }
}

pub fn ms_head(m: MS) -> usize {
    match m {
        MS::E(o) => o.head,
        MS::N(h) => h.head,
    }
}

/// `State::semantical_tail_offset` in mirror form.
pub fn ms_tail(m: MS) -> usize {
    match m {
        MS::E(o) => o.tail.unwrap_or(o.payload_tail),
        MS::N(h) => h.tail,
    }
}

pub fn mi_of(i: &InternalState) -> MI {
    match i {
        InternalState::Unknown { tail_offset } => MI::Unknown(tail_offset.get()),
        InternalState::UnterminatedLimiterSlice { tail_offset } => {
            MI::UnterminatedLimiterSlice(tail_offset.get())
        }
        InternalState::UnterminatedDeLimiterSlice { tail_offset } => {
            MI::UnterminatedDeLimiterSlice(tail_offset.get())
        }
        InternalState::Clean => MI::Clean,
        InternalState::Archived => MI::Archived,
        InternalState::CleanArchived => MI::CleanArchived,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Phantom (zero-width, `tail_offset == None`) Envelop: the first real item is a NonEnvelop.
    Phantom,
    /// Zero-width NonEnvelop: two adjacent archived Envelops.
    ZeroHole,
    /// Archived NonEnvelop with this text.
    Hole(String),
    /// Archived Envelop with this payload.
    Env(String),
    /// Unarchived (pending) Envelop with this payload.
    Pending(String),
}

pub const PH: Item = Item::Phantom;
pub const ZH: Item = Item::ZeroHole;

pub fn h(s: &str) -> Item {
    Item::Hole(s.to_string())
}

pub fn e(s: &str) -> Item {
    Item::Env(s.to_string())
}

pub fn p(s: &str) -> Item {
    Item::Pending(s.to_string())
}

fn text(buffer: &[char], from: usize, to: usize) -> String {
    buffer[from..to].iter().collect()
}

pub fn item_of(state: &State, buffer: &[char]) -> Item {
    match state {
        State::N { non_envelop } => {
            if non_envelop.is_phantom() {
                Item::ZeroHole
            } else {
                Item::Hole(text(
                    buffer,
                    non_envelop.head_offset().get(),
                    non_envelop.tail_offset().get(),
                ))
            }
        }
        State::E { envelop } => {
            if envelop.is_phantom() {
                Item::Phantom
            } else {
                let payload = text(
                    buffer,
                    envelop.payload_head_offset().get(),
                    envelop.payload_tail_offset().get(),
                );
                if envelop.tail_offset().is_some() {
                    Item::Env(payload)
                } else {
                    Item::Pending(payload)
                }
            }
        }
    }
}

pub fn items_of<'a, 'l, 'v, A: Allocator + Clone>(
    fd: &mut FlatDeserializer<'a, 'l, 'v, char, A>,
) -> Vec<Item> {
    let states: Vec<State> = fd.iter().collect();
    let buffer = fd.buffer();
    states.iter().map(|s| item_of(s, buffer)).collect()
}

pub fn offs(bounds: &[Envelop]) -> Vec<Off> {
    bounds.iter().map(off_of).collect()
}

pub fn map_of<'l, 'v, A: Allocator + Clone>(
    state: &FlatDeserializerState<'l, 'v, char, A>,
) -> Vec<Option<usize>> {
    state
        .repeat_map_on_cursor()
        .unwrap()
        .iter()
        .map(|c| c.map(|n| n.get()))
        .collect()
}

/// `(least_repeat, limiter, delimiter)` of a pair, in `Debug`-able form.
///
/// The core types derive no `Debug`, so tests never put a library value into an
/// `assert_eq!`; everything they compare is one of the view types in this module.
pub fn pair_fields<'l, A: Allocator + Clone>(
    p: &LimiterPair<'l, char, A>,
) -> (Option<usize>, String, String) {
    (
        p.least_repeat().map(|r| r.get()),
        p.limiter().iter().collect(),
        p.delimiter().iter().collect(),
    )
}

pub fn nm(n: usize) -> NonMaxUsize {
    NonMaxUsize::new(n)
}

/// `FlatDeserializerState::is_limiter_pairs_safe` for the `char`/`Global` fixtures.
pub fn safe(pairs: &Pairs) -> bool {
    // The `'v` parameter is not part of the predicate; pin it so nothing is left to infer.
    FlatDeserializerState::<'static, 'static, char, Global>::is_limiter_pairs_safe(pairs)
}

// ---------------------------------------------------------------------------
// one-shot execution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    pub items: Vec<Item>,
    pub bounds: Vec<Off>,
    pub tail_non_envelop: Option<usize>,
    pub working_on: MS,
    pub next: MS,
    pub internal: MI,
    pub limiter: String,
    pub delimiter: String,
    pub least_repeat: Option<usize>,
    pub map: Vec<Option<usize>>,
}

#[allow(clippy::ptr_arg)]
pub fn one_shot_with<'a, 'l, 'v, A: Allocator + Clone>(
    buffer: &'a [char],
    variant: Variant,
    pairs: &'v Vec<LimiterPair<'l, char, A>, A>,
    alloc: A,
) -> Shot {
    let mut fd = FlatDeserializer::new(
        buffer,
        variant,
        pairs,
        Vec::new_in(alloc.clone()),
        Vec::new_in(alloc),
    );
    fd.deserialize_all();
    let bounds = offs(fd.archived_boundaries());
    let tail_non_envelop = fd.tail_non_envelop().map(|o| o.get());
    let working_on = ms_of(fd.deserializer_state().working_on());
    let next = ms_of(fd.deserializer_state().next());
    let internal = mi_of(fd.deserializer_state().internal());
    let limiter_pair = fd.deserializer_state().limiter_pair();
    let limiter: String = limiter_pair.limiter().iter().collect();
    let delimiter: String = limiter_pair.delimiter().iter().collect();
    let least_repeat = limiter_pair.least_repeat().map(|r| r.get());
    let map = map_of(fd.deserializer_state());
    let items = items_of(&mut fd);
    Shot {
        items,
        bounds,
        tail_non_envelop,
        working_on,
        next,
        internal,
        limiter,
        delimiter,
        least_repeat,
        map,
    }
}

pub fn one_shot(buffer: &[char], variant: Variant, pairs: &Pairs) -> Shot {
    one_shot_with(buffer, variant, pairs, Global)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snap {
    pub bounds: Vec<Off>,
    pub tail_non_envelop: Option<usize>,
    pub working_on: MS,
    pub internal: MI,
    pub items: Vec<Item>,
}

pub fn snap_of<'a, 'l, 'v, A: Allocator + Clone>(
    fd: &mut FlatDeserializer<'a, 'l, 'v, char, A>,
) -> Snap {
    let bounds = offs(fd.archived_boundaries());
    let tail_non_envelop = fd.tail_non_envelop().map(|o| o.get());
    let working_on = ms_of(fd.deserializer_state().working_on());
    let internal = mi_of(fd.deserializer_state().internal());
    let items = items_of(fd);
    Snap {
        bounds,
        tail_non_envelop,
        working_on,
        internal,
        items,
    }
}

pub fn one_shot_snap(buffer: &[char], variant: Variant, pairs: &Pairs) -> Snap {
    let mut fd = FlatDeserializer::new(buffer, variant, pairs, Vec::new(), Vec::new());
    fd.deserialize_all();
    snap_of(&mut fd)
}

// ---------------------------------------------------------------------------
// streaming / buffer growth
// ---------------------------------------------------------------------------

/// Re-slice a deserializer's buffer, keeping the whole state (the documented
/// `decouple` / `new_unchecked` equivalent of an append).
pub fn grow<'a, 'l, 'v, A: Allocator + Clone>(
    full: &'a [char],
    n: usize,
    fd: FlatDeserializer<'a, 'l, 'v, char, A>,
) -> FlatDeserializer<'a, 'l, 'v, char, A> {
    let (_, bounds, tail_non_envelop, state) = fd.decouple();
    // Safety: the new buffer is the old buffer plus appended contents.
    unsafe { FlatDeserializer::new_unchecked((&full[..n], bounds, tail_non_envelop, state)) }
}

/// Feed `full` prefix by prefix, `deserialize_all` after each prefix.
pub fn run_prefixes(full: &[char], variant: Variant, pairs: &Pairs, lens: &[usize]) -> Vec<Snap> {
    let mut fd = FlatDeserializer::new(&full[..0], variant, pairs, Vec::new(), Vec::new());
    let mut snaps = Vec::new();
    for &n in lens {
        fd = grow(full, n, fd);
        fd.deserialize_all();
        snaps.push(snap_of(&mut fd));
    }
    snaps
}

/// Assert the invariant that a confirmed boundary is never revised, and that only the
/// tail item of the `iter()` view is ever provisional.
pub fn assert_snaps_are_prefixes(snaps: &[Snap], final_snap: &Snap) {
    for (k, snap) in snaps.iter().enumerate() {
        assert!(
            snap.bounds.len() <= final_snap.bounds.len(),
            "prefix #{k}: confirmed {} boundaries, final has {}",
            snap.bounds.len(),
            final_snap.bounds.len()
        );
        for (i, b) in snap.bounds.iter().enumerate() {
            assert_eq!(
                Some(b),
                final_snap.bounds.get(i),
                "prefix #{k}: confirmed boundary {i} was revised"
            );
        }
        let n = snap.items.len();
        for (i, item) in snap.items.iter().enumerate() {
            if i + 1 < n {
                assert!(
                    !matches!(item, Item::Pending(_)),
                    "prefix #{k}: item {i} of {:?} is an unarchived state before the tail",
                    snap.items
                );
                assert_eq!(
                    Some(item),
                    final_snap.items.get(i),
                    "prefix #{k}: item {i} was revised"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// raw one-step state machine
// ---------------------------------------------------------------------------

pub fn state_eq<'l1, 'v1, 'l2, 'v2, A: Allocator + Clone>(
    new: &FlatDeserializerState<'l1, 'v1, char, A>,
    old: &FlatDeserializerState<'l2, 'v2, char, A>,
    old_repeat_map_on_cursor: &Vec<Option<NonMaxUsize>, A>,
) -> bool {
    let (aw, ai, an, av, ap, _, _alp) = new.clone().decouple();
    let (bw, bi, bn, bv, bp, _, _blp) = old.clone().decouple();
    let ar1 = new.repeat_map_on_cursor().unwrap();
    let br1 = old_repeat_map_on_cursor;
    aw == bw
        && ai == bi
        && an == bn
        && av == bv
        && pair_fields(&ap) == pair_fields(&bp)
        && ar1 == br1
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawStep {
    pub changed: bool,
    pub working_on: MS,
    pub next: MS,
    pub internal: MI,
    pub map: Vec<Option<usize>>,
}

/// Drive `deserialize_incremental` one step at a time until a step changes nothing,
/// recording every step (including the no-op fixpoint step).
pub fn raw_run<'l, 'v>(
    buffer: &[char],
    variant: Variant,
    pairs: &'v Vec<LimiterPair<'l, char, Global>, Global>,
) -> Vec<RawStep> {
    let mut state = FlatDeserializerState::new(variant, pairs, Vec::new());
    let mut steps = Vec::new();
    loop {
        let old_repeat_map_on_cursor = state.repeat_map_on_cursor().unwrap().clone();
        let next = unsafe { deserialize_incremental(buffer, &mut state) };
        let changed = !state_eq(&next, &state, &old_repeat_map_on_cursor);
        steps.push(RawStep {
            changed,
            working_on: ms_of(next.working_on()),
            next: ms_of(next.next()),
            internal: mi_of(next.internal()),
            map: map_of(&next),
        });
        if !changed {
            return steps;
        }
        state = next;
    }
}

/// The settled state after the raw fixpoint loop.
pub fn raw_fixpoint<'l, 'v>(
    buffer: &[char],
    variant: Variant,
    pairs: &'v Vec<LimiterPair<'l, char, Global>, Global>,
) -> FlatDeserializerState<'l, 'v, char, Global> {
    let mut state = FlatDeserializerState::new(variant, pairs, Vec::new());
    loop {
        let old_repeat_map_on_cursor = state.repeat_map_on_cursor().unwrap().clone();
        let next = unsafe { deserialize_incremental(buffer, &mut state) };
        if state_eq(&next, &state, &old_repeat_map_on_cursor) {
            return next;
        }
        state = next;
    }
}

// ---------------------------------------------------------------------------
// unsafe one-liners, so the test bodies stay declarative
// ---------------------------------------------------------------------------

pub fn adv<'a, 'l, 'v, A: Allocator + Clone>(
    fd: &mut FlatDeserializer<'a, 'l, 'v, char, A>,
    n: usize,
) -> usize {
    unsafe { fd.advance(NonMaxUsize::new(n)) }
}

pub fn replace<'a, 'l, 'v, A: Allocator + Clone>(
    fd: &mut FlatDeserializer<'a, 'l, 'v, char, A>,
    buffer: &'a [char],
) {
    unsafe { fd.replace_buffer(buffer) }
}

// ---------------------------------------------------------------------------
// application supplied allocators
// ---------------------------------------------------------------------------

pub fn chars_in<A: Allocator + Clone>(s: &str, alloc: &A) -> Vec<char, A> {
    let mut v = Vec::new_in(alloc.clone());
    v.extend(s.chars());
    v
}

/// Push every limiter/delimiter unit of `specs` into `store` (in pair order).
pub fn fill_store<A: Allocator + Clone>(
    alloc: &A,
    store: &mut Vec<Vec<char, A>, A>,
    specs: &[(&str, &str, usize)],
) {
    for (limiter, delimiter, _) in specs {
        store.push(chars_in(limiter, alloc));
        store.push(chars_in(delimiter, alloc));
    }
}

/// Build the pair set out of the units held by `store`.
pub fn pairs_from<'l, A: Allocator + Clone>(
    store: &'l Vec<Vec<char, A>, A>,
    alloc: A,
    specs: &[(&str, &str, usize)],
) -> Vec<LimiterPair<'l, char, A>, A> {
    let mut pairs = Vec::with_capacity_in(specs.len(), alloc);
    for (i, (_, _, repeat)) in specs.iter().enumerate() {
        pairs.push(LimiterPair::new(
            Some(NonZeroUsize::new(*repeat).unwrap()),
            OnceCell::from(&store[2 * i]),
            OnceCell::from(&store[2 * i + 1]),
        ));
    }
    pairs
}

pub trait Recorder: Allocator + Clone {
    fn allocation_requests(&self) -> Vec<(usize, usize)>;
}

/// A counting proxy over `Global`: records every `(size, align)` the application allocator
/// was asked for and keeps the live byte balance.
#[derive(Clone)]
pub struct Counting {
    state: Arc<Mutex<Counted>>,
}

#[derive(Default)]
struct Counted {
    requests: Vec<(usize, usize)>,
    allocs: u64,
    deallocs: u64,
    live: i64,
}

impl Counting {
    pub fn new() -> Self {
        Counting {
            state: Arc::new(Mutex::new(Counted::default())),
        }
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Counted> {
        self.state.lock().unwrap()
    }
    pub fn requests(&self) -> Vec<(usize, usize)> {
        self.lock().requests.clone()
    }
    pub fn alloc_count(&self) -> u64 {
        self.lock().allocs
    }
    pub fn dealloc_count(&self) -> u64 {
        self.lock().deallocs
    }
    pub fn live_bytes(&self) -> i64 {
        self.lock().live
    }
}

impl Default for Counting {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl Allocator for Counting {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        {
            let mut s = self.lock();
            s.requests.push((layout.size(), layout.align()));
            s.allocs += 1;
            s.live += layout.size() as i64;
        }
        Global.allocate(layout)
    }
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        {
            let mut s = self.lock();
            s.deallocs += 1;
            s.live -= layout.size() as i64;
        }
        unsafe { Global.deallocate(ptr, layout) }
    }
}

impl Recorder for Counting {
    fn allocation_requests(&self) -> Vec<(usize, usize)> {
        self.requests()
    }
}

/// A bump arena over a fixed `Box<[u8]>`; `deallocate` is a no-op, so every byte it hands
/// out stays handed out. Running a scenario inside it proves the scenario fits in a budget
/// the *test* chose.
#[derive(Clone)]
pub struct Arena {
    state: Arc<Mutex<ArenaState>>,
}

struct ArenaState {
    buf: Box<[u8]>,
    used: usize,
    requests: Vec<(usize, usize)>,
}

impl Arena {
    pub fn new(bytes: usize) -> Self {
        Arena {
            state: Arc::new(Mutex::new(ArenaState {
                buf: vec![0u8; bytes].into_boxed_slice(),
                used: 0,
                requests: Vec::new(),
            })),
        }
    }
    pub fn requests(&self) -> Vec<(usize, usize)> {
        self.state.lock().unwrap().requests.clone()
    }
    pub fn used(&self) -> usize {
        self.state.lock().unwrap().used
    }
}

unsafe impl Allocator for Arena {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let mut s = self.state.lock().unwrap();
        s.requests.push((layout.size(), layout.align()));
        let align = layout.align();
        let base = s.buf.as_ptr() as usize;
        let aligned = (base + s.used + align - 1) & !(align - 1);
        let offset = aligned - base;
        let end = offset.checked_add(layout.size()).ok_or(AllocError)?;
        if end > s.buf.len() {
            return Err(AllocError);
        }
        s.used = end;
        let ptr = NonNull::new(unsafe { s.buf.as_mut_ptr().add(offset) }).expect("arena pointer");
        Ok(NonNull::slice_from_raw_parts(ptr, layout.size()))
    }
    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {}
}

impl Recorder for Arena {
    fn allocation_requests(&self) -> Vec<(usize, usize)> {
        self.requests()
    }
}

/// Build the pair set, run one whole buffer, and return both the result and every
/// allocation request the application allocator saw.
pub fn run_scenario<A: Recorder>(
    alloc: A,
    buffer: &[char],
    variant: Variant,
    specs: &[(&str, &str, usize)],
) -> (Shot, Vec<(usize, usize)>) {
    let mut store: Vec<Vec<char, A>, A> = Vec::new_in(alloc.clone());
    fill_store(&alloc, &mut store, specs);
    let pairs = pairs_from(&store, alloc.clone(), specs);
    let shot = one_shot_with(buffer, variant, &pairs, alloc.clone());
    (shot, alloc.allocation_requests())
}
