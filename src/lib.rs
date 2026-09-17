//! for limiter_pairs not exceeding assertions for simple algorithm(~not recommended), SDAVE parser just select the first matching valid limiter_pair, and thus you get the freedom to sort the limiter_pairs and influent how would SDAVE parser select limiter_pair (e.g. [("ab","de",2),("ababac","dede",1)] for buffer "ababacedede" -> limiter slice "abab" ++ payload "ace" ++ delimiter slice "dede";[("ababac","dede",1),("ab","de",2)] for buffer "ababacedede" -> limiter slice "ababac" ++ payload "e" ++ delimiter slice "dede").

use std::num::NonZeroUsize;

use crate::State::Unknown;

pub struct NonMaxUsize {
    non_max: NonZeroUsize,
}
const NON_MAX_ERROR_MESSAGE: &str = "[SDAVE] NonMaxUsize exceeds usize::MAX before OOM";
impl NonMaxUsize {
    pub fn get(&self) -> usize {
        self.non_max.get() - 1
    }
    /// # Panics
    /// Panic when offset exceeds usize::MAX.
    pub unsafe fn new(offset: usize) -> Self {
        NonMaxUsize {
            non_max: NonZeroUsize::new(offset + 1).expect(NON_MAX_ERROR_MESSAGE),
        }
    }
    /// # Panics
    /// Panic when `self.get() - offset` exceeds usize::MAX.
    pub unsafe fn offset_by(&mut self, offset: usize) {
        self.non_max = NonZeroUsize::new(self.get() - offset + 1).expect(NON_MAX_ERROR_MESSAGE)
    }
}
/// absolute offset on buffer index.
type Offset = NonMaxUsize;

/// `T`: basic data unit of a buffer.
/// `limiter`/`delimiter` a slice of vec of T, e.g. UTF-8 1B/2B/3B char in a u8 buffer.
/// Technically, limiter can equal to (a repeating unit/a sequence of)delimiter: it would cause conflicts between empty Envelop and long limiter slice; in case of that, SDAVE always consider the slice as long limiter slice, and an empty-payload-Envelop is impossible; it's not recommended.
/// Technically, one limiter can has multiple delimiters in different LimiterPairs, it's not recommended.
/// Technically, one delimiter can has multiple limiters in different LimiterPairs, it's not recommended.
#[derive(Clone)]
pub struct LimiterPair<T: Sized + PartialEq + Clone> {
    /// Case parsing: match `limiter`/`delimiter` first, then guard on `least_repeat` where assertion(repeat>=1) is always true.
    /// Case serializing: the minimal repeat to frame certain payload being inserted at `buffer[..offset] _here_ buffer[offset..]` without introducing conflicts.
    ///
    ///`None`: reserved to tell this limiter pair is not capatible to frame certain payload at `buffer[..offset] here buffer[offset..]` without conflicts.
    pub least_repeat: Option<NonZeroUsize>,
    pub limiter: Vec<T>,
    pub delimiter: Vec<T>,
}
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
pub enum Variant {
    V1,
    V2,
}

pub struct FlatParserIter<'a, T: Sized + PartialEq + Clone> {
    parser: &'a mut FlatParser<'a, T>,
    pos: usize,
    // todo: more iter state to allow better performance for fn `next()`.
}
impl<'a, T> Iterator for FlatParserIter<'a, T>
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
    /// otherwise, try parse_incremental to update FlatParser::archived_boundaries & FlatParser::tail_non_envelop then retry.
    /// then return latest unarchived state if it is either an Envelop or a NonEnvelop.
    /// then return None if latest unarchived Envelop or unarchived NonEnvelop is ALREADY iterated or latest state is PartialLimiterSlice or Unknown.
    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}
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
        FlatParserState::new(Unknown, limiter)
    }
}
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
                state: Unknown,
                limiter: current_limiter,
            },
        }
    }
    /// you can advance the buffer(or just alloc new one and copy tailer to new buffer) to save memory or append new contents to it alfter FlatParser release(drop the ref to buffer alfter FlatParser::decouple) read access to your buffer, but you MUST call FlatParser::advance(offset) if you advance your buffer.
    /// you can replace limiter_pairs, which would be applied to future(parser_state based) envelopes(limiter for current unarchived envelop is cloned to avoid semantic issue).
    /// you can replace variant, which would be applied right next parse_incremental.
    pub fn decouple(
        self,
    ) -> (
        &'a [T],
        Variant,
        Vec<LimiterPair<T>>,
        Vec<Envelop>,
        Option<Offset>,
        FlatParserState<T>,
    ) {
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
    /// return how many archived items being fully removed from cache.
    pub unsafe fn advance(&mut self, offset: Offset) -> usize {
        todo!()
    }
    /// replace the buffer FlatParser can read.
    ///
    /// # Safety
    /// new buffer contents MUST be equal to operations of conbination of [advance, append] being done to old buffer, you MUST call FlatParser::advance(offset) if new buffer is advanced.
    pub unsafe fn replace_buffer(&mut self, buffer: &'a [T]) {
        self.buffer = buffer
    }
    /// expected usage: loop until returned state is unarchived or break anytime you want.
    pub fn parse_incremental(&mut self) -> &State {
        todo!()
    }
    pub fn iter(&'a mut self) -> FlatParserIter<'a, T> {
        FlatParserIter {
            parser: self,
            pos: 0,
        }
    }
    /// # Safety
    /// pos MUST be valid -- within cached archived items.
    pub unsafe fn iter_from(&'a mut self, pos: usize) -> FlatParserIter<'a, T> {
        FlatParserIter {
            parser: self,
            pos: pos,
        }
    }
}

/// Return `Some(least_repeat)` if serializable with given `limiter_pair` under given context where least_repeat can be grater than `limiter_pair.least_repeat`, or `None` if not serializable.
///
/// Serializable: `buffer[..offset] ++ limiter_pair.limiter * least_repeat.get() ++ payload ++ limiter_pair.delimiter * least_repeat.get() ++ following`
/// can be parsed to get an Envelop framing payload.
///
/// # Safety
/// `buffer[..offset]` MUST end/tail with NonEnvelop or Unknown, MUST NOT end/tail with
pub unsafe fn can_serialize<T: Sized + PartialEq + Clone>(
    variant: Variant,
    buffer: &[T],
    offset: Offset,
    payload: &[T],
    following: &[T],
    limiter_pair: &LimiterPair<T>,
) -> Option<NonZeroUsize> {
    todo!()
}

/// Return a vec of all limiter_pairs that CAN serialize given `payload` under given context.
pub unsafe fn get_all_serializable_limiter_pairs<T: Sized + PartialEq + Clone>(
    variant: Variant,
    buffer: &[T],
    offset: Offset,
    payload: &[T],
    following: &[T],
    limiter_pair: &[LimiterPair<T>],
) -> Vec<LimiterPair<T>> {
    todo!()
}

/// pass old_parser_state as FlatParserState::default(whatever) to start fresh parsing.
/// pass last returned non-unknown parser_state for incremental parsing.
///
/// returned State's head_offset != state's head_offset means old item in old state is confirmed archived, and thus returned State start alfter tail_offset of that item, otherwise current parsing slice cannot be confirmed archived by SDAVE alone, you need to tell whether the producer still stream contents to this buffer and decide how would you call parse_incremental in future.
/// specially, E(Envelop) whose tail_offset is_some() means this envelop is confirmed archived.
///
/// for a streaming `buffer` already containing parsed non-unknown contents, never pass an default state or it reparse from offset 0.
///
/// expected usage: call parse_incremental inside a loop until returned state is unarchived, or break midturn if you want.
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
    todo!()
}
