//! Serialization side of the SDAVE codec.
//!
//! A [`Serialize`] implementation turns `self` into one SDAVE Envelop and
//! appends its slices to a `T` buffer. Because SDAVE core needs the whole
//! payload up front (to find the longest delimiter run inside it and grow the
//! repeats past it), a composite serializes its children *first*, then frames
//! the concatenation of the children's slices.

use core::{alloc::Allocator, num::NonZeroUsize};

use alloc::vec::Vec;

use sdave::{LimiterPair, Offset, Variant, can_serialize};

use crate::{Error, NamePolicy, TypeName, render_type};

/// An Envelop's appended `(limiter, delimiter)` offset ranges.
pub type Frame = ((Offset, Offset), (Offset, Offset));

/// The `name` prefix of a `Signature`: the bytes the caller has already appended
/// to `buffer[head_offset..tail_offset]` which name the value about to be
/// serialized.
///
/// The caller MUST ensure `tail_offset == buffer.len()` before calling
/// [`Serialize::serialize_typed`].
pub struct Name {
    /// Head offset of the name slice in the buffer.
    pub head_offset: Offset,
    /// Tail offset of the name slice in the buffer. MUST equal `buffer.len()`.
    pub tail_offset: Offset,
}

/// The `Signature` NonEnvelop written before a typed value: the caller-written
/// `name`, then the `separator`, then the `type_describer`.
pub struct Signature<'a, T> {
    /// The `name` prefix already appended to the buffer by the caller.
    pub name: Name,
    /// The bytes separating the name from the type describer (e.g. `b": "`).
    pub separator: &'a [T],
    /// The bytes describing the type of the value.
    pub type_describer: &'a [T],
}

/// How a [`Serialize`] implementation picks a [`LimiterPair`] among the candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strategy {
    /// Use the first candidate (in [`Vec`] order) that can frame the payload.
    FirstMatch,
    /// Start scanning at the wrapped index, then continue cyclically.
    RoundRobin(u16),
    /// Use the candidate with the fewest repeats (ties broken by the lowest index).
    LeastRepeat,
}

/// Serialize a value into a SDAVE buffer.
///
/// `T` is the basic data unit of the buffer, `A` the allocator used for any
/// intermediate allocation.
pub trait Serialize<T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    /// The length of the implementor's [`Serialize::Slices`] array.
    const SLICES: usize;
    /// MUST be `[Option<(Offset, Offset)>; n]` where `n` is the greatest possible
    /// number of slices [`Serialize::serialize`] can produce.
    ///
    /// The `IntoIterator` bound lets a composite copy its fields' slices without
    /// naming their concrete array types.
    type Slices: IntoIterator<Item = Option<(Offset, Offset)>>;
    /// MUST be `[Option<(Offset, Offset)>; n]` where `n` is the greatest possible
    /// number of slices [`Serialize::serialize_typed`] can produce.
    ///
    /// Typically equal to the greatest possible slices of `serialize` plus 3 (the
    /// outer limiter slice, the signature NonEnvelop, the outer delimiter slice).
    ///
    /// The `IntoIterator` bound lets a `dyn Trait` wrapper flatten the result
    /// without naming its concrete array type.
    type SlicesTyped: IntoIterator<Item = Option<(Offset, Offset)>>;
    /// Serialize `self` into `buffer`, from its original `len()`, by extending
    /// `buffer` with the serialize result.
    ///
    /// Return a slice containing `Option<(Offset, Offset)>` pointing back to
    /// `buffer`, which you MUST use to flatten the unflattened serialize result
    /// from `buffer`. Reading the returned slices in array order yields the
    /// logical byte stream of a single valid SDAVE Envelop.
    ///
    /// You don't need to flatten the serialize result if you only need the
    /// serialize result to be framed by SDAVE, because [`sdave::can_serialize`]
    /// supports serializing unflattened payload.
    ///
    /// `N` is the [`NamePolicy`] used to render every field/concrete-type
    /// signature written by this call.
    ///
    /// # Safety
    /// Same contract as [`sdave::can_serialize`]: the tail of `buffer` MUST be a
    /// clean item boundary (its head, or the tail of an archived item).
    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<T, A>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, T, A>],
    ) -> Result<Self::Slices, Error>;
    /// Typically used to serialize a value when you don't know the exact
    /// type/variant of it, e.g. an enum or a `dyn Trait` field.
    ///
    /// Assume the caller has serialized the name in
    /// `buffer[name.head_offset..name.tail_offset]`, assert
    /// `name.tail_offset == buffer.len()`, then push a separator if the name is
    /// non-empty, push its type describer (rendered through `N`), then serialize
    /// its value.
    ///
    /// # Safety
    /// Same contract as [`sdave::can_serialize`].
    unsafe fn serialize_typed<N: NamePolicy>(
        &self,
        buffer: &mut Vec<T, A>,
        name: Name,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, T, A>],
    ) -> Result<Self::SlicesTyped, Error>;
}

/// Pick the index of a limiter pair able to frame `payload` given `buffer` (the
/// already-settled tail before the Envelop) and `strategy`.
///
/// Returns the index into `limiter_pairs` and the minimal working repeat.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`]: the tail of `buffer` MUST be a
/// clean item boundary.
pub unsafe fn select_limiter_pair<'l, T, A>(
    buffer: &[T],
    payload: &[Option<&[T]>],
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'l, T, A>],
) -> Result<(usize, NonZeroUsize), Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let len = limiter_pairs.len();
    if len == 0 {
        return Err(Error::NoSerializableLimiterPair);
    }
    let start = match strategy {
        Strategy::RoundRobin(start) => (start as usize) % len,
        _ => 0,
    };
    // `can_serialize`'s `Slices` helpers require a non-empty leading slice, so an
    // empty payload is handled directly.
    let empty_payload = payload
        .iter()
        .all(|slice| slice.is_none_or(|slice| slice.is_empty()));
    let mut best: Option<(usize, NonZeroUsize)> = None;
    for k in 0..len {
        let i = (start + k) % len;
        let repeats = if empty_payload {
            let limiter_pair = &limiter_pairs[i];
            match limiter_pair.least_repeat() {
                Some(least_repeat)
                    if !limiter_pair.limiter().is_empty()
                        && !limiter_pair.delimiter().is_empty()
                        && !buffer.ends_with(limiter_pair.limiter()) =>
                {
                    Some(least_repeat)
                }
                _ => None,
            }
        } else {
            unsafe { can_serialize(buffer, payload, variant, &mut limiter_pairs[i]) }
        };
        if let Some(repeats) = repeats {
            match strategy {
                Strategy::LeastRepeat => {
                    if best.is_none_or(|(_, current)| current.get() > repeats.get()) {
                        best = Some((i, repeats));
                    }
                }
                _ => return Ok((i, repeats)),
            }
        }
    }
    best.ok_or(Error::NoSerializableLimiterPair)
}

/// Append `repeats` copies of the limiter slice followed by `repeats` copies of
/// the delimiter slice, returning `(limiter_slice, delimiter_slice)` offsets.
///
/// The caller is responsible for placing the payload slices into its returned
/// slice array between the two returned slices.
pub fn append_frame<T, A>(
    buffer: &mut Vec<T, A>,
    limiter_pair: &LimiterPair<'_, T, A>,
    repeats: NonZeroUsize,
) -> Frame
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let limiter_head = Offset::new(buffer.len());
    for _ in 0..repeats.get() {
        buffer.extend_from_slice(limiter_pair.limiter());
    }
    let limiter_tail = Offset::new(buffer.len());
    for _ in 0..repeats.get() {
        buffer.extend_from_slice(limiter_pair.delimiter());
    }
    let delimiter_tail = Offset::new(buffer.len());
    ((limiter_head, limiter_tail), (limiter_tail, delimiter_tail))
}

/// Frame the logically ordered sub-range `range` of `slices` (offsets into
/// `buffer`; `None` entries are padding and are skipped) as one Envelop,
/// appending the limiter/delimiter repeats after them. Returns the appended
/// `(limiter, delimiter)` offset pairs.
///
/// `slices` MUST already be written into `buffer`, and the frame is chosen as if
/// the Envelop's predecessor tail were `buffer[..payload_head]`.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`]: the tail of `buffer[..payload_head]`
/// MUST be a clean item boundary.
pub unsafe fn frame_payload<T, A, const N: usize>(
    buffer: &mut Vec<T, A>,
    payload_head: usize,
    slices: &[Option<(Offset, Offset)>; N],
    range: core::ops::Range<usize>,
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, T, A>],
) -> Result<Frame, Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let (index, repeats) = {
        let payload: [Option<&[T]>; N] =
            core::array::from_fn(|i| slices[i].map(|(head, tail)| &buffer[head.get()..tail.get()]));
        let payload_slice: &[Option<&[T]>] = &payload;
        unsafe {
            select_limiter_pair(
                &buffer[..payload_head],
                &payload_slice[range],
                variant,
                strategy,
                limiter_pairs,
            )?
        }
    };
    let (limiter, delimiter) = append_frame(buffer, &limiter_pairs[index], repeats);
    Ok((limiter, delimiter))
}

/// Frame the already-appended region `buffer[payload_head..]` as the payload of a
/// single Envelop, appending its limiter/delimiter repeats.
///
/// Returns the 3 logical slices `[limiter, payload, delimiter]`. The payload tail
/// is captured before the frame is appended, so it still points at the payload.
///
/// This is the one-region shorthand for [`frame_payload`]: it frames
/// `buffer[payload_head..]` as if it were the sole slice of a single Envelop.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`]: the tail of `buffer[..payload_head]`
/// MUST be a clean item boundary.
pub unsafe fn frame_region<T, A>(
    buffer: &mut Vec<T, A>,
    payload_head: usize,
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, T, A>],
) -> Result<[Option<(Offset, Offset)>; 3], Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let payload = (Offset::new(payload_head), Offset::new(buffer.len()));
    let (limiter, delimiter) = unsafe {
        frame_payload(
            buffer,
            payload_head,
            &[Some(payload)],
            0..1,
            variant,
            strategy,
            limiter_pairs,
        )?
    };
    Ok([Some(limiter), Some(payload), Some(delimiter)])
}

/// Frame a single `payload` slice: append it (unflattened), then frame it as one
/// Envelop, returning the 3 logical slices `[limiter, payload, delimiter]`.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`].
pub unsafe fn frame_slice<T, A>(
    buffer: &mut Vec<T, A>,
    payload: &[T],
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, T, A>],
) -> Result<[Option<(Offset, Offset)>; 3], Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let payload_head = buffer.len();
    buffer.extend_from_slice(payload);
    unsafe { frame_region(buffer, payload_head, variant, strategy, limiter_pairs) }
}

/// Append a [`Signature`] NonEnvelop to `buffer`: the caller-written `name`,
/// then `separator` (only when the name is non-empty), then `type_describer`.
///
/// Returns the `(head, tail)` offsets of the appended signature.
///
/// # Errors
/// Returns [`Error::UnexpectedItem`] when `signature.name.tail_offset` is not
/// `buffer.len()` (the name was not appended immediately before this call), or when
/// `signature.name.head_offset` is past `tail_offset` (the range does not name an
/// already-appended slice, so the returned offsets would not be a valid slice).
pub fn write_signature<T, A>(
    buffer: &mut Vec<T, A>,
    signature: Signature<'_, T>,
) -> Result<(Offset, Offset), Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let Signature {
        name,
        separator,
        type_describer,
    } = signature;
    if name.tail_offset.get() != buffer.len() || name.head_offset.get() > name.tail_offset.get() {
        return Err(Error::UnexpectedItem);
    }
    if name.tail_offset.get() > name.head_offset.get() {
        buffer.extend_from_slice(separator);
    }
    buffer.extend_from_slice(type_describer);
    Ok((name.head_offset, Offset::new(buffer.len())))
}

/// The bytes separating a typed value's `name` from its rendered type in a
/// [`Signature`], e.g. `b": "` for a `u8` buffer (a `::` path can appear *inside*
/// the rendered type, so the separator MUST be unambiguous).
pub const TYPE_SEPARATOR: &[u8] = b": ";

/// The typed [`Signature`] NonEnvelop `name ++ TYPE_SEPARATOR ++ shorten(T::FULL)`
/// of a *leaf* type `T`.
///
/// A leaf type is one whose [`TypeName::render`] is the trait's default, i.e. it
/// has no generic arguments to compose, so its `type_describer` is a slice of its
/// own `&'static str` full name and needs no intermediate buffer (unlike
/// [`write_type_signature`], which renders through a `Vec`).
pub fn leaf_signature<N, T>(name: Name) -> Signature<'static, u8>
where
    N: NamePolicy,
    T: TypeName + ?Sized,
{
    Signature {
        name,
        separator: TYPE_SEPARATOR,
        type_describer: <N as NamePolicy>::shorten(<T as TypeName>::FULL).as_bytes(),
    }
}

/// Write the typed-value `Signature` NonEnvelop `name ++ TYPE_SEPARATOR ++
/// S::render::<N>()` to `buffer`, returning its `(head, tail)` offsets.
///
/// The caller has already appended the `name`, and `S` is the value's type,
/// rendered through `N`. This is the typed-signature shorthand shared by the
/// codec's own typed serializers and the `sdave-derive` codegen.
///
/// # Errors
/// Returns [`Error::UnexpectedItem`] when `name.tail_offset` is not
/// `buffer.len()`, i.e. the name was not appended immediately before this call.
pub fn write_type_signature<N, S, A>(
    buffer: &mut Vec<u8, A>,
    name: Name,
) -> Result<(Offset, Offset), Error>
where
    N: NamePolicy,
    S: TypeName + ?Sized,
    A: Allocator + Clone,
{
    let describer = render_type::<N, S, _>(buffer.allocator().clone());
    write_signature(
        buffer,
        Signature {
            name,
            separator: TYPE_SEPARATOR,
            type_describer: describer.as_slice(),
        },
    )
}

/// Like [`frame_slice`], but wraps the value in an additional outer Envelop
/// prefixed by the `Signature` NonEnvelop `name + separator + type_describer`.
///
/// Returns the 6 logical slices
/// `[outer limiter, signature, inner limiter, payload, inner delimiter, outer delimiter]`.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`].
pub unsafe fn frame_slice_typed<T, A>(
    buffer: &mut Vec<T, A>,
    signature: Signature<'_, T>,
    payload: &[T],
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, T, A>],
) -> Result<[Option<(Offset, Offset)>; 6], Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let signature = write_signature(buffer, signature)?;
    let inner = unsafe { frame_slice(buffer, payload, variant, strategy, limiter_pairs)? };
    let mut out: [Option<(Offset, Offset)>; 6] = [None; 6];
    unsafe {
        frame_typed_envelop(
            buffer,
            signature,
            inner,
            variant,
            strategy,
            limiter_pairs,
            &mut out,
        )?;
    }
    Ok(out)
}

/// Flatten `typed_slices` — the result of a [`Serialize::serialize_typed`] call
/// with an empty [`Name`] whose slices start at `start` in `buffer` — into a
/// contiguous single Envelop, returning its `[limiter, payload, delimiter]`
/// slices.
///
/// The first present slice is the outer limiter and the last present slice is the
/// outer delimiter; every slice in between is part of the payload. The flattening
/// itself is delegated to [`flatten_in_place`].
///
/// # Errors
/// Returns [`Error::UnexpectedItem`] when `typed_slices` yields no slice.
///
/// # Safety
/// `typed_slices` MUST be the slice set of a successful `serialize_typed` call
/// that started at `start`.
pub unsafe fn flatten_typed_envelop<T, A, I>(
    buffer: &mut Vec<T, A>,
    start: usize,
    typed_slices: I,
) -> Result<[Option<(Offset, Offset)>; 3], Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
    I: IntoIterator<Item = Option<(Offset, Offset)>>,
{
    /// Records the length of the first and last present (`Some`) slice, so the
    /// limiter/payload/delimiter boundaries can be recovered while the slices are
    /// consumed exactly once.
    struct TrackBoundaries<I> {
        inner: I,
        first: Option<usize>,
        last: usize,
    }

    impl<I> Iterator for TrackBoundaries<I>
    where
        I: Iterator<Item = Option<(Offset, Offset)>>,
    {
        type Item = Option<(Offset, Offset)>;

        fn next(&mut self) -> Option<Self::Item> {
            let item = self.inner.next()?;
            if let Some((head, tail)) = item {
                let len = tail.get() - head.get();
                if self.first.is_none() {
                    self.first = Some(len);
                }
                self.last = len;
            }
            Some(item)
        }
    }

    let mut boundaries = TrackBoundaries {
        inner: typed_slices.into_iter(),
        first: None,
        last: 0,
    };
    let flat_len = unsafe { flatten_in_place(buffer, start, &mut boundaries) };
    let Some(limiter_len) = boundaries.first else {
        return Err(Error::UnexpectedItem);
    };
    let delimiter_len = boundaries.last;
    let limiter_end = start + limiter_len;
    let payload_end = start + flat_len - delimiter_len;
    Ok([
        Some((Offset::new(start), Offset::new(limiter_end))),
        Some((Offset::new(limiter_end), Offset::new(payload_end))),
        Some((Offset::new(payload_end), Offset::new(start + flat_len))),
    ])
}

/// Flatten the logical slice sequence `slices` in place, moving the concatenation
/// down to `start`, discarding the unflattened bytes, and returning the flattened
/// length.
///
/// Every present (`Some`) slice is concatenated in iterator order. This is the one
/// flattening primitive shared by the iterable, `dyn Trait` and generic-parameter
/// code paths.
///
/// `start` MUST be the offset at which the bytes described by `slices` begin.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`]: `start` MUST be a clean flattening
/// anchor and every slice offset MUST be within `buffer`.
pub unsafe fn flatten_in_place<T, A, I>(buffer: &mut Vec<T, A>, start: usize, slices: I) -> usize
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
    I: IntoIterator<Item = Option<(Offset, Offset)>>,
{
    let flat_start = buffer.len();
    for (head, tail) in slices.into_iter().flatten() {
        buffer.extend_from_within(head.get()..tail.get());
    }
    let flat_len = buffer.len() - flat_start;
    // Move the flattened copy down over the unflattened region `[start..flat_start)`
    // and discard the rest. `swap`/`truncate` need neither `T: Copy` (unlike
    // `copy_within`) nor the nightly allocator bound (unlike `drain`).
    for i in 0..flat_len {
        buffer.swap(start + i, flat_start + i);
    }
    buffer.truncate(start + flat_len);
    flat_len
}

/// Wrap an already-written `Signature` NonEnvelop and the `inner` logical slices
/// of a value Envelop as one outer typed Envelop, scattering the result into
/// `out`.
///
/// `out` receives `[outer limiter, signature, inner…, outer delimiter]`. The
/// settled tail before the Envelop is `buffer[..signature.0]`.
///
/// Every typed-framing path (`frame_slice_typed` and the `sdave-derive` codegen)
/// funnels through here, so the layout lives in exactly one place.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`], and `out` MUST hold at least
/// `inner.into_iter().count() + 3` elements.
pub unsafe fn frame_typed_envelop<T, A, I, const N: usize>(
    buffer: &mut Vec<T, A>,
    signature: (Offset, Offset),
    inner: I,
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, T, A>],
    out: &mut [Option<(Offset, Offset)>; N],
) -> Result<(), Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
    I: IntoIterator<Item = Option<(Offset, Offset)>>,
{
    out[1] = Some(signature);
    let mut cursor = 2;
    for slot in inner {
        out[cursor] = slot;
        cursor += 1;
    }
    let (limiter, delimiter) = unsafe {
        frame_payload(
            buffer,
            signature.0.get(),
            out,
            1..cursor,
            variant,
            strategy,
            limiter_pairs,
        )?
    };
    out[0] = Some(limiter);
    out[cursor] = Some(delimiter);
    Ok(())
}

/// The `serialize_typed` body shared by every *generic* (flattened) impl: write
/// the `name: type` signature NonEnvelop, serialize `value` through its own
/// `serialize`, then wrap both in one outer Envelop and scatter the result into
/// `out`.
///
/// The 6 slots are exactly enough for the outer limiter, the signature, a single
/// three-slice Envelop and the outer delimiter, which is the shape every generic
/// impl's `serialize` reports.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`], and `value`'s `serialize` MUST yield
/// at most 3 present-or-padded slices (i.e. one Envelop), so `out` can hold them.
pub unsafe fn frame_typed_generic<N, S, A>(
    buffer: &mut Vec<u8, A>,
    name: Name,
    value: &S,
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, u8, A>],
    out: &mut [Option<(Offset, Offset)>; 6],
) -> Result<(), Error>
where
    N: NamePolicy,
    S: Serialize<u8, A> + TypeName + ?Sized,
    A: Allocator + Clone,
{
    let signature = write_type_signature::<N, S, _>(buffer, name)?;
    let inner = unsafe {
        <S as Serialize<u8, A>>::serialize::<N>(value, buffer, variant, strategy, limiter_pairs)?
    };
    unsafe {
        frame_typed_envelop(
            buffer,
            signature,
            inner,
            variant,
            strategy,
            limiter_pairs,
            out,
        )
    }
}
