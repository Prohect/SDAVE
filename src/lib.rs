#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::{alloc::Allocator, cell::OnceCell, num::NonZeroUsize};

const NON_MAX_ERROR_MESSAGE: &str = "[SDAVE] NonMaxUsize exceeds usize::MAX";

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NonMaxUsize {
    non_max: NonZeroUsize,
}
impl NonMaxUsize {
    pub fn get(&self) -> usize {
        self.non_max.get() - 1
    }
    /// # Panics
    /// Panic when `non_max == usize::MAX`.
    pub fn new(non_max: usize) -> Self {
        NonMaxUsize {
            non_max: NonZeroUsize::new(non_max.wrapping_add(1)).expect(NON_MAX_ERROR_MESSAGE),
        }
    }
}
/// Absolute offset for buffer index.
pub type Offset = NonMaxUsize;

/// Delimiter match algorithm variant. There's not a full winner, choose based on scene.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// The every first full matched delimiter slice is just the delimiter slice. No delimiter slice confirmation latency, but tail of payload MUST NOT collide with the delimiter unit.
    V1,
    // For inplace insertion usage where the item behind MUST be materialized BEFORE this V2 Envelop, the item behind this might disable a limiter pair. This usage is NOT supported.
    /// The every first full matched delimiter slice FOLLOWED by a NON-delimiter is the delimiter slice. Tail of payload won't disable a limiter pair, but introduces delimiter slice confirmation latency (solvable on implementation layer by pushing a non-delimiter to the mixed channel).
    /// For streaming, a V2 Envelop reserves the delimiter from the header of the item behind.
    V2,
}

/// `T`: basic data unit of a slice.
/// `limiter`/`delimiter` immutable once inited, Vec of T, e.g. UTF-8 1B/2B/3B char in a u8 buffer.
#[derive(Clone, PartialEq)]
pub struct LimiterPair<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    least_repeat: Option<NonZeroUsize>,
    limiter: OnceCell<&'l Vec<T, A>>,
    delimiter: OnceCell<&'l Vec<T, A>>,
}
impl<'l, T, A> LimiterPair<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// # Panics
    /// Panic when limiter or delimiter or both not inited.
    pub fn new(
        least_repeat: Option<NonZeroUsize>,
        limiter: OnceCell<&'l Vec<T, A>>,
        delimiter: OnceCell<&'l Vec<T, A>>,
    ) -> Self {
        assert!(limiter.get().is_some());
        assert!(delimiter.get().is_some());
        Self {
            least_repeat,
            limiter,
            delimiter,
        }
    }
    /// Case deserialization: as the algorithm match `limiter`/`delimiter` first, then guard on `least_repeat`, assertion(repeat>=1) is always true for niche optimization; LimiterPair with `least_repeat` == `None` would be ignored.
    ///
    /// Case serialization: the minimal repeat to frame certain payload being inserted at `buffer[..offset] _here_ buffer[offset..]` without introducing conflicts, or `None` if this LimiterPair is impossible to frame such payload without introducing conflicts under such condition.
    pub fn least_repeat(&self) -> Option<NonZeroUsize> {
        self.least_repeat
    }
    /// LimiterPair with empty limiter would be ignored.
    pub fn limiter(&self) -> &'l Vec<T, A> {
        self.limiter.get().unwrap()
    }
    /// LimiterPair with empty delimiter would be ignored.
    pub fn delimiter(&self) -> &'l Vec<T, A> {
        self.delimiter.get().unwrap()
    }
}

#[derive(Clone, PartialEq)]
pub enum State {
    E { envelop: Envelop },
    N { non_envelop: NonEnvelop },
}
impl State {
    pub fn head_offset(&self) -> Offset {
        match self {
            State::E { envelop } => envelop.head_offset(),
            State::N { non_envelop } => non_envelop.head_offset(),
        }
    }
    pub fn semantical_tail_offset(&self) -> Offset {
        match self {
            State::E { envelop } => {
                if let Some(tail_offset) = envelop.tail_offset() {
                    tail_offset
                } else {
                    envelop.payload_tail_offset()
                }
            }
            State::N { non_envelop } => non_envelop.tail_offset(),
        }
    }
}
#[derive(Clone, PartialEq)]
pub struct Envelop {
    head_offset: Offset,
    payload_head_offset: Offset,
    payload_tail_offset: Offset,
    tail_offset: Option<Offset>,
}
impl Envelop {
    pub fn head_offset(&self) -> Offset {
        self.head_offset
    }
    /// Limiter slice is guaranteed well defined(at least one non-limiter T(either payload head or delimiter != to limiter) exists following limiter slice).
    pub fn payload_head_offset(&self) -> Offset {
        self.payload_head_offset
    }
    // An archived Envelop, with limiter != delimiter, can has empty payload, e.g. `^^~~`, in case of which, payload_tail_offset == payload_head_offset.
    /// Can monotonic increase in next updates, `buffer[payload_head_offset..payload_tail_offset]` is guaranteed NOT 1.contains valid delimiter slice; 2.ends with potential delimiter slice.
    pub fn payload_tail_offset(&self) -> Offset {
        self.payload_tail_offset
    }
    /// `Some(offset)` means this Envelop is confirmed archived, would never grow again.
    pub fn tail_offset(&self) -> Option<Offset> {
        self.tail_offset
    }
    /// A phantom Envelop is a zero-width none-tail_offset marker.
    /// Can be used as first element of `[FlatDeserializer::archived_boundaries]` to indicate the first archived item is an NonEnvelop.
    pub fn is_phantom(&self) -> bool {
        self.tail_offset().is_none()
            && self.head_offset() == self.payload_head_offset()
            && self.payload_head_offset() == self.payload_tail_offset()
    }
    /// Construct a phantom Envelop with `offset`.
    ///
    /// # Safety
    /// You should not use rely on this function.
    pub unsafe fn phantom(offset: Offset) -> Self {
        Envelop {
            head_offset: offset,
            payload_head_offset: offset,
            payload_tail_offset: offset,
            tail_offset: None,
        }
    }
}
#[derive(Clone, PartialEq)]
pub struct NonEnvelop {
    head_offset: Offset,
    tail_offset: Offset,
}
impl NonEnvelop {
    pub fn head_offset(&self) -> Offset {
        self.head_offset
    }
    /// Can monotonic increase in next updates, `buffer[head_offset..tail_offset]` is guaranteed NOT 1.contains valid limiter slice; 2.ends with potential limiter slice.
    pub fn tail_offset(&self) -> Offset {
        self.tail_offset
    }
    /// A phantom NonEnvelop is a zero-width marker.
    /// Can be used to indicate sequential Envelopes for `FlatDeserializer`.
    pub fn is_phantom(&self) -> bool {
        self.head_offset == self.tail_offset
    }
    /// Construct a phantom NonEnvelop with `offset`.
    ///
    /// # Safety
    /// You should not use rely on this function.
    pub unsafe fn phantom(offset: Offset) -> Self {
        NonEnvelop {
            head_offset: offset,
            tail_offset: offset,
        }
    }
}

// ANY `tail_offset`, when constructed, MUST be eq to buffer.len().
// Store confirmed midway results at `next`.
/// Current deserializing slice: `buffer[head_offset..tail_offset]`.
#[derive(Clone, PartialEq)]
pub enum InternalState {
    // Current deserializing slice MUST be scanned/checked AGAIN next `deserialize_incremental`.
    /// Unknown whether current deserializing slice is part of an unarchived NonEnvelop OR part of a live streaming limiter slice, or is part of payload of an unarchived Envelop OR part of a live streaming delimiter slice.
    Unknown {
        // head_offset: from `next.semantical_tail_offset()`.
        tail_offset: Offset,
    },
    // Current deserializing slice MUST be scanned/checked AGAIN next `deserialize_incremental`.
    /// Current deserializing slice MUST be a limiter slice but limiter_pair/repeats are not well defined yet (not terminated by a not-repeating `T`.
    UnterminatedLimiterSlice {
        // head_offset: from `next.semantical_tail_offset()`.
        tail_offset: Offset,
    },
    // Current deserializing slice MUST be scanned/checked AGAIN next `deserialize_incremental`.
    /// Current deserializing slice MUST be a delimiter slice but not well defined yet (not terminated by a not-repeating `T`. Legal only when `working_on` is a V2 Envelop.
    UnterminatedDeLimiterSlice {
        // head_offset: from `next.semantical_tail_offset()`.
        tail_offset: Offset,
    },
    // The deserilizer MUST not rescan/recheck `buffer[..working_on.semantical_tail]`.
    /// The buffer is guaranteed fully deserialized. Semantical tail of `working_on` == current buffer.len().
    Clean,
    // Legal clean && archived state MUST be promoted to `CleanArchived`.
    // The buffer MUST not be fully deserialized.
    // The deserilizer MUST not rescan/recheck `buffer[..working_on.semantical_tail]`.
    /// `working_on` is confirmed archived.
    /// `next` would differ from `working_on` telling what's next.
    Archived,
    // When senmantically Clean:
    // a. when the last item is NonEnvelop,
    // it's impossible for SDAVE alone to assert this NonEnvelop is archived;
    // b. when the last item is Envelop,
    // ONLY V1 Envelop can be asserted archived -- V2 Envelop MUST need header of the next item to terminated it.
    /// `Clean` + `Archived`. Legal only when `working_on` is a V1 Envelop.
    /// `next` would be a phantom NonEnvelop.
    CleanArchived,
}
impl InternalState {
    pub fn is_archived(&self) -> bool {
        matches!(self, InternalState::Archived | InternalState::CleanArchived)
    }
    pub fn is_clean(&self) -> bool {
        matches!(self, InternalState::Clean | InternalState::CleanArchived)
    }
    /// Is the buffer fully deserialized?
    pub fn is_exhausted(&self) -> bool {
        !matches!(self, InternalState::Archived)
    }
}

pub type DecoupledFlatDeserializerState<'l, 'v, T, A> = (
    State,
    InternalState,
    State,
    Variant,
    LimiterPair<'l, T, A>,
    Option<Vec<Option<NonMaxUsize>, A>>,
    &'v Vec<LimiterPair<'l, T, A>, A>,
);
pub struct FlatDeserializerState<'l, 'v, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    working_on: State,
    internal: InternalState,
    next: State,
    variant: Variant,
    limiter_pair: LimiterPair<'l, T, A>,
    repeat_map_on_cursor: Option<Vec<Option<NonMaxUsize>, A>>,
    limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
}
impl<'l, 'v, T, A> FlatDeserializerState<'l, 'v, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// `repeat_map_on_cursor` is set to `None` instead of cloning.
    /// Move or mannual clone it if needed.
    pub fn clone(&self) -> Self {
        Self {
            working_on: self.working_on.clone(),
            internal: self.internal.clone(),
            next: self.next.clone(),
            variant: self.variant.clone(),
            limiter_pair: self.limiter_pair.clone(),
            repeat_map_on_cursor: None,
            limiter_pairs: self.limiter_pairs,
        }
    }
    /// `repeat_map_on_cursor` woulb be cleared.
    pub fn new(
        variant: Variant,
        limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
        mut repeat_map_on_cursor: Vec<Option<NonMaxUsize>, A>,
    ) -> Self {
        let limiter_pair = limiter_pairs
            .first()
            .expect("[SDAVE] empty limiter_pairs")
            .clone();
        repeat_map_on_cursor.clear();
        for limiter_pair in limiter_pairs {
            if limiter_pair.least_repeat().is_some()
                && !limiter_pair.limiter().is_empty()
                && !limiter_pair.delimiter().is_empty()
            {
                repeat_map_on_cursor.push(Some(NonMaxUsize::new(0)));
            } else {
                repeat_map_on_cursor.push(None);
            }
        }
        FlatDeserializerState {
            working_on: State::N {
                non_envelop: unsafe { NonEnvelop::phantom(NonMaxUsize::new(0)) },
            },
            internal: InternalState::Archived,
            next: State::N {
                non_envelop: unsafe { NonEnvelop::phantom(NonMaxUsize::new(0)) },
            },
            variant,
            limiter_pair,
            repeat_map_on_cursor: Some(repeat_map_on_cursor),
            limiter_pairs,
        }
    }
    pub fn decouple(self) -> DecoupledFlatDeserializerState<'l, 'v, T, A> {
        (
            self.working_on,
            self.internal,
            self.next,
            self.variant,
            self.limiter_pair,
            self.repeat_map_on_cursor,
            self.limiter_pairs,
        )
    }
    /// # Safety
    /// Operation to buffer contents MUST equivalently be conbination of [advance, append], you MUST call FlatDeserializer::advance(offset) if buffer is advanced.
    pub unsafe fn new_unchecked(
        flat_deserializer_state: DecoupledFlatDeserializerState<'l, 'v, T, A>,
    ) -> Self {
        FlatDeserializerState {
            working_on: flat_deserializer_state.0,
            internal: flat_deserializer_state.1,
            next: flat_deserializer_state.2,
            variant: flat_deserializer_state.3,
            limiter_pair: flat_deserializer_state.4,
            repeat_map_on_cursor: flat_deserializer_state.5,
            limiter_pairs: flat_deserializer_state.6,
        }
    }
    /// # Safety
    /// You MUST call this if you directly use `deserialize_incremental` on a buffer, instead of use `FlatDeserializer`, when you equivalently advance the buffer. You MUST not call this if you use `FlatDeserializer`(via `decouple`) since `FlatDeserialier::advance` already advanve its `FlatDeserializerState`.
    ///
    /// # Panics
    /// Panic when offset > semantical tail of `working_on`.
    pub unsafe fn advance(&mut self, offset: Offset) {
        if self.internal.is_archived() {
            let tail = {
                match &self.working_on {
                    State::N { non_envelop } => non_envelop.tail_offset.get(),
                    State::E { envelop } => envelop.tail_offset.expect("[SDAVE] Illegal state: FlatDeserializerState.is_archived() but working_on Envelop has None tail_offset").get(),
                }
            };
            assert!(offset.get() <= tail);
        } else {
            // Safety:
            // Once a State of Envelop is constructed, its LimiterPair is confirmed archived and stored as `limiter_pair`, which means truncating header of such Envelop won't cause semantics stability issue, but truncating Unknow tail or UnterminatedDeLimiterSlice tail following MUST cause semantics stability issue.
            let tail = {
                match &self.working_on {
                    State::N { non_envelop } => non_envelop.tail_offset.get(),
                    State::E { envelop } => envelop.payload_tail_offset.get(),
                }
            };
            assert!(offset.get() <= tail);
        }
        let shift = |x: Offset| NonMaxUsize::new(x.get().saturating_sub(offset.get()));
        match &mut self.working_on {
            State::N { non_envelop } => {
                non_envelop.head_offset = shift(non_envelop.head_offset);
                non_envelop.tail_offset = shift(non_envelop.tail_offset);
            }
            State::E { envelop } => {
                envelop.head_offset = shift(envelop.head_offset);
                envelop.payload_head_offset = shift(envelop.payload_head_offset);
                envelop.payload_tail_offset = shift(envelop.payload_tail_offset);
                if let Some(tail_offset) = envelop.tail_offset {
                    envelop.tail_offset = Some(shift(tail_offset));
                }
            }
        }
        match &mut self.next {
            State::N { non_envelop } => {
                non_envelop.head_offset = shift(non_envelop.head_offset);
                non_envelop.tail_offset = shift(non_envelop.tail_offset);
            }
            State::E { envelop } => {
                envelop.head_offset = shift(envelop.head_offset);
                envelop.payload_head_offset = shift(envelop.payload_head_offset);
                envelop.payload_tail_offset = shift(envelop.payload_tail_offset);
                if let Some(tail_offset) = envelop.tail_offset {
                    envelop.tail_offset = Some(shift(tail_offset));
                }
            }
        }
        match &mut self.internal {
            InternalState::Unknown { tail_offset } => {
                *tail_offset = shift(*tail_offset);
            }
            InternalState::UnterminatedLimiterSlice { tail_offset } => {
                *tail_offset = shift(*tail_offset);
            }
            InternalState::UnterminatedDeLimiterSlice { tail_offset } => {
                *tail_offset = shift(*tail_offset);
            }
            _ => {}
        }
    }
    /// Current State working on.
    pub fn working_on(&self) -> &State {
        &self.working_on
    }
    pub fn internal(&self) -> &InternalState {
        &self.internal
    }
    pub fn next(&self) -> &State {
        &self.next
    }
    pub fn variant(&self) -> Variant {
        self.variant
    }
    // `least_repeat` can >= origin.
    /// Current_limiter make sense when and only when current `working_on` is E(Envelop) which guarantees a well defined limiter slice.
    pub fn limiter_pair(&self) -> &LimiterPair<'l, T, A> {
        &self.limiter_pair
    }
    // ALL valid LimiterPair MUST be reset with Some(NonMaxUsize::new(0)) on each cursor advancing.
    /// Index parallel to `limiter_pairs`.
    /// `None` indicates mismatch.
    /// If let Some(non_max) = `repeat_map_on_cursor()[i]` { non_max.get() } indicates current repeats of `limiter_pairs()[i]` in `buffer[head_offset..tail_offset]` where head_offset & tail_offset come from InternalState::Unknown or InternalState::UnterminatedLimiterSlice.
    ///
    /// Assume `len` as-in `[head_offset..head_offset + len]` and `index` as-in limiter_pairs, SDAVE archive the first(1.len; 2.index;) LimiterPair well defining limiter slice(MUST be terminated to be well defined) from cursor..cursor + len growing len from 1 until `head_offset + len` exceeds buffer.len() or repeat_map_on_cursor.any(|l|l.is_some()).is_none() as the ONLY chosen one AKA `limiter_pair()`.
    ///
    /// This simplification can cause semantics stability issue for limiter_pairs matching certain condition. It's considered acceptable.
    pub fn repeat_map_on_cursor(&self) -> Option<&Vec<Option<NonMaxUsize>, A>> {
        self.repeat_map_on_cursor.as_ref()
    }
    pub fn limiter_pairs(&self) -> &'v Vec<LimiterPair<'l, T, A>, A> {
        self.limiter_pairs
    }
    /// `true` means it's guaranteed semantically safe to use the simplification introduced by `repeat_map_on_cursor` for this `limiter_pairs`.
    pub fn is_limiter_pairs_safe(limiter_pairs: &Vec<LimiterPair<'l, T, A>, A>) -> bool {
        // TODO: prove.
        for (i, p) in limiter_pairs.iter().enumerate() {
            if p.limiter() == p.delimiter() {
                return false;
            }
            for p1 in limiter_pairs.iter().skip(i + 1) {
                if p.limiter() == p1.limiter()
                    || p.limiter() == p1.delimiter()
                    || p.delimiter() == p1.limiter()
                    || p.delimiter() == p1.delimiter()
                {
                    return false;
                }
            }
        }
        true
    }
}

pub type DecoupledFlatDeserializer<'a, 'l, 'v, T, A> = (
    &'a [T],
    Vec<Envelop, A>,
    Option<Offset>,
    FlatDeserializerState<'l, 'v, T, A>,
);
pub struct FlatDeserializer<'a, 'l, 'v, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    buffer: &'a [T],
    archived_boundaries: Vec<Envelop, A>,
    tail_non_envelop: Option<Offset>,
    deserializer_state: FlatDeserializerState<'l, 'v, T, A>,
}
impl<'a, 'l, 'v, T, A> FlatDeserializer<'a, 'l, 'v, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// `archived_boundaries` and `repeat_map_on_cursor` would be cleared.
    ///
    /// # Panics
    /// Panic when `limiter_pairs` is empty.
    pub fn new(
        buffer: &'a [T],
        variant: Variant,
        limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
        mut archived_boundaries: Vec<Envelop, A>,
        repeat_map_on_cursor: Vec<Option<NonMaxUsize>, A>,
    ) -> Self {
        archived_boundaries.clear();
        Self {
            buffer,
            archived_boundaries,
            tail_non_envelop: None,
            deserializer_state: FlatDeserializerState::new(
                variant,
                limiter_pairs,
                repeat_map_on_cursor,
            ),
        }
    }
    /// You can advance the buffer(or just alloc new one and copy tailer to new buffer) to save memory or append new contents to it alfter FlatDeserializer release(drop the ref to buffer alfter FlatDeserializer::decouple) read access to your buffer, but you MUST call FlatDeserializer::advance(offset) if you advance your buffer.
    ///
    /// You can replace limiter_pairs, which would be applied to future(deserializer_state based) envelopes(LimiterPair for current unarchived envelop is cloned to avoid semantics stability issue).
    ///
    /// You can replace variant, which would be applied right next deserialize_incremental.
    pub fn decouple(self) -> DecoupledFlatDeserializer<'a, 'l, 'v, T, A> {
        (
            self.buffer,
            self.archived_boundaries,
            self.tail_non_envelop,
            self.deserializer_state,
        )
    }
    /// # Safety
    /// Operation to buffer contents MUST equivalently be conbination of [advance, append], you MUST call FlatDeserializer::advance(offset) if buffer is advanced.
    pub unsafe fn new_unchecked(
        flat_deserializer: DecoupledFlatDeserializer<'a, 'l, 'v, T, A>,
    ) -> Self {
        Self {
            buffer: flat_deserializer.0,
            archived_boundaries: flat_deserializer.1,
            tail_non_envelop: flat_deserializer.2,
            deserializer_state: flat_deserializer.3,
        }
    }
    pub fn iter(&mut self) -> FlatDeserializerIter<'a, '_, 'l, 'v, T, A> {
        self.iter_from(0)
    }
    pub fn iter_from(&mut self, pos: usize) -> FlatDeserializerIter<'a, '_, 'l, 'v, T, A> {
        FlatDeserializerIter {
            flat_deserializer: self,
            pos,
        }
    }
    pub fn deserialize_incremental(&mut self) -> &FlatDeserializerState<'l, 'v, T, A> {
        {
            let new = unsafe {
                crate::deserialize_incremental(self.buffer, &mut self.deserializer_state)
            };
            let old = self.deserializer_state();
            if new.internal.is_archived()
                && (!old.internal.is_archived()
                    || (old.internal.is_archived() && new.working_on != old.working_on))
            {
                match &new.working_on {
                    State::N { non_envelop } => {
                        self.archive_non_envelop(non_envelop.tail_offset().get());
                    }
                    State::E { envelop } => {
                        self.archive_envelop(envelop.clone());
                    }
                }
            }
            self.deserializer_state = new;
        };
        &self.deserializer_state
    }
    /// Fully deserialize the buffer.
    pub fn deserialize_all(&mut self) -> &FlatDeserializerState<'l, 'v, T, A> {
        loop {
            if self.deserialize_incremental().internal().is_exhausted() || self.buffer().is_empty()
            {
                break;
            }
        }
        &self.deserializer_state
    }
    /// Advance `archived_boundaries` & `tail_non_envelop` & `deserializer_state`.
    ///
    /// Archived items fully covered by `[0, offset)` are dropped; an archived Envelop straddling `offset` is kept but truncated (its offsets are clamped, so `buffer[head..payload_head]` no longer matches original limiter slice).
    ///
    /// Return how many archived items being fully removed from cache. Only real items are counted: non-phantom Envelopes and non-empty NonEnvelop holes (phantom markers are structural, not items).
    ///
    /// # Safety
    /// You MUST call this if you use `FlatDeserializer` on a buffer, when you equivalently advance the buffer.
    ///
    /// # Panics
    /// Panic when offset > semantical tail of deserializer_state's working_on.
    pub unsafe fn advance(&mut self, offset: Offset) -> usize {
        let off = offset.get();
        let mut removed = 0usize;
        let len = self.archived_boundaries.len();
        let mut drop = 0usize;
        for i in 0..len {
            let envelop = &self.archived_boundaries[i];
            let hole_end = if i + 1 < len {
                Some(self.archived_boundaries[i + 1].head_offset.get())
            } else {
                self.tail_non_envelop
                    .map(|tail_non_envelop| tail_non_envelop.get())
            };
            match envelop.tail_offset {
                // phantom marker: dropped once the leading NonEnvelop hole is fully consumed.
                None => match hole_end {
                    Some(end) if end <= off => {
                        if end > 0 {
                            removed += 1;
                        }
                        drop += 1;
                    }
                    _ => break,
                },
                Some(tail) => {
                    if tail.get() > off {
                        break;
                    }
                    removed += 1; // the Envelop itself
                    drop += 1;
                    if let Some(end) = hole_end
                        && end <= off
                        && end > tail.get()
                    {
                        removed += 1; // the NonEnvelop hole behind it
                    }
                }
            }
        }
        if len == 0
            && let Some(tail) = self.tail_non_envelop
            && tail.get() <= off
            && tail.get() > 0
        {
            // a standalone leading tail NonEnvelop (reachable via new_unchecked).
            removed += 1;
        }
        let len = self.archived_boundaries.len();
        for i in 0..(len - drop) {
            self.archived_boundaries.swap(i, i + drop);
        }
        self.archived_boundaries.truncate(len - drop);
        let shift = |x: Offset| NonMaxUsize::new(x.get().saturating_sub(off));
        for e in &mut self.archived_boundaries {
            e.head_offset = shift(e.head_offset);
            e.payload_head_offset = shift(e.payload_head_offset);
            e.payload_tail_offset = shift(e.payload_tail_offset);
            if let Some(t) = e.tail_offset {
                e.tail_offset = Some(shift(t));
            }
        }
        self.tail_non_envelop = match self.tail_non_envelop {
            Some(tail_non_envelop) if tail_non_envelop.get() <= off => None,
            Some(tail_non_envelop) => Some(NonMaxUsize::new(tail_non_envelop.get() - off)),
            None => None,
        };
        // a remaining leading hole before the first Envelop needs the phantom marker.
        if let Some(first) = self.archived_boundaries.first() {
            if first.tail_offset.is_some() && first.head_offset.get() > 0 {
                self.archived_boundaries
                    .insert(0, unsafe { Envelop::phantom(NonMaxUsize::new(0)) });
            }
        } else if self.tail_non_envelop.is_some_and(|t| t.get() > 0) {
            self.archived_boundaries
                .push(unsafe { Envelop::phantom(NonMaxUsize::new(0)) });
        }
        unsafe {
            self.deserializer_state.advance(offset);
        }
        removed
    }
    /// Replace the buffer FlatDeserializer can read.
    ///
    /// # Safety
    /// New buffer contents MUST equivalently equal to result contents of any conbination of [advance, append] being done to old buffer, you MUST call FlatDeserializer::advance(offset) if new buffer is advanced.
    pub unsafe fn replace_buffer(&mut self, buffer: &'a [T]) {
        self.buffer = buffer
    }
    pub fn buffer(&self) -> &'a [T] {
        self.buffer
    }
    /// Holes between Envelopes indicates archived NonEnvelopes.
    /// A phantom archived envelop whose tail_offset is None can be the first element of this vec to indicate the first item is an NonEnvelop.
    /// A truncated archived envelop who matches (buffer[head_offset..payload_head_offset] as limiter slice not matching buffer[payload_tail_offset..tail_offset] as delimiter slice) can be the first element due to advancing.
    pub fn archived_boundaries(&self) -> &Vec<Envelop, A> {
        &self.archived_boundaries
    }
    /// Some(offset) indicates an archived NonEnvelop between last Envelop's tail_offset(or 0 if no Envelop exsits) and offset.
    pub fn tail_non_envelop(&self) -> Option<Offset> {
        self.tail_non_envelop
    }
    pub fn deserializer_state(&self) -> &FlatDeserializerState<'l, 'v, T, A> {
        &self.deserializer_state
    }

    fn archive_envelop(&mut self, envelop: Envelop) {
        if self.archived_boundaries.is_empty() && envelop.head_offset.get() > 0 {
            self.archived_boundaries
                .push(unsafe { Envelop::phantom(NonMaxUsize::new(0)) });
        }
        self.archived_boundaries.push(envelop);
        self.tail_non_envelop = None;
    }
    fn archive_non_envelop(&mut self, tail: usize) {
        if self.archived_boundaries.is_empty() && tail > 0 {
            self.archived_boundaries
                .push(unsafe { Envelop::phantom(NonMaxUsize::new(0)) });
        }
        self.tail_non_envelop = Some(NonMaxUsize::new(tail));
    }
}
pub struct FlatDeserializerIter<'a, 'd, 'l, 'v, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    flat_deserializer: &'d mut FlatDeserializer<'a, 'l, 'v, T, A>,
    pos: usize,
}
impl<'a, 'd, 'l, 'v, T, A> Iterator for FlatDeserializerIter<'a, 'd, 'l, 'v, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    type Item = State;
    /// Odd pos => Envelop; Even pos => NonEnvelop;.
    /// Some(phantom Envelop) can be returned as first item to indicate the real first item is a NonEnvelop.
    /// Some(phantom NonEnvelop) can be returned if there's no NonEnvelop between two Envelops.
    fn next(&mut self) -> Option<Self::Item> {
        // Two sequential archived Envelop is legal, two sequential archived NonEnvelop is impossible(MUST be considered as one NonEnvelop by deserializer).
        // Iterate on cache first. Otherwise when cache miss, drive deserialize_incremental expecting an item is newly cached to be iterated until the deserializer settles. Then iterate on latest unarchived working_on. Then terminate the iterator.
        loop {
            let cached = {
                if !self.flat_deserializer.archived_boundaries.is_empty() {
                    (2 * self.flat_deserializer.archived_boundaries.len())
                        - (usize::from(self.flat_deserializer.tail_non_envelop.is_none()))
                } else {
                    0
                }
            };
            if self.pos < cached {
                let i = self.pos / 2;
                let item = if self.pos.is_multiple_of(2) {
                    State::E {
                        envelop: self.flat_deserializer.archived_boundaries[i].clone(),
                    }
                } else {
                    // The NonEnvelop hole behind archived_boundaries[i]
                    let start = self.flat_deserializer.archived_boundaries[i]
                        .tail_offset
                        .map(|t| t.get())
                        // Safety: for ALL Envelop who has tail_offset as None, ONLY phantom Envelop who indicates the first item is NonEnvelop is legal to be add to archived_boundaries.
                        .unwrap_or(0);
                    let end = if i + 1 < self.flat_deserializer.archived_boundaries.len() {
                        self.flat_deserializer.archived_boundaries[i + 1]
                            .head_offset
                            .get()
                    } else {
                        self.flat_deserializer
                            .tail_non_envelop
                            .expect("[SDAVE] illegal state: last cached slot is index-odd but tail_non_envelop is None")
                            .get()
                    };
                    State::N {
                        non_envelop: NonEnvelop {
                            head_offset: NonMaxUsize::new(start),
                            tail_offset: NonMaxUsize::new(end),
                        },
                    }
                };
                return {
                    self.pos += 1;
                    Some(item)
                };
            }
            // Drive the deserializer expecting an new item is cached until it settles.
            let before = (
                self.flat_deserializer.archived_boundaries.len(),
                self.flat_deserializer.tail_non_envelop,
            );
            self.flat_deserializer.deserialize_incremental();
            let after = (
                self.flat_deserializer.archived_boundaries.len(),
                self.flat_deserializer.tail_non_envelop,
            );
            if after != before {
                // New item absorbed to cache.
                continue;
            } else {
                // No new item absorbed, `cached` stays valid.
                if self.pos == cached {
                    // The trailing item.
                    if matches!(
                        self.flat_deserializer.deserializer_state.internal,
                        InternalState::CleanArchived
                    ) {
                        // # Safety
                        // Tail V1 Envelop matching `CleanArchived` MUST be absorbed and already iterated.
                        return None;
                    } else {
                        return {
                            self.pos += 1;
                            Some(self.flat_deserializer.deserializer_state.working_on.clone())
                        };
                    }
                }
            }
            return None;
        }
    }
}

/// Return `Some(least_repeat)` if it is serializable with given `limiter_pair` under given context where least_repeat is guaranteed >= `limiter_pair.least_repeat` AND to be the minimal working boundary, or `None` if it is not serializable.
///
/// Serializable: `buffer[0..buffer.len()] ++ limiter_pair.limiter * least_repeat ++ payload ++ limiter_pair.delimiter * least_repeat` MUST be able to be deserialized to get an Envelop framing such payload.
///
/// # Safety
/// `offset` MUST be a clean item boundary of `buffer`: head of buffer, or tail of an item meant to be archived. i.e. somewhere alfter constructing a FlatDeserializer on buffer then calling deserialize_all() on it then on its result you would get internal() matching internal().is_exhausted() AND if working_on() is E you can assert internal() is either `UnterminatedDeLimiterSlice` or `CleanArchived` else if working_on() is N you can assert internal() is `Clean`.
#[must_use]
pub unsafe fn can_serialize<T, A>(
    buffer: &[T],
    payload: &[T],
    variant: Variant,
    limiter_pair: &LimiterPair<T, A>,
) -> Option<NonZeroUsize>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let least_repeat = limiter_pair.least_repeat()?.get();
    let limiter = limiter_pair.limiter();
    let delimiter = limiter_pair.delimiter();
    if limiter.is_empty() || buffer.ends_with(limiter) || payload.starts_with(limiter) {
        return None;
    }
    if delimiter.is_empty() || (variant == Variant::V1 && payload.ends_with(delimiter)) {
        return None;
    }
    let mut max_run = 0usize;
    let mut offset = 0usize;
    let mut tail = payload.len();
    if variant == Variant::V2 {
        loop {
            let slice = &payload[..tail];
            if slice.ends_with(delimiter) && tail >= delimiter.len() {
                tail -= delimiter.len();
            } else {
                break;
            }
        }
    }
    if tail >= delimiter.len() {
        loop {
            if offset >= tail {
                break;
            }
            let slice = &payload[offset..tail];
            if slice.starts_with(delimiter) {
                let mut run = 1usize;
                'inner: loop {
                    let buf_ = &slice[run * delimiter.len()..];
                    if buf_.starts_with(delimiter) {
                        run += 1;
                    } else {
                        max_run = max_run.max(run);
                        break 'inner;
                    }
                }
            }
            offset += 1;
        }
    }
    NonZeroUsize::new(least_repeat.max(max_run + 1))
}

/// Store all limiter_pairs that each of them is guaranteed to be able to serialize given `payload` under given context to `result`.
///
/// Each pair is guaranteed to have Some `least_repeat` which carries the minimal working boundary its `least_repeat`.
///
/// # Safety
/// same contract as [`can_serialize`].
pub unsafe fn get_all_serializable_limiter_pairs<'l, T, A>(
    variant: Variant,
    buffer: &[T],
    payload: &[T],
    limiter_pairs: &Vec<LimiterPair<'l, T, A>, A>,
    result: &mut Vec<LimiterPair<'l, T, A>, A>,
) where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    result.clear();
    for limiter_pair in limiter_pairs {
        if let Some(least_repeat) = unsafe { can_serialize(buffer, payload, variant, limiter_pair) }
        {
            let mut limiter_pair = limiter_pair.clone();
            limiter_pair.least_repeat = Some(least_repeat);
            result.push(limiter_pair);
        }
    }
}

/// Advance the FlatDeserializerState by exactly one step, that is, the returned FlatDeserializerState of which, comparing to the old one, when the old FlatDeserializerState was not exhausted claim exactly one new item(head of the new item == tail of the old item) as archived or unarchived OR claim exhausted, or when the old FlatDeserializerState was exhausted if the buffer not ever grown since then(semantical tail of FlatDeserializerState still eq to buffer.len()) else claim more confirmed contents to the old item and update InternalState to new not exhausted or new exhausted.
///
/// # Safety
/// `old_deserializer_state` MUST match buffer's content by offset and semantics.
/// You MUST manually call FlatDeserializerState::advance(offset) on old_deserializer_state if you advance the buffer.
#[must_use]
pub unsafe fn deserialize_incremental<'l, 'v, T, A>(
    buffer: &[T],
    old_deserializer_state: &mut FlatDeserializerState<'l, 'v, T, A>,
) -> FlatDeserializerState<'l, 'v, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let repeat_map_on_cursor = old_deserializer_state.repeat_map_on_cursor.take();
    let mut old_deserializer_state = old_deserializer_state.clone().decouple();
    old_deserializer_state.5 = repeat_map_on_cursor;
    let internal = old_deserializer_state.1.clone();
    let next = old_deserializer_state.2.clone();
    {
        if buffer.len() != next.semantical_tail_offset().get() {
            match internal {
                InternalState::CleanArchived => {
                    seek_next_limiter_slice(
                        buffer,
                        next.semantical_tail_offset().get(),
                        &mut old_deserializer_state,
                    );
                }
                InternalState::Archived => match &next {
                    State::N { .. } => {
                        seek_next_limiter_slice(
                            buffer,
                            next.semantical_tail_offset().get(),
                            &mut old_deserializer_state,
                        );
                    }
                    State::E { .. } => {
                        seek_next_delimiter_slice(
                            buffer,
                            next.semantical_tail_offset().get(),
                            &mut old_deserializer_state,
                        );
                    }
                },
                InternalState::Unknown { tail_offset } => {
                    if buffer.len() != tail_offset.get() {
                        match next {
                            State::E { .. } => {
                                seek_next_delimiter_slice(
                                    buffer,
                                    tail_offset.get(),
                                    &mut old_deserializer_state,
                                );
                            }
                            State::N { .. } => {
                                seek_next_limiter_slice(
                                    buffer,
                                    tail_offset.get(),
                                    &mut old_deserializer_state,
                                );
                            }
                        }
                    }
                }
                InternalState::UnterminatedLimiterSlice { tail_offset } => {
                    if buffer.len() != tail_offset.get() {
                        seek_next_limiter_slice(
                            buffer,
                            tail_offset.get(),
                            &mut old_deserializer_state,
                        );
                    }
                }
                InternalState::UnterminatedDeLimiterSlice { tail_offset } => {
                    if buffer.len() != tail_offset.get() {
                        seek_next_delimiter_slice(
                            buffer,
                            tail_offset.get(),
                            &mut old_deserializer_state,
                        );
                    }
                }
                InternalState::Clean => match next {
                    State::E { .. } => {
                        seek_next_delimiter_slice(
                            buffer,
                            next.semantical_tail_offset().get(),
                            &mut old_deserializer_state,
                        );
                    }
                    State::N { .. } => {
                        seek_next_limiter_slice(
                            buffer,
                            next.semantical_tail_offset().get(),
                            &mut old_deserializer_state,
                        );
                    }
                },
            }
        }
    }
    unsafe { FlatDeserializerState::new_unchecked(old_deserializer_state) }
}

/// `from` expected to be `payload_tail_offset` of `next`.
/// If found { `next` { expected as Envelop => archive it to `working_on` & update `internal` to match `is_archived` then update `next` to a phantom NonEnvelop }  } else { update `next` & `internal` then clone `next` to `working_on` }.
fn seek_next_delimiter_slice<'a, 'l, 'v, T, A>(
    buffer: &'a [T],
    mut cursor: usize,
    old_deserializer_state: &mut DecoupledFlatDeserializerState<'l, 'v, T, A>,
) where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let working_on = &mut old_deserializer_state.0;
    let internal = &mut old_deserializer_state.1;
    let next = &mut old_deserializer_state.2;
    let variant = old_deserializer_state.3;
    let limiter_pair = &old_deserializer_state.4;

    match next.clone() {
        State::N { .. } => panic!("[SDAVE] Illegal state."),
        State::E { mut envelop } => {
            debug_assert!(envelop.tail_offset.is_none());
            let delimiter = limiter_pair.delimiter();
            loop {
                let from = envelop.payload_tail_offset.get();
                if from == buffer.len() {
                    *next = State::E {
                        envelop: envelop.clone(),
                    };
                    *working_on = next.clone();
                    *internal = InternalState::Clean;
                    return;
                }
                let mut cached_repeat = (cursor - from) / delimiter.len();
                let match_start = from + (cached_repeat * delimiter.len());
                let slice = &buffer[match_start..];
                if slice.starts_with(delimiter) {
                    cached_repeat += 1;
                    cursor = match_start + delimiter.len();
                    if cached_repeat >= limiter_pair.least_repeat().unwrap().get() {
                        let diff = cached_repeat - limiter_pair.least_repeat().unwrap().get();
                        match variant {
                            Variant::V1 => {
                                debug_assert_eq!(diff, 0);
                                envelop.tail_offset = Some(NonMaxUsize::new(cursor));
                                *next = State::N {
                                    non_envelop: unsafe {
                                        NonEnvelop::phantom(NonMaxUsize::new(cursor))
                                    },
                                };
                                *working_on = State::E {
                                    envelop: envelop.clone(),
                                };
                                if buffer.len() == cursor {
                                    *internal = InternalState::CleanArchived;
                                } else {
                                    *internal = InternalState::Archived
                                }
                                return;
                            }
                            Variant::V2 => {
                                if diff > 0 {
                                    envelop.payload_tail_offset =
                                        NonMaxUsize::new(from + diff * delimiter.len());
                                }
                                continue;
                            }
                        }
                    }
                } else if delimiter.starts_with(slice) {
                    *next = State::E {
                        envelop: envelop.clone(),
                    };
                    *working_on = next.clone();
                    match variant {
                        Variant::V1 => {
                            *internal = InternalState::Unknown {
                                tail_offset: NonMaxUsize::new(buffer.len()),
                            };
                            return;
                        }
                        Variant::V2 => {
                            if cached_repeat == limiter_pair.least_repeat().unwrap().get() {
                                *internal = InternalState::UnterminatedDeLimiterSlice {
                                    tail_offset: NonMaxUsize::new(buffer.len()),
                                };
                                return;
                            } else {
                                *internal = InternalState::Unknown {
                                    tail_offset: NonMaxUsize::new(buffer.len()),
                                };
                                return;
                            }
                        }
                    }
                } else {
                    if cached_repeat == limiter_pair.least_repeat().unwrap().get() {
                        debug_assert!(matches!(variant, Variant::V2));
                        envelop.tail_offset = Some(NonMaxUsize::new(match_start));
                        *next = State::N {
                            non_envelop: unsafe {
                                NonEnvelop::phantom(NonMaxUsize::new(match_start))
                            },
                        };
                        *working_on = State::E {
                            envelop: envelop.clone(),
                        };
                        *internal = InternalState::Archived;
                        return;
                    } else {
                        envelop.payload_tail_offset = NonMaxUsize::new(from + 1);
                        cursor = from + 1;
                        continue;
                    }
                }
            }
        }
    }
}

/// If found { `next` { expected as NonEnvelop => archive it to `working_on` & update `internal` to match `is_archived` then update `next` to a zero-width non-phantom none-tail Envelop, exception: this NonEnvelop meant to be archived matches zero-width => additionally call `seek_next_delimiter_slice` in place(the archived phantom NonEnvelop is not returned/committed) } } else { update `next` & `internal` then clone `next` to `working_on` }.
fn seek_next_limiter_slice<'a, 'l, 'v, T, A>(
    buffer: &'a [T],
    mut cursor: usize,
    old_deserializer_state: &mut DecoupledFlatDeserializerState<'l, 'v, T, A>,
) where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let working_on = &mut old_deserializer_state.0;
    let internal = &mut old_deserializer_state.1;
    let next = &mut old_deserializer_state.2;
    let limiter_pair = &mut old_deserializer_state.4;
    let repeat_map_on_cursor = old_deserializer_state.5.as_mut().unwrap();
    let limiter_pairs = old_deserializer_state.6;

    match next.clone() {
        State::E { .. } => panic!("[SDAVE] Illegal state."),
        State::N { mut non_envelop } => loop {
            let from = non_envelop.tail_offset.get();
            if from == buffer.len() {
                *next = State::N {
                    non_envelop: non_envelop.clone(),
                };
                *working_on = next.clone();
                *internal = InternalState::Clean;
                return;
            } else if cursor == buffer.len() {
                *next = State::N {
                    non_envelop: non_envelop.clone(),
                };
                *working_on = next.clone();
                for i in 0..limiter_pairs.len() {
                    if let Some(least_repeat) = limiter_pairs[i].least_repeat()
                        && repeat_map_on_cursor[i].map_or(0, |r| r.get()) >= least_repeat.get()
                    {
                        *internal = InternalState::UnterminatedLimiterSlice {
                            tail_offset: NonMaxUsize::new(buffer.len()),
                        };
                        return;
                    }
                }
                *internal = InternalState::Unknown {
                    tail_offset: NonMaxUsize::new(buffer.len()),
                };
                return;
            }
            cursor += 1;
            let mut any_some = false;
            for i in 0..repeat_map_on_cursor.len() {
                if let Some(cached_repeat) = repeat_map_on_cursor[i] {
                    let cached_repeat = cached_repeat.get();
                    let limiter = limiter_pairs[i].limiter();
                    if (cursor - from).is_multiple_of(limiter.len()) {
                        let match_start = from + (cached_repeat * limiter.len());
                        let slice = &buffer[match_start..cursor];
                        debug_assert_eq!(slice.len(), limiter.len());
                        if slice.starts_with(limiter) {
                            debug_assert_eq!(cached_repeat + 1, (cursor - from) / limiter.len());
                            repeat_map_on_cursor[i] = Some(NonMaxUsize::new(cached_repeat + 1));
                            any_some = true;
                            continue;
                        } else {
                            let least_repeat = limiter_pairs[i].least_repeat().unwrap().get();
                            if cached_repeat >= least_repeat {
                                *working_on = State::N {
                                    non_envelop: non_envelop.clone(),
                                };
                                *next = State::E {
                                    envelop: Envelop {
                                        head_offset: NonMaxUsize::new(from),
                                        payload_head_offset: NonMaxUsize::new(match_start),
                                        payload_tail_offset: NonMaxUsize::new(match_start),
                                        tail_offset: None,
                                    },
                                };
                                *internal = InternalState::Archived;
                                let mut limiter_pair_confirmed = limiter_pairs[i].clone();
                                limiter_pair_confirmed.least_repeat =
                                    Some(NonZeroUsize::new(cached_repeat).unwrap());
                                *limiter_pair = limiter_pair_confirmed;
                                for i in 0..repeat_map_on_cursor.len() {
                                    let lpi = &limiter_pairs[i];
                                    if lpi.least_repeat.is_some()
                                        && !lpi.limiter().is_empty()
                                        && !lpi.delimiter().is_empty()
                                    {
                                        repeat_map_on_cursor[i] = Some(NonMaxUsize::new(0));
                                    }
                                }
                                if non_envelop.is_phantom() {
                                    seek_next_delimiter_slice(
                                        buffer,
                                        match_start,
                                        old_deserializer_state,
                                    );
                                }
                                return;
                            }
                            repeat_map_on_cursor[i] = None;
                            continue;
                        }
                    } else {
                        any_some = true;
                    }
                }
            }
            if any_some {
                continue;
            } else {
                non_envelop.tail_offset = NonMaxUsize::new(from + 1);
                cursor = from + 1;
                for i in 0..repeat_map_on_cursor.len() {
                    let lpi = &limiter_pairs[i];
                    if lpi.least_repeat.is_some()
                        && !lpi.limiter().is_empty()
                        && !lpi.delimiter().is_empty()
                    {
                        repeat_map_on_cursor[i] = Some(NonMaxUsize::new(0));
                    }
                }
                continue;
            }
        },
    }
}
