#![no_std]
#![warn(missing_docs)]

//! SDAVE — **S**treamable **D**elimiter-**A**daptive **V**erbatim **E**nvelope.
//!
//! A flat(nesting-agnostic, payload-agnostic) envelope protocol: a serialize-time-defined delimiter
//! slice is reserved from the payload, and the payload is decoded verbatim(byte-identical).

extern crate alloc;

use alloc::vec::Vec;
use core::{alloc::Allocator, cmp::Ordering, num::NonZeroUsize, ops::BitXor, ptr::NonNull};

const NON_MAX_ERROR_MESSAGE: &str = "[SDAVE] NonMaxUsize exceeds usize::MAX";

/// A `usize` that can hold every value except `usize::MAX`, keeping `Option<NonMaxUsize>` as small
/// as a `usize`.
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct NonMaxUsize(NonZeroUsize);
impl Ord for NonMaxUsize {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.0.cmp(&other.0) {
            Ordering::Less => Ordering::Greater,
            Ordering::Equal => Ordering::Equal,
            Ordering::Greater => Ordering::Less,
        }
    }
}
impl PartialOrd for NonMaxUsize {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl NonMaxUsize {
    /// Construct a `NonMaxUsize` holding `non_max`.
    ///
    /// # Panics
    /// Panics when `non_max == usize::MAX`.
    pub fn new(non_max: usize) -> Self {
        NonMaxUsize(NonZeroUsize::new(non_max.bitxor(usize::MAX)).expect(NON_MAX_ERROR_MESSAGE))
    }
    /// The held value.
    pub fn get(&self) -> usize {
        self.0.get().bitxor(usize::MAX)
    }
    /// The held value as a `NonZeroUsize`, AKA [`NonMaxUsize::get`] ^ `usize::MAX`.
    pub fn decouple(self) -> NonZeroUsize {
        self.0
    }
    /// # Safety
    /// `non_zero_usize` MUST transport the semantic value as `rhs ^ usize::MAX`, i.e. [`NonMaxUsize::get`] returns `non_zero_usize.get() ^ usize::MAX`.
    pub unsafe fn new_unchecked(non_zero_usize: NonZeroUsize) -> Self {
        NonMaxUsize(non_zero_usize)
    }
}
/// Absolute offset for buffer index.
pub type Offset = NonMaxUsize;

/// Delimiter match algorithm variant. There's not a full winner, choose based on scene.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// The very first full matched delimiter slice is just the delimiter slice. No delimiter slice confirmation latency, but tail of payload MUST NOT collide with the delimiter unit.
    V1,
    // For in-place insertion usage where the item behind MUST be materialized BEFORE this V2 Envelop, the item behind this might disable a limiter pair. This usage is NOT supported.
    /// The very first full matched delimiter slice FOLLOWED by a NON-delimiter is the delimiter slice. Tail of payload won't disable a limiter pair, but introduces delimiter slice confirmation latency (solvable on implementation layer by pushing a non-delimiter to the mixed channel).
    /// For streaming, a V2 Envelop reserves the delimiter from the header of the item behind.
    V2,
}

/// Decoupled form of [`LimiterPair`]: `(least_repeat, limiter, delimiter, bsize)`.
pub type DecoupledLimiterPair<'l, T, A> =
    (Option<NonZeroUsize>, &'l Vec<T, A>, &'l Vec<T, A>, usize);
/// A limiter/delimiter pair: a `limiter` slice whose repeats can mark the head of an Envelop, and the
/// `delimiter` slice of same pair repeated the same times which marks its tail.
///
/// `T`: the basic data unit of a slice.
/// `limiter`/`delimiter` are immutable once initialized, each a `Vec` of `T`, e.g. a UTF-8 1B/2B/3B char in a `u8` buffer.
pub struct LimiterPair<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    least_repeat: Option<NonZeroUsize>,
    limiter: &'l Vec<T, A>,
    delimiter: &'l Vec<T, A>,
    bsize: usize,
}
impl<'l, T, A> PartialEq for LimiterPair<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// The reserved value is fully ignored.
    fn eq(&self, other: &Self) -> bool {
        self.least_repeat == other.least_repeat
            && self.limiter == other.limiter
            && self.delimiter == other.delimiter
    }
}
const USIZE_BITS: usize = usize::BITS as usize;
impl<'l, T, A> Clone for LimiterPair<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// Shallow clone 3 usizes, erase the reserved value.
    fn clone(&self) -> Self {
        Self {
            least_repeat: self.least_repeat,
            limiter: self.limiter,
            delimiter: self.delimiter,
            bsize: 0usize,
        }
    }
}
impl<'l, T, A> LimiterPair<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// Construct a `LimiterPair` from `least_repeat`, `limiter` and `delimiter`, with the reserved value(see [`LimiterPair::bsize`]) reset to 0.
    pub fn new(
        least_repeat: Option<NonZeroUsize>,
        limiter: &'l Vec<T, A>,
        delimiter: &'l Vec<T, A>,
    ) -> Self {
        Self {
            least_repeat,
            limiter,
            delimiter,
            bsize: 0usize,
        }
    }
    /// Return the decoupled form(including the reserved value) of this `LimiterPair`.
    pub fn decouple(self) -> DecoupledLimiterPair<'l, T, A> {
        (self.least_repeat, self.limiter, self.delimiter, self.bsize)
    }
    /// `bsize` is a `usize`, used as a bitmap indicating which offsets(relative to `FlatDeserializerState::working_on().semantical_tail_offset()`) are unnecessary to scan with this `limiter_pair` on `FlatDeserializer::buffer()`.
    ///
    /// # Safety
    /// Using this incorrectly leads to undefined behaviour.
    pub unsafe fn new_unchecked(decoupled_limiter_pair: DecoupledLimiterPair<'l, T, A>) -> Self {
        Self {
            least_repeat: decoupled_limiter_pair.0,
            limiter: decoupled_limiter_pair.1,
            delimiter: decoupled_limiter_pair.2,
            bsize: decoupled_limiter_pair.3,
        }
    }
    /// Deserialization case: as the algorithm matches `limiter`/`delimiter` first, then guards on `least_repeat`, the assertion(repeat >= 1) is always true for niche optimization; a LimiterPair with `least_repeat` == `None` would be ignored.
    ///
    /// Serialization case: the minimal repeat to frame certain payload being inserted at `buffer[..offset] _here_ buffer[offset..]` without introducing conflicts, or `None` if this LimiterPair is impossible to frame such payload without introducing conflicts under such condition.
    pub fn least_repeat(&self) -> Option<NonZeroUsize> {
        self.least_repeat
    }
    /// LimiterPair with empty limiter would be ignored.
    pub fn limiter(&self) -> &'l Vec<T, A> {
        self.limiter
    }
    /// LimiterPair with empty delimiter would be ignored.
    pub fn delimiter(&self) -> &'l Vec<T, A> {
        self.delimiter
    }
    /// `clone` and `eq` fully ignore/skip this value.
    /// Warning: this is a copy instead of a ref or a mut ref.
    ///
    /// # Safety
    /// Reserved to optimize the matching algorithm.
    /// Inserting `LimiterPair` (via `decouple` then mutate `limiter_pairs`) from TAKING(AKA ownership move) `LimiterPair` from other `FlatDeserializerState` that had ALREADY performed some deserialization can lead to undefined behaviour.
    /// `clone` erases that value and erasing it is guaranteed NOT to lead to undefined behaviour.
    /// For insight, not recommend to rely on this.
    pub unsafe fn bsize(&self) -> usize {
        self.bsize
    }
}

/// Item boundary: `E` an `Envelop`, `N` a `NonEnvelop`.
#[derive(Clone, PartialEq)]
pub enum Item {
    /// The Envelop variant.
    E {
        /// The Envelop payload.
        envelop: Envelop,
    },
    /// The NonEnvelop variant.
    N {
        /// The NonEnvelop payload.
        non_envelop: NonEnvelop,
    },
}
impl Item {
    /// The head offset of this item.
    pub fn head_offset(&self) -> Offset {
        match self {
            Item::E { envelop } => envelop.head_offset,
            Item::N { non_envelop } => non_envelop.head_offset,
        }
    }
    /// The semantical tail offset of this item: `Envelop::tail_offset` if the Envelop is archived,
    /// else `Envelop::payload_tail_offset`; `NonEnvelop::tail_offset` for a NonEnvelop.
    pub fn semantical_tail_offset(&self) -> Offset {
        match self {
            Item::E { envelop } => {
                if let Some(tail_offset) = envelop.tail_offset {
                    tail_offset
                } else {
                    envelop.payload_tail_offset
                }
            }
            Item::N { non_envelop } => non_envelop.tail_offset,
        }
    }
    /// [`Envelop::is_phantom()`].
    /// [`NonEnvelop::is_phantom()`].
    pub fn is_phantom(&self) -> bool {
        match self {
            Item::E { envelop } => envelop.is_phantom(),
            Item::N { non_envelop } => non_envelop.is_phantom(),
        }
    }
}
/// An Envelop: a payload framed by a limiter slice and the matching delimiter slice of a `LimiterPair`.
///
/// Limiter slice is guaranteed well defined(at least one non-limiter `T`(either payload slice header or delimiter slice header not match limiter) exists following limiter slice) when SDAVE core constructs a real(not meant to be phantom) Envelop.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct Envelop {
    /// Head offset of this Envelop, AKA the head of its limiter slice.
    pub head_offset: Offset,
    /// Head offset of payload of this Envelop.
    pub payload_head_offset: Offset,
    // An archived Envelop, with limiter != delimiter, can have empty payload, e.g. `^^~~`, in case of which, payload_tail_offset == payload_head_offset.
    /// Can monotonically increase in next updates, `buffer[payload_head_offset..payload_tail_offset]` is guaranteed NOT 1.contains valid delimiter slice; 2.ends with potential delimiter slice.
    pub payload_tail_offset: Offset,
    /// Tail offset of this Envelop, AKA the tail of its delimiter slice.
    ///
    /// `Some(offset)` means this Envelop is confirmed archived, would never grow again.
    pub tail_offset: Option<Offset>,
}
impl Envelop {
    /// An Envelop would NEVER grow again once archived.
    pub fn is_archived(&self) -> bool {
        self.tail_offset.is_some()
    }
    /// A phantom Envelop is a zero-width marker whose `tail_offset` is `None`.
    /// Can be used as first element of [`FlatDeserializer::archived_boundaries()`] to indicate the first archived item is an NonEnvelop.
    pub fn is_phantom(&self) -> bool {
        self.tail_offset.is_none()
            && self.head_offset == self.payload_head_offset
            && self.payload_head_offset == self.payload_tail_offset
    }
    /// Construct a phantom Envelop with `offset`.
    ///
    /// # Safety
    /// For insight, not recommend to rely on this.
    pub unsafe fn phantom(offset: Offset) -> Self {
        Envelop {
            head_offset: offset,
            payload_head_offset: offset,
            payload_tail_offset: offset,
            tail_offset: None,
        }
    }
    /// # Safety
    /// Constructing an `Envelop` and inserting it into `FlatDeserializer::archived_boundaries` can lead to undefined behaviour in `FlatDeserializer::iter()` and `FlatDeserializer::iter_from()`.
    /// Constructing an `Envelop` and setting `FlatDeserializerState::next` with it can lead to undefined behaviour in `deserialize_incremental`(`FlatDeserializer` also depends on it).
    pub unsafe fn new_unchecked(
        head_offset: Offset,
        payload_head_offset: Offset,
        payload_tail_offset: Offset,
        tail_offset: Option<Offset>,
    ) -> Self {
        Self {
            head_offset,
            payload_head_offset,
            payload_tail_offset,
            tail_offset,
        }
    }
}
/// A confirmed buffer slice that is not an Envelop.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct NonEnvelop {
    /// Head offset of this NonEnvelop.
    pub head_offset: Offset,
    /// Can monotonically increase in next updates, `buffer[head_offset..tail_offset]` is guaranteed NOT 1.contains valid limiter slice; 2.ends with potential limiter slice.
    pub tail_offset: Offset,
}
impl NonEnvelop {
    /// A phantom NonEnvelop is a zero-width marker.
    /// Can be used to indicate sequential Envelopes for `FlatDeserializer`.
    pub fn is_phantom(&self) -> bool {
        self.head_offset == self.tail_offset
    }
    /// Construct a phantom NonEnvelop with `offset`.
    ///
    /// # Safety
    /// For insight, not recommend to rely on this.
    pub unsafe fn phantom(offset: Offset) -> Self {
        NonEnvelop {
            head_offset: offset,
            tail_offset: offset,
        }
    }
    /// # Safety
    /// Constructing a `NonEnvelop` and setting `FlatDeserializerState::next` with it can lead to undefined behaviour in `deserialize_incremental`(`FlatDeserializer` also depends on it).
    pub unsafe fn new_unchecked(head_offset: Offset, tail_offset: Offset) -> Self {
        NonEnvelop {
            head_offset,
            tail_offset,
        }
    }
}

// ANY `tail_offset`, when constructed by SDAVE core, MUST eq to buffer.len().
// Confirmed midway results are stored at `next` instead.
// Current deserializing slice: `buffer[next.semantical_tail_offset()..tail_offset]`.
/// Internal deserialization state of the slice `buffer[next.semantical_tail_offset()..tail_offset]`.
///
/// # Safety
/// Constructing an `InternalState` and setting `FlatDeserializerState::internal` with it can lead to undefined behaviour in `deserialize_incremental`.
/// Specially, setting the tail_offset(if it has tail_offset) of an `InternalState` with value lte `FlatDeserializerState::next.semantical_tail_offset()` combining with `update_internal_cache(repeat_map, limiter_pairs, None)` which clears the internal cache, just forces the deserializer to deserialize from the same known correct state without what's removed from cache and thus is guaranteed NOT to lead undefined behaviour in `deserialize_incremental`.
#[derive(Clone, PartialEq)]
pub enum InternalState {
    /// Unknown whether current deserializing slice is part of an unarchived NonEnvelop OR part of a live streaming limiter slice, or is part of payload of an unarchived Envelop OR part of a live streaming delimiter slice.
    Unknown {
        // head_offset: from `next.semantical_tail_offset()`.
        /// The very last offset `deserialize_incremental` has scanned.
        tail_offset: Offset,
    },
    /// Current deserializing slice MUST be a limiter slice but limiter_pair/repeats are not well defined yet (not terminated by a not-repeating `T`).
    UnterminatedLimiterSlice {
        // head_offset: from `next.semantical_tail_offset()`.
        /// The very last offset `deserialize_incremental` has scanned.
        tail_offset: Offset,
    },
    /// Current deserializing slice MUST be a delimiter slice but not well defined yet (not terminated by a not-repeating `T`). Legal only when `working_on` is a V2 Envelop.
    UnterminatedDeLimiterSlice {
        // head_offset: from `next.semantical_tail_offset()`.
        /// The very last offset `deserialize_incremental` has scanned.
        tail_offset: Offset,
    },
    /// The buffer is guaranteed fully deserialized. Semantical tail of `working_on` == current buffer.len().
    Clean,
    // Legal clean && archived state MUST be promoted to `CleanArchived`.
    // Thus the buffer MUST not be fully deserialized.
    // The deserializer MUST not rescan/recheck `buffer[..working_on.semantical_tail]`.
    /// `working_on` is confirmed archived.
    /// `next` would differ from `working_on` caching midway results.
    Archived,
    // When semantically Clean:
    // a. assume the last item is NonEnvelop,
    // it's impossible for SDAVE core alone to assert this NonEnvelop is archived;
    // b. assume the last item is Envelop,
    // ONLY V1 Envelop can be asserted archived -- V2 Envelop MUST need the header of the next item to terminate it.
    /// `Clean` + `Archived`. Legal only when `working_on` is a V1 Envelop.
    /// `next` would be a phantom NonEnvelop because there's nothing to cache.
    CleanArchived,
}
impl InternalState {
    /// You can get the newly confirmed archived item from `FlatDeserializerState::working_on` when `FlatDeserializerState::internal` matches this.
    pub fn is_archived(&self) -> bool {
        matches!(self, InternalState::Archived | InternalState::CleanArchived)
    }
    /// Nothing unknown(AKA, to be well defined), every `T` the deserializer reads has been settled to some `working_on`.
    pub fn is_clean(&self) -> bool {
        matches!(self, InternalState::Clean | InternalState::CleanArchived)
    }
    /// Current contents in buffer are fully read, `deserialize_incremental` MUST not make progress on the same buffer with the same `FlatDeserializerState` when its `internal` matches this.
    pub fn is_exhausted(&self) -> bool {
        !matches!(self, InternalState::Archived)
    }
}

/// Decoupled form of [`FlatDeserializerState`]: `(working_on, internal, next, variant, limiter_pair, repeat_map, limiter_pairs)`.
pub type DecoupledFlatDeserializerState<'l, T, A> = (
    Item,
    InternalState,
    Item,
    Variant,
    LimiterPair<'l, T, A>,
    Option<Vec<Option<NonMaxUsize>, A>>,
    Option<Vec<LimiterPair<'l, T, A>, A>>,
);
/// Deserialization state of a `T` slice, decoupled from the buffer itself, so that it can be
/// advanced or swapped(see [`FlatDeserializerState::decouple`]) independently.
pub struct FlatDeserializerState<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    working_on: Item,
    internal: InternalState,
    next: Item,
    variant: Variant,
    limiter_pair: LimiterPair<'l, T, A>,
    repeat_map: Option<Vec<Option<NonMaxUsize>, A>>,
    limiter_pairs: Option<Vec<LimiterPair<'l, T, A>, A>>,
}
impl<'l, T, A> FlatDeserializerState<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// # Safety
    ///
    /// `repeat_map` and `limiter_pairs` are `None` in the returned instance instead of being cloned.
    /// Move/transfer or manually clone them if needed(override via decouple).
    pub unsafe fn clone(&self) -> Self {
        Self {
            working_on: self.working_on.clone(),
            internal: self.internal.clone(),
            next: self.next.clone(),
            variant: self.variant,
            limiter_pair: self.limiter_pair.clone(),
            repeat_map: None,
            limiter_pairs: None,
        }
    }
    /// Construct a fresh `FlatDeserializerState` from `variant` and `limiter_pairs`, starting from a
    /// zero-width phantom NonEnvelop and a `InternalState` not matching `InternalState::is_exhausted`.
    ///
    /// `repeat_map` would be cleared and refilled with one `Option<NonMaxUsize>` per entry of `limiter_pairs`.
    ///
    /// # Panics
    /// Panics when `limiter_pairs` is empty.
    pub fn new(
        variant: Variant,
        limiter_pairs: Vec<LimiterPair<'l, T, A>, A>,
        mut repeat_map: Vec<Option<NonMaxUsize>, A>,
    ) -> Self {
        let limiter_pair = limiter_pairs
            .first()
            .expect("[SDAVE] empty limiter_pairs")
            .clone();
        repeat_map.clear();
        for i in 0..limiter_pairs.len() {
            let limiter_pair = limiter_pairs.get(i).unwrap();
            if limiter_pair.least_repeat().is_some()
                && !limiter_pair.limiter().is_empty()
                && !limiter_pair.delimiter().is_empty()
            {
                repeat_map.push(Some(NonMaxUsize::new(0)));
            } else {
                repeat_map.push(None);
            }
        }
        FlatDeserializerState {
            working_on: Item::N {
                non_envelop: unsafe { NonEnvelop::phantom(Offset::new(0)) },
            },
            internal: InternalState::Archived,
            next: Item::N {
                non_envelop: unsafe { NonEnvelop::phantom(Offset::new(0)) },
            },
            variant,
            limiter_pair,
            repeat_map: Some(repeat_map),
            limiter_pairs: Some(limiter_pairs),
        }
    }
    /// Return the decoupled form of this `FlatDeserializerState`, including `repeat_map` and `limiter_pairs`.
    pub fn decouple(self) -> DecoupledFlatDeserializerState<'l, T, A> {
        (
            self.working_on,
            self.internal,
            self.next,
            self.variant,
            self.limiter_pair,
            self.repeat_map,
            self.limiter_pairs,
        )
    }
    /// # Safety
    /// Operation to buffer contents MUST equivalently be combination of [advance, append], you MUST call FlatDeserializer::advance(offset) if buffer is advanced.
    /// Any incorrect(disagree with implementation of SDAVE core) modification to the decoupled FlatDeserializerState would lead to undefined behaviour in `deserialize_incremental`.
    pub unsafe fn new_unchecked(
        flat_deserializer_state: DecoupledFlatDeserializerState<'l, T, A>,
    ) -> Self {
        FlatDeserializerState {
            working_on: flat_deserializer_state.0,
            internal: flat_deserializer_state.1,
            next: flat_deserializer_state.2,
            variant: flat_deserializer_state.3,
            limiter_pair: flat_deserializer_state.4,
            repeat_map: flat_deserializer_state.5,
            limiter_pairs: flat_deserializer_state.6,
        }
    }
    /// # Safety
    /// You MUST call this if you directly use `deserialize_incremental` instead of using `FlatDeserializer` on a buffer, when you equivalently advance the buffer. You MUST not call this if you use `FlatDeserializer`(via `decouple`) since `FlatDeserializer::advance` already advances its `FlatDeserializerState`.
    ///
    /// # Panics
    /// Panics when offset > semantical tail of `working_on`.
    pub unsafe fn advance(&mut self, offset: Offset) {
        if self.internal.is_archived() {
            let tail = {
                match &self.working_on {
                    Item::N { non_envelop } => non_envelop.tail_offset.get(),
                    Item::E { envelop } => envelop.tail_offset.expect("[SDAVE] Illegal state: FlatDeserializerState.is_archived() but working_on Envelop has None tail_offset").get(),
                }
            };
            assert!(offset.get() <= tail);
        } else {
            // Safety:
            // Once a Item of Envelop is constructed, its LimiterPair is confirmed archived and stored as `limiter_pair`, which means truncating header of such Envelop won't cause semantics stability issue, but truncating Unknown tail or UnterminatedDeLimiterSlice tail following MUST cause semantics stability issue.
            let tail = {
                match &self.working_on {
                    Item::N { non_envelop } => non_envelop.tail_offset.get(),
                    Item::E { envelop } => envelop.payload_tail_offset.get(),
                }
            };
            assert!(offset.get() <= tail);
        }
        let shift = |x: Offset| Offset::new(x.get().saturating_sub(offset.get()));
        match &mut self.working_on {
            Item::N { non_envelop } => {
                non_envelop.head_offset = shift(non_envelop.head_offset);
                non_envelop.tail_offset = shift(non_envelop.tail_offset);
            }
            Item::E { envelop } => {
                envelop.head_offset = shift(envelop.head_offset);
                envelop.payload_head_offset = shift(envelop.payload_head_offset);
                envelop.payload_tail_offset = shift(envelop.payload_tail_offset);
                if let Some(tail_offset) = envelop.tail_offset {
                    envelop.tail_offset = Some(shift(tail_offset));
                }
            }
        }
        match &mut self.next {
            Item::N { non_envelop } => {
                non_envelop.head_offset = shift(non_envelop.head_offset);
                non_envelop.tail_offset = shift(non_envelop.tail_offset);
            }
            Item::E { envelop } => {
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
    /// Get a confirmed archived Item when [`FlatDeserializerState::internal()`] matches [`InternalState::is_archived()`].
    /// Or get an unarchived Item.
    pub fn working_on(&self) -> &Item {
        &self.working_on
    }
    /// Current `InternalState`.
    pub fn internal(&self) -> &InternalState {
        &self.internal
    }
    /// The cache storing confirmed midway results for `deserialize_incremental`.
    pub fn next(&self) -> &Item {
        &self.next
    }
    /// Delimiter match algorithm variant in use.
    pub fn variant(&self) -> Variant {
        self.variant
    }
    /// Current limiter_pair, stores the delimiter and the serialize-time-defined repeat times.
    /// Make sense when and only when `next` is E(Envelop) which guarantees a well defined limiter slice.
    pub fn limiter_pair(&self) -> &LimiterPair<'l, T, A> {
        &self.limiter_pair
    }
    // ALL valid LimiterPair MUST be reset with Some(NonMaxUsize::new(0)) on each cursor advancing when seek for well defined limiter slice.
    /// Index parallel to `limiter_pairs`.
    /// `None` indicates mismatch.
    /// If let Some(non_max) = `repeat_map()[i]` { non_max.get() } indicates current repeats of `limiter_pairs()[i]` in `buffer[next.semantical_tail_offset()..cursor]` where `cursor` initially come from InternalState::Unknown or InternalState::UnterminatedLimiterSlice for each `deserialize_incremental`.
    ///
    /// Assuming `from` as-in `[from..cursor]` and `index` as-in limiter_pairs, SDAVE archive the first(1.cursor; 2.index;) LimiterPair exceeding a well defined limiter slice(MUST be terminated to be well defined) on `buffer [from..cursor]` growing `cursor` from `from` until `cursor` exceeds buffer.len() or repeat_map.any(|ri|ri.is_some()).is_none() as the ONLY chosen one(AKA `limiter_pair()`).
    ///
    /// This simplification can cause semantics stability issue for limiter_pairs matching certain condition(i.e. a limiter whose slice itself exceeds a well defined limiter slice for another LimiterPair in `limiter_pairs()`). It's considered acceptable and is absorbed as a core semantics assertion for current SDAVE core implementation.
    ///
    /// # Safety
    /// For insight, not recommend to rely on this.
    pub unsafe fn repeat_map(&self) -> Option<&Vec<Option<NonMaxUsize>, A>> {
        self.repeat_map.as_ref()
    }
    /// # Safety
    /// Using this incorrectly leads undefined behaviour to `deserialize_incremental`.
    pub unsafe fn take_repeat_map(&mut self) -> Option<Vec<Option<NonMaxUsize>, A>> {
        self.repeat_map.take()
    }
    /// # Safety
    /// Using this incorrectly leads undefined behaviour to `deserialize_incremental`.
    pub unsafe fn swap_repeat_map(
        &mut self,
        repeat_map: Option<Vec<Option<NonMaxUsize>, A>>,
    ) -> Option<Vec<Option<NonMaxUsize>, A>> {
        let result = unsafe { self.take_repeat_map() };
        self.repeat_map = repeat_map;
        result
    }
    /// All limiter pairs in use, index-parallel to `repeat_map()`.
    pub fn limiter_pairs(&self) -> Option<&Vec<LimiterPair<'l, T, A>, A>> {
        self.limiter_pairs.as_ref()
    }
    /// # Safety
    /// Using this incorrectly leads to undefined behaviour in `deserialize_incremental`.
    pub unsafe fn take_limiter_pairs(&mut self) -> Option<Vec<LimiterPair<'l, T, A>, A>> {
        self.limiter_pairs.take()
    }
    /// # Safety
    /// Using this incorrectly leads to undefined behaviour in `deserialize_incremental`.
    pub unsafe fn swap_limiter_pairs(
        &mut self,
        limiter_pairs: Option<Vec<LimiterPair<'l, T, A>, A>>,
    ) -> Option<Vec<LimiterPair<'l, T, A>, A>> {
        let result = unsafe { self.take_limiter_pairs() };
        self.limiter_pairs = limiter_pairs;
        result
    }
    /// `true` means it's guaranteed semantically safe to use the simplification introduced by `repeat_map` for this `limiter_pairs`.
    pub fn is_limiter_pairs_valid(limiter_pairs: &Vec<LimiterPair<'l, T, A>, A>) -> bool {
        // TODO: prove below is the minimal requirements.
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
/// # Arguments
/// `repeat_map`: from decouple -- for each valid `LimiterPair`, reset to Some(0);
/// `limiter_pairs`: from decouple;
/// `offset`: case MUST NOT: Some(offset) -- `limiter_pair.bsize` shr(`>>`) offset.get(); case MUST: None -- reset `limiter_pair.bsize` to 0;
///
/// # Safety
/// When you can assert there (MUST || MUST NOT) be a well defined (limiter/delimiter(depending on `next`(== `FlatDeserializerState::next`))) slice from header of `buffer[next.semantical_tail_offset()..]`, call this to update the internal cache.
/// Using this incorrectly leads undefined behaviour to `deserialize_incremental`.
#[allow(clippy::ptr_arg)]
pub unsafe fn update_internal_cache<'l, T, A>(
    repeat_map: &mut Vec<Option<NonMaxUsize>, A>,
    limiter_pairs: &mut Vec<LimiterPair<'l, T, A>, A>,
    offset: Option<Offset>,
) where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    for i in 0..repeat_map.len() {
        let limiter_pair = &mut limiter_pairs[i];
        if limiter_pair.least_repeat.is_some()
            && !limiter_pair.limiter().is_empty()
            && !limiter_pair.delimiter().is_empty()
        {
            repeat_map[i] = Some(NonMaxUsize::new(0));
            if let Some(offset) = offset {
                limiter_pair.bsize >>= offset.get();
            } else {
                limiter_pair.bsize = 0;
            }
        }
    }
}

/// Decoupled form of [`FlatDeserializer`]: `(buffer, archived_boundaries, tail_non_envelop, deserializer_state)`.
pub type DecoupledFlatDeserializer<'a, 'l, T, A> = (
    &'a [T],
    Vec<Envelop, A>,
    Option<Offset>,
    FlatDeserializerState<'l, T, A>,
);
/// A deserializer over a `T` buffer, caching the confirmed items(`archived_boundaries`/`tail_non_envelop`)
/// and driving [`deserialize_incremental`].
pub struct FlatDeserializer<'a, 'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    buffer: &'a [T],
    archived_boundaries: Vec<Envelop, A>,
    tail_non_envelop: Option<Offset>,
    deserializer_state: FlatDeserializerState<'l, T, A>,
}
impl<'a, 'l, T, A> FlatDeserializer<'a, 'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// Construct a `FlatDeserializer` over `buffer`.
    ///
    /// `archived_boundaries` would be cleared.
    /// `repeat_map` would be cleared and refilled with one `Option<NonMaxUsize>` per entry of `limiter_pairs`.
    ///
    /// # Panics
    /// Panics when `limiter_pairs` is empty.
    pub fn new(
        buffer: &'a [T],
        variant: Variant,
        limiter_pairs: Vec<LimiterPair<'l, T, A>, A>,
        mut archived_boundaries: Vec<Envelop, A>,
        repeat_map: Vec<Option<NonMaxUsize>, A>,
    ) -> Self {
        archived_boundaries.clear();
        Self {
            buffer,
            archived_boundaries,
            tail_non_envelop: None,
            deserializer_state: FlatDeserializerState::new(variant, limiter_pairs, repeat_map),
        }
    }
    /// You can advance the buffer(or just alloc new one and copy the tail to new buffer) to save memory or append new contents to it after FlatDeserializer release(drop the ref to buffer after FlatDeserializer::decouple) read access to your buffer, but you MUST call FlatDeserializer::advance(offset) if you advance your buffer.
    ///
    /// You can replace limiter_pairs, which would be applied to future(deserializer_state based) envelopes(LimiterPair for current unarchived envelop is cloned to avoid semantics stability issue).
    ///
    /// You can replace variant, which would be applied right next deserialize_incremental.
    pub fn decouple(self) -> DecoupledFlatDeserializer<'a, 'l, T, A> {
        (
            self.buffer,
            self.archived_boundaries,
            self.tail_non_envelop,
            self.deserializer_state,
        )
    }
    /// # Safety
    /// Operation to buffer contents MUST equivalently be combination of [advance, append], you MUST call FlatDeserializer::advance(offset) if buffer is advanced.
    /// Incorrect state leads to undefined behaviour.
    pub unsafe fn new_unchecked(
        flat_deserializer: DecoupledFlatDeserializer<'a, 'l, T, A>,
    ) -> Self {
        Self {
            buffer: flat_deserializer.0,
            archived_boundaries: flat_deserializer.1,
            tail_non_envelop: flat_deserializer.2,
            deserializer_state: flat_deserializer.3,
        }
    }
    /// `iter` may call `deserialize_incremental` internally.
    pub fn iter(&mut self) -> FlatDeserializerIter<'a, '_, 'l, T, A> {
        self.iter_from(0)
    }
    /// `iter_from` may call `deserialize_incremental` internally.
    pub fn iter_from(&mut self, pos: usize) -> FlatDeserializerIter<'a, '_, 'l, T, A> {
        FlatDeserializerIter {
            flat_deserializer: self,
            pos,
        }
    }
    /// Deserialize one step, driving [`deserialize_incremental`] and caching the newly archived item
    /// into `archived_boundaries`/`tail_non_envelop`.
    pub fn deserialize_incremental(&mut self) -> &FlatDeserializerState<'l, T, A> {
        {
            let new_state = unsafe {
                crate::deserialize_incremental(self.buffer, &mut self.deserializer_state)
            };
            let old_state = self.deserializer_state();
            if new_state.internal.is_archived()
                && (!old_state.internal.is_archived()
                    || (old_state.internal.is_archived()
                        && new_state.working_on != old_state.working_on))
            {
                match &new_state.working_on {
                    Item::N { non_envelop } => {
                        {
                            let tail = non_envelop.tail_offset.get();
                            if self.archived_boundaries.is_empty() && tail > 0 {
                                self.archived_boundaries
                                    .push(unsafe { Envelop::phantom(Offset::new(0)) });
                            }
                            self.tail_non_envelop = Some(Offset::new(tail));
                        };
                    }
                    Item::E { envelop } => {
                        {
                            self.archived_boundaries.push(envelop.clone());
                            self.tail_non_envelop = None;
                        };
                    }
                }
            }
            self.deserializer_state = new_state;
        };
        &self.deserializer_state
    }
    /// Fully deserialize the buffer.
    /// Relies on `FlatDeserializer::deserialize_incremental`.
    pub fn deserialize_all(&mut self) -> &FlatDeserializerState<'l, T, A> {
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
    /// Panics when offset > semantical tail of deserializer_state's working_on.
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
        let shift = |x: Offset| Offset::new(x.get().saturating_sub(off));
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
            Some(tail_non_envelop) => Some(Offset::new(tail_non_envelop.get() - off)),
            None => None,
        };
        // a remaining leading hole before the first Envelop needs the phantom marker.
        if let Some(first) = self.archived_boundaries.first() {
            if first.tail_offset.is_some() && first.head_offset.get() > 0 {
                self.archived_boundaries
                    .insert(0, unsafe { Envelop::phantom(Offset::new(0)) });
            }
        } else if self.tail_non_envelop.is_some_and(|t| t.get() > 0) {
            self.archived_boundaries
                .push(unsafe { Envelop::phantom(Offset::new(0)) });
        }
        unsafe {
            self.deserializer_state.advance(offset);
        }
        removed
    }
    /// Replace the buffer FlatDeserializer can read.
    ///
    /// # Safety
    /// New buffer contents MUST equivalently equal to result contents of any combination of [advance, append] being done to old buffer, you MUST call FlatDeserializer::advance(offset) if new buffer is advanced.
    pub unsafe fn replace_buffer(&mut self, buffer: &'a [T]) {
        self.buffer = buffer
    }
    /// The buffer this `FlatDeserializer` reads.
    pub fn buffer(&self) -> &'a [T] {
        self.buffer
    }
    /// Holes between `Envelop`es indicates archived `NonEnvelop`es.
    /// A phantom archived envelop which matches `Envelop::is_phantom` can be the first element of this to indicate the first item is an `NonEnvelop`.
    /// A truncated archived envelop who matches (buffer[head_offset..payload_head_offset] as limiter slice not matching buffer[payload_tail_offset..tail_offset] as delimiter slice within `limiter_pairs`) can be the first element due to advancing.
    pub fn archived_boundaries(&self) -> &Vec<Envelop, A> {
        &self.archived_boundaries
    }
    /// Some(offset) indicates an archived NonEnvelop between last Envelop's tail_offset(or 0 if no Envelop exists) and offset.
    pub fn tail_non_envelop(&self) -> Option<Offset> {
        self.tail_non_envelop
    }
    /// Current `FlatDeserializerState`.
    pub fn deserializer_state(&self) -> &FlatDeserializerState<'l, T, A> {
        &self.deserializer_state
    }
}
/// Iterator over the `State`s of a `FlatDeserializer`, reusing cache or driving deserialization as it goes.
pub struct FlatDeserializerIter<'a, 'd, 'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    flat_deserializer: &'d mut FlatDeserializer<'a, 'l, T, A>,
    pos: usize,
}
impl<'a, 'd, 'l, T, A> Iterator for FlatDeserializerIter<'a, 'd, 'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    type Item = Item;
    /// Odd `pos` => Envelop; even `pos` => NonEnvelop.
    /// Some(phantom Envelop) can be returned as first item to indicate the real first item is a NonEnvelop.
    /// Some(phantom NonEnvelop) can be returned if there's no NonEnvelop between two Envelops.
    fn next(&mut self) -> Option<Self::Item> {
        // Two sequential archived Envelop is legal, two sequential archived NonEnvelop is impossible(MUST be considered as one NonEnvelop by deserializer).
        // Iterate on cache first. Otherwise when cache miss, drive deserialize_incremental expecting a new item to be cached and iterated until the deserializer settles. Then iterate on latest unarchived working_on. Then terminate the iterator.
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
                    Item::E {
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
                    Item::N {
                        non_envelop: NonEnvelop {
                            head_offset: Offset::new(start),
                            tail_offset: Offset::new(end),
                        },
                    }
                };
                return {
                    self.pos += 1;
                    Some(item)
                };
            }
            // Drive the deserializer expecting a new item to be cached until it settles.
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
                // # Safety
                // No new item absorbed, `cached` stays valid.
                if self.pos == cached {
                    if matches!(
                        self.flat_deserializer.deserializer_state.internal(),
                        InternalState::CleanArchived
                    ) {
                        // # Safety
                        // Tail V1 Envelop matching `CleanArchived` MUST be absorbed to cache and already iterated.
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

/// Cursor for [`Slices`].
/// First usize for outer index, inclusive.
///
/// second usize for inner index, inclusive for [`Slices::starts_with()`] and [`Slices::len_until()`], exclusive for [`Slices::ends_with()`] and [`Slices::len_till()`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct Cursor2D(pub usize, pub usize);
impl Cursor2D {
    /// Construct a cursor at `slice_index`/`element_index` for a [`Slices`].
    ///
    /// # Safety
    /// Functions of SDAVE core avoid range-checking this `Cursor2D` as much as possible, so you MUST ensure, for each `Slices` this `Cursor2D` is used on:
    /// - `slice_index` `Slices.0[slice_index]` MUST not panic;
    /// - `element_index <=` `Slices.0[slice_index].len()`;
    ///
    /// A cursor SDAVE core handed out always satisfies the above; a violated one makes whichever `Cursor2D` depender it passed to panic or produce undefined behaviour.
    pub unsafe fn new_unchecked(slice_index: usize, element_index: usize) -> Self {
        Cursor2D(slice_index, element_index)
    }
}
/// Readonly 2D view of slices.
pub struct Slices<'a, 'b, T>(pub &'a [Option<&'b [T]>]);
impl<'a, 'b, T> Slices<'a, 'b, T>
where
    T: Clone + PartialEq + Sized,
{
    /// # Panics
    /// Panics if `self.0` contains no `Some(slice)`.
    pub fn head(&self) -> Cursor2D {
        let slice_index = self
            .0
            .iter()
            .position(|entry| entry.is_some())
            .expect("[SDAVE] Slices::head() on a Slices that carries no Some(slice)");
        Cursor2D(slice_index, 0)
    }
    /// # Panics
    /// Panics if `self.0` contains no `Some(slice)`.
    pub fn tail(&self) -> Cursor2D {
        let slice_index = self
            .0
            .iter()
            .rposition(|entry| entry.is_some())
            .expect("[SDAVE] Slices::tail() on a Slices that carries no Some(slice)");
        Cursor2D(slice_index, self.0[slice_index].unwrap_or(&[]).len())
    }
    /// Total length of ALL slices.
    pub fn len(&self) -> usize {
        self.0.iter().map(|slice| slice.unwrap_or(&[]).len()).sum()
    }
    /// Is this `Slices` empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// `len_till` of contents before `cursor` in slices.
    ///
    /// # Panics
    /// Panics if out of bounds.
    pub fn len_till(&self, cursor: &Cursor2D) -> usize {
        let slice_index = cursor.0;
        self.0[slice_index].unwrap_or(&[])[..cursor.1].len() + Slices(&self.0[..slice_index]).len()
    }
    /// `len_until` of contents alfter `cursor` in slices.
    ///
    /// # Panics
    /// Panics if out of bounds.
    pub fn len_until(&self, cursor: &Cursor2D) -> usize {
        let slice_index = cursor.0;
        self.0[slice_index].unwrap_or(&[])[cursor.1..].len()
            + Slices(&self.0[slice_index + 1..]).len()
    }
    /// Move `cursor` forward by `offset` `T`s in this `Slices`.
    /// `Cursor2D` is guaranteed NOT to point to a `Slices.0[cursor.0]` that is `None` or contains 0 `T`.
    ///
    /// # Panics
    /// Panics if out of bounds.
    pub fn forward_cursor(&self, cursor: &mut Cursor2D, offset: usize) {
        let mut slice_index = cursor.0;
        let mut element_index = cursor.1;
        if self.0[slice_index].unwrap().len() < element_index {
            panic!("[SDAVE] Slices::forward_cursor cursor out of bounds")
        }
        let mut need = offset;
        while need > 0 {
            let consumable = self.0[slice_index].unwrap_or(&[]).len() - element_index;
            if need < consumable {
                element_index += need;
                break;
            }
            need -= consumable;
            slice_index += 1;
            element_index = 0;
        }
        loop {
            if slice_index == self.0.len() {
                // `slice_index` is inclusive: park on the end of the last Some(slice).
                slice_index = self.0.iter().rposition(|entry| entry.is_some()).unwrap();
                element_index = self.0[slice_index].unwrap_or(&[]).len();
                break;
            }
            // Step over empty slice.
            if element_index < self.0[slice_index].unwrap_or(&[]).len() {
                break;
            }
            slice_index += 1;
            element_index = 0;
        }
        cursor.0 = slice_index;
        cursor.1 = element_index;
    }
    /// Move `cursor` backward in this `Slices`.
    ///
    /// # Panics
    /// Panics if out of bounds.
    pub fn backward_cursor(&self, cursor: &mut Cursor2D, offset: usize) {
        let mut slice_index = cursor.0;
        let mut element_index = cursor.1;
        if self.0[slice_index].unwrap().len() < element_index {
            panic!("[SDAVE] Slices::backward_cursor cursor out of bounds")
        }
        let mut need = offset;
        while need > 0 {
            if element_index >= need {
                element_index -= need;
                break;
            } else {
                if slice_index == 0 {
                    panic!("[SDAVE] Slices::backward_cursor cursor out of bounds")
                }
                need -= element_index;
                slice_index -= 1;
                element_index = self.0[slice_index].unwrap_or(&[]).len();
            }
        }
        cursor.0 = slice_index;
        cursor.1 = element_index;
    }
    /// Would `(slices[slice_index..].first[slice..] ++ slices[slice_index..].exclude_first flattened)` start with needle?
    ///
    /// # Panics
    /// Panics if out of bounds.
    pub fn starts_with(&self, cursor: &Cursor2D, needle: &[T]) -> bool {
        if needle.is_empty() {
            return true;
        }
        let mut slice_index = cursor.0;
        let mut element_index = cursor.1;
        if element_index >= self.0[slice_index].unwrap().len() {
            panic!("[SDAVE] Slices::starts_with cursor out of bounds")
        }
        let needed = needle.len();
        let mut consumed = 0;
        loop {
            let entry = self.0[slice_index].unwrap_or(&[]);
            let remaining = &entry[element_index..];
            if remaining.len() >= needed - consumed {
                return remaining[..needed - consumed] == needle[consumed..];
            }
            if remaining != &needle[consumed..consumed + remaining.len()] {
                return false;
            }
            consumed += remaining.len();
            slice_index += 1;
            if slice_index == self.0.len() {
                return false;
            }
            element_index = 0;
        }
    }
    /// Would `(slices[..slice_index].exclude_last flattened ++ slices[..slice_index].last[slice..])` end with needle?
    ///
    /// # Panics
    /// Panics if out of bounds.
    pub fn ends_with(&self, cursor: &Cursor2D, needle: &[T]) -> bool {
        if needle.is_empty() {
            return true;
        }
        let mut slice_index = cursor.0;
        let element_index = cursor.1;
        let slice = self.0[slice_index].unwrap();
        if element_index > slice.len() {
            panic!("[SDAVE] Slices::ends_with cursor out of bounds")
        }
        let mut needed = needle.len();
        if element_index >= needed {
            return slice[element_index - needed..element_index] == *needle;
        }
        if slice[..element_index] != needle[needed - element_index..] {
            return false;
        }
        // The entries before `slice_index` owe the rest of the needle: `slice` of it is already matched.
        needed -= element_index;
        loop {
            if slice_index == 0 {
                return false;
            }
            slice_index -= 1;
            let slice = self.0[slice_index].unwrap_or(&[]);
            if slice.len() >= needed {
                return slice[slice.len() - needed..] == needle[..needed];
            }
            if slice != &needle[needed - slice.len()..needed] {
                return false;
            }
            needed -= slice.len();
        }
    }
}

/// A data structure based on native stack frame for compile-time size unknown Iterator processing.
///
/// # Usage
/// [`Stack::with()`].
#[non_exhaustive]
pub struct Stack<T: Sized> {
    data: T,
    next: Option<NonNull<Stack<T>>>,
}
/// Iterator of a [`Stack`].
#[non_exhaustive]
pub struct StackIter<'a, T: Sized> {
    cursor: Option<&'a Stack<T>>,
}
impl<'a, T: Sized> Iterator for StackIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        let node = self.cursor?;
        // # Safety
        // The stack frames beneath MUST live.
        self.cursor = node.next.map(|next| unsafe { next.as_ref() });
        Some(&node.data)
    }
}
impl<T: Sized> Stack<T> {
    /// Builds the stack from `source`/`producer` and then hands the complete stack to `consumer`, which is called exactly once, from the deepest frame, where all node's frame is guaranteed alive.
    ///
    /// `producer` pulls whatever data it needs out of `source` to construct the next `Option<T>`; returning `None` marks the boundary of the stack, which stops the recursion and calls `consumer`.
    ///
    /// `consumer` uses Stack::iter() on the captured Stack to consume elements produced by `producer` with the exact same sequence `producer` produces them.
    ///
    /// # Safety
    /// `producer` MUST be bounded by producing `None`, stack overflow is unsound.
    pub unsafe fn with<I, P, W>(mut source: I, mut producer: P, consumer: W)
    where
        I: Iterator,
        P: FnMut(&mut I) -> Option<T>,
        W: for<'a> FnOnce(&'a Stack<T>),
    {
        let mut head: Stack<T> = match producer(&mut source) {
            Some(data) => Stack { data, next: None },
            None => return,
        };
        let head_ptr = NonNull::from_mut(&mut head);
        let mut consumer = Some(consumer);
        Self::push(
            head_ptr,
            head_ptr,
            &mut source,
            &mut producer,
            &mut consumer,
        );
    }
    fn push<I, P, W>(
        head: NonNull<Stack<T>>,
        parent: NonNull<Stack<T>>,
        source: &mut I,
        producer: &mut P,
        consumer: &mut Option<W>,
    ) where
        I: Iterator,
        P: FnMut(&mut I) -> Option<T>,
        W: for<'a> FnOnce(&'a Stack<T>),
    {
        let mut node: Stack<T> = match producer(source) {
            Some(data) => Stack { data, next: None },
            None => {
                if let Some(consumer) = consumer.take() {
                    consumer(unsafe { head.as_ref() });
                }
                return;
            }
        };
        let node_ptr = NonNull::from_mut(&mut node);
        unsafe { (*parent.as_ptr()).next = Some(node_ptr) };
        Self::push(head, node_ptr, source, producer, consumer);
    }
    /// Construct an Iterator for this [`Stack`].
    pub fn iter(&self) -> StackIter<'_, T> {
        StackIter { cursor: Some(self) }
    }
}

/// Return `Some(least_repeat)` if it is serializable with given `limiter_pair` under given context where least_repeat is guaranteed >= `limiter_pair.least_repeat` AND to be the minimal working boundary, or `None` if it is not serializable.
///
/// Serializable: `buffer[0..buffer.len()] ++ limiter_pair.limiter * least_repeat ++ payload (support unflattened slices, which would be considered as a sequential slice (logically flattened)) ++ limiter_pair.delimiter * least_repeat` MUST be able to be deserialized to get an Envelop framing such payload.
///
/// # Side Effects
/// `limiter_pair.bsize` would be reset and mutated.
///
/// # Safety
/// The tail of `buffer` MUST be a clean item boundary: head of the buffer, or tail of an item meant to be archived. i.e. somewhere after constructing a FlatDeserializer on buffer then calling deserialize_all() on it then on its result you MUST get `internal()` matching [`InternalState::is_exhausted()`] AND you can assert `internal()` is NOT `Unknown` or `UnterminatedLimiterSlice`.
///
/// # Panics
/// Panics if `payload` doesn't contain any slice.
#[must_use]
pub unsafe fn can_serialize<T, A>(
    buffer: &[T],
    payload: &[Option<&[T]>],
    variant: Variant,
    limiter_pair: &mut LimiterPair<T, A>,
) -> Option<NonZeroUsize>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let least_repeat = limiter_pair.least_repeat()?.get();
    let limiter = limiter_pair.limiter();
    let delimiter = limiter_pair.delimiter();
    let payload = Slices(payload);
    let mut cursor = Cursor2D(0, 0);
    let mut tail = payload.tail();
    if limiter.is_empty() || buffer.ends_with(limiter) || payload.starts_with(&cursor, limiter) {
        return None;
    }
    if delimiter.is_empty() || (variant == Variant::V1 && payload.ends_with(&tail, delimiter)) {
        return None;
    }
    if variant == Variant::V2 {
        loop {
            if payload.ends_with(&tail, delimiter) {
                payload.backward_cursor(&mut tail, delimiter.len());
            } else {
                break;
            }
        }
    }
    limiter_pair.bsize = 0;
    let mut max_run = 0usize;
    if payload.len_till(&tail) >= delimiter.len() {
        loop {
            if !(cursor < tail) {
                break;
            }
            let mut run = 0usize;
            let mut cursor_ = cursor.clone();
            'inner: loop {
                let repeating = run * delimiter.len();
                if !(limiter_pair.bsize & (1 << repeating) == 0) {
                    break;
                }
                if payload.starts_with(&cursor, delimiter) {
                    if 2 < repeating && repeating < USIZE_BITS + 1 {
                        limiter_pair.bsize |= 1 << (repeating - 1);
                    }
                    payload.forward_cursor(&mut cursor, delimiter.len());
                    run += 1;
                } else {
                    max_run = max_run.max(run);
                    break 'inner;
                }
            }
            payload.forward_cursor(&mut cursor_, 1);
            cursor = cursor_;
            limiter_pair.bsize >>= 1;
        }
    }
    NonZeroUsize::new(least_repeat.max(max_run + 1))
}
/// Store all limiter_pairs that each of them is guaranteed to be able to serialize given `payload` under given context to `result`.
///
/// Each pair is guaranteed to have Some `least_repeat` which carries the minimal working boundary its `least_repeat`.
///
/// # Safety
/// Same contract as [`can_serialize`].
pub unsafe fn get_all_serializable_limiter_pairs<'l, T, A>(
    buffer: &[T],
    payload: &[Option<&[T]>],
    variant: Variant,
    limiter_pairs: &mut Vec<LimiterPair<'l, T, A>, A>,
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

/// If found { `next` { expected as NonEnvelop => archive it to `working_on` & update `internal` to match `is_archived` then update `next` to an empty-payload non-phantom Envelop whose `tail_offset` is `None`, exception: this NonEnvelop meant to be archived matches zero-width => additionally call `seek_next_delimiter_slice` in place(the archived phantom NonEnvelop is not returned/committed) } } else { update `next` & `internal` then clone `next` to `working_on` }.
///
/// # Safety
/// For insight, not recommend to rely on this.
#[must_use]
pub unsafe fn seek_next_limiter_slice<'l, T, A>(
    buffer: &[T],
    mut cursor: usize,
    mut old_deserializer_state: DecoupledFlatDeserializerState<'l, T, A>,
) -> DecoupledFlatDeserializerState<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let working_on = &mut old_deserializer_state.0;
    let internal = &mut old_deserializer_state.1;
    let next = &mut old_deserializer_state.2;
    let confirmed_limiter_pair = &mut old_deserializer_state.4;
    let repeat_map = old_deserializer_state.5.as_mut().unwrap();
    let limiter_pairs = old_deserializer_state.6.as_mut().unwrap();

    match next.clone() {
        Item::E { .. } => panic!("[SDAVE] Illegal state."),
        Item::N { mut non_envelop } => loop {
            let from = non_envelop.tail_offset.get();
            cursor = cursor.max(from);
            if from == buffer.len() {
                *next = Item::N {
                    non_envelop: non_envelop.clone(),
                };
                *working_on = next.clone();
                *internal = InternalState::Clean;
                return old_deserializer_state;
            } else if cursor == buffer.len() {
                *next = Item::N {
                    non_envelop: non_envelop.clone(),
                };
                *working_on = next.clone();
                for i in 0..limiter_pairs.len() {
                    if let Some(cached_repeat) = repeat_map[i]
                        && cached_repeat.get() >= limiter_pairs[i].least_repeat().unwrap().get()
                    {
                        *internal = InternalState::UnterminatedLimiterSlice {
                            tail_offset: Offset::new(buffer.len()),
                        };
                        return old_deserializer_state;
                    }
                }
                *internal = InternalState::Unknown {
                    tail_offset: Offset::new(buffer.len()),
                };
                return old_deserializer_state;
            }
            cursor += 1;
            let mut any_some = false;
            for i in 0..repeat_map.len() {
                if let Some(cached_repeat) = repeat_map[i] {
                    let cached_repeat = cached_repeat.get();
                    let limiter_pair = &mut limiter_pairs[i];
                    let limiter = limiter_pair.limiter();
                    let repeating = cached_repeat * limiter.len();
                    if !(limiter_pair.bsize & (1 << repeating) == 0) {
                        // the previous bigger repeat turns out not exceeding the archive condition, then this one would be impossible.
                        continue;
                    }
                    let mut archive_check = false;
                    let aligned = (cursor - from).is_multiple_of(limiter.len());
                    if aligned || cursor == buffer.len() {
                        let match_start = from + repeating;
                        if aligned {
                            if buffer[match_start..cursor].starts_with(limiter) {
                                if 2 < repeating && repeating < USIZE_BITS + 1 {
                                    limiter_pair.bsize |= 1 << (repeating - 1);
                                }
                                debug_assert_eq!(
                                    cached_repeat + 1,
                                    (cursor - from) / limiter.len()
                                );
                                repeat_map[i] = Some(NonMaxUsize::new(cached_repeat + 1));
                                any_some = true;
                                continue;
                            } else {
                                archive_check = true;
                            }
                        }
                        if !aligned && cursor == buffer.len() && cursor - match_start != 0 {
                            if limiter.starts_with(&buffer[match_start..]) {
                                any_some = true;
                                continue;
                            } else {
                                archive_check = true;
                            }
                        }
                        if archive_check {
                            let least_repeat = limiter_pair.least_repeat().unwrap().get();
                            if cached_repeat >= least_repeat {
                                *working_on = Item::N {
                                    non_envelop: non_envelop.clone(),
                                };
                                *next = Item::E {
                                    envelop: Envelop {
                                        head_offset: Offset::new(from),
                                        payload_head_offset: Offset::new(match_start),
                                        payload_tail_offset: Offset::new(match_start),
                                        tail_offset: None,
                                    },
                                };
                                *internal = InternalState::Archived;
                                let mut limiter_pair_confirmed = limiter_pair.clone();
                                limiter_pair_confirmed.least_repeat =
                                    Some(NonZeroUsize::new(cached_repeat).unwrap());
                                *confirmed_limiter_pair = limiter_pair_confirmed;
                                unsafe {
                                    update_internal_cache(repeat_map, limiter_pairs, None);
                                }
                                if non_envelop.is_phantom() {
                                    return unsafe {
                                        seek_next_delimiter_slice(
                                            buffer,
                                            match_start,
                                            old_deserializer_state,
                                        )
                                    };
                                } else {
                                    return old_deserializer_state;
                                }
                            } else {
                                repeat_map[i] = None;
                                continue;
                            }
                        }
                    }
                    any_some = true;
                }
            }
            if any_some {
                continue;
            } else {
                non_envelop.tail_offset = Offset::new(from + 1);
                cursor = from + 1;
                unsafe {
                    update_internal_cache(repeat_map, limiter_pairs, Some(Offset::new(1)));
                }
                continue;
            }
        },
    }
}
/// `cursor` expected to be the very last offset `deserialize_incremental` last scanned.
/// If found { `next` { expected as Envelop => archive it to `working_on` & update `internal` to match `is_archived` then update `next` to a phantom NonEnvelop }  } else { update `next` & `internal` then clone `next` to `working_on` }.
///
/// # Safety
/// For insight, not recommend to rely on this.
#[must_use]
pub unsafe fn seek_next_delimiter_slice<'l, T, A>(
    buffer: &[T],
    mut cursor: usize,
    mut old_deserializer_state: DecoupledFlatDeserializerState<'l, T, A>,
) -> DecoupledFlatDeserializerState<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let working_on = &mut old_deserializer_state.0;
    let internal = &mut old_deserializer_state.1;
    let next = &mut old_deserializer_state.2;
    let variant = old_deserializer_state.3;
    let limiter_pair = &mut old_deserializer_state.4;

    match next.clone() {
        Item::N { .. } => panic!("[SDAVE] Illegal state."),
        Item::E { mut envelop } => {
            debug_assert!(envelop.tail_offset.is_none());
            let delimiter = limiter_pair.delimiter();
            let least_repeat = limiter_pair.least_repeat().unwrap().get();
            loop {
                let from = envelop.payload_tail_offset.get();
                cursor = cursor.max(from);
                if from == buffer.len() {
                    *next = Item::E {
                        envelop: envelop.clone(),
                    };
                    *working_on = next.clone();
                    *internal = InternalState::Clean;
                    return old_deserializer_state;
                }
                let mut cached_repeat = (cursor - from) / delimiter.len();
                let repeating = cached_repeat * delimiter.len();
                if !(limiter_pair.bsize & (1 << repeating) == 0) {
                    if variant == Variant::V2
                        && let InternalState::UnterminatedDeLimiterSlice { tail_offset } = internal
                    {
                        if tail_offset.get() == buffer.len() {
                            *next = Item::E {
                                envelop: envelop.clone(),
                            };
                            *working_on = next.clone();
                            return old_deserializer_state;
                        } else {
                            cursor += delimiter.len();
                            continue;
                        }
                    } else {
                        envelop.payload_tail_offset = Offset::new(from + 1);
                        limiter_pair.bsize >>= 1;
                        cursor = from + 1;
                        continue;
                    }
                }
                let match_start = from + repeating;
                let slice = &buffer[match_start..];
                if slice.starts_with(delimiter) {
                    // `repeating` == 1 => already calculated and thus cannot save future work. while bit for `repeating` == 2 MUST survive << 1.
                    if 2 < repeating && repeating < USIZE_BITS + 1 {
                        limiter_pair.bsize |= 1 << (repeating - 1);
                    }
                    cached_repeat += 1;
                    cursor = match_start + delimiter.len();
                    if cached_repeat >= least_repeat {
                        let diff = cached_repeat - least_repeat;
                        match variant {
                            Variant::V1 => {
                                debug_assert_eq!(diff, 0);
                                envelop.tail_offset = Some(Offset::new(cursor));
                                *next = Item::N {
                                    non_envelop: unsafe {
                                        NonEnvelop::phantom(Offset::new(cursor))
                                    },
                                };
                                *working_on = Item::E {
                                    envelop: envelop.clone(),
                                };
                                if buffer.len() == cursor {
                                    *internal = InternalState::CleanArchived;
                                } else {
                                    *internal = InternalState::Archived
                                }
                                limiter_pair.bsize = 0;
                                return old_deserializer_state;
                            }
                            Variant::V2 => {
                                if diff > 0 {
                                    let shift = diff * delimiter.len();
                                    envelop.payload_tail_offset = Offset::new(from + shift);
                                    if shift < USIZE_BITS {
                                        limiter_pair.bsize >>= shift;
                                    } else {
                                        limiter_pair.bsize = 0;
                                    }
                                    *internal = InternalState::UnterminatedDeLimiterSlice {
                                        tail_offset: Offset::new(cursor),
                                    };
                                    if slice.len() == delimiter.len() {
                                        *next = Item::E {
                                            envelop: envelop.clone(),
                                        };
                                        *working_on = next.clone();
                                        return old_deserializer_state;
                                    }
                                }
                                continue;
                            }
                        }
                    }
                } else if delimiter.starts_with(slice) {
                    *next = Item::E {
                        envelop: envelop.clone(),
                    };
                    *working_on = next.clone();
                    match variant {
                        Variant::V1 => {
                            *internal = InternalState::Unknown {
                                tail_offset: Offset::new(buffer.len()),
                            };
                            return old_deserializer_state;
                        }
                        Variant::V2 => {
                            if cached_repeat == least_repeat {
                                *internal = InternalState::UnterminatedDeLimiterSlice {
                                    tail_offset: Offset::new(buffer.len()),
                                };
                            } else {
                                *internal = InternalState::Unknown {
                                    tail_offset: Offset::new(buffer.len()),
                                };
                            }
                            return old_deserializer_state;
                        }
                    }
                } else {
                    if cached_repeat == least_repeat {
                        debug_assert!(matches!(variant, Variant::V2));
                        envelop.tail_offset = Some(Offset::new(match_start));
                        *next = Item::N {
                            non_envelop: unsafe { NonEnvelop::phantom(Offset::new(match_start)) },
                        };
                        *working_on = Item::E {
                            envelop: envelop.clone(),
                        };
                        *internal = InternalState::Archived;
                        limiter_pair.bsize = 0;
                        return old_deserializer_state;
                    } else {
                        envelop.payload_tail_offset = Offset::new(from + 1);
                        limiter_pair.bsize >>= 1;
                        cursor = from + 1;
                        continue;
                    }
                }
            }
        }
    }
}
/// Advance the FlatDeserializerState by exactly one step, that is, the returned FlatDeserializerState of which, comparing to the old one, when the old FlatDeserializerState was not exhausted claim exactly one new item(head of the new item == tail of the old item) as archived or unarchived OR claim exhausted, or when the old FlatDeserializerState was exhausted if the buffer not ever grown since then(semantical tail of FlatDeserializerState still eq to buffer.len()) else claim more confirmed contents to the old item and update InternalState to new not exhausted or new exhausted.
///
/// # Side Effects
/// `repeat_map` and `limiter_pairs` would be taken from `old_deserializer_state` to the returned FlatDeserializerState.
///
/// # Safety
/// `old_deserializer_state` MUST match buffer's content by offset and semantics.
/// You MUST manually call FlatDeserializerState::advance(offset) on old_deserializer_state if you advance the buffer.
#[must_use]
pub unsafe fn deserialize_incremental<'l, T, A>(
    buffer: &[T],
    old_deserializer_state: &mut FlatDeserializerState<'l, T, A>,
) -> FlatDeserializerState<'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let repeat_map = old_deserializer_state.repeat_map.take();
    let limiter_pairs = old_deserializer_state.limiter_pairs.take();
    let mut old_deserializer_state = unsafe { old_deserializer_state.clone() }.decouple();
    old_deserializer_state.5 = repeat_map;
    old_deserializer_state.6 = limiter_pairs;
    let internal = old_deserializer_state.1.clone();
    let next = old_deserializer_state.2.clone();
    unsafe {
        FlatDeserializerState::new_unchecked(
            if buffer.len() != next.semantical_tail_offset().get() {
                match internal {
                    InternalState::CleanArchived => seek_next_limiter_slice(
                        buffer,
                        next.semantical_tail_offset().get(),
                        old_deserializer_state,
                    ),
                    InternalState::Archived => match next {
                        Item::N { .. } => seek_next_limiter_slice(
                            buffer,
                            next.semantical_tail_offset().get(),
                            old_deserializer_state,
                        ),
                        Item::E { .. } => seek_next_delimiter_slice(
                            buffer,
                            next.semantical_tail_offset().get(),
                            old_deserializer_state,
                        ),
                    },
                    InternalState::Unknown { tail_offset } => match next {
                        Item::E { .. } => seek_next_delimiter_slice(
                            buffer,
                            tail_offset.get(),
                            old_deserializer_state,
                        ),
                        Item::N { .. } => seek_next_limiter_slice(
                            buffer,
                            tail_offset.get(),
                            old_deserializer_state,
                        ),
                    },
                    InternalState::UnterminatedLimiterSlice { tail_offset } => {
                        seek_next_limiter_slice(buffer, tail_offset.get(), old_deserializer_state)
                    }
                    InternalState::UnterminatedDeLimiterSlice { tail_offset } => {
                        seek_next_delimiter_slice(buffer, tail_offset.get(), old_deserializer_state)
                    }
                    InternalState::Clean => match next {
                        Item::E { .. } => seek_next_delimiter_slice(
                            buffer,
                            next.semantical_tail_offset().get(),
                            old_deserializer_state,
                        ),
                        Item::N { .. } => seek_next_limiter_slice(
                            buffer,
                            next.semantical_tail_offset().get(),
                            old_deserializer_state,
                        ),
                    },
                }
            } else {
                old_deserializer_state
            },
        )
    }
}
