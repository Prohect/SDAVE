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
//! Both modules go through the same helpers — [`frame_tagged`] writes the shape, and
//! [`read_tagged`] (over [`tagged_parts`]) splits a deframed payload into tag and value,
//! which [`decode_tagged_value`] then decodes — so the shape lives in exactly one place.

use core::alloc::Allocator;

use alloc::vec::Vec;

use sdave::{FlatDeserializer, Item, LimiterPair, Offset, Variant};

use crate::{
    Error,
    deserialize::Deserialize,
    impls::{Deframed, collect_items, envelop_payload, with_deserializer},
    name::NamePolicy,
    serialize::{Serialize, Strategy, flatten_in_place, frame_region},
};

/// Serialize `tag ++ value` as one Envelop, returning its 3 logical slices
/// `[limiter, payload, delimiter]`.
///
/// The payload is flattened in place, so the tag NonEnvelop and the value Envelop
/// occupy one contiguous region like any other leaf value.
///
/// # Errors
/// Returns what the value's own `serialize` and [`frame_region`] report.
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
    // The tag and the value are one contiguous payload from here on, so the flattened
    // length the helper reports is not needed.
    let _ = unsafe { flatten_in_place(buffer, head, [name].into_iter().chain(inner)) };
    unsafe { frame_region(buffer, head, variant, strategy, limiter_pairs) }
}

/// A deframed tagged-value payload: the variant tag, and the value Envelop behind it.
///
/// Every offset here is absolute in [`Tagged::buffer`], the raw buffer the tagged
/// Envelop was deframed from.
pub(crate) struct Tagged<'a, T> {
    /// The raw buffer every range below indexes.
    pub(crate) buffer: &'a [T],
    /// The variant tag — the payload's leading `NonEnvelop` — as absolute `(head, tail)`
    /// offsets.
    pub(crate) tag_range: (Offset, Offset),
    /// The tagged payload as a whole: where a value Envelop that is missing was looked
    /// for. See [`Tagged::value_envelop_range`].
    pub(crate) payload: (Offset, Offset),
    /// The value Envelop's absolute `(head, tail)` frame, absent for a payload-less
    /// variant such as `Option::None`.
    pub(crate) value: Option<(Offset, Offset)>,
}

impl<'a, T> Tagged<'a, T> {
    /// The tag's bytes.
    pub(crate) fn tag(&self) -> &'a [T] {
        &self.buffer[self.tag_range.0.get()..self.tag_range.1.get()]
    }

    /// The range the value Envelop is required in: from the tag's tail to the end of this
    /// payload. It is what [`Error::MissingEnvelop`] reports as `caught` when the value is
    /// absent.
    pub(crate) fn value_envelop_range(&self) -> (Offset, Offset) {
        (self.tag_range.1, self.payload.1)
    }

    /// The value Envelop's frame, or [`Error::MissingEnvelop`] naming the tag that required
    /// it.
    ///
    /// A payload-less variant such as `Option::None` never calls this: its tag is complete
    /// on its own, and the absent value is the answer rather than a fault.
    pub(crate) fn value_envelop(&self) -> Result<(Offset, Offset), Error> {
        self.value.ok_or(Error::MissingEnvelop {
            required_by: Some(self.tag_range),
            caught: self.value_envelop_range(),
        })
    }
}

/// Deframe the tagged payload `deframed` — the payload of the Envelop `deserializer` is
/// positioned on — into its variant tag and its value frame.
///
/// The value is `None` when the payload holds no value Envelop after the tag, which is how
/// a payload-less variant such as `Option::None` is written.
///
/// Only the first two items matter, so the channel walk stores them in a fixed-size array:
/// no `Vec<Item>` is allocated.
///
/// # Errors
/// Returns [`Error::UnexpectedSignature`] when the payload does not start with a
/// `NonEnvelop` tag.
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize): `deframed`
/// MUST be the payload of the Envelop `deserializer` is positioned on.
pub(crate) unsafe fn tagged_parts<'a, 'l, T, A>(
    deserializer: &mut FlatDeserializer<'a, 'l, T, A>,
    deframed: Deframed<'a, T>,
) -> Result<Tagged<'a, T>, Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let (buffer, payload) = deframed;
    let mut slots: [Option<Item>; 2] = [None, None];
    unsafe {
        with_deserializer(deserializer, payload, |channel| {
            let _ = collect_items(channel, &mut slots);
            Ok(())
        })?;
    }
    let Some(Item::N { non_envelop }) = &slots[0] else {
        return Err(Error::UnexpectedSignature {
            valid_signatures: &["NonEnvelop"],
            caught: slots[0].as_ref().map_or(payload, |item| {
                (item.head_offset(), item.semantical_tail_offset())
            }),
        });
    };
    let tag_range = (non_envelop.head_offset, non_envelop.tail_offset);
    let value = match &slots[1] {
        Some(Item::E { envelop }) => Some((
            envelop.head_offset,
            envelop.tail_offset.unwrap_or(payload.1),
        )),
        _ => None,
    };
    Ok(Tagged {
        buffer,
        tag_range,
        payload,
        value,
    })
}

/// Deframe the value `deserializer` is positioned on as one tagged Envelop,
/// returning its [`Tagged`] parts.
///
/// This is the read half of the tagged-variant shape — deframe the single Envelop,
/// then split its payload with [`tagged_parts`] — factored out because `Option` and
/// `Result` do exactly this and differ only in the tags they accept.
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub(crate) unsafe fn read_tagged<'a, 'l, T, A>(
    deserializer: &mut FlatDeserializer<'a, 'l, T, A>,
    final_buffer: Option<Offset>,
) -> Result<Tagged<'a, T>, Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let deframed = unsafe { envelop_payload(deserializer, final_buffer)? };
    unsafe { tagged_parts(deserializer, deframed) }
}

/// Decode `value` — the value frame [`tagged_parts`] handed back — as `V`.
///
/// The frame is one whole Envelop, so the value is decoded straight from it and
/// through the enclosing deserializer's recycled storage: a tagged variant adds one
/// nesting level, not one set of `Vec`s.
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize), and
/// `value` MUST be a sub-range of `deserializer.buffer()` holding one value Envelop.
pub(crate) unsafe fn decode_tagged_value<'a, 'l, N, V, T, A>(
    deserializer: &mut FlatDeserializer<'a, 'l, T, A>,
    value: (Offset, Offset),
    allocator: A,
) -> Result<V, Error>
where
    N: NamePolicy,
    V: Deserialize<T, A, Type = V>,
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    // The child reads `buffer[..value.1]`, so the value's tail is its final length.
    let final_value = value.1;
    unsafe {
        with_deserializer(deserializer, value, |channel| {
            <V as Deserialize<T, A>>::deserialize::<N>(channel, Some(final_value), allocator)
        })
    }
}
