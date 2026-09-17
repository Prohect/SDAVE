//! # SDAVE
//!
//! **S**treamable **D**elimiter-**A**daptive **V**erbatim **E**nvelope protocol.
//!
//! SDAVE is a fundamental, flat (nesting-agnostic, payload-agnostic)
//! serialization protocol optimized for LLM agentic harness usage. It reserves
//! a serialize-time-defined delimiter slice from the payload and decodes the
//! payload verbatim — byte-identical to what was written. It tells serialized
//! envelopes, potentially streaming envelopes and non-envelope slices from one
//! slice (channel) mixing them together, and those non-envelope slices won't
//! be considered as unexpected results.
//!
//! See `README.md` for the full protocol design.
//!
//! ## Usage model
//!
//! SDAVE never owns the buffer. The application feeds a monotonically growing
//! slice `s` and drives the parser state by state:
//!
//! 1. [`parse`] returns the [`State`] of the segment starting at offset 0.
//! 2. [`parse_incremental`] takes the current state and either
//!    - grows a pending (`!archived`) state monotonically as more of `s`
//!      arrives, or
//!    - once the state is archived, returns the state of the next segment,
//!      starting right after the archived state's tail.
//!
//! A state is *archived* (confirmed, will never grow again) when
//! [`State::is_archived`] returns true. For a pending state, being told a
//! returned state whose [`State::head_offset`] differs from the previous
//! state's means the previous state is confirmed archived (a [`NonEnvelop`]
//! is confirmed by a valid limiter slice right behind its `tail_offset`).
//!
//! ## Checked vs unchecked
//!
//! [`parse`]/[`parse_incremental`] handle arbitrary limiter pair sets
//! (ambiguities resolve by pair order). When the pair set obeys the
//! recommended invariants (see [`limiter_pairs_recommended`]),
//! [`parse_unchecked`]/[`parse_incremental_unchecked`] skip the
//! ambiguity-resolution work and provide better performance.
//!
//! ```text
//! buffer = [content_slice0 /* State::Empty */, OK(SDAVE0), content_slice1, OK(SDAVE1), PENDING(SDAVE2)]
//! ```

use core::num::NonZeroUsize;

/// Delimiter match algorithm variant. There is no full winner; choose based on
/// the scene. See `README.md` § "Delimiter match algorithm".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The first full matched delimiter slice is just the delimiter slice.
    /// No delimiter slice confirmation latency, but the tail of the payload
    /// MUST NOT collide with the delimiter.
    V1,
    /// The first full matched delimiter slice FOLLOWED by a non-delimiter is
    /// the delimiter slice. The tail of the payload won't disable a limiter
    /// pair, but introduces delimiter slice confirmation latency (solvable on
    /// application level by pushing a non-delimiter to the mixed channel).
    V2,
}

/// One of a configurable, SDAVE-parse-time-fixed set of limiter pairs.
///
/// The *limiter slice* is `<$limiter>[...]`: `least_repeat` or more repeats of
/// `limiter`. The *delimiter slice* is then `delimiter` (of the same pair)
/// repeated the same times; `(de)limiter` below means "limiter or delimiter".
/// E.g. with `limiter == '^'`, `delimiter == '~'` and two repeats, `^^~~` is
/// an archived envelope with an empty payload.
///
/// A *potential* limiter/delimiter slice is a suffix of the currently
/// available buffer that may complete into a valid slice when more input
/// arrives; it is held back, i.e. not yet *committed*.
///
/// When several pairs could match at one position, the first pair in the
/// input slice wins; the application's responsibility/freedom is to stabilize
/// the behaviour by sorting the input pairs.
///
/// Degenerate pairs are handled but not recommended: an empty `limiter`
/// never matches, and an empty `delimiter` makes every envelope archive
/// immediately behind its limiter slice with an empty payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimiterPair<'a, T: Sized> {
    /// A value of 0 would behave like 1; it is `NonZeroUsize`, so the
    /// assertion `repeat >= 1` always holds.
    pub least_repeat: NonZeroUsize,
    /// Opens an envelope when repeated at least `least_repeat` times.
    pub limiter: &'a [T],
    /// Closes the envelope when repeated as many times as the limiter was.
    pub delimiter: &'a [T],
}

impl<'a, T: Sized> LimiterPair<'a, T> {
    /// Both `limiter` and `delimiter` SHOULD be non-empty; see the struct
    /// docs for how degenerate pairs behave.
    pub fn new(least_repeat: NonZeroUsize, limiter: &'a [T], delimiter: &'a [T]) -> Self {
        Self {
            least_repeat,
            limiter,
            delimiter,
        }
    }
}

/// Returns `true` if `limiter_pairs` obeys the recommended invariants
/// required by [`parse_unchecked`]/[`parse_incremental_unchecked`]:
///
/// - every `limiter` and `delimiter` is non-empty;
/// - `limiter != delimiter` for every pair, and no (de)limiter is equal to
///   (a repeating unit / a sequence of) another (de)limiter — neither within
///   a pair nor across pairs;
/// - all limiters are pairwise distinct, all delimiters are pairwise
///   distinct, and the limiter set and the delimiter set are disjoint;
/// - no two limiters are prefix-comparable (two limiters matching at one
///   position would need ambiguity resolution, and re-deriving the pair of a
///   pending [`Envelop`] would be ambiguous);
/// - no proper rotation of a limiter exposes a prefix of any limiter
///   (including itself), so no limiter can match inside another limiter's
///   run, which the unchecked scan skips wholesale. A rotation mapping a
///   limiter onto itself (a period of it, e.g. `^^` by 1) is harmless: it
///   can only extend the very same run.
///
/// Sets of single-character (de)limiters with distinct limiter chars and
/// distinct delimiter chars — the best practice for u8 buffers, reserving
/// UTF-8 only chars — always satisfy this.
pub fn limiter_pairs_recommended<T: Sized + PartialEq>(limiter_pairs: &[LimiterPair<T>]) -> bool {
    let mut slices: Vec<&[T]> = Vec::with_capacity(2 * limiter_pairs.len());
    for p in limiter_pairs {
        if p.limiter.is_empty() || p.delimiter.is_empty() {
            return false;
        }
        slices.push(p.limiter);
        slices.push(p.delimiter);
    }
    for (i, a) in slices.iter().enumerate() {
        for b in &slices[i + 1..] {
            if a == b || is_repetition_of(a, b) || is_repetition_of(b, a) {
                return false;
            }
        }
    }
    for (ai, a) in limiter_pairs.iter().enumerate() {
        let pa = a.limiter;
        // No two limiters are prefix-comparable.
        for b in &limiter_pairs[ai + 1..] {
            let pb = b.limiter;
            if (pa.len() < pb.len() && pb.starts_with(pa))
                || (pb.len() < pa.len() && pa.starts_with(pb))
            {
                return false;
            }
        }
        // No proper rotation of a limiter exposes a prefix of any limiter.
        for t in 1..pa.len() {
            let self_period = (0..pa.len()).all(|k| pa[k] == pa[(t + k) % pa.len()]);
            for (bi, b) in limiter_pairs.iter().enumerate() {
                if ai == bi && self_period {
                    continue;
                }
                let pb = b.limiter;
                let interferes =
                    (1..=pb.len()).any(|m| (0..m).all(|k| pb[k] == pa[(t + k) % pa.len()]));
                if interferes {
                    return false;
                }
            }
        }
    }
    true
}

/// `x` is a repetition (sequence) of `unit`: `x == unit` repeated >= 2 times.
fn is_repetition_of<T: Sized + PartialEq>(x: &[T], unit: &[T]) -> bool {
    x.len() >= 2 * unit.len()
        && x.len().is_multiple_of(unit.len())
        && x.chunks(unit.len()).all(|c| c == unit)
}

/// State of the segment of `s` currently being told apart.
///
/// Invariants for a returned state over buffer `s`:
/// - `NonEnvelop`: `s[head_offset..tail_offset]` MUST NOT contain a valid
///   limiter slice and MUST NOT end with a potential limiter slice.
/// - `Envelop`: `s[payload_head_offset..payload_tail_offset]` MUST NOT
///   contain a valid delimiter slice and MUST NOT end with a potential
///   delimiter slice.
///
/// A state passed back to [`parse_incremental`] MUST have been produced by a
/// SDAVE parse fn (checked or unchecked) and may only have been changed by
/// [`State::offset`]. Fields are public for inspection and pattern matching,
/// not for construction or mutation. Returned offsets are within `s`; in
/// particular, a `NonEnvelop` has `head_offset < tail_offset`, and a pending
/// `Envelop` has a well defined limiter slice (`payload_head_offset.is_some()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Empty, or `s[..]` is a potential limiter slice, or `s[..]` MUST be a
    /// limiter slice but repeats are not well defined yet (not terminated by
    /// a not-repeating `T`). Carries no offsets; its `head_offset` is 0.
    Empty,
    /// A non-envelope segment: plain content of the mixed channel.
    N(NonEnvelop),
    /// An envelope segment.
    Y(Envelop),
}

/// A non-envelope slice: plain content of the mixed channel.
///
/// See [`State`] for the validity contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEnvelop {
    /// Start offset of this segment.
    pub head_offset: usize,
    /// Can monotonically increase in next updates.
    pub tail_offset: usize,
    /// `true` means this `NonEnvelop` is confirmed archived, would never grow
    /// again (there MUST be a valid limiter slice behind `tail_offset`).
    pub ok: bool,
}

/// A (potentially streaming) envelope.
///
/// See [`State`] for the validity contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelop {
    /// Head of the limiter slice.
    pub head_offset: usize,
    /// `Some` once the limiter slice is well defined (at least one
    /// non-limiter `T` — either payload head or a delimiter not equal to the
    /// limiter — exists following the limiter slice); the offset is right
    /// behind the limiter slice. An archived `Envelop` with an empty payload
    /// stores `None` here instead (see `tail_offset`).
    pub payload_head_offset: Option<NonZeroUsize>,
    /// End of the committed payload while pending (`None` before anything is
    /// committed); can monotonically increase in next updates. An archived
    /// `Envelop` stores `Some` here if and only if its payload is non-empty.
    pub payload_tail_offset: Option<NonZeroUsize>,
    /// `Some(offset)` means this `Envelop` is confirmed archived, would never
    /// grow again; `offset` is right behind the delimiter slice. An archived
    /// `Envelop` with `limiter != delimiter` can have an empty payload, e.g.
    /// `^^~~`, in which case both payload offsets are `None`. An empty
    /// `Envelop` is an empty `Envelop`, not a `NonEnvelop`. Behind it there
    /// could be a `NonEnvelop` or an `Envelop`.
    pub tail_offset: Option<NonZeroUsize>,
}

impl State {
    /// Head offset of the segment this state describes; 0 for `State::Empty`.
    ///
    /// A [`parse_incremental`] result whose `head_offset` differs from the
    /// input state's means the input state is confirmed archived.
    pub fn head_offset(&self) -> usize {
        match self {
            State::Empty => 0,
            State::N(n) => n.head_offset,
            State::Y(e) => e.head_offset,
        }
    }

    /// Offset right behind this segment: `tail_offset` for `N`/`Y`, `None`
    /// for `State::Empty` (which holds no offsets).
    pub fn tail_offset(&self) -> Option<usize> {
        match self {
            State::Empty => None,
            State::N(n) => Some(n.tail_offset),
            State::Y(e) => e.tail_offset.map(NonZeroUsize::get),
        }
    }

    /// `true` if this state is confirmed archived and will never grow again.
    pub fn is_archived(&self) -> bool {
        match self {
            State::Empty => false,
            State::N(n) => n.ok,
            State::Y(e) => e.tail_offset.is_some(),
        }
    }

    /// Shift every offset held by this state down by `offset`, e.g. after
    /// the application drops an archived prefix of the buffer.
    ///
    /// # Safety
    /// `self` MUST satisfy [`State`]'s validity contract. For `State::N` and
    /// `State::Y`, `offset` MUST NOT exceed [`State::head_offset`], and
    /// shifted offsets MUST stay valid for the buffer the state refers to.
    /// `State::Empty` holds no offsets and is unchanged. Shifting an
    /// archived state by its own `tail_offset` is a contract violation (an
    /// offset would become 0): consume archived states instead of shifting
    /// them.
    pub unsafe fn offset(&mut self, offset: usize) {
        match self {
            State::Empty => {}
            State::N(n) => unsafe { n.offset(offset) },
            State::Y(e) => unsafe { e.offset(offset) },
        }
    }
}

impl NonEnvelop {
    /// Shift every offset held by this state down by `offset`.
    ///
    /// # Safety
    /// `self` MUST satisfy [`State`]'s validity contract, `offset` MUST NOT
    /// exceed `head_offset`, and shifted offsets MUST stay valid for the
    /// buffer the state refers to.
    pub unsafe fn offset(&mut self, offset: usize) {
        self.head_offset = sub(self.head_offset, offset);
        self.tail_offset = sub(self.tail_offset, offset);
    }
}

impl Envelop {
    /// Range of the committed payload — the prefix that cannot still become
    /// part of a delimiter slice: `Some(head..tail)` once payload contents
    /// are committed or the payload is archived non-empty; `None` for an
    /// empty payload and for a pending `Envelop` that has committed nothing
    /// yet.
    pub fn payload_range(&self) -> Option<core::ops::Range<usize>> {
        Some(self.payload_head_offset?.get()..self.payload_tail_offset?.get())
    }

    /// Shift every offset held by this state down by `offset`.
    ///
    /// # Safety
    /// `self` MUST satisfy [`State`]'s validity contract, `offset` MUST NOT
    /// exceed `head_offset`, and shifted offsets MUST stay valid for the
    /// buffer the state refers to. Shifting an archived state by its own
    /// `tail_offset` is a contract violation (an offset would become 0).
    pub unsafe fn offset(&mut self, offset: usize) {
        self.head_offset = sub(self.head_offset, offset);
        self.payload_head_offset = self.payload_head_offset.map(|x| nz(sub(x.get(), offset)));
        self.payload_tail_offset = self.payload_tail_offset.map(|x| nz(sub(x.get(), offset)));
        self.tail_offset = self.tail_offset.map(|x| nz(sub(x.get(), offset)));
    }
}

fn nz(x: usize) -> NonZeroUsize {
    NonZeroUsize::new(x).expect("SDAVE internal: offset MUST be non-zero")
}

fn sub(x: usize, offset: usize) -> usize {
    x.checked_sub(offset)
        .expect("SDAVE state offset MUST NOT underflow: offset exceeds a held offset")
}

/// Parses the segment of `s` starting at offset 0.
///
/// # Safety
/// SDAVE never owns the buffer; it can not confirm the buffer is monotonic
/// receiving. The caller MUST guarantee that every offset of the returned
/// state is only ever used with the same `s` (or a monotonic growth of it,
/// see [`parse_incremental`], or shifted via [`State::offset`] after buffer
/// compaction). Violating this contract yields unspecified states or panics
/// (but no memory unsafety: SDAVE never dereferences the buffer itself).
pub unsafe fn parse<T: Sized + PartialEq>(
    v: Variant,
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
) -> State {
    scan_segment::<T, true>(v, s, limiter_pairs, 0)
}

/// Like [`parse`], but with leaner scan paths that assume the limiter pair
/// set obeys the recommended invariants (see [`limiter_pairs_recommended`]).
/// When the contract holds, results coincide with [`parse`]; states produced
/// by the checked and unchecked fns are interoperable.
///
/// # Safety
/// In addition to [`parse`]'s buffer contract, the caller MUST guarantee
/// that `limiter_pairs` satisfies [`limiter_pairs_recommended`]. Violating
/// this contract yields unspecified states (but no memory unsafety).
pub unsafe fn parse_unchecked<T: Sized + PartialEq>(
    v: Variant,
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
) -> State {
    debug_assert!(
        limiter_pairs_recommended(limiter_pairs),
        "SDAVE contract: *_unchecked fns require a recommended limiter pair set"
    );
    scan_segment::<T, false>(v, s, limiter_pairs, 0)
}

/// Incrementally advances `state` over the (monotonically grown) buffer `s`.
///
/// - If `state` is archived (`NonEnvelop.ok` / `Envelop.tail_offset.is_some()`),
///   returns the state of the next segment, starting right after the archived
///   state's tail.
/// - Otherwise grows the pending state monotonically over the newly arrived
///   contents of `s`.
///
/// A returned state whose `head_offset` differs from `state`'s means `state`
/// is confirmed archived.
///
/// Pass [`State::Empty`] back only while it still describes the unconfirmed
/// segment at offset 0 (an `Empty` input re-parses from offset 0). When a
/// call on an archived state yields `State::Empty` — nothing behind the
/// archived state's tail yet — do NOT pass that `Empty` back over the now
/// non-empty `s`; keep the archived state and pass it again once more
/// contents arrive.
///
/// # Safety
/// The caller MUST guarantee that `state` satisfies [`State`]'s validity
/// contract, that `state` was derived using the same `v` and the same
/// ordered `limiter_pairs`, and that `s` is the same buffer, monotonically
/// grown (after buffer compaction, shift `state` with [`State::offset`]
/// before calling). Violating this contract yields unspecified states or
/// panics (but no memory unsafety: SDAVE never dereferences the buffer
/// itself).
pub unsafe fn parse_incremental<T: Sized + PartialEq>(
    v: Variant,
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
    state: &State,
) -> State {
    parse_incremental_impl::<T, true>(v, s, limiter_pairs, state)
}

/// Like [`parse_incremental`], but with leaner scan paths that assume the
/// limiter pair set obeys the recommended invariants — see
/// [`parse_unchecked`] and [`limiter_pairs_recommended`].
///
/// # Safety
/// In addition to [`parse_incremental`]'s buffer contract, the caller MUST
/// guarantee that `limiter_pairs` satisfies [`limiter_pairs_recommended`].
/// Violating this contract yields unspecified states (but no memory
/// unsafety).
pub unsafe fn parse_incremental_unchecked<T: Sized + PartialEq>(
    v: Variant,
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
    state: &State,
) -> State {
    debug_assert!(
        limiter_pairs_recommended(limiter_pairs),
        "SDAVE contract: *_unchecked fns require a recommended limiter pair set"
    );
    parse_incremental_impl::<T, false>(v, s, limiter_pairs, state)
}

fn parse_incremental_impl<T: Sized + PartialEq, const CHECKED: bool>(
    v: Variant,
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
    state: &State,
) -> State {
    match state {
        State::Empty => scan_segment::<T, CHECKED>(v, s, limiter_pairs, 0),
        State::N(ne) => {
            if ne.ok {
                scan_segment::<T, CHECKED>(v, s, limiter_pairs, ne.tail_offset)
            } else {
                match text_scan::<T, CHECKED>(s, limiter_pairs, ne.tail_offset) {
                    TextScan::Envelope {
                        head,
                        limiter_end,
                        pair,
                        count,
                    } => scan_envelop(v, s, limiter_pairs, head, limiter_end, pair, count),
                    TextScan::Potential { tail } => State::N(NonEnvelop {
                        head_offset: ne.head_offset,
                        tail_offset: tail,
                        ok: false,
                    }),
                    TextScan::End => State::N(NonEnvelop {
                        head_offset: ne.head_offset,
                        tail_offset: s.len(),
                        ok: false,
                    }),
                }
            }
        }
        State::Y(e) => {
            if let Some(tail) = e.tail_offset {
                scan_segment::<T, CHECKED>(v, s, limiter_pairs, tail.get())
            } else {
                let payload_head = e
                    .payload_head_offset
                    .expect(
                        "SDAVE contract: a pending Envelop MUST have a well defined limiter slice",
                    )
                    .get();
                let (pair, count) =
                    re_derive_limiter::<T, CHECKED>(s, limiter_pairs, e.head_offset, payload_head)
                        .expect(
                            "SDAVE contract: limiter slice MUST re-derive from the same buffer the \
                             state was parsed from",
                        );
                let scan_from = e
                    .payload_tail_offset
                    .map_or(payload_head, NonZeroUsize::get);
                match delim_scan(v, s, scan_from, limiter_pairs[pair].delimiter, count) {
                    DelimScan::Confirmed {
                        payload_end,
                        envelop_end,
                    } => archived_envelop(e.head_offset, payload_head, payload_end, envelop_end),
                    DelimScan::Pending { committed } => State::Y(Envelop {
                        head_offset: e.head_offset,
                        payload_head_offset: Some(nz(payload_head)),
                        payload_tail_offset: if committed > payload_head {
                            Some(nz(committed))
                        } else {
                            None
                        },
                        tail_offset: None,
                    }),
                }
            }
        }
    }
}

/// Parses the segment starting at `head`.
fn scan_segment<T: Sized + PartialEq, const CHECKED: bool>(
    v: Variant,
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
    head: usize,
) -> State {
    match text_scan::<T, CHECKED>(s, limiter_pairs, head) {
        TextScan::Envelope {
            head: limiter_head,
            limiter_end,
            pair,
            count,
        } => {
            if limiter_head > head {
                State::N(NonEnvelop {
                    head_offset: head,
                    tail_offset: limiter_head,
                    ok: true,
                })
            } else {
                scan_envelop(v, s, limiter_pairs, limiter_head, limiter_end, pair, count)
            }
        }
        TextScan::Potential { tail } => {
            if tail > head {
                State::N(NonEnvelop {
                    head_offset: head,
                    tail_offset: tail,
                    ok: false,
                })
            } else {
                State::Empty
            }
        }
        TextScan::End => {
            if s.len() > head {
                State::N(NonEnvelop {
                    head_offset: head,
                    tail_offset: s.len(),
                    ok: false,
                })
            } else {
                State::Empty
            }
        }
    }
}

/// Result of scanning for a limiter slice.
enum TextScan {
    /// A confirmed limiter slice `s[head..limiter_end]` of `count` repeats of
    /// `limiter_pairs[pair].limiter`.
    Envelope {
        head: usize,
        limiter_end: usize,
        pair: usize,
        count: usize,
    },
    /// `s[tail..]` is a potential limiter slice (run of a limiter reaching
    /// the end of the buffer; repeats not well defined yet).
    Potential { tail: usize },
    /// No (potential) limiter slice up to the end of the buffer.
    End,
}

/// Scans `s[from..]` for the first (potential) limiter slice.
///
/// With `CHECKED == false` the recommended-set invariants are assumed: at
/// most one pair can match at a position (no prefix-comparable limiters), so
/// the first pair matching at a position owns it, and a run shorter than
/// `least_repeat` is skipped wholesale — a limiter slice of the same pair
/// cannot confirm inside it (the region only shrinks) and the contract
/// forbids any limiter interfering inside the run.
fn text_scan<T: Sized + PartialEq, const CHECKED: bool>(
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
    from: usize,
) -> TextScan {
    let mut i = from;
    while i < s.len() {
        let mut skip_run_to = None;
        for (pi, p) in limiter_pairs.iter().enumerate() {
            let ll = p.limiter.len();
            if ll == 0 {
                continue;
            }
            if !s[i..].starts_with(p.limiter) {
                // A partial match of this pair's limiter at the end of the
                // buffer is a potential limiter slice; an earlier pair's
                // potential defers later pairs, so that a streaming parse
                // never commits to a later pair while an earlier pair's
                // limiter is still in flight (the first pair in the input
                // slice wins).
                if s.len() - i < ll && s[i..] == p.limiter[..s.len() - i] {
                    return TextScan::Potential { tail: i };
                }
                continue;
            }
            // Maximal run of `limiter` repeats starting at `i`.
            // No arithmetic overflow: `count` is bounded by an actual run in
            // the buffer, so `(count + 1) * ll <= s.len() + ll`.
            let mut count = 0;
            while i + (count + 1) * ll <= s.len()
                && s[i + count * ll..i + (count + 1) * ll] == p.limiter[..]
            {
                count += 1;
            }
            let end = i + count * ll;
            if end == s.len() {
                // Run reaches the end of the buffer: potential limiter slice.
                return TextScan::Potential { tail: i };
            }
            if s.len() - end < ll && s[end..] == p.limiter[..s.len() - end] {
                // Run terminated by a partial limiter at the end of the
                // buffer: repeats not well defined yet.
                return TextScan::Potential { tail: i };
            }
            if count >= p.least_repeat.get() {
                return TextScan::Envelope {
                    head: i,
                    limiter_end: end,
                    pair: pi,
                    count,
                };
            }
            if !CHECKED {
                // First-hit-owns: a short run is skipped wholesale.
                skip_run_to = Some(end);
                break;
            }
            // `count < least_repeat`: not a limiter slice for this pair;
            // other pairs may still match at `i`.
        }
        i = skip_run_to.unwrap_or(i + 1);
    }
    // No (potential) limiter slice found; but a partial limiter at the end of
    // the buffer is a potential limiter slice: hold it back.
    let mut hold = 0;
    for p in limiter_pairs {
        let ll = p.limiter.len();
        if ll == 0 {
            continue;
        }
        let max = (ll - 1).min(s.len() - from);
        for h in (hold + 1..=max).rev() {
            if s[s.len() - h..] == p.limiter[..h] {
                hold = h;
                break;
            }
        }
    }
    if hold > 0 {
        TextScan::Potential {
            tail: s.len() - hold,
        }
    } else {
        TextScan::End
    }
}

/// Builds the `Envelop` state opened by a confirmed limiter slice
/// `s[head..limiter_end]` of `count` repeats of `limiter_pairs[pair].limiter`.
fn scan_envelop<T: Sized + PartialEq>(
    v: Variant,
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
    head: usize,
    limiter_end: usize,
    pair: usize,
    count: usize,
) -> State {
    match delim_scan(v, s, limiter_end, limiter_pairs[pair].delimiter, count) {
        DelimScan::Confirmed {
            payload_end,
            envelop_end,
        } => archived_envelop(head, limiter_end, payload_end, envelop_end),
        DelimScan::Pending { committed } => State::Y(Envelop {
            head_offset: head,
            payload_head_offset: Some(nz(limiter_end)),
            payload_tail_offset: if committed > limiter_end {
                Some(nz(committed))
            } else {
                None
            },
            tail_offset: None,
        }),
    }
}

/// Builds an archived `Envelop`; an empty payload (`payload_head ==
/// payload_end`, e.g. `^^~~`) yields `None` payload offsets.
fn archived_envelop(
    head: usize,
    payload_head: usize,
    payload_end: usize,
    envelop_end: usize,
) -> State {
    if payload_end > payload_head {
        State::Y(Envelop {
            head_offset: head,
            payload_head_offset: Some(nz(payload_head)),
            payload_tail_offset: Some(nz(payload_end)),
            tail_offset: Some(nz(envelop_end)),
        })
    } else {
        State::Y(Envelop {
            head_offset: head,
            payload_head_offset: None,
            payload_tail_offset: None,
            tail_offset: Some(nz(envelop_end)),
        })
    }
}

/// Re-derives which limiter pair opened a pending `Envelop`, and how many
/// times its limiter repeats: the first pair whose maximal `limiter` run at
/// `head` is exactly `s[head..limiter_end]` with `count >= least_repeat`.
///
/// With `CHECKED == false` the contract guarantees exactly one pair could
/// have opened the slice, so it is selected without re-validation.
fn re_derive_limiter<T: Sized + PartialEq, const CHECKED: bool>(
    s: &[T],
    limiter_pairs: &[LimiterPair<T>],
    head: usize,
    limiter_end: usize,
) -> Option<(usize, usize)> {
    let span = limiter_end.checked_sub(head)?;
    if !CHECKED {
        return limiter_pairs.iter().enumerate().find_map(|(pi, p)| {
            let ll = p.limiter.len();
            (ll != 0 && span % ll == 0 && s[head..].starts_with(p.limiter))
                .then_some((pi, span / ll))
        });
    }
    for (pi, p) in limiter_pairs.iter().enumerate() {
        let ll = p.limiter.len();
        if ll == 0 || span % ll != 0 {
            continue;
        }
        let count = span / ll;
        if count < p.least_repeat.get() {
            continue;
        }
        // `s[head..limiter_end]` MUST be `limiter` repeated `count` times...
        if (0..count).any(|r| s[head + r * ll..head + (r + 1) * ll] != p.limiter[..]) {
            continue;
        }
        // ...and the run MUST be maximal (terminated by a non-limiter `T`).
        if limiter_end < s.len() && s[limiter_end..].starts_with(p.limiter) {
            continue;
        }
        return Some((pi, count));
    }
    None
}

/// Result of scanning for a delimiter slice.
enum DelimScan {
    /// Delimiter slice confirmed: `s[payload_end..envelop_end]`.
    Confirmed {
        payload_end: usize,
        envelop_end: usize,
    },
    /// No confirmed delimiter slice yet; `committed` is the largest offset
    /// such that the payload region ends neither inside, nor with a potential
    /// delimiter slice.
    Pending { committed: usize },
}

/// Scans `s[from..]` for the delimiter slice: `delimiter` repeated `count`
/// times.
fn delim_scan<T: Sized + PartialEq>(
    v: Variant,
    s: &[T],
    from: usize,
    delimiter: &[T],
    count: usize,
) -> DelimScan {
    let dl = delimiter.len();
    // No arithmetic overflow: `dlen` units of an actual delimiter slice must
    // fit the buffer for any match to happen.
    let dlen = dl * count;
    if dlen == 0 {
        // Degenerate pair (empty delimiter): the delimiter slice matches
        // right behind the limiter slice.
        return DelimScan::Confirmed {
            payload_end: from,
            envelop_end: from,
        };
    }
    let matches_at =
        |pos: usize| (0..count).all(|r| s[pos + r * dl..pos + (r + 1) * dl] == delimiter[..]);
    let mut pos = from;
    while pos + dlen <= s.len() {
        if matches_at(pos) {
            match v {
                // The every first full matched delimiter slice is just the
                // delimiter slice.
                Variant::V1 => {
                    return DelimScan::Confirmed {
                        payload_end: pos,
                        envelop_end: pos + dlen,
                    };
                }
                Variant::V2 => {
                    let rest = &s[pos + dlen..];
                    if rest.is_empty() || (rest.len() < dl && rest == &delimiter[..rest.len()]) {
                        // Full match at the end of the buffer, or followed by
                        // a partial delimiter: cannot yet tell whether the
                        // delimiter run continues — delimiter slice
                        // confirmation latency.
                        return DelimScan::Pending { committed: pos };
                    }
                    if !rest.starts_with(delimiter) {
                        // The every first full matched delimiter slice
                        // FOLLOWED by a non-delimiter is the delimiter slice.
                        return DelimScan::Confirmed {
                            payload_end: pos,
                            envelop_end: pos + dlen,
                        };
                    }
                    // The delimiter run continues; this repeat belongs to the
                    // payload.
                }
            }
        }
        pos += 1;
    }
    DelimScan::Pending {
        committed: s.len() - proper_delimiter_prefix_suffix(s, from, delimiter, dlen),
    }
}

/// Length of the longest suffix of `s[from..]` that is a proper prefix of
/// `delimiter` repeated `count` times (`dlen == delimiter.len() * count`):
/// a potential delimiter slice held back from the committed payload.
fn proper_delimiter_prefix_suffix<T: Sized + PartialEq>(
    s: &[T],
    from: usize,
    delimiter: &[T],
    dlen: usize,
) -> usize {
    let dl = delimiter.len();
    let max = (dlen - 1).min(s.len() - from);
    (1..=max)
        .rev()
        .find(|&h| {
            s[s.len() - h..]
                .iter()
                .enumerate()
                .all(|(i, t)| *t == delimiter[i % dl])
        })
        .unwrap_or(0)
}
