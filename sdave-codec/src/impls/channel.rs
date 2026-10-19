//! Deframing helpers: everything that reads a buffer back.
//!
//! The single-Envelop shorthand, the mixed-item channel and the iterable element walk
//! ([`for_each_element_range`]). Every codec deserializer is built from these, so the "what is
//! one item" rules live in exactly one place.
//!
//! Nothing here builds or drives a deserializer: a `deserialize` takes the one it is lent
//! and hands it on, so a document is read through *one* [`FlatDeserializer`] whose storage
//! is recycled ([`with_deserializer`]). Building that instance — and the buffer-taking
//! one-shot drivers on top of it (`flat_deserializer`, `deserialize_one`, `items_of`,
//! `single_item_buffer_as_envelop_payload`, `iterable_element_ranges`) — is the caller's
//! step, so the crate offers none of them. The tests keep their own set in
//! `tests/common/mod.rs`.
//!
//! Nothing here allocates for the traversal either: an iterable's element ranges are stacked
//! on native frames ([`Stack`]) like its serialization slices, and the one intermediate
//! element list left in the crate is `[T; N]`'s, which
//! [`deserialize_iterable_elements`] hands back to the caller.
//!
//! # Absolute offsets
//!
//! A nested frame never gets a sub-slice capped at both ends: it reads the raw
//! *prefix* `parent.buffer()[..tail]` of the very buffer its parent reads (see
//! [`with_deserializer`]). Every offset the codec ever hands back is therefore
//! absolute in the origin buffer, whatever the nesting depth, which is what lets an
//! [`Error`] point at the bytes that failed instead of at a renumbered copy.
//!
//! # Storage recycling
//!
//! A [`FlatDeserializer`] owns three `Vec`s (`archived_boundaries`, `repeat_map`,
//! `limiter_pairs`). [`with_deserializer`] lends those very instances to a child
//! deserializer over a sub-frame and takes them back afterwards, so a nested
//! deserialization costs no allocation: only the items a value *returns* to the
//! caller (a `Vec`, a `Box`, a `String`, …) are allocated.

use core::{alloc::Allocator, cell::RefCell, ops::RangeFrom};

use alloc::vec::Vec;

use sdave::{
    Envelop, FlatDeserializer, FlatDeserializerState, InternalState, Item, NonEnvelop, Offset,
    Stack, update_internal_cache,
};

use crate::{Error, deserialize::Deserialize, name::NamePolicy};

/// A deframed Envelop: the raw buffer its offsets index, and the payload's absolute
/// `(head, tail)` range in that buffer.
///
/// The buffer is the raw one the deframing deserializer reads, so every offset -- of the
/// payload itself and of every item a channel walk behind it reports -- stays absolute.
pub type Deframed<'a, T> = (&'a [T], (Offset, Offset));

/// An iterable's deserialized elements, together with the absolute `(head, tail)` frame of the
/// payload they were read from.
pub type IterableElements<T, A> = (Vec<T, A>, (Offset, Offset));

/// The iteration index a nested channel starts at: [`with_deserializer`] installs the frame
/// marker as item `0`, so a channel read through it begins at `1`.
const FRAME_CHANNEL_START: usize = 1;

/// Drive the single Envelop `deserializer` is positioned on and return its
/// [`Deframed`] payload.
///
/// `deserializer` MUST start at an Envelop; anything after that Envelop is not
/// examined (walk a whole channel with `collect_items`/`FlatDeserializer::iter`
/// instead). The codec's own recursive calls always hand over one exact item, so
/// the leniency only shows for a caller that hands over a longer buffer.
///
/// # Errors
/// Returns [`Error::UnconfirmedDelimiter`] for an unconfirmed `Variant::V2`
/// Envelop, and [`Error::MissingEnvelop`] when the buffer does not start with an
/// Envelop, or starts with a malformed one. Both place the failing bytes by their
/// absolute offsets.
///
/// # Panics
/// Panics when `final_buffer` is `Some(offset)` and `offset` is not
/// `deserializer.buffer().len()`, i.e. when the caller's "this buffer is complete"
/// promise does not hold.
///
/// # Safety
/// Same contract as the `final_buffer` parameter of
/// [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub unsafe fn envelop_payload<'a, 'l, T, A>(
    deserializer: &mut FlatDeserializer<'a, 'l, T, A>,
    final_buffer: Option<Offset>,
) -> Result<Deframed<'a, T>, Error>
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
    let caught = (
        state.working_on().head_offset(),
        state.working_on().semantical_tail_offset(),
    );
    match state.working_on() {
        Item::E { envelop } => {
            let payload = (envelop.payload_head_offset, envelop.payload_tail_offset);
            match state.internal() {
                InternalState::Archived | InternalState::CleanArchived => Ok((buffer, payload)),
                // An unterminated limiter slice is a whole Envelop only once the scan
                // reached the end of the buffer; otherwise the frame is not confirmed, so
                // its payload MUST NOT be handed out.
                InternalState::UnterminatedLimiterSlice { tail_offset }
                    if tail_offset.get() == buffer.len() =>
                {
                    Ok((buffer, payload))
                }
                // A `Variant::V2` Envelop whose delimiter slice has no following
                // non-delimiter is unconfirmed: accept it only when the caller
                // declared the buffer final, else report it distinctly.
                InternalState::UnterminatedDeLimiterSlice { .. } if is_final => {
                    Ok((buffer, payload))
                }
                InternalState::UnterminatedDeLimiterSlice { tail_offset } => {
                    Err(Error::UnconfirmedDelimiter {
                        limiter_slice: (envelop.head_offset, envelop.payload_head_offset),
                        unconfirmed: (envelop.payload_tail_offset, *tail_offset),
                    })
                }
                // Any other state leaves the Envelop unconfirmed, so the buffer does not
                // start with a well-formed Envelop.
                _ => Err(Error::MissingEnvelop {
                    required_by: None,
                    caught,
                }),
            }
        }
        Item::N { .. } => Err(Error::MissingEnvelop {
            required_by: None,
            caught,
        }),
    }
}

/// Run `consumer` with a [`FlatDeserializer`] over the frame `frame` (absolute
/// `(head, tail)` offsets), reusing `parent`'s `Vec`s instead of allocating new ones.
///
/// The child reads `parent.buffer()[..frame.1]`: the raw prefix of the parent's
/// buffer, so the offsets it reports stay absolute (see the module documentation).
/// `frame.0` is where the child's channel starts; because the raw prefix keeps the
/// bytes before it, that start is confirmed by a *frame marker* the child's
/// `archived_boundaries` leads with: an archived [`Envelop`] spanning the prefix
/// `[0, frame.0)`, which no other code path can produce (its delimiter slice
/// `[frame.0, frame.0)` is empty). The marker is iteration item `0` of the child's
/// channel -- [`collect_items`] and the iterable walk skip it -- and it is what keeps
/// a leading `NonEnvelop` of that channel reported as starting at `frame.0` rather
/// than at 0.
///
/// On return `parent` is put back at its own position (buffer, tail, working
/// item, internal state) with the recycled storage, and the item cache is cleared:
/// a [`FlatDeserializer`] keeps a cache only for the channel it is walking, and the
/// parent walks a different one, so the child's items MUST NOT survive. The
/// recycled `archived_boundaries` is empty, i.e. the parent's cache is cleared as
/// well (the documented cost of the reuse). A channel walk over `parent` therefore MUST
/// NOT be resumed after a nested call — and its iterator cannot be alive during one
/// anyway, since the call needs `&mut parent`.
///
/// # Panics
/// Panics when `frame` is not a sub-range of `parent.buffer()`: a caller bug, never
/// something a document can cause.
///
/// # Safety
/// * `frame` MUST be a sub-range of `parent.buffer()`, `head <= tail`, and
///   `buffer[head..tail]` MUST be exactly one value's bytes.
/// * `parent` MUST own its storage. A [`with_deserializer`] child owns the storage it was
///   lent, so it MAY be used — provided its own channel walk has finished and is never
///   resumed, because the nested call hands the storage back with an empty cache.
pub unsafe fn with_deserializer<'a, 'l, T, A, R, F>(
    parent: &mut FlatDeserializer<'a, 'l, T, A>,
    frame: (Offset, Offset),
    consumer: F,
) -> Result<R, Error>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
    F: FnOnce(&mut FlatDeserializer<'a, 'l, T, A>) -> Result<R, Error>,
{
    let parent_buffer = parent.buffer();
    let head = frame.0.get();
    let tail = frame.1.get();
    assert!(
        head <= tail && tail <= parent_buffer.len(),
        "[SDAVE] a nested frame MUST be a sub-range of the parent buffer"
    );
    // The raw prefix, not a sub-slice capped at both ends: `[0, tail)` keeps every
    // offset of the parent's buffer meaningful, and ends exactly where the value ends.
    let child_buffer = &parent_buffer[..tail];
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
    let (_, _, _, _, taken_limiter_pair, taken_repeat_map, taken_limiter_pairs) =
        taken_state.decouple();
    let mut boundaries =
        taken_boundaries.expect("[SDAVE] parent deserializer lost its archived_boundaries");
    let mut repeat_map = taken_repeat_map.expect("[SDAVE] parent deserializer lost its repeat_map");
    let mut limiter_pairs =
        taken_limiter_pairs.expect("[SDAVE] parent deserializer lost its limiter_pairs");
    // The child frames a channel of its own, so the parent's scanning cache means
    // nothing here: reset it (`bsize`/repeats are recomputed on the first scan).
    unsafe { update_internal_cache(&mut repeat_map, &mut limiter_pairs, None) };
    // The frame marker. Constructing it is the one way to tell a recursive frame where
    // its channel starts: `archived_boundaries[0]` is an archived Envelop spanning
    // `[0, head)`, so the item after it -- the channel's leading hole -- starts at
    // `head`, and `next` is the zero-width NonEnvelop the scan resumes from.
    let marker = unsafe { Envelop::new_unchecked(Offset::new(0), frame.0, frame.0, Some(frame.0)) };
    boundaries.clear();
    boundaries.push(marker.clone());
    let mut child = unsafe {
        FlatDeserializer::new_unchecked((
            child_buffer,
            Some(boundaries),
            None,
            FlatDeserializerState::new_unchecked((
                Item::E { envelop: marker },
                InternalState::Archived,
                Item::N {
                    non_envelop: NonEnvelop::phantom(frame.0),
                },
                variant,
                taken_limiter_pair,
                Some(repeat_map),
                Some(limiter_pairs),
            )),
        ))
    };
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
    // The child's items belong to the child's channel (and lead with its marker), so hand
    // the recycled `Vec` back empty: the parent's item cache is cleared, as documented.
    let mut boundaries =
        child_boundaries.expect("[SDAVE] child deserializer lost its archived_boundaries");
    boundaries.clear();
    let rebuilt = unsafe {
        FlatDeserializer::new_unchecked((
            parent_buffer,
            Some(boundaries),
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

/// Collect the non-phantom items of the channel `deserializer` walks into `slots`,
/// returning how many items the channel holds.
///
/// The walk starts at item `1`: item `0` of a nested channel is the frame marker
/// [`with_deserializer`] installs, not an item of the channel (see its
/// documentation). `deserializer` MUST therefore be a child of `with_deserializer`,
/// which is what every caller inside the codec has.
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
    for item in deserializer.iter_from(FRAME_CHANNEL_START) {
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

/// How far the walk into an iterable payload has got.
///
/// The channel leads with the mandatory marker Envelop — optionally behind the `Signature`
/// NonEnvelop a `serialize_typed` iterable writes — and every item after it is one element
/// Envelop.
#[derive(Clone, Copy)]
enum ElementWalk {
    /// Nothing has been consumed. The next item is the marker Envelop, or the signature in
    /// front of it.
    Marker,
    /// The `Signature` at this range was seen, so the marker Envelop MUST follow it.
    MarkerBehindSignature((Offset, Offset)),
    /// The marker is confirmed: every following item is one element Envelop.
    Elements,
}

/// The iterable element walk, shared by the range collector and the decoder.
///
/// [`ElementProducer::produce`] consumes exactly one item of the payload channel per call, so
/// the walk holds no iterator: the progress is the slot index [`Stack::with`] streams in as
/// its source, and the deserializer is borrowed for one pull and released again. That is what
/// lets the `Stack` consumer decode with the very deserializer the walk read from.
///
/// A frame is `Some(range)` for an element whose tail is known, and `None` for a source item
/// that is not a complete element range — the marker Envelop, the signature, or the first
/// element, whose tail is the *next* item's head. `None` is already [`Stack`]'s stop signal,
/// which is why the frame type is `Option<(Offset, Offset)>` rather than `(Offset, Offset)`.
struct ElementProducer<'c, 'a, 'l, A>
where
    A: Allocator + Clone,
{
    deserializer: &'c mut FlatDeserializer<'a, 'l, u8, A>,
    /// End of the channel's payload: the last element's tail, and where a missing item is
    /// looked for.
    payload_end: Offset,
    walk: ElementWalk,
    /// Head of the element whose tail this walk has not reached yet.
    pending: Option<Offset>,
    /// The first failure, recorded instead of returned: a [`Stack`] producer can only end the
    /// stack, so the report travels out through here.
    error: Option<Error>,
}

impl<'c, 'a, 'l, A> ElementProducer<'c, 'a, 'l, A>
where
    A: Allocator + Clone,
{
    /// Walk the payload channel of `deserializer`, which MUST be a [`with_deserializer`] child
    /// (its item `0` is the frame marker the walk starts behind).
    fn new(deserializer: &'c mut FlatDeserializer<'a, 'l, u8, A>, payload_end: Offset) -> Self {
        Self {
            deserializer,
            payload_end,
            walk: ElementWalk::Marker,
            pending: None,
            error: None,
        }
    }

    /// Consume the channel's next non-phantom item and return the frame to push.
    ///
    /// `None` ends the stack: the channel is exhausted, or a failure was recorded in `error`
    /// (`Stack`'s producer cannot hand one back any other way).
    fn produce(&mut self, slots: &mut RangeFrom<usize>) -> Option<Option<(Offset, Offset)>> {
        let payload_end = self.payload_end;
        loop {
            if self.error.is_some() {
                return None;
            }
            // `RangeFrom` is unbounded: it streams the channel's slot indices, which is the
            // whole state a pull needs (`FlatDeserializerIter` is a function of the slot and
            // the deserializer's own cache).
            let slot = slots.next()?;
            let item = self.deserializer.iter_from(slot).next();
            let Some(item) = item else {
                // The channel is exhausted. Before the marker that is a missing Envelop; once
                // the elements are running it is the last element's tail.
                return match self.walk {
                    ElementWalk::Marker => {
                        self.error = Some(Error::MissingEnvelop {
                            required_by: None,
                            caught: (payload_end, payload_end),
                        });
                        None
                    }
                    ElementWalk::MarkerBehindSignature(signature) => {
                        self.error = Some(Error::MissingEnvelop {
                            required_by: Some(signature),
                            caught: (signature.1, payload_end),
                        });
                        None
                    }
                    ElementWalk::Elements => {
                        self.pending.take().map(|head| Some((head, payload_end)))
                    }
                };
            };
            // A phantom item is a zero-width boundary marker, never a slice of the channel.
            if item.is_phantom() {
                continue;
            }
            return match self.walk {
                ElementWalk::Marker => {
                    match item {
                        Item::E { .. } => self.walk = ElementWalk::Elements,
                        Item::N { non_envelop } => {
                            self.walk = ElementWalk::MarkerBehindSignature((
                                non_envelop.head_offset,
                                non_envelop.tail_offset,
                            ));
                        }
                    }
                    Some(None)
                }
                ElementWalk::MarkerBehindSignature(signature) => match item {
                    Item::E { .. } => {
                        self.walk = ElementWalk::Elements;
                        Some(None)
                    }
                    _ => {
                        self.error = Some(Error::MissingEnvelop {
                            required_by: Some(signature),
                            caught: (signature.1, payload_end),
                        });
                        None
                    }
                },
                ElementWalk::Elements => match item {
                    // This element ends where the next one begins, and the next one's tail is
                    // whatever follows it: the frame carries the range the previous item
                    // completed, so the first element pushes a placeholder.
                    Item::E { envelop } => Some(
                        self.pending
                            .replace(envelop.head_offset)
                            .map(|head| (head, envelop.head_offset)),
                    ),
                    // An element slot is a value Envelop alone, so a signature here belongs to
                    // nothing.
                    item => {
                        self.error = Some(Error::UnexpectedSignature {
                            valid_signatures: &[],
                            caught: (item.head_offset(), item.semantical_tail_offset()),
                        });
                        None
                    }
                },
            };
        }
    }

    /// Decode every element `frames` holds, in order, handing each to `visit`.
    ///
    /// A failure stops the walk and is recorded in `error`.
    fn consume<N, T, F>(&mut self, frames: &Stack<Option<(Offset, Offset)>>, state: &mut (F, &A))
    where
        N: NamePolicy,
        T: Deserialize<u8, A, Type = T>,
        F: FnMut(T) -> Result<(), Error>,
    {
        let (visit, allocator) = state;
        // A plain `&A`: the decode closure is built per element, so it must capture something
        // `Copy` rather than take the `&mut &A` the destructuring yields.
        let allocator: &A = allocator;
        for frame in frames.iter() {
            let Some((head, tail)) = *frame else {
                // A placeholder: the marker, the signature, or (above) a first element whose
                // tail the walk had not seen yet.
                continue;
            };
            if self.error.is_some() {
                return;
            }
            // The child reads `buffer[..tail]`, so the element's tail is its final length.
            let decoded = unsafe {
                with_deserializer(&mut *self.deserializer, (head, tail), |channel| {
                    <T as Deserialize<u8, A>>::deserialize::<N>(
                        channel,
                        Some(tail),
                        (*allocator).clone(),
                    )
                })
            };
            let outcome = match decoded {
                Ok(element) => visit(element),
                Err(failure) => Err(failure),
            };
            if let Err(failure) = outcome {
                self.error = Some(failure);
                return;
            }
        }
    }
}

/// Hand every element range `frames` holds to `visit`, in order.
fn visit_frames<F>(frames: &Stack<Option<(Offset, Offset)>>, visit: &mut F) -> Result<(), Error>
where
    F: FnMut(Offset, Offset) -> Result<(), Error>,
{
    for frame in frames.iter() {
        // A placeholder carries no range: the marker, the signature, or a first element whose
        // tail the walk had not seen yet.
        if let Some((head, tail)) = *frame {
            visit(head, tail)?;
        }
    }
    Ok(())
}

/// Visit the byte range of every element Envelop of the iterable value `deserializer` is
/// positioned on, whose payload frame is `payload`.
///
/// This is the one walk of the iterable channel, and it builds nothing: it is handed the
/// deserializer (lent, like every other helper here) and reads the element ranges out of it.
/// [`visit_iterable_elements`] collects the same ranges and then decodes each element; a caller
/// that wants the ranges alone writes exactly that first half.
///
/// An iterable payload is a leading empty marker Envelop followed by one Envelop per element
/// (optionally preceded by a `Signature` NonEnvelop when the iterable was serialized with
/// `serialize_typed`). The ranges are absolute in the deserializer's buffer, and an element
/// can be deserialized from `buffer[head..tail]` — which stays correct for a trailing
/// `Variant::V2` Envelop, whose `tail_offset` is still `None`.
///
/// The ranges live on native stack frames ([`Stack`]), one frame per channel item, so an
/// unbounded element count never becomes a heap list.
///
/// # Errors
/// Returns [`Error::MissingEnvelop`] when the channel has no mandatory marker Envelop (or
/// the signature that carries it), and [`Error::UnexpectedSignature`] when an element slot
/// holds a signature; an error returned by `visit` stops the walk and is returned as-is.
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize): `payload`
/// MUST be a sub-range of `deserializer.buffer()` holding exactly one iterable payload —
/// normally the payload of the Envelop the deserializer is positioned on, and the whole buffer
/// for a deserializer that is its own origin.
pub unsafe fn for_each_element_range<'a, 'l, A, F>(
    deserializer: &mut FlatDeserializer<'a, 'l, u8, A>,
    payload: (Offset, Offset),
    mut visit: F,
) -> Result<(), Error>
where
    A: Allocator + Clone,
    F: FnMut(Offset, Offset) -> Result<(), Error>,
{
    // SAFETY: `payload` is the value's payload frame, and the `Stack::with` call inside the
    // callback (which this block covers, being the enclosing unsafe scope) is bounded by the
    // channel it walks.
    let failure = unsafe {
        with_deserializer(deserializer, payload, |channel| {
            let walk = RefCell::new(ElementProducer::new(channel, payload.1));
            Stack::with(
                FRAME_CHANNEL_START..,
                |slots| walk.borrow_mut().produce(slots),
                |frames, visit: &mut F| {
                    if let Err(failure) = visit_frames(frames, visit) {
                        walk.borrow_mut().error = Some(failure);
                    }
                },
                &mut visit,
            );
            Ok(walk.into_inner().error)
        })?
    };
    match failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// Deserialize every element of the iterable `deserializer` frames, handing each to
/// `visit` in order.
///
/// This is the streaming core of every iterable deserializer: the element ranges are stacked
/// from the payload channel first (they are plain offsets), then each element is decoded
/// through the recycled [`FlatDeserializer`] storage, so a caller that pushes into its own
/// container needs no intermediate `Vec` of elements.
///
/// # Errors
/// Returns the errors [`for_each_element_range`] reports, plus whatever an element's
/// deserializer or `visit` returns (the walk stops there).
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub unsafe fn visit_iterable_elements<N, T, A, F>(
    deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
    final_buffer: Option<Offset>,
    allocator: &A,
    visit: F,
) -> Result<(), Error>
where
    N: NamePolicy,
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
    F: FnMut(T) -> Result<(), Error>,
{
    let (_, payload) = unsafe { envelop_payload(deserializer, final_buffer)? };
    unsafe { visit_iterable_payload::<N, T, A, F>(deserializer, payload, allocator, visit) }
}

/// Deserialize every element of the already deframed iterable `payload`, handing each to
/// `visit` in order.
///
/// The body of [`visit_iterable_elements`] once the value's own Envelop is deframed, so a
/// caller that needs the payload frame as well deframes it only once.
///
/// The walk and the decodes share one frame, because they need two different things from the
/// same deserializer: the walk needs the frame marker a [`with_deserializer`] child installs,
/// and a decode needs a parent that owns its storage. They cannot hold it at the same time, so
/// the walk pulls one item per borrow (see [`ElementProducer`]) and hands the ranges to the
/// `Stack` consumer, which then decodes each one through the same child — that child's own
/// walk being finished by then.
///
/// # Errors
/// Returns the errors [`for_each_element_range`] reports, plus whatever an element's
/// deserializer or `visit` returns (the walk stops there).
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize), and
/// `payload` MUST be the payload of the Envelop `deserializer` is positioned on.
unsafe fn visit_iterable_payload<N, T, A, F>(
    deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
    payload: (Offset, Offset),
    allocator: &A,
    visit: F,
) -> Result<(), Error>
where
    N: NamePolicy,
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
    F: FnMut(T) -> Result<(), Error>,
{
    // SAFETY: `payload` is the payload of the Envelop the deserializer is positioned on, and
    // the `Stack::with` call inside the callback (which this block covers, being the enclosing
    // unsafe scope) is bounded by the channel it walks.
    let failure = unsafe {
        with_deserializer(deserializer, payload, |channel| {
            let walk = RefCell::new(ElementProducer::new(channel, payload.1));
            let mut state = (visit, allocator);
            Stack::with(
                FRAME_CHANNEL_START..,
                |slots| walk.borrow_mut().produce(slots),
                |frames, state: &mut (F, &A)| {
                    walk.borrow_mut().consume::<N, T, F>(frames, state);
                },
                &mut state,
            );
            Ok(walk.into_inner().error)
        })?
    };
    match failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// Deserialize every element of the iterable `deserializer` frames into a fresh `Vec`,
/// together with the iterable payload's own absolute `(head, tail)` frame.
///
/// This is the shared body of the iterable deserializers that need an owned `Vec` of the
/// elements (`[T; N]`, which then builds its array from it). A caller that can push straight
/// into its own container should use [`visit_iterable_elements`] instead and skip this `Vec` —
/// it is the one intermediate element list the iterable path still allocates.
///
/// # Errors
/// Returns what [`visit_iterable_elements`] reports.
///
/// # Safety
/// Same contract as [`Deserialize::deserialize`](crate::Deserialize::deserialize).
pub unsafe fn deserialize_iterable_elements<N, T, A>(
    deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
    final_buffer: Option<Offset>,
    allocator: A,
) -> Result<IterableElements<T, A>, Error>
where
    N: NamePolicy,
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
{
    let (_, payload) = unsafe { envelop_payload(deserializer, final_buffer)? };
    let mut elements: Vec<T, A> = Vec::new_in(allocator.clone());
    unsafe {
        visit_iterable_payload::<N, T, A, _>(deserializer, payload, &allocator, |element| {
            elements.push(element);
            Ok(())
        })?;
    }
    Ok((elements, payload))
}
