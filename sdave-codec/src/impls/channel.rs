//! Deframing helpers: everything that reads a buffer back.
//!
//! The single-Envelop shorthand, the mixed-item channel, the iterable element
//! ranges and the [`FlatDeserializer`] builder. Every codec deserializer is built
//! from these, so the "what is one item" rules live in exactly one place.
//!
//! # Storage recycling
//!
//! A [`FlatDeserializer`] owns three `Vec`s (`archived_boundaries`, `repeat_map`,
//! `limiter_pairs`). [`with_deserializer`] lends those very instances to a child
//! deserializer over a sub-slice and takes them back afterwards, so a nested
//! deserialization costs no allocation: only the items a value *returns* to the
//! caller (a `Vec`, a `Box`, a `String`, …) are allocated.

use core::alloc::Allocator;

use alloc::vec::Vec;

use sdave::{
    FlatDeserializer, FlatDeserializerState, InternalState, Item, LimiterPair, Offset, Variant,
    update_internal_cache,
};

use crate::{Error, deserialize::Deserialize, name::NamePolicy};

/// One-shot: build a [`FlatDeserializer`] over `buffer` and deserialize one value
/// from it.
///
/// This is the convenience entry point for a caller that owns the whole buffer;
/// it is what the tests use. A deserializer that already has a
/// [`FlatDeserializer`] (every nested call) uses
/// [`Deserialize::deserialize`](crate::Deserialize::deserialize) directly, so the
/// intermediate instances are recycled rather than built.
///
/// The contract of the parameters is the one documented on
/// [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub fn deserialize_one<N, D, A>(
    buffer: &[u8],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'_, u8, A>],
    final_buffer: Option<Offset>,
    allocator: A,
) -> Result<<D as Deserialize<u8, A>>::Type, Error>
where
    N: NamePolicy,
    D: Deserialize<u8, A>,
    A: Allocator + Clone,
{
    let mut deserializer = flat_deserializer(buffer, variant, limiter_pairs, &allocator)?;
    <D as Deserialize<u8, A>>::deserialize::<N>(&mut deserializer, final_buffer, allocator)
}

/// Drive the single Envelop `deserializer` is positioned on and return its payload
/// slice.
///
/// `deserializer` MUST start at an Envelop; anything after that Envelop is not
/// examined (walk a whole channel with `collect_items`/`FlatDeserializer::iter`
/// instead). The codec's own recursive calls always hand over one exact item, so
/// the leniency only shows for a caller that hands over a longer buffer.
///
/// # Errors
/// Returns [`Error::UnconfirmedDelimiter`] for an unconfirmed `Variant::V2`
/// Envelop, and [`Error::UnexpectedItem`] when the buffer does not start with an
/// Envelop, or starts with a malformed one.
///
/// # Safety
/// Same contract as the `final_buffer` parameter of
/// [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub unsafe fn envelop_payload<'a, 'l, T, A>(
    deserializer: &mut FlatDeserializer<'a, 'l, T, A>,
    final_buffer: Option<Offset>,
) -> Result<&'a [T], Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let buffer = deserializer.buffer();
    let is_final = match final_buffer {
        Some(offset) => {
            assert_eq!(
                offset.get(),
                buffer.len(),
                "[SDAVE] final_buffer offset MUST equal buffer.len()"
            );
            true
        }
        None => false,
    };
    let state = deserializer.deserialize_incremental();
    match state.working_on() {
        Item::E { envelop } => {
            let payload =
                &buffer[envelop.payload_head_offset.get()..envelop.payload_tail_offset.get()];
            match state.internal() {
                InternalState::Archived | InternalState::CleanArchived => Ok(payload),
                // An unterminated limiter slice is a whole Envelop only once the scan
                // reached the end of the buffer; otherwise the frame is not confirmed, so
                // its payload MUST NOT be handed out.
                InternalState::UnterminatedLimiterSlice { tail_offset }
                    if tail_offset.get() == buffer.len() =>
                {
                    Ok(payload)
                }
                // A `Variant::V2` Envelop whose delimiter slice has no following
                // non-delimiter is unconfirmed: accept it only when the caller
                // declared the buffer final, else report it distinctly.
                InternalState::UnterminatedDeLimiterSlice { .. } if is_final => Ok(payload),
                InternalState::UnterminatedDeLimiterSlice { .. } => {
                    Err(Error::UnconfirmedDelimiter)
                }
                // Any other state leaves the Envelop unconfirmed, so the buffer does not
                // start with a well-formed Envelop.
                _ => Err(Error::UnexpectedItem),
            }
        }
        Item::N { .. } => Err(Error::UnexpectedItem),
    }
}

/// Run `consumer` with a [`FlatDeserializer`] over exactly `buffer`, reusing
/// `parent`'s `Vec`s instead of allocating new ones.
///
/// On return `parent` is put back at its own position (buffer, tail, working
/// item, internal state) with the recycled storage, and the item cache the child
/// filled is cleared: a [`FlatDeserializer`] keeps a cache only for the channel it
/// is walking, and the parent walks a different one, so nothing of the child's
/// cache may survive. (`with_deserializer` therefore MUST be called at an item
/// boundary, never in the middle of a channel walk whose iterator is still in
/// use, see the safety note.)
///
/// # Safety
/// * `buffer` MUST be a sub-slice of `parent.buffer()`.
/// * `parent` MUST be the owner of its storage, i.e. not itself the child of an
///   in-flight `with_deserializer` call.
pub unsafe fn with_deserializer<'a, 'l, T, A, R, F>(
    parent: &mut FlatDeserializer<'a, 'l, T, A>,
    buffer: &'a [T],
    consumer: F,
) -> Result<R, Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
    F: FnOnce(&mut FlatDeserializer<'a, 'l, T, A>) -> Result<R, Error>,
{
    let parent_buffer = parent.buffer();
    let parent_tail_non_envelop = parent.tail_non_envelop();
    let variant = parent.deserializer_state().variant();
    // A shallow copy of the parent's position: `repeat_map`/`limiter_pairs` are `None`
    // there (they move to the child below) and `limiter_pair.bsize` is erased, which only
    // costs a rescan -- both are covered by the safety contract.
    let position = unsafe { parent.deserializer_state().clone() };
    let placeholder = unsafe {
        FlatDeserializer::new_unchecked((
            parent_buffer,
            None,
            parent_tail_non_envelop,
            position.clone(),
        ))
    };
    // Take the parent's recycleable storage out for the child.
    let taken = core::mem::replace(parent, placeholder);
    let (_, taken_boundaries, _, taken_state) = taken.decouple();
    let (_, _, _, _, _, taken_repeat_map, taken_limiter_pairs) = taken_state.decouple();
    let mut child = FlatDeserializer::new(
        buffer,
        variant,
        taken_limiter_pairs.expect("[SDAVE] parent deserializer lost its limiter_pairs"),
        taken_boundaries.expect("[SDAVE] parent deserializer lost its archived_boundaries"),
        taken_repeat_map.expect("[SDAVE] parent deserializer lost its repeat_map"),
    );
    let result = consumer(&mut child);

    // Recycle the child's storage back into the parent, at the parent's own position.
    let (_, child_boundaries, _, child_state) = child.decouple();
    let (_, _, _, _, _, child_repeat_map, child_limiter_pairs) = child_state.decouple();
    let mut repeat_map = child_repeat_map.expect("[SDAVE] child deserializer lost its repeat_map");
    let mut limiter_pairs =
        child_limiter_pairs.expect("[SDAVE] child deserializer lost its limiter_pairs");
    // The child clobbered the scanning cache stored in those `Vec`s; clear it so the parent
    // resumes with a clean cache (`bsize`/repeats are recomputed on the next scan).
    unsafe { update_internal_cache(&mut repeat_map, &mut limiter_pairs, None) };
    let (working_on, mut internal, next, variant, limiter_pair, _, _) = position.decouple();
    // A partially scanned item keeps its running count in `repeat_map`, which was just
    // cleared; rewinding the scan to the last settled boundary is the documented way to
    // resume from a cleared cache -- the scan is redone, the result is the same.
    match &mut internal {
        InternalState::Unknown { tail_offset }
        | InternalState::UnterminatedLimiterSlice { tail_offset }
        | InternalState::UnterminatedDeLimiterSlice { tail_offset } => {
            *tail_offset = next.semantical_tail_offset();
        }
        _ => {}
    }
    let rebuilt = unsafe {
        FlatDeserializer::new_unchecked((
            parent_buffer,
            Some(
                child_boundaries.expect("[SDAVE] child deserializer lost its archived_boundaries"),
            ),
            parent_tail_non_envelop,
            FlatDeserializerState::new_unchecked((
                working_on,
                internal,
                next,
                variant,
                limiter_pair,
                Some(repeat_map),
                Some(limiter_pairs),
            )),
        ))
    };
    let _ = core::mem::replace(parent, rebuilt);
    result
}

/// Deframe `buffer` and return the payload slice of its first Envelop.
///
/// The buffer-taking form of [`envelop_payload`]; it builds and drops a
/// [`FlatDeserializer`] of its own, so a nested deserializer should prefer
/// [`envelop_payload`] and keep its storage.
///
/// # Safety
/// Same contract as the `final_buffer` parameter of
/// [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub unsafe fn single_item_buffer_as_envelop_payload<'a, 'l, T, A>(
    buffer: &'a [T],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, T, A>],
    final_buffer: Option<Offset>,
    allocator: A,
) -> Result<&'a [T], Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let mut deserializer = flat_deserializer(buffer, variant, limiter_pairs, &allocator)?;
    unsafe { envelop_payload(&mut deserializer, final_buffer) }
}

/// Deframe `buffer` as a mixed channel, returning its logical items (phantom
/// boundary markers excluded) in order.
///
/// A payload produced by a composite [`Serialize`](crate::Serialize) is exactly
/// such a channel: a sequence of `NonEnvelop` signatures each followed by its
/// value `Envelop`.
///
/// A `Variant::V2` Envelop that ends `buffer` is returned unarchived; verify its
/// integrity manually. A deserializer that already owns a [`FlatDeserializer`]
/// collects the items of its channel with `collect_items` instead, which needs no
/// new allocation.
///
/// # Errors
/// Returns [`Error::EmptyLimiterPairs`] when `limiter_pairs` is empty.
pub fn items_of<'a, 'l, T, A>(
    buffer: &'a [T],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, T, A>],
    allocator: A,
) -> Result<Vec<Item, A>, Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let mut deserializer = flat_deserializer(buffer, variant, limiter_pairs, &allocator)?;
    let mut result: Vec<Item, A> = Vec::new_in(allocator.clone());
    for item in deserializer.iter() {
        if !item.is_phantom() {
            result.push(item);
        }
    }
    Ok(result)
}

/// Collect the non-phantom items of the channel `deserializer` walks into `slots`,
/// returning how many items the channel holds.
///
/// Surplus items (more than `slots.len()`) are counted but not stored: a derived
/// type knows how many items its own channel can hold, so it sizes `slots`
/// exactly and never needs a heap `Vec` of items.
pub fn collect_items<T, A>(
    deserializer: &mut FlatDeserializer<'_, '_, T, A>,
    slots: &mut [Option<Item>],
) -> usize
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let mut count = 0usize;
    for item in deserializer.iter() {
        if item.is_phantom() {
            continue;
        }
        if let Some(slot) = slots.get_mut(count) {
            *slot = Some(item);
        }
        count += 1;
    }
    count
}

/// The element byte-ranges of an iterable payload, in order.
///
/// An iterable payload is a leading empty marker Envelop followed by one Envelop
/// per element (optionally preceded by a `Signature` NonEnvelop when the iterable
/// was serialized with `serialize_typed`). Returns the `(head, tail)` range of
/// every element Envelop, so a caller can deserialize each element from
/// `&payload[head..tail]` — which stays correct for a trailing `Variant::V2`
/// Envelop, whose `tail_offset` is still `None`.
///
/// # Errors
/// Returns [`Error::EmptyLimiterPairs`] when `limiter_pairs` is empty and
/// [`Error::UnexpectedItem`] when `payload` is not a well-formed iterable channel.
pub fn iterable_element_ranges<'l, A>(
    payload: &[u8],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, u8, A>],
    allocator: A,
) -> Result<Vec<(Offset, Offset), A>, Error>
where
    A: Allocator + Clone,
{
    let mut deserializer = flat_deserializer(payload, variant, limiter_pairs, &allocator)?;
    let mut ranges: Vec<(Offset, Offset), A> = Vec::new_in(allocator);
    for_each_element_range(&mut deserializer, payload.len(), |head, tail| {
        ranges.push((Offset::new(head), Offset::new(tail)));
        Ok(())
    })?;
    Ok(ranges)
}

/// Visit the byte range of every element Envelop of the iterable channel
/// `deserializer` walks, in order.
///
/// This is the one walk of the iterable channel: [`iterable_element_ranges`] collects the
/// ranges, while [`visit_iterable_elements`] decodes each element after the walk, so neither
/// needs to materialize the whole item list first. `payload_end` is the end offset of the
/// channel's payload, which is the last element's tail (`None` on a trailing unarchived
/// Envelop).
///
/// # Errors
/// Returns [`Error::UnexpectedItem`] when the channel is not a well-formed iterable channel;
/// an error returned by `visit` stops the walk and is returned as-is.
fn for_each_element_range<'a, 'l, A, F>(
    deserializer: &mut FlatDeserializer<'a, 'l, u8, A>,
    payload_end: usize,
    mut visit: F,
) -> Result<(), Error>
where
    A: Allocator + Clone,
    F: FnMut(usize, usize) -> Result<(), Error>,
{
    let mut items = deserializer.iter().filter(|item| !item.is_phantom());

    // A `serialize_typed` iterable carries a leading signature NonEnvelop.
    let mut marker = items.next().ok_or(Error::UnexpectedItem)?;
    if matches!(marker, Item::N { .. }) {
        marker = items.next().ok_or(Error::UnexpectedItem)?;
    }
    // The mandatory empty marker Envelop.
    if !matches!(marker, Item::E { .. }) {
        return Err(Error::UnexpectedItem);
    }

    // Items are contiguous, so an element spans from its head to the next item's
    // head (or the payload tail for the last one): one item of lookahead is enough.
    let mut head: Option<usize> = None;
    for item in items {
        let Item::E { envelop } = item else {
            return Err(Error::UnexpectedItem);
        };
        let element_head = envelop.head_offset.get();
        if let Some(previous_head) = head.replace(element_head) {
            visit(previous_head, element_head)?;
        }
    }
    if let Some(head) = head {
        visit(head, payload_end)?;
    }
    Ok(())
}

/// Deserialize every element of the iterable `deserializer` frames, handing each to
/// `visit` in order.
///
/// This is the streaming core of every iterable deserializer: the element ranges are
/// collected from the payload channel first (they are plain offsets), then each element is
/// decoded through the recycled [`FlatDeserializer`] storage, so a caller that pushes into
/// its own container needs no intermediate `Vec` of elements.
///
/// # Errors
/// Returns the errors [`iterable_element_ranges`] reports, plus whatever an element's
/// deserializer or `visit` returns (the walk stops there).
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub unsafe fn visit_iterable_elements<N, T, A, F>(
    deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
    final_buffer: Option<Offset>,
    allocator: &A,
    mut visit: F,
) -> Result<(), Error>
where
    N: NamePolicy,
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
    F: FnMut(T) -> Result<(), Error>,
{
    let payload = unsafe { envelop_payload(deserializer, final_buffer)? };
    // The element list is unbounded, so the ranges cannot live in a fixed-size array; they
    // are the only heap use of the walk (the elements themselves are the caller's output).
    let mut ranges: Vec<(usize, usize), A> = Vec::new_in(allocator.clone());
    unsafe {
        with_deserializer(deserializer, payload, |channel| {
            for_each_element_range(channel, payload.len(), |head, tail| {
                ranges.push((head, tail));
                Ok(())
            })
        })?;
    }
    for (head, tail) in ranges {
        let value = &payload[head..tail];
        let element = unsafe {
            with_deserializer(deserializer, value, |channel| {
                <T as Deserialize<u8, A>>::deserialize::<N>(
                    channel,
                    Some(Offset::new(value.len())),
                    allocator.clone(),
                )
            })?
        };
        visit(element)?;
    }
    Ok(())
}

/// Deserialize every element of the iterable `deserializer` frames into a fresh `Vec`.
///
/// This is the shared body of the iterable deserializers that need an owned `Vec` of the
/// elements (`[T; N]`, which then builds its array from it). A caller that can push straight
/// into its own container should use [`visit_iterable_elements`] instead and skip this `Vec`.
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub unsafe fn deserialize_iterable_elements<N, T, A>(
    deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
    final_buffer: Option<Offset>,
    allocator: A,
) -> Result<Vec<T, A>, Error>
where
    N: NamePolicy,
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
{
    let mut elements: Vec<T, A> = Vec::new_in(allocator.clone());
    unsafe {
        visit_iterable_elements::<N, T, A, _>(deserializer, final_buffer, &allocator, |element| {
            elements.push(element);
            Ok(())
        })?;
    }
    Ok(elements)
}

/// Build a [`FlatDeserializer`] over `buffer`, cloning `limiter_pairs` into the
/// `allocator`'s memory.
///
/// # Errors
/// Returns [`Error::EmptyLimiterPairs`] when `limiter_pairs` is empty.
pub fn flat_deserializer<'a, 'l, T, A>(
    buffer: &'a [T],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, T, A>],
    allocator: &A,
) -> Result<FlatDeserializer<'a, 'l, T, A>, Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    if limiter_pairs.is_empty() {
        return Err(Error::EmptyLimiterPairs);
    }
    let mut pairs: Vec<LimiterPair<'l, T, A>, A> = Vec::new_in(allocator.clone());
    for limiter_pair in limiter_pairs {
        pairs.push(limiter_pair.clone());
    }
    Ok(FlatDeserializer::new(
        buffer,
        variant,
        pairs,
        Vec::new_in(allocator.clone()),
        Vec::new_in(allocator.clone()),
    ))
}
