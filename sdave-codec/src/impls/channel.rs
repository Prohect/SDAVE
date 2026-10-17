//! Deframing helpers: everything that reads a buffer back.
//!
//! The single-Envelop shorthand, the mixed-item channel, the iterable element
//! ranges and the [`FlatDeserializer`] builder. Every codec deserializer is built
//! from these, so the "what is one item" rules live in exactly one place.

use core::alloc::Allocator;

use alloc::vec::Vec;

use sdave::{FlatDeserializer, InternalState, Item, LimiterPair, Offset, Variant};

use crate::{Error, deserialize::Deserialize, name::NamePolicy};

/// Deframe `buffer` and return the payload slice of its first Envelop.
///
/// `buffer` MUST start with an Envelop; anything after that Envelop is not
/// examined (walk a whole channel with [`items_of`] or a [`FlatDeserializer`]
/// instead). The codec's own recursive calls always pass one exact item slice, so
/// the leniency only shows for a caller that hands over a longer buffer.
///
/// `final_buffer` has the meaning documented once on
/// [`Deserialize::deserialize`](crate::Deserialize::deserialize); this function asserts
/// the `offset == buffer.len()` half of it, and maps an unconfirmed `Variant::V2`
/// delimiter to [`Error::UnconfirmedDelimiter`].
///
/// # Errors
/// Returns [`Error::EmptyLimiterPairs`] when `limiter_pairs` is empty,
/// [`Error::UnconfirmedDelimiter`] for an unconfirmed `Variant::V2` Envelop, and
/// [`Error::UnexpectedItem`] when `buffer` does not start with an Envelop, or starts
/// with a malformed one.
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
    let mut deserializer = flat_deserializer(buffer, variant, limiter_pairs, &allocator)?;
    let state = deserializer.deserialize_incremental();
    match state.working_on() {
        Item::E { envelop } => {
            let payload =
                &buffer[envelop.payload_head_offset.get()..envelop.payload_tail_offset.get()];
            match state.internal() {
                InternalState::Archived | InternalState::CleanArchived => Ok(payload),
                // An unterminated limiter slice is a whole Envelop only once the scan
                // reached the end of `buffer`; otherwise the frame is not confirmed, so
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
                // Any other state leaves the Envelop unconfirmed, so `buffer` does not
                // start with a well-formed Envelop.
                _ => Err(Error::UnexpectedItem),
            }
        }
        Item::N { .. } => Err(Error::UnexpectedItem),
    }
}

/// Deframe `buffer` as a mixed channel, returning its logical items (phantom
/// boundary markers excluded) in order.
///
/// A payload produced by a composite [`Serialize`](crate::Serialize) is exactly
/// such a channel: a sequence of `NonEnvelop` signatures each followed by its
/// value `Envelop`.
///
/// A `Variant::V2` Envelop that ends `buffer` is returned unarchived; verify its
/// integrity manually.
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
    let items = items_of(payload, variant, limiter_pairs, allocator.clone())?;

    let mut index = 0;
    // A `serialize_typed` iterable carries a leading signature NonEnvelop.
    if matches!(items.first(), Some(Item::N { .. })) {
        index += 1;
    }
    // The mandatory empty marker Envelop.
    if !matches!(items.get(index), Some(Item::E { .. })) {
        return Err(Error::UnexpectedItem);
    }
    index += 1;

    let mut ranges: Vec<(Offset, Offset), A> = Vec::new_in(allocator);
    for k in index..items.len() {
        let Item::E { envelop } = &items[k] else {
            return Err(Error::UnexpectedItem);
        };
        // Items are contiguous, so an element spans from its head to the next
        // item's head (or the payload tail).
        let head = envelop.head_offset.get();
        let tail = items
            .get(k + 1)
            .map_or(payload.len(), |item| item.head_offset().get());
        ranges.push((Offset::new(head), Offset::new(tail)));
    }
    Ok(ranges)
}

/// Deserialize every element of the iterable `buffer` into a fresh `Vec`.
///
/// This is the shared body of every iterable deserializer (`Vec`/`VecDeque`/
/// `LinkedList`/`BTreeSet` and `[T; N]`), which rebuild their own container from
/// the returned elements.
///
/// # Safety
/// Same contract as [`single_item_buffer_as_envelop_payload`].
pub unsafe fn deserialize_iterable_elements<N, T, A>(
    buffer: &[u8],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'_, u8, A>],
    final_buffer: Option<Offset>,
    allocator: A,
) -> Result<Vec<T, A>, Error>
where
    N: NamePolicy,
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
{
    let payload = unsafe {
        single_item_buffer_as_envelop_payload(
            buffer,
            variant,
            limiter_pairs,
            final_buffer,
            allocator.clone(),
        )?
    };
    let ranges = iterable_element_ranges(payload, variant, limiter_pairs, allocator.clone())?;

    let mut elements: Vec<T, A> = Vec::new_in(allocator.clone());
    for (head, tail) in ranges {
        let value = &payload[head.get()..tail.get()];
        elements.push(<T as Deserialize<u8, A>>::deserialize::<N>(
            value,
            variant,
            limiter_pairs,
            Some(Offset::new(value.len())),
            allocator.clone(),
        )?);
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
