//! for limiter_pairs not exceeding assertions for simple algorithm(~not recommended), SDAVE parser just select the first matching valid limiter_pair, and thus you get the freedom to sort the limiter_pairs and influent how would SDAVE parser select limiter_pair (e.g. [("ab","de",2),("ababac","dede",1)] for buffer "ababacedede" -> limiter slice "abab" ++ payload "ace" ++ delimiter slice "dede";[("ababac","dede",1),("ab","de",2)] for buffer "ababacedede" -> limiter slice "ababac" ++ payload "e" ++ delimiter slice "dede").
extern crate self as sdave;

mod codec;
pub use codec::*;

#[cfg(feature = "derive")]
pub use sdave_derive::{Deserialize, Serialize};

use std::num::NonZeroUsize;

const NON_MAX_ERROR_MESSAGE: &str = "[SDAVE] NonMaxUsize exceeds usize::MAX before OOM";

/// A `usize` newtype able to represent every value except `usize::MAX`.
///
/// Used for buffer offsets: a buffer can hold at most `usize::MAX` bytes before OOM,
/// so any real buffer index fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NonMaxUsize {
    non_max: NonZeroUsize,
}
impl NonMaxUsize {
    pub fn get(&self) -> usize {
        self.non_max.get() - 1
    }
    /// # Panics
    /// Panic when offset exceeds usize::MAX - 1 (i.e. `offset == usize::MAX`).
    ///
    /// # Safety
    /// Safe to call; marked `unsafe` only to draw attention to the panic contract:
    /// it is the caller's responsibility that offsets never reach `usize::MAX`
    /// (any real buffer OOMs long before that).
    pub unsafe fn new(offset: usize) -> Self {
        NonMaxUsize {
            non_max: NonZeroUsize::new(offset.wrapping_add(1)).expect(NON_MAX_ERROR_MESSAGE),
        }
    }
    /// Shift this offset backward by `offset` (used when the buffer is advanced).
    /// # Panics
    /// Panic when `offset > self.get()` (would underflow below zero).
    ///
    /// # Safety
    /// Safe to call; marked `unsafe` only to draw attention to the panic contract.
    pub unsafe fn offset_by(&mut self, offset: usize) {
        self.non_max =
            NonZeroUsize::new(self.get().checked_sub(offset).expect(NON_MAX_ERROR_MESSAGE) + 1)
                .expect(NON_MAX_ERROR_MESSAGE)
    }
}
/// absolute offset on buffer index.
pub type Offset = NonMaxUsize;

/// build an [`Offset`] from a raw buffer index.
#[inline]
fn off(raw: usize) -> Offset {
    // SAFETY: panics instead of misbehaving when raw == usize::MAX.
    unsafe { NonMaxUsize::new(raw) }
}

/// `T`: basic data unit of a buffer.
/// `limiter`/`delimiter` a slice of vec of T, e.g. UTF-8 1B/2B/3B char in a u8 buffer.
/// Technically, limiter can equal to (a repeating unit/a sequence of)delimiter: it would cause conflicts between empty Envelop and long limiter slice; in case of that, SDAVE always consider the slice as long limiter slice, and an empty-payload-Envelop is impossible; it's not recommended.
/// Technically, one limiter can has multiple delimiters in different LimiterPairs, it's not recommended.
/// Technically, one delimiter can has multiple limiters in different LimiterPairs, it's not recommended.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LimiterPair<T: Sized + PartialEq + Clone> {
    /// Case parsing: match `limiter`/`delimiter` first, then guard on `least_repeat` where assertion(repeat>=1) is always true.
    /// Case serializing: the minimal repeat to frame certain payload being inserted at `buffer[..offset] _here_ buffer[offset..]` without introducing conflicts.
    ///
    ///`None`: reserved to tell this limiter pair is not capatible to frame certain payload at `buffer[..offset] here buffer[offset..]` without conflicts.
    /// Pairs with `None` are skipped entirely while parsing.
    pub least_repeat: Option<NonZeroUsize>,
    pub limiter: Vec<T>,
    pub delimiter: Vec<T>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Unknown whether current parsing slice is part of an unarchived NonEnvelop or part of a live streaming limiter slice
    Unknown,
    /// Current parsing slice MUST be a limiter slice but limiter_pair/repeats are not well defined yet (not terminated by a not-repeating `T`, while usize is offset of HEAD of it.
    PartialLimiterSlice {
        head_offset: Offset,
    },
    NonEnvelop {
        head_offset: Offset,
        /// can monotonic increase in next updates, `buffer[head_offset..tail_offset]` MUST NOT 1.contains valid limiter slice; 2.ends with potential limiter slice.
        /// tail_offset != buffer.len could mean there's Unknown contents at tail of buffer that might be part of this NonEnvelop.
        tail_offset: Offset,
    },
    E {
        envelop: Envelop,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Envelop {
    head_offset: Offset,
    /// limiter slice MUST be well defined(at least one non-limiter T(either payload head or delimiter != to limiter) exists following limiter slice).
    payload_head_offset: Offset,
    /// can monotonic increase in next updates, `buffer[payload_head_offset..payload_tail_offset]` MUST NOT 1.contains valid delimiter slice; 2.ends with potential delimiter slice.
    /// payload_tail_offset != buffer.len() could mean there's unknown contents at tail of buffer that might be part of payload.
    /// An archived Envelop, with limiter != delimiter, can has empty payload, e.g. `^^~~`, in case of which, payload_tail_offset == payload_head_offset.
    payload_tail_offset: Offset,
    /// Some(offset) means this Envelop is confirmed archived, would never grow again.
    /// It could be an NonEnvelop or an Envelop behind.
    tail_offset: Option<Offset>,
}
impl Envelop {
    /// head of the limiter slice.
    pub fn head_offset(&self) -> Offset {
        self.head_offset
    }
    /// head of the payload (== end of the limiter slice).
    pub fn payload_head_offset(&self) -> Offset {
        self.payload_head_offset
    }
    /// confirmed tail of the payload; monotonic while the Envelop is unarchived.
    pub fn payload_tail_offset(&self) -> Offset {
        self.payload_tail_offset
    }
    /// `Some(offset)` once this Envelop is confirmed archived (end of the delimiter slice).
    pub fn tail_offset(&self) -> Option<Offset> {
        self.tail_offset
    }
    /// repeat count of the limiter/delimiter slice of this Envelop under `limiter_unit`.
    pub fn repeat(&self, limiter_unit: usize) -> usize {
        (self.payload_head_offset.get() - self.head_offset.get()) / limiter_unit.max(1)
    }
    /// a phantom Envelop is a zero-width marker used as first element of
    /// [`FlatParser::archived_boundaries`] to indicate the first archived item is a NonEnvelop.
    pub fn is_phantom(&self) -> bool {
        self.tail_offset.is_none()
            && self.head_offset.get() == 0
            && self.payload_head_offset.get() == 0
            && self.payload_tail_offset.get() == 0
    }
    fn phantom() -> Self {
        Envelop {
            head_offset: off(0),
            payload_head_offset: off(0),
            payload_tail_offset: off(0),
            tail_offset: None,
        }
    }
}

/// Delimiter match algorithm variant. There's not a full winner, choose based on scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The every first full matched delimiter slice is just the delimiter slice. No
    /// delimiter slice confirmation latency, but tail of payload MUST NOT collide with
    /// the delimiter unit.
    V1,
    /// The every first full matched delimiter slice FOLLOWED by a NON-delimiter is the
    /// delimiter slice. Tail of payload won't disable a limiter pair, but introduces
    /// delimiter slice confirmation latency (solvable on application level by pushing a
    /// non-delimiter to the mixed channel).
    V2,
}

// ---------------------------------------------------------------------------
// internal scanners
// ---------------------------------------------------------------------------

/// does `unit` fully match at `pos`?
#[inline]
fn unit_match<T: PartialEq>(buf: &[T], pos: usize, unit: &[T]) -> bool {
    pos + unit.len() <= buf.len() && buf[pos..pos + unit.len()] == unit[..]
}

/// does `unit` repeated `k` times match at `pos`?
fn repeat_match<T: PartialEq>(buf: &[T], pos: usize, unit: &[T], k: usize) -> bool {
    let u = unit.len();
    if pos + k * u > buf.len() {
        return false;
    }
    (0..k * u).all(|i| buf[pos + i] == unit[i % u])
}

/// is `slice` a nonempty prefix of `unit` repeated indefinitely?
fn is_prefix_of_units<T: PartialEq>(slice: &[T], unit: &[T]) -> bool {
    !slice.is_empty()
        && slice
            .iter()
            .enumerate()
            .all(|(i, t)| *t == unit[i % unit.len()])
}

struct Run {
    /// full unit repeats.
    repeats: usize,
    /// false when the run reaches end of buffer (exactly at a unit boundary or mid-unit)
    /// and could thus grow with more streamed contents.
    terminated: bool,
    /// index right after the last full unit.
    run_end: usize,
}

/// count the maximal run of `unit` starting at `pos`.
fn count_run<T: PartialEq>(buf: &[T], pos: usize, unit: &[T]) -> Run {
    let u = unit.len();
    let mut i = pos;
    let mut repeats = 0;
    loop {
        if i + u > buf.len() {
            let rem = &buf[i..];
            if rem.is_empty() || unit.starts_with(rem) {
                // reached end of buffer at a unit boundary or mid-unit: the run may grow.
                return Run {
                    repeats,
                    terminated: false,
                    run_end: i,
                };
            }
            return Run {
                repeats,
                terminated: true,
                run_end: i,
            };
        }
        if unit_match(buf, i, unit) {
            repeats += 1;
            i += u;
        } else {
            return Run {
                repeats,
                terminated: true,
                run_end: i,
            };
        }
    }
}

enum Limit {
    /// a limiter run reaches end of buffer; pair/repeats not well defined yet.
    Unterminated,
    /// first valid limiter_pair (in list order) matched with a well defined limiter slice.
    Valid { pair: usize, run_end: usize },
    /// nothing that can grow into a valid limiter slice starts here.
    NotLimiter,
}

/// a LimiterPair participates in parsing only when it is fully defined.
fn pair_usable<T: Sized + PartialEq + Clone>(pair: &LimiterPair<T>) -> bool {
    pair.least_repeat.is_some() && !pair.limiter.is_empty() && !pair.delimiter.is_empty()
}

/// try to resolve a limiter slice starting at `pos`.
fn resolve_limiter<T: Sized + PartialEq + Clone>(
    buf: &[T],
    limiter_pairs: &[LimiterPair<T>],
    pos: usize,
) -> Limit {
    if pos >= buf.len() {
        return Limit::NotLimiter;
    }
    for (i, pair) in limiter_pairs.iter().enumerate() {
        if !pair_usable(pair) {
            continue;
        }
        let unit = pair.limiter.as_slice();
        if pos + unit.len() <= buf.len() {
            if !unit_match(buf, pos, unit) {
                continue;
            }
            let run = count_run(buf, pos, unit);
            if !run.terminated {
                // every other pair matching here also reaches end of buffer.
                return Limit::Unterminated;
            }
            // SAFETY: checked by pair_usable.
            if run.repeats >= pair.least_repeat.unwrap().get() {
                return Limit::Valid {
                    pair: i,
                    run_end: run.run_end,
                };
            }
        } else if unit.starts_with(&buf[pos..]) {
            // a partial unit at end of buffer can grow into a limiter slice.
            return Limit::Unterminated;
        }
    }
    Limit::NotLimiter
}

/// scan NonEnvelop contents `[from, buf.len())`, `base` being the head of the current
/// NonEnvelop (or the next item's start when nothing precedes).
fn scan_non_envelop<T: Sized + PartialEq + Clone>(
    buf: &[T],
    limiter_pairs: &[LimiterPair<T>],
    base: usize,
    from: usize,
    fallback: LimiterPair<T>,
) -> FlatParserState<T> {
    let mut p = from;
    while p < buf.len() {
        match resolve_limiter(buf, limiter_pairs, p) {
            Limit::Unterminated => {
                return FlatParserState {
                    state: State::PartialLimiterSlice {
                        head_offset: off(p),
                    },
                    limiter: fallback,
                };
            }
            Limit::Valid { pair, run_end } => {
                return FlatParserState {
                    state: State::E {
                        envelop: Envelop {
                            head_offset: off(p),
                            payload_head_offset: off(run_end),
                            payload_tail_offset: off(run_end),
                            tail_offset: None,
                        },
                    },
                    limiter: limiter_pairs[pair].clone(),
                };
            }
            Limit::NotLimiter => p += 1,
        }
    }
    FlatParserState {
        state: State::NonEnvelop {
            head_offset: off(base),
            tail_offset: off(buf.len()),
        },
        limiter: fallback,
    }
}

/// continue the delimiter slice scan of an unarchived Envelop.
///
/// `resume` is the current payload_tail_offset: `buffer[payload_head..resume]` is already
/// confirmed free of valid delimiter slice and not ending with a potential delimiter slice.
fn scan_delimiter<T: Sized + PartialEq + Clone>(
    variant: Variant,
    buf: &[T],
    pair: &LimiterPair<T>,
    head: usize,
    payload_head: usize,
    resume: usize,
) -> Envelop {
    let unit = pair.delimiter.as_slice();
    let u = unit.len();
    let k = (payload_head - head) / pair.limiter.len().max(1);
    let w = k * u;
    let unarchived = |payload_tail: usize| Envelop {
        head_offset: off(head),
        payload_head_offset: off(payload_head),
        payload_tail_offset: off(payload_tail),
        tail_offset: None,
    };
    if w == 0 {
        return unarchived(resume);
    }
    let mut p = resume;
    while p + w <= buf.len() {
        if repeat_match(buf, p, unit, k) {
            match variant {
                Variant::V1 => {
                    return Envelop {
                        head_offset: off(head),
                        payload_head_offset: off(payload_head),
                        payload_tail_offset: off(p),
                        tail_offset: Some(off(p + w)),
                    };
                }
                Variant::V2 => {
                    let after = p + w;
                    if after == buf.len() {
                        // confirmation latency: the delimiter slice is not followed by a
                        // non-delimiter T yet.
                        return unarchived(p);
                    }
                    let rem = &buf[after..];
                    if rem.len() < u {
                        if unit.starts_with(rem) {
                            // partial unit at end of buffer: unconfirmed.
                            return unarchived(p);
                        }
                        // terminated by a partial non-unit: confirmed.
                    } else if unit_match(buf, after, unit) {
                        // the run continues: this window is payload content, slide on.
                        p += 1;
                        continue;
                    }
                    // followed by a non-delimiter: confirmed archived.
                    return Envelop {
                        head_offset: off(head),
                        payload_head_offset: off(payload_head),
                        payload_tail_offset: off(p),
                        tail_offset: Some(off(after)),
                    };
                }
            }
        }
        p += 1;
    }
    // reached end of buffer without a full match: locate the earliest trailing potential
    // delimiter slice; everything before it is confirmed payload.
    let start = resume.max(buf.len().saturating_sub(w.saturating_sub(1)));
    let mut tail = buf.len();
    let mut q = start;
    while q < buf.len() {
        if is_prefix_of_units(&buf[q..], unit) {
            tail = q;
            break;
        }
        q += 1;
    }
    unarchived(tail)
}

// ---------------------------------------------------------------------------
// FlatParserState
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct FlatParserState<T: Sized + PartialEq + Clone> {
    /// current unarchived state, contains head_offset for incremental_parse. State::Unknown means nothing parsed yet and the parser would parse from head of buffer.
    state: State,
    /// current_limiter make sense when and only when current state is E(Envelop) whose tail_offset is none(unarchived).
    limiter: LimiterPair<T>,
}
impl<T> FlatParserState<T>
where
    T: Sized + PartialEq + Clone,
{
    pub fn new(state: State, limiter: LimiterPair<T>) -> Self {
        FlatParserState { state, limiter }
    }
    /// any limiter pair is valid.
    pub fn default(limiter: LimiterPair<T>) -> Self {
        FlatParserState::new(State::Unknown, limiter)
    }
    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn limiter(&self) -> &LimiterPair<T> {
        &self.limiter
    }
}

// ---------------------------------------------------------------------------
// FlatParserIter
// ---------------------------------------------------------------------------

pub struct FlatParserIter<'a, 'b, T: Sized + PartialEq + Clone> {
    parser: &'a mut FlatParser<'b, T>,
    pos: usize,
}
impl<'a, 'b, T> Iterator for FlatParserIter<'a, 'b, T>
where
    T: Sized + PartialEq + Clone,
{
    type Item = State;
    // two sequential archived Envelop is legal, two sequential archived NonEnvelop is illegal and impossible.
    /// odd pos == Envelop; even pos == NonEnvelop;.
    /// Some(phantom Envelop) can be returned as first item to indicate the real first item is a NonEnvelop.
    /// Some(phantom NonEnvelop) can be returned if no NonEnvelop between two Envelops.
    ///
    /// reuse cached FlatParser::archived_boundaries & FlatParser::tail_non_envelop where pos < FlatParser::archived_boundaries.len() * 2 - if FlatParser::tail_non_envelop.is_none(){1}.
    /// otherwise, drive parse_incremental step by step until an item is cached or the
    /// parser settles. Cache-neutral steps that only make progress (for example locating an
    /// Envelop that is not archived yet, or growing a pending payload) are swallowed, so
    /// each call yields exactly one observable item -- never an intermediate unarchived step.
    /// then return latest unarchived state if it is either an Envelop or a NonEnvelop.
    /// then return None if latest unarchived Envelop or unarchived NonEnvelop is ALREADY iterated or latest state is PartialLimiterSlice or Unknown.
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let n = self.parser.archived_boundaries.len();
            let cached = if n == 0 {
                0
            } else {
                2 * n - usize::from(self.parser.tail_non_envelop.is_none())
            };
            if self.pos < cached {
                let i = self.pos / 2;
                let item = if self.pos.is_multiple_of(2) {
                    State::E {
                        envelop: self.parser.archived_boundaries[i].clone(),
                    }
                } else {
                    // the NonEnvelop hole behind archived_boundaries[i]
                    let start = self.parser.archived_boundaries[i]
                        .tail_offset
                        .map(|t| t.get())
                        .unwrap_or(0);
                    let end = if i + 1 < n {
                        self.parser.archived_boundaries[i + 1].head_offset.get()
                    } else {
                        self.parser
                            .tail_non_envelop
                            .expect("[SDAVE] cached odd slot without tail_non_envelop")
                            .get()
                    };
                    State::NonEnvelop {
                        head_offset: off(start),
                        tail_offset: off(end),
                    }
                };
                self.pos += 1;
                return Some(item);
            }
            // Drive the parser step by step until an item is cached or it settles. A step
            // may cache a new item, make progress without caching (an Envelop located but
            // not archived yet, or a pending payload growing), or settle. Only a settled
            // step exposes the trailing unarchived item; intermediate steps are consumed
            // here so one `next` call still yields exactly one observable item.
            let before_cache = (
                self.parser.archived_boundaries.len(),
                self.parser.tail_non_envelop,
            );
            let before_state = self.parser.parser_state.state.clone();
            self.parser.parse_incremental();
            let after_cache = (
                self.parser.archived_boundaries.len(),
                self.parser.tail_non_envelop,
            );
            if after_cache != before_cache {
                continue;
            }
            if self.parser.parser_state.state != before_state {
                continue;
            }
            if self.pos == cached {
                // settled: the latest unarchived state is the trailing item.
                return match &self.parser.parser_state.state {
                    State::E { .. } | State::NonEnvelop { .. } => {
                        let item = self.parser.parser_state.state.clone();
                        self.pos += 1;
                        Some(item)
                    }
                    State::PartialLimiterSlice { .. } | State::Unknown => None,
                };
            }
            // the latest unarchived state is already iterated.
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// FlatParser
// ---------------------------------------------------------------------------

/// the parts of a decoupled [`FlatParser`]:
/// `(buffer, variant, limiter_pairs, archived_boundaries, tail_non_envelop, parser_state)`.
pub type DecoupledFlatParser<'a, T> = (
    &'a [T],
    Variant,
    Vec<LimiterPair<T>>,
    Vec<Envelop>,
    Option<Offset>,
    FlatParserState<T>,
);

pub struct FlatParser<'a, T: Sized + PartialEq + Clone> {
    buffer: &'a [T],
    variant: Variant,
    /// free padding memory waiting to be used to extend struct functionality.
    _reserved: [u8; 7],
    limiter_pairs: Vec<LimiterPair<T>>,
    /// holes between Envelopes indicates archived NonEnvelopes.
    /// a phantom archived envelop whose tail_offset is None can be the first element of this vec to indicate the first item is a NonEnvelop.
    /// a truncated archived envelop who matches (buffer[head_offset..payload_head_offset] as limiter slice not matching buffer[payload_tail_offset..tail_offset] as delimiter slice) can be the first element of this Vec due to advancing.
    archived_boundaries: Vec<Envelop>,
    /// Some(offset) indicates an archived NonEnvelop between last Envelop's tail_offset(or 0 if no Envelop exsits) and offset.
    tail_non_envelop: Option<Offset>,
    parser_state: FlatParserState<T>,
}
impl<'a, T> FlatParser<'a, T>
where
    T: Sized + PartialEq + Clone,
{
    pub fn new(buffer: &'a [T], variant: Variant, limiter_pairs: Vec<LimiterPair<T>>) -> Self {
        let current_limiter = limiter_pairs
            .first()
            .expect("[SDAVE] empty limiter_pairs")
            .clone();
        Self {
            buffer,
            variant,
            _reserved: [0u8; 7],
            limiter_pairs,
            archived_boundaries: Vec::new(),
            tail_non_envelop: None,
            parser_state: FlatParserState {
                state: State::Unknown,
                limiter: current_limiter,
            },
        }
    }
    /// you can advance the buffer(or just alloc new one and copy tailer to new buffer) to save memory or append new contents to it alfter FlatParser release(drop the ref to buffer alfter FlatParser::decouple) read access to your buffer, but you MUST call FlatParser::advance(offset) if you advance your buffer.
    /// you can replace limiter_pairs, which would be applied to future(parser_state based) envelopes(limiter for current unarchived envelop is cloned to avoid semantic issue).
    /// you can replace variant, which would be applied right next parse_incremental.
    pub fn decouple(self) -> DecoupledFlatParser<'a, T> {
        (
            self.buffer,
            self.variant,
            self.limiter_pairs,
            self.archived_boundaries,
            self.tail_non_envelop,
            self.parser_state,
        )
    }
    /// # Safety
    /// operation to buffer contents MUST be conbination of [advance, append], you MUST call FlatParser::advance(offset) if buffer is advanced.
    pub unsafe fn new_unchecked(
        buffer: &'a [T],
        variant: Variant,
        limiter_pairs: Vec<LimiterPair<T>>,
        archived_boundaries: Vec<Envelop>,
        tail_non_envelop: Option<Offset>,
        parser_state: FlatParserState<T>,
    ) -> Self {
        Self {
            buffer,
            variant,
            _reserved: [0u8; 7],
            limiter_pairs,
            archived_boundaries,
            tail_non_envelop,
            parser_state,
        }
    }
    /// advance `archived_boundaries` & `tail_non_envelop` & `current_state`.
    ///
    /// Archived items fully covered by `[0, offset)` are dropped; an archived Envelop
    /// straddling `offset` is kept as a truncated marker (its offsets are clamped, so
    /// `buffer[head..payload_head]` no longer matches a real limiter slice).
    ///
    /// return how many archived items being fully removed from cache. Only real items are
    /// counted: non-phantom Envelopes and non-empty NonEnvelop holes (phantom markers are
    /// structural, not items).
    ///
    /// # Safety
    /// `offset` MUST match how far the buffer was (or will be, before next use) advanced.
    /// Advancing into the unarchived parser_state is permitted but leaves it clamped to
    /// buffer head, best-effort.
    pub unsafe fn advance(&mut self, offset: Offset) -> usize {
        let o = offset.get();
        let mut removed = 0usize;
        let n = self.archived_boundaries.len();
        let mut drop = 0usize;
        for i in 0..n {
            let e = &self.archived_boundaries[i];
            let hole_end = if i + 1 < n {
                Some(self.archived_boundaries[i + 1].head_offset.get())
            } else {
                self.tail_non_envelop.map(|t| t.get())
            };
            match e.tail_offset {
                // phantom marker: dropped once the leading NonEnvelop hole is fully consumed.
                None => match hole_end {
                    Some(end) if end <= o => {
                        if end > 0 {
                            removed += 1;
                        }
                        drop += 1;
                    }
                    _ => break,
                },
                Some(tail) => {
                    if tail.get() > o {
                        break;
                    }
                    removed += 1; // the Envelop itself
                    drop += 1;
                    if let Some(end) = hole_end
                        && end <= o
                        && end > tail.get()
                    {
                        removed += 1; // the NonEnvelop hole behind it
                    }
                }
            }
        }
        if n == 0
            && let Some(tail) = self.tail_non_envelop
            && tail.get() <= o
            && tail.get() > 0
        {
            // a standalone leading tail NonEnvelop (reachable via new_unchecked).
            removed += 1;
        }
        self.archived_boundaries.drain(..drop);
        let shift = |x: Offset| off(x.get().saturating_sub(o));
        for e in &mut self.archived_boundaries {
            e.head_offset = shift(e.head_offset);
            e.payload_head_offset = shift(e.payload_head_offset);
            e.payload_tail_offset = shift(e.payload_tail_offset);
            if let Some(t) = e.tail_offset {
                e.tail_offset = Some(shift(t));
            }
        }
        self.tail_non_envelop = match self.tail_non_envelop {
            Some(t) if t.get() <= o => None,
            Some(t) => Some(off(t.get() - o)),
            None => None,
        };
        // a remaining leading hole before the first Envelop needs the phantom marker.
        if let Some(first) = self.archived_boundaries.first()
            && first.tail_offset.is_some()
            && first.head_offset.get() > 0
        {
            self.archived_boundaries.insert(0, Envelop::phantom());
        }
        if self.archived_boundaries.is_empty() && self.tail_non_envelop.is_some_and(|t| t.get() > 0)
        {
            self.archived_boundaries.push(Envelop::phantom());
        }
        match &mut self.parser_state.state {
            State::Unknown => {}
            State::PartialLimiterSlice { head_offset } => *head_offset = shift(*head_offset),
            State::NonEnvelop {
                head_offset,
                tail_offset,
            } => {
                *head_offset = shift(*head_offset);
                *tail_offset = shift(*tail_offset);
            }
            State::E { envelop } => {
                envelop.head_offset = shift(envelop.head_offset);
                envelop.payload_head_offset = shift(envelop.payload_head_offset);
                envelop.payload_tail_offset = shift(envelop.payload_tail_offset);
                if let Some(t) = envelop.tail_offset {
                    envelop.tail_offset = Some(shift(t));
                }
            }
        }
        removed
    }
    /// replace the buffer FlatParser can read.
    ///
    /// # Safety
    /// new buffer contents MUST be equal to operations of conbination of [advance, append] being done to old buffer, you MUST call FlatParser::advance(offset) if new buffer is advanced.
    pub unsafe fn replace_buffer(&mut self, buffer: &'a [T]) {
        self.buffer = buffer
    }
    /// Advance the state machine by exactly one step and return the new state.
    ///
    /// A step locates the next limiter slice (a freshly unarchived `E`), archives an
    /// unarchived `E` once its delimiter slice is confirmed, scans past an archived `E`
    /// toward the next item, or makes no progress (a settled unarchived `E`/`NonEnvelop`,
    /// a trailing `PartialLimiterSlice`, or `Unknown`). Confirmed items are absorbed into
    /// `archived_boundaries` / `tail_non_envelop` as each step runs.
    ///
    /// A single call does NOT drain the buffer. To consume everything currently available,
    /// call repeatedly until the returned state stops advancing, or use [`parse_all`].
    ///
    /// [`parse_all`]: FlatParser::parse_all
    pub fn parse_incremental(&mut self) -> &State {
        let old = self.parser_state.clone();
        let new = unsafe {
            crate::parse_incremental(self.variant, self.buffer, &self.limiter_pairs, old.clone())
        };
        self.absorb(&old, &new);
        self.parser_state = new;
        &self.parser_state.state
    }
    /// Drain the buffer: call [`parse_incremental`](FlatParser::parse_incremental) until the
    /// state stops advancing, then return the terminal state.
    ///
    /// The terminal state is a settled unarchived `E` or `NonEnvelop`, a trailing
    /// `PartialLimiterSlice`, or `Unknown`. This is the "parse everything currently
    /// available" convenience. Streaming callers that interleave buffer growth with
    /// parsing should step [`parse_incremental`](FlatParser::parse_incremental) themselves
    /// so one call stays bounded to one item.
    pub fn parse_all(&mut self) -> &State {
        loop {
            let before = self.parser_state.state.clone();
            self.parse_incremental();
            if self.parser_state.state == before {
                break;
            }
        }
        &self.parser_state.state
    }
    /// absorb one incremental step: archive whatever the transition confirms.
    fn absorb(&mut self, old: &FlatParserState<T>, new: &FlatParserState<T>) {
        // the offset the previous item was confirmed up to:
        // contents in `[base, new_head)` are an archived NonEnvelop.
        let base = match &old.state {
            State::Unknown => 0,
            State::PartialLimiterSlice { head_offset } => head_offset.get(),
            State::NonEnvelop { head_offset, .. } => head_offset.get(),
            State::E { envelop } => match envelop.tail_offset {
                Some(tail) => tail.get(),
                None => envelop.head_offset.get(),
            },
        };
        match &new.state {
            State::E { envelop } => {
                if envelop.tail_offset.is_some() {
                    // freshly archived Envelop (the free function never lingers on an
                    // already archived Envelop, it moves past it within the same call).
                    self.archive_envelop(envelop.clone());
                } else {
                    let head = envelop.head_offset.get();
                    if head > base {
                        self.archive_non_envelop(head);
                    }
                }
            }
            State::PartialLimiterSlice { head_offset } => {
                let head = head_offset.get();
                if head > base {
                    self.archive_non_envelop(head);
                }
            }
            State::NonEnvelop { .. } | State::Unknown => {}
        }
    }
    /// archive the NonEnvelop `[last archived Envelop's tail (or 0), tail)`.
    ///
    /// overwriting an existing `tail_non_envelop` is the merge of two contiguous
    /// NonEnvelop slices (e.g. a PartialLimiterSlice dissolving back into NonEnvelop
    /// contents), never two sequential archived NonEnvelops.
    fn archive_non_envelop(&mut self, tail: usize) {
        if self.archived_boundaries.is_empty() && tail > 0 {
            self.archived_boundaries.push(Envelop::phantom());
        }
        self.tail_non_envelop = Some(off(tail));
    }
    fn archive_envelop(&mut self, envelop: Envelop) {
        if self.archived_boundaries.is_empty() && envelop.head_offset.get() > 0 {
            self.archived_boundaries.push(Envelop::phantom());
        }
        self.archived_boundaries.push(envelop);
        self.tail_non_envelop = None;
    }
    pub fn iter(&mut self) -> FlatParserIter<'_, 'a, T> {
        FlatParserIter {
            parser: self,
            pos: 0,
        }
    }
    /// # Safety
    /// pos MUST be valid -- within cached archived items.
    pub unsafe fn iter_from(&mut self, pos: usize) -> FlatParserIter<'_, 'a, T> {
        FlatParserIter { parser: self, pos }
    }
    pub fn buffer(&self) -> &'a [T] {
        self.buffer
    }
    pub fn variant(&self) -> Variant {
        self.variant
    }
    pub fn limiter_pairs(&self) -> &[LimiterPair<T>] {
        &self.limiter_pairs
    }
    pub fn archived_boundaries(&self) -> &[Envelop] {
        &self.archived_boundaries
    }
    pub fn tail_non_envelop(&self) -> Option<Offset> {
        self.tail_non_envelop
    }
    pub fn parser_state(&self) -> &FlatParserState<T> {
        &self.parser_state
    }
}

// ---------------------------------------------------------------------------
// serializing
// ---------------------------------------------------------------------------

/// Return `Some(least_repeat)` if serializable with given `limiter_pair` under given context where least_repeat can be grater than `limiter_pair.least_repeat`, or `None` if not serializable.
///
/// Serializable: `buffer[..offset] ++ limiter_pair.limiter * least_repeat.get() ++ payload ++ limiter_pair.delimiter * least_repeat.get() ++ following`
/// can be parsed to get an Envelop framing payload.
///
/// # Safety
/// `offset` MUST be a clean item boundary of `buffer`: head of buffer, or tail of an
/// archived item (NonEnvelop or Envelop), i.e. somewhere parsing would restart scanning
/// fresh. `buffer[..offset]` MUST NOT tail with live contents (an unarchived Envelop, a
/// PartialLimiterSlice, or a potential limiter slice); if it does, the returned answer
/// is meaningless. Note that tailing with the limiter unit is fine when those Ts belong
/// to an archived Envelop's delimiter slice (e.g. two contiguous V1 Envelopes sharing
/// the same limiter pair): the parser restarts at `offset`, so the limiter run there is
/// exactly the written one.
pub unsafe fn can_serialize<T: Sized + PartialEq + Clone>(
    variant: Variant,
    buffer: &[T],
    offset: Offset,
    payload: &[T],
    following: &[T],
    limiter_pair: &LimiterPair<T>,
) -> Option<NonZeroUsize> {
    let base = limiter_pair.least_repeat?.get();
    let ul = limiter_pair.limiter.as_slice();
    let ud = limiter_pair.delimiter.as_slice();
    if ul.is_empty() || ud.is_empty() {
        return None;
    }
    let (ulen, dlen) = (ul.len(), ud.len());
    let o = offset.get();
    if o > buffer.len() {
        return None;
    }
    // NOTE: no check on `buffer[..offset]`'s tail here — per the safety contract,
    // `offset` is a clean item boundary where parsing restarts fresh, so the limiter
    // run written at `offset` is exactly `k` units regardless of preceding Ts.
    // (a) the limiter run MUST be exactly `k` units long: the content right after the
    // written limiter slice (payload, or the written delimiter slice for empty/tiny
    // payloads) MUST NOT start with a full limiter unit.
    let mut extends = true;
    for i in 0..ulen {
        let t = if i < payload.len() {
            &payload[i]
        } else {
            &ud[(i - payload.len()) % dlen]
        };
        if *t != ul[i] {
            extends = false;
            break;
        }
    }
    if extends {
        return None;
    }
    // (b) find the minimal repeat whose delimiter slice is first confirmed exactly at
    // payload end. Repeats greater than the maximal delimiter unit run inside payload
    // can only fail for repeat-independent (boundary/confirmation) reasons, so they
    // bound the search.
    let mut max_run = 0usize;
    for p in 0..payload.len() {
        if unit_match(payload, p, ud) {
            let mut r = 0usize;
            while unit_match(payload, p + r * dlen, ud) {
                r += 1;
            }
            max_run = max_run.max(r);
        }
    }
    let k_max = base.max(max_run + 1);
    for k in base..=k_max {
        let w = k * dlen;
        // simulate `payload ++ delimiter*k ++ following`.
        let mut ctx = Vec::with_capacity(payload.len() + w + following.len());
        ctx.extend_from_slice(payload);
        for _ in 0..k {
            ctx.extend_from_slice(ud);
        }
        ctx.extend_from_slice(following);
        let mut p = 0usize;
        let mut ok = false;
        while p + w <= ctx.len() && p <= payload.len() {
            if repeat_match(&ctx, p, ud, k) {
                match variant {
                    Variant::V1 => {
                        ok = p == payload.len();
                        break;
                    }
                    Variant::V2 => {
                        let after = p + w;
                        if after == ctx.len() {
                            // the delimiter slice can never be confirmed
                            // (application should push a non-delimiter T).
                            return None;
                        }
                        let rem = &ctx[after..];
                        if rem.len() < dlen {
                            if ud.starts_with(rem) {
                                return None; // unconfirmable partial unit at the end
                            }
                            ok = p == payload.len();
                            break;
                        } else if unit_match(&ctx, after, ud) {
                            p += 1;
                            continue;
                        } else {
                            ok = p == payload.len();
                            break;
                        }
                    }
                }
            }
            p += 1;
        }
        if ok {
            return NonZeroUsize::new(k);
        }
    }
    None
}

/// Return a vec of all limiter_pairs that CAN serialize given `payload` under given context.
///
/// Each returned pair carries the minimal working repeat in its `least_repeat`.
///
/// # Safety
/// same contract as [`can_serialize`].
pub unsafe fn get_all_serializable_limiter_pairs<T: Sized + PartialEq + Clone>(
    variant: Variant,
    buffer: &[T],
    offset: Offset,
    payload: &[T],
    following: &[T],
    limiter_pairs: &[LimiterPair<T>],
) -> Vec<LimiterPair<T>> {
    limiter_pairs
        .iter()
        .filter_map(|pair| {
            // SAFETY: same contract as the caller's.
            let k = unsafe { can_serialize(variant, buffer, offset, payload, following, pair) }?;
            let mut pair = pair.clone();
            pair.least_repeat = Some(k);
            Some(pair)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// free incremental parser
// ---------------------------------------------------------------------------

/// pass old_parser_state as FlatParserState::default(whatever) to start fresh parsing.
/// pass last returned non-unknown parser_state for incremental parsing.
///
/// returned State's head_offset != state's head_offset means old item in old state is confirmed archived, and thus returned State start alfter tail_offset of that item, otherwise current parsing slice cannot be confirmed archived by SDAVE alone, you need to tell whether the producer still stream contents to this buffer and decide how would you call parse_incremental in future.
/// specially, E(Envelop) whose tail_offset is_some() means this envelop is confirmed archived.
///
/// for a streaming `buffer` already containing parsed non-unknown contents, never pass an default state or it reparse from offset 0.
///
/// each call advances the state machine by at most one archived item:
/// - Unknown / NonEnvelop / PartialLimiterSlice: scan NonEnvelop contents until a valid
///   limiter slice (returns a fresh unarchived E), a potential one (PartialLimiterSlice),
///   or end of buffer (NonEnvelop).
/// - unarchived E: scan payload for the delimiter slice; on confirmation the same Envelop
///   is returned with `tail_offset: Some(..)` (archived), otherwise with a grown
///   `payload_tail_offset`.
/// - archived E (tail_offset is Some): start the next item after its tail_offset.
///
/// expected usage: call in a loop until the returned state stops changing (a settled
/// unarchived E/NonEnvelop is terminal), or break midturn if you want. One call returns
/// after a single step, so whole-buffer draining requires that loop.
///
/// # Safety
/// old_parser_state MUST match buffer's content by offset and semantics.
/// You MUST manually offset old_parser_state if you advance the buffer.
pub unsafe fn parse_incremental<T: Sized + PartialEq + Clone>(
    variant: Variant,
    buffer: &[T],
    limiter_pairs: &[LimiterPair<T>],
    old_parser_state: FlatParserState<T>,
) -> FlatParserState<T> {
    let FlatParserState { state, limiter } = old_parser_state;
    match state {
        State::Unknown => scan_non_envelop(buffer, limiter_pairs, 0, 0, limiter),
        State::NonEnvelop {
            head_offset,
            tail_offset,
        } => scan_non_envelop(
            buffer,
            limiter_pairs,
            head_offset.get(),
            tail_offset.get(),
            limiter,
        ),
        State::PartialLimiterSlice { head_offset } => {
            let head = head_offset.get();
            match resolve_limiter(buffer, limiter_pairs, head) {
                Limit::Unterminated => FlatParserState {
                    state: State::PartialLimiterSlice { head_offset },
                    limiter,
                },
                Limit::Valid { pair, run_end } => FlatParserState {
                    state: State::E {
                        envelop: Envelop {
                            head_offset,
                            payload_head_offset: off(run_end),
                            payload_tail_offset: off(run_end),
                            tail_offset: None,
                        },
                    },
                    limiter: limiter_pairs[pair].clone(),
                },
                // the run dissolved into NonEnvelop contents; resume scanning behind it.
                Limit::NotLimiter => {
                    scan_non_envelop(buffer, limiter_pairs, head, head + 1, limiter)
                }
            }
        }
        State::E { envelop } => match envelop.tail_offset {
            Some(tail) => scan_non_envelop(buffer, limiter_pairs, tail.get(), tail.get(), limiter),
            None => {
                let envelop = scan_delimiter(
                    variant,
                    buffer,
                    &limiter,
                    envelop.head_offset.get(),
                    envelop.payload_head_offset.get(),
                    envelop.payload_tail_offset.get(),
                );
                FlatParserState {
                    state: State::E { envelop },
                    limiter,
                }
            }
        },
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(x: usize) -> NonZeroUsize {
        NonZeroUsize::new(x).unwrap()
    }
    fn pair(l: &[u8], d: &[u8], r: usize) -> LimiterPair<u8> {
        LimiterPair {
            least_repeat: Some(nz(r)),
            limiter: l.to_vec(),
            delimiter: d.to_vec(),
        }
    }
    /// the README example set, minus the multi-byte ('♦','♣',1) which doesn't fit a u8 buffer.
    fn std_pairs() -> Vec<LimiterPair<u8>> {
        vec![
            pair(b"^", b"~", 2),
            pair(b"?", b"!", 2),
            pair(b"%", b"%", 2),
            pair(b"{", b"}", 2),
        ]
    }

    #[derive(Clone, Debug, PartialEq)]
    enum Item {
        Unknown,
        Partial(usize),
        Ne(usize, usize),
        PhantomE,
        Env(usize, usize, usize, Option<usize>),
    }
    fn item_of(s: &State) -> Item {
        match s {
            State::Unknown => Item::Unknown,
            State::PartialLimiterSlice { head_offset } => Item::Partial(head_offset.get()),
            State::NonEnvelop {
                head_offset,
                tail_offset,
            } => Item::Ne(head_offset.get(), tail_offset.get()),
            State::E { envelop } => {
                if envelop.is_phantom() {
                    Item::PhantomE
                } else {
                    Item::Env(
                        envelop.head_offset.get(),
                        envelop.payload_head_offset.get(),
                        envelop.payload_tail_offset.get(),
                        envelop.tail_offset.map(|t| t.get()),
                    )
                }
            }
        }
    }
    fn collect(variant: Variant, buf: &[u8], pairs: &[LimiterPair<u8>]) -> Vec<Item> {
        let mut parser = FlatParser::new(buf, variant, pairs.to_vec());
        parser.parse_incremental();
        parser.iter().map(|s| item_of(&s)).collect()
    }

    #[test]
    fn non_max_usize_basics() {
        let mut x = unsafe { NonMaxUsize::new(41) };
        assert_eq!(x.get(), 41);
        unsafe { x.offset_by(11) };
        assert_eq!(x.get(), 30);
    }

    // ---- Variant1: README examples ----

    #[test]
    fn v1_mixed() {
        assert_eq!(
            collect(Variant::V1, b"0%%A%%%%B%%%%C%%1", &std_pairs()),
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
    }

    #[test]
    fn v1_mixed_trailing_limiter_char() {
        // `0%%A%%%%B%%%%C%%1%` -> [0, A, B, C, 1]: the trailing `%` is a potential
        // limiter slice, it neither archives nor extends the trailing NonEnvelop.
        assert_eq!(
            collect(Variant::V1, b"0%%A%%%%B%%%%C%%1%", &std_pairs()),
            collect(Variant::V1, b"0%%A%%%%B%%%%C%%1", &std_pairs())
        );
    }

    #[test]
    fn v1_pending_long_run() {
        assert_eq!(
            collect(Variant::V1, b"0%%%%%%B%%%%C%%1", &std_pairs()),
            vec![Item::PhantomE, Item::Ne(0, 1), Item::Env(1, 7, 16, None)]
        );
    }

    #[test]
    fn v1_empty_payload() {
        assert_eq!(
            collect(Variant::V1, b"0^^~~^^B~~{{C}}1", &std_pairs()),
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
    }

    #[test]
    fn v1_long_run() {
        assert_eq!(
            collect(Variant::V1, b"1^^^echo^^hello~~~~~2", &std_pairs()),
            vec![
                Item::PhantomE,
                Item::Ne(0, 1),
                Item::Env(1, 4, 15, Some(18)),
                Item::Ne(18, 21),
            ]
        );
    }

    #[test]
    fn v1_shared_delimiter_limiter() {
        let pairs = vec![pair(b"^", b"~", 2), pair(b"~", b"%", 2)];
        assert_eq!(
            collect(Variant::V1, b"0^^A~~~~B%%~~C%%1", &pairs),
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
    }

    #[test]
    fn v1_shared_limiter() {
        let pairs = vec![
            pair(b"^", b"~", 2),
            pair(b"^", b"~~", 2),
            pair(b"^", b"!", 2),
        ];
        assert_eq!(
            collect(Variant::V1, b"1^^A!B!~~~~C", &pairs),
            vec![
                Item::PhantomE,
                Item::Ne(0, 1),
                Item::Env(1, 3, 7, Some(9)),
                Item::Ne(9, 12),
            ]
        );
    }

    // ---- Variant2: README examples ----

    #[test]
    fn v2_mixed() {
        assert_eq!(
            collect(Variant::V2, b"0%%A%%%%B%%%%C%%1", &std_pairs()),
            vec![
                Item::PhantomE,
                Item::Ne(0, 1),
                Item::Env(1, 3, 6, Some(8)),
                Item::Ne(8, 9),
                Item::Env(9, 13, 17, None),
            ]
        );
    }

    #[test]
    fn v2_braces() {
        assert_eq!(
            collect(Variant::V2, b"0{{A}}{{B}}??C!!1", &std_pairs()),
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
    }

    #[test]
    fn v2_braces_pending() {
        assert_eq!(
            collect(Variant::V2, b"0{{A}}{{B}}??C!!!", &std_pairs()),
            vec![
                Item::PhantomE,
                Item::Ne(0, 1),
                Item::Env(1, 3, 4, Some(6)),
                Item::Ne(6, 6),
                Item::Env(6, 8, 9, Some(11)),
                // the pending Envelop shares E2's tail: no NonEnvelop slot between them.
                // payload_tail=15 is the settled report: the `!!` slice at 14..16 is
                // followed by another `!` and the `!!` at 15..17 ends at EOB, so neither
                // is confirmed; `buffer[13..15]` (`C!`) is the confirmed-free prefix.
                Item::Env(11, 13, 15, None),
            ]
        );
    }

    #[test]
    fn v2_long_run() {
        assert_eq!(
            collect(Variant::V2, b"1^^^echo^^hello~~~~~2", &std_pairs()),
            vec![
                Item::PhantomE,
                Item::Ne(0, 1),
                Item::Env(1, 4, 17, Some(20)),
                Item::Ne(20, 21),
            ]
        );
    }

    #[test]
    fn v2_long_run_pending() {
        assert_eq!(
            collect(Variant::V2, b"1^^^echo^^hello~~~~~", &std_pairs()),
            vec![Item::PhantomE, Item::Ne(0, 1), Item::Env(1, 4, 17, None)]
        );
    }

    #[test]
    fn v2_shared_delimiter_limiter() {
        let pairs = vec![pair(b"^", b"~", 2), pair(b"~", b"%", 2)];
        assert_eq!(
            collect(Variant::V2, b"0^^A~~~~B%%~~C%%1", &pairs),
            vec![
                Item::PhantomE,
                Item::Ne(0, 1),
                Item::Env(1, 3, 6, Some(8)),
                Item::Ne(8, 11),
                Item::Env(11, 13, 14, Some(16)),
                Item::Ne(16, 17),
            ]
        );
    }

    #[test]
    fn v2_shared_limiter_order_a() {
        let pairs = vec![
            pair(b"^", b"~", 2),
            pair(b"^", b"~~", 2),
            pair(b"^", b"!", 2),
        ];
        assert_eq!(
            collect(Variant::V2, b"1^^A!B!~~~~C", &pairs),
            vec![
                Item::PhantomE,
                Item::Ne(0, 1),
                Item::Env(1, 3, 9, Some(11)),
                Item::Ne(11, 12),
            ]
        );
    }

    #[test]
    fn v2_shared_limiter_order_b() {
        let pairs = vec![
            pair(b"^", b"~~", 2),
            pair(b"^", b"~", 2),
            pair(b"^", b"!", 2),
        ];
        assert_eq!(
            collect(Variant::V2, b"1^^A!B!~~~~C", &pairs),
            vec![
                Item::PhantomE,
                Item::Ne(0, 1),
                Item::Env(1, 3, 7, Some(11)),
                Item::Ne(11, 12),
            ]
        );
    }

    // ---- streaming / advancing ----

    #[test]
    fn streaming_growth() {
        let pairs = std_pairs();
        let buf1: &[u8] = b"abc^";
        let mut parser = FlatParser::new(buf1, Variant::V1, pairs.clone());
        parser.parse_incremental();
        assert!(matches!(
            parser.parser_state().state(),
            State::PartialLimiterSlice { .. }
        ));
        // append more contents: the partial limiter slice resolves, an Envelop archives.
        let buf2: &[u8] = b"abc^^x~~";
        unsafe { parser.replace_buffer(buf2) };
        parser.parse_incremental();
        let items: Vec<Item> = parser.iter().map(|s| item_of(&s)).collect();
        assert_eq!(
            items,
            vec![
                Item::PhantomE,
                Item::Ne(0, 3),
                Item::Env(3, 5, 6, Some(8)),
                Item::Ne(8, 8),
            ]
        );
    }

    #[test]
    fn partial_limiter_slice_state() {
        let mut parser = FlatParser::new(b"abc^", Variant::V1, std_pairs());
        parser.parse_incremental();
        match parser.parser_state().state() {
            State::PartialLimiterSlice { head_offset } => assert_eq!(head_offset.get(), 3),
            s => panic!("expected PartialLimiterSlice, got {s:?}"),
        }
        let items: Vec<Item> = parser.iter().map(|s| item_of(&s)).collect();
        assert_eq!(items, vec![Item::PhantomE, Item::Ne(0, 3)]);
    }

    #[test]
    fn advance_shifts_and_counts() {
        let buf: &[u8] = b"0%%A%%1%%B%%2";
        let mut parser = FlatParser::new(buf, Variant::V1, std_pairs());
        parser.parse_all();
        // items: PhantomE, Ne(0,1), E(1..6), Ne(6,7), E(7..12), Ne(12,13)
        let removed = unsafe {
            parser.replace_buffer(&buf[7..]);
            parser.advance(off(7))
        };
        // removed: leading Ne(0,1), E(1..6), Ne(6,7)
        assert_eq!(removed, 3);
        let items: Vec<Item> = parser.iter().map(|s| item_of(&s)).collect();
        assert_eq!(items, vec![Item::Env(0, 2, 3, Some(5)), Item::Ne(5, 6)]);
    }

    // ---- serializing ----

    #[test]
    fn serialize_v1() {
        let p = pair(b"%", b"%", 2);
        assert_eq!(
            unsafe { can_serialize(Variant::V1, b"0", off(1), b"A", b"1", &p) },
            Some(nz(2))
        );
        // payload containing a delimiter run forces a longer repeat
        assert_eq!(
            unsafe { can_serialize(Variant::V1, b"0", off(1), b"A%%B", b"1", &p) },
            Some(nz(3))
        );
        // payload tail colliding with the delimiter unit disables the pair for V1
        assert_eq!(
            unsafe { can_serialize(Variant::V1, b"0", off(1), b"A%", b"1", &p) },
            None
        );
    }

    #[test]
    fn serialize_v2() {
        let p = pair(b"%", b"%", 2);
        assert_eq!(
            unsafe { can_serialize(Variant::V2, b"0", off(1), b"A", b"1", &p) },
            Some(nz(2))
        );
        // V2 tolerates payload tail colliding with the delimiter unit
        assert_eq!(
            unsafe { can_serialize(Variant::V2, b"0", off(1), b"A%", b"1", &p) },
            Some(nz(2))
        );
        // but needs a non-delimiter following to confirm the delimiter slice
        assert_eq!(
            unsafe { can_serialize(Variant::V2, b"0", off(1), b"A", b"", &p) },
            None
        );
        assert_eq!(
            unsafe { can_serialize(Variant::V2, b"0", off(1), b"A", b"%", &p) },
            None
        );
    }

    #[test]
    fn serialize_edge_cases() {
        // least_repeat None: never serializable
        let mut p = pair(b"^", b"~", 2);
        p.least_repeat = None;
        assert_eq!(
            unsafe { can_serialize(Variant::V1, b"0", off(1), b"A", b"1", &p) },
            None
        );
        let p2 = pair(b"^", b"~", 2);
        // payload starting with the limiter unit can never be framed exactly
        assert_eq!(
            unsafe { can_serialize(Variant::V1, b"0", off(1), b"^A", b"1", &p2) },
            None
        );
        // empty payload works when limiter != delimiter
        assert_eq!(
            unsafe { can_serialize(Variant::V1, b"0", off(1), b"", b"1", &p2) },
            Some(nz(2))
        );
        // but not when limiter == delimiter (the slice becomes one long limiter run)
        let p3 = pair(b"%", b"%", 2);
        assert_eq!(
            unsafe { can_serialize(Variant::V1, b"0", off(1), b"", b"1", &p3) },
            None
        );
        // context tailing with an archived delimiter slice is a clean restart boundary,
        // so two contiguous V1 Envelopes can share the same limiter pair
        let p4 = pair(b"%", b"%", 2);
        assert_eq!(
            unsafe { can_serialize(Variant::V1, b"0%%A%%", off(6), b"B", b"1", &p4) },
            Some(nz(2))
        );
    }

    #[test]
    fn all_serializable_pairs() {
        let pairs = vec![pair(b"^", b"~", 2), pair(b"%", b"%", 2)];
        let got = unsafe {
            get_all_serializable_limiter_pairs(Variant::V1, b"0", off(1), b"A~~B", b"1", &pairs)
        };
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].least_repeat, Some(nz(3)));
        assert_eq!(got[1].least_repeat, Some(nz(2)));
    }

    // ---- free function ----

    #[test]
    fn free_fn_stepwise() {
        let pairs = std_pairs();
        let buf: &[u8] = b"0^^A~~1";
        let st = FlatParserState::default(pairs[0].clone());
        let st = unsafe { parse_incremental(Variant::V1, buf, &pairs, st) };
        match &st.state {
            State::E { envelop } => {
                assert_eq!(envelop.head_offset.get(), 1);
                assert!(envelop.tail_offset.is_none());
            }
            s => panic!("expected unarchived E, got {s:?}"),
        }
        let st = unsafe { parse_incremental(Variant::V1, buf, &pairs, st) };
        match &st.state {
            State::E { envelop } => {
                assert_eq!(envelop.payload_tail_offset.get(), 4);
                assert_eq!(envelop.tail_offset.map(|t| t.get()), Some(6));
            }
            s => panic!("expected archived E, got {s:?}"),
        }
        let st = unsafe { parse_incremental(Variant::V1, buf, &pairs, st) };
        assert!(matches!(st.state, State::NonEnvelop { .. }));
        let st2 = unsafe { parse_incremental(Variant::V1, buf, &pairs, st.clone()) };
        assert_eq!(st2.state, st.state, "state machine should converge");
    }

    // ---- one-step method contract ----

    #[test]
    fn parse_incremental_method_is_one_step() {
        let buf: &[u8] = b"0%%A%%1%%B%%2";
        let mut parser = FlatParser::new(buf, Variant::V1, std_pairs());
        // step 1: the leading NonEnvelop is archived and the first limiter slice located.
        parser.parse_incremental();
        assert_eq!(item_of(parser.parser_state().state()), Item::Env(1, 3, 3, None));
        assert_eq!(parser.archived_boundaries().len(), 1);
        assert_eq!(parser.tail_non_envelop(), Some(off(1)));
        // step 2: the first Envelop archives.
        parser.parse_incremental();
        assert_eq!(item_of(parser.parser_state().state()), Item::Env(1, 3, 4, Some(6)));
        assert_eq!(parser.archived_boundaries().len(), 2);
        assert_eq!(parser.tail_non_envelop(), None);
        // step 3: scan toward the next item.
        parser.parse_incremental();
        assert_eq!(item_of(parser.parser_state().state()), Item::Env(7, 9, 9, None));
        // step 4: it archives.
        parser.parse_incremental();
        assert_eq!(item_of(parser.parser_state().state()), Item::Env(7, 9, 10, Some(12)));
        // step 5: the trailing NonEnvelop.
        parser.parse_incremental();
        assert_eq!(item_of(parser.parser_state().state()), Item::Ne(12, 13));
        // step 6+: settled; further calls change nothing.
        let settled = parser.parser_state().state().clone();
        let boundaries = parser.archived_boundaries().len();
        parser.parse_incremental();
        parser.parse_incremental();
        assert_eq!(parser.parser_state().state(), &settled);
        assert_eq!(parser.archived_boundaries().len(), boundaries);
    }

    #[test]
    fn parse_incremental_leaves_partial_tail_unconsumed() {
        // trailing `1^`: the lone `^` is a potential limiter slice, not an item.
        let mut parser = FlatParser::new(b"0^^A~~1^", Variant::V1, std_pairs());
        parser.parse_all();
        match parser.parser_state().state() {
            State::PartialLimiterSlice { head_offset } => assert_eq!(head_offset.get(), 7),
            s => panic!("expected PartialLimiterSlice, got {s:?}"),
        }
        let boundaries = parser.archived_boundaries().len();
        let tail = parser.tail_non_envelop();
        parser.parse_incremental();
        parser.parse_incremental();
        assert_eq!(parser.archived_boundaries().len(), boundaries);
        assert_eq!(parser.tail_non_envelop(), tail);
        assert!(matches!(parser.parser_state().state(), State::PartialLimiterSlice { .. }));
    }

    #[test]
    fn iter_yields_one_item_without_intermediate_steps() {
        // One-step-parser regression pin: `iter` must present exactly one observable item
        // per yield. When two Envelopes are contiguous (a zero-length NonEnvelop gap), the
        // intermediate "limiter located but not yet archived/delimiter-scanned" step is
        // internal and must be swallowed. The worked examples above assert exact sequences;
        // this states the invariant they rely on: no yielded Envelop carries a `None` tail.
        let items = collect(Variant::V1, b"0%%A%%%%B%%%%C%%1", &std_pairs());
        assert_eq!(items.len(), 8);
        assert_eq!(items[3], Item::Ne(6, 6));
        assert_eq!(items[5], Item::Ne(11, 11));
        for item in &items {
            if let Item::Env(_, _, _, tail) = item {
                assert!(tail.is_some(), "iter leaked an unarchived Envelop: {item:?}");
            }
        }
    }

    #[test]
    fn parse_all_equals_stepping_to_settle() {
        let buf: &[u8] = b"0%%A%%1%%B%%2";
        let mut drained = FlatParser::new(buf, Variant::V1, std_pairs());
        drained.parse_all();

        let mut stepped = FlatParser::new(buf, Variant::V1, std_pairs());
        let mut prev = stepped.parser_state().state().clone();
        loop {
            stepped.parse_incremental();
            let now = stepped.parser_state().state().clone();
            if now == prev {
                break;
            }
            prev = now;
        }
        assert_eq!(drained.parser_state().state(), stepped.parser_state().state());
        assert_eq!(
            drained.archived_boundaries().len(),
            stepped.archived_boundaries().len()
        );
        assert_eq!(drained.tail_non_envelop(), stepped.tail_non_envelop());
    }
}
