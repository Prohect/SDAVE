//! The wire shape of a *tagged variant*, shared by `Option` and `Result`.
//!
//! `Ok(x)` and `Err(e)` serialize to one Envelop whose payload is the variant's name
//! bytes (`Ok` / `Err`) directly followed by the value's own Envelop; a payload-less
//! variant such as `None` writes the tag alone:
//!
//! ```text
//! Envelop(NonEnvelop(tag) ++ Envelop(value))
//! ```
//!
//! This is *not* the shape `#[derive(Serialize)]` gives a data-carrying enum
//! variant: a derived variant wraps its fields in an inner Envelop (its payload is
//! `NonEnvelop(tag) ++ Envelop(field signatures ++ field values)`) because it may
//! carry any number of them. `Option`/`Result` carry exactly one value, so they nest
//! it directly and the tag stays a plain name.
//!
//! Both modules go through the helpers below, so the shape lives in exactly one
//! place.

use core::alloc::Allocator;

use alloc::vec::Vec;

use sdave::{Item, LimiterPair, Offset, Variant};

use crate::{
    Error,
    impls::{items_of, single_item_buffer_as_envelop_payload},
    name::NamePolicy,
    serialize::{Serialize, Strategy, flatten_in_place, frame_region},
};

/// Serialize `tag ++ value` as one Envelop, returning its 3 logical slices
/// `[limiter, payload, delimiter]`.
///
/// The payload is flattened in place, so the tag NonEnvelop and the value Envelop
/// occupy one contiguous region like any other leaf value.
///
/// # Safety
/// Same contract as [`sdave::can_serialize`]: the tail of `buffer` MUST be a clean
/// item boundary.
pub(crate) unsafe fn frame_tagged<N, T, A>(
    buffer: &mut Vec<u8, A>,
    tag: &[u8],
    value: &T,
    variant: Variant,
    strategy: Strategy,
    limiter_pairs: &mut [LimiterPair<'_, u8, A>],
) -> Result<[Option<(Offset, Offset)>; 3], Error>
where
    N: NamePolicy,
    T: Serialize<u8, A> + ?Sized,
    A: Allocator + Clone,
{
    let head = buffer.len();
    buffer.extend_from_slice(tag);
    let name = Some((Offset::new(head), Offset::new(buffer.len())));
    let inner = unsafe {
        <T as Serialize<u8, A>>::serialize::<N>(value, buffer, variant, strategy, limiter_pairs)?
    };
    unsafe { flatten_in_place(buffer, head, [name].into_iter().chain(inner)) };
    unsafe { frame_region(buffer, head, variant, strategy, limiter_pairs) }
}

/// Deframe a tagged Envelop `payload` into its variant `tag` and its value bytes
/// (the value Envelop's whole frame, ready to be deserialized).
///
/// The value is `None` when the payload holds no value Envelop after the tag,
/// which is how a payload-less variant such as `Option::None` is written.
///
/// # Errors
/// Returns [`Error::UnexpectedItem`] when `payload` does not start with a
/// `NonEnvelop` tag.
pub(crate) fn tagged_parts<'a, 'l, T, A>(
    payload: &'a [T],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, T, A>],
    allocator: A,
) -> Result<(&'a [T], Option<&'a [T]>), Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let items = items_of(payload, variant, limiter_pairs, allocator)?;
    let Some(Item::N { non_envelop }) = items.first() else {
        return Err(Error::UnexpectedItem);
    };
    let tag = &payload[non_envelop.head_offset.get()..non_envelop.tail_offset.get()];
    let value = match items.get(1) {
        Some(Item::E { envelop }) => {
            let tail = envelop.tail_offset.map_or(payload.len(), |tail| tail.get());
            Some(&payload[envelop.head_offset.get()..tail])
        }
        _ => None,
    };
    Ok((tag, value))
}

/// Deframe `buffer` as one tagged Envelop, returning its variant `tag` and its
/// value bytes.
///
/// This is the read half of the tagged-variant shape — deframe the single Envelop,
/// then split its payload with [`tagged_parts`] — factored out because `Option` and
/// `Result` do exactly this and differ only in the tags they accept.
///
/// # Safety
/// Same contract as [`single_item_buffer_as_envelop_payload`].
pub(crate) unsafe fn read_tagged<'a, 'l, T, A>(
    buffer: &'a [T],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, T, A>],
    final_buffer: Option<Offset>,
    allocator: A,
) -> Result<(&'a [T], Option<&'a [T]>), Error>
where
    T: Clone + PartialEq + Sized,
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
    tagged_parts(payload, variant, limiter_pairs, allocator)
}
