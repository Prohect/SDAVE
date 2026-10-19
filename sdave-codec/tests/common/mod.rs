//! Helpers shared by the `sdave-codec` integration tests.
//!
//! Every test file is organised around one concern (`primitive`, `iterable`,
//! `composite`, `derive`, `boundary`, `features`); the small pieces they share —
//! limiter-pair factories, the type-name renderer, flattening, round-trip drivers and
//! the buffer-taking deserializer drivers — live here.
//!
//! The deserializer drivers ([`flat_deserializer`], [`deserialize_one`], [`items_of`],
//! [`single_item_buffer_as_envelop_payload`], [`iterable_element_ranges`]) build a
//! `FlatDeserializer` per call, i.e. they allocate the three `Vec`s the codec lends down
//! the recursion instead of recycling them. That is fine for a test that owns a finished
//! buffer, and it is exactly why the library does not offer them: a document is read
//! through one deserializer, handed from value to value.

#![allow(dead_code)]

use std::{
    alloc::{Allocator, Global},
    num::NonZeroUsize,
};

use sdave_codec::*;

/// Build a `LimiterPair` over byte slices.
///
/// The slice storage is leaked so the pair can live for `'static`.
pub fn pair(
    least_repeat: usize,
    limiter: &[u8],
    delimiter: &[u8],
) -> LimiterPair<'static, u8, Global> {
    let limiter: &'static Vec<u8> = Box::leak(Box::new(limiter.to_vec()));
    let delimiter: &'static Vec<u8> = Box::leak(Box::new(delimiter.to_vec()));
    LimiterPair::new(NonZeroUsize::new(least_repeat), limiter, delimiter)
}

/// The single limiter pair the primitive tests frame with.
pub fn primitive_pairs() -> Vec<LimiterPair<'static, u8, Global>> {
    vec![pair(2, b"^", b"~")]
}

/// Two distinct pairs, so nested Envelops never share a delimiter character.
pub fn pairs() -> Vec<LimiterPair<'static, u8, Global>> {
    vec![pair(2, b"^", b"~"), pair(2, b"?", b"!")]
}

/// Three distinct pairs: enough for one level of nested Envelops (the iterables).
pub fn iterable_pairs() -> Vec<LimiterPair<'static, u8, Global>> {
    vec![
        pair(2, b"^", b"~"),
        pair(2, b"?", b"!"),
        pair(2, b"#", b"$"),
    ]
}

/// Six distinct pairs: enough for the deepest nesting the tests reach.
pub fn many_pairs() -> Vec<LimiterPair<'static, u8, Global>> {
    vec![
        pair(2, b"^", b"~"),
        pair(2, b"?", b"!"),
        pair(2, b"#", b"$"),
        pair(2, b"%", b"&"),
        pair(2, b"*", b"+"),
        pair(2, b"=", b"@"),
    ]
}

/// Render `T`'s wire name under the [`NamePolicy`] `N`.
///
/// Every signature the codec writes goes through a `TypeName::render` call, so this is
/// the direct way a test reads one back without serializing a value.
pub fn render<N: NamePolicy, T: TypeName + ?Sized>() -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    T::render::<N>(&mut BufferWriter(&mut out)).unwrap();
    out
}

/// Flatten one `serialize` result by reading its slices in array order.
///
/// This is the *caller's* step, needed only to hand a deserializer one contiguous
/// document: SDAVE appends an Envelop's frame after its payload, so a serialize
/// result is in logical order but not in physical order. No sized impl flattens.
///
/// Empty slices are skipped, so an empty payload (e.g. `()`'s) flattens to none.
pub fn flatten(buffer: &[u8], slices: &[Option<(Offset, Offset)>]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for (head, tail) in slices.iter().flatten() {
        out.extend_from_slice(&buffer[head.get()..tail.get()]);
    }
    out
}

/// Serialize `value` (untyped) into one flattened document.
///
/// `pairs` is taken by `&mut` because serializing mutates each pair's reserved
/// `bsize`.
pub fn document<T>(value: &T, pairs: &mut [LimiterPair<'static, u8, Global>]) -> Vec<u8>
where
    T: Serialize<u8, Global> + ?Sized,
{
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        value.serialize::<NoShortening>(&mut buffer, Variant::V1, Strategy::FirstMatch, pairs)
    }
    .unwrap();
    let slices: Vec<_> = slices.into_iter().collect();
    flatten(&buffer, &slices)
}

/// Serialize `value`, flatten it, then deserialize it back, using `pairs`.
///
/// The set is cloned for the serializer (which mutates it); the deserializer then
/// gets the pristine set.
pub fn round_trip_with<T>(value: T, pairs: &[LimiterPair<'static, u8, Global>]) -> T
where
    T: Serialize<u8, Global> + Deserialize<u8, Global, Type = T> + PartialEq + core::fmt::Debug,
{
    let mut working = pairs.to_vec();
    let flat = document(&value, &mut working);
    deserialize_one::<NoShortening, T, _>(&flat, Variant::V1, pairs, None, Global).unwrap()
}

/// [`round_trip_with`] under the single primitive pair set.
pub fn round_trip<T>(value: T) -> T
where
    T: Serialize<u8, Global> + Deserialize<u8, Global, Type = T> + PartialEq + core::fmt::Debug,
{
    round_trip_with(value, &primitive_pairs())
}

/// Like [`round_trip_with`], for a type that is not `PartialEq`.
pub fn round_trip_no_eq<T>(value: &T, pairs: &[LimiterPair<'static, u8, Global>]) -> T
where
    T: Serialize<u8, Global> + Deserialize<u8, Global, Type = T>,
{
    let mut working = pairs.to_vec();
    let flat = document(value, &mut working);
    deserialize_one::<NoShortening, T, _>(&flat, Variant::V1, pairs, None, Global).unwrap()
}

/// The payload bytes of the single Envelop `value` serializes to.
pub fn payload_of<T>(value: &T, pairs: &[LimiterPair<'static, u8, Global>]) -> Vec<u8>
where
    T: Serialize<u8, Global> + ?Sized,
{
    let mut working = pairs.to_vec();
    let flat = document(value, &mut working);
    unsafe { single_item_buffer_as_envelop_payload(&flat, Variant::V1, pairs, None, Global) }
        .unwrap()
        .to_vec()
}

/// Serialize `text` as a `str` payload, then deserialize it as `T`.
///
/// This drives a deserializer from hand-written wire bytes instead of from `T`'s
/// own serializer, e.g. to check which payloads a type accepts or rejects.
pub fn from_text<T>(text: &str, pairs: &[LimiterPair<'static, u8, Global>]) -> Result<T, Error>
where
    T: Deserialize<u8, Global, Type = T>,
{
    let mut working = pairs.to_vec();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        text.serialize::<NoShortening>(&mut buffer, Variant::V1, Strategy::FirstMatch, &mut working)
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);
    deserialize_one::<NoShortening, T, _>(&flat, Variant::V1, pairs, None, Global)
}

// ---------------------------------------------------------------------------
// Buffer-taking deserializer drivers.
//
// Each one builds a `FlatDeserializer` of its own over a finished buffer — the first one
// just hands it back — and none of them recycles that instance, so each call allocates the
// three `Vec`s the codec otherwise lends down the recursion (`with_deserializer`). They live
// here, not in the crate, because a library function shaped like this invites exactly the
// per-value deserializer the design avoids; a test that owns a whole buffer has nothing to
// recycle into. Building one deserializer per document is the *caller's* step, so the crate
// offers none.
// ---------------------------------------------------------------------------

/// Build a [`FlatDeserializer`] over `buffer`, cloning `limiter_pairs` into the
/// `allocator`'s memory.
///
/// This is the test-side twin of the step every caller owns: the crate itself never builds a
/// deserializer, it only lends the one it is given ([`with_deserializer`]).
///
/// # Panics
/// Panics when `limiter_pairs` is empty: [`FlatDeserializer::new`](sdave_codec::FlatDeserializer::new)
/// requires at least one pair, and that is a caller invariant rather than something a document
/// can get wrong.
pub fn flat_deserializer<'a, 'l, T, A>(
    buffer: &'a [T],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, T, A>],
    allocator: &A,
) -> FlatDeserializer<'a, 'l, T, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let mut pairs: Vec<LimiterPair<'l, T, A>, A> = Vec::new_in(allocator.clone());
    for limiter_pair in limiter_pairs {
        pairs.push(limiter_pair.clone());
    }
    FlatDeserializer::new(
        buffer,
        variant,
        pairs,
        Vec::new_in(allocator.clone()),
        Vec::new_in(allocator.clone()),
    )
}

/// One-shot: build a [`FlatDeserializer`] over `buffer` and deserialize one value from it.
///
/// The parameter contract is the one documented on
/// [`Deserialize::deserialize`](sdave_codec::Deserialize::deserialize).
///
/// # Errors
/// Returns whatever `D`'s own `deserialize` reports.
///
/// # Panics
/// Panics when `limiter_pairs` is empty (see [`flat_deserializer`]).
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
    let mut deserializer = flat_deserializer(buffer, variant, limiter_pairs, &allocator);
    <D as Deserialize<u8, A>>::deserialize::<N>(&mut deserializer, final_buffer, allocator)
}

/// Deframe `buffer` and return the payload slice of its first Envelop.
///
/// Offsets are not reported because `buffer` *is* the origin here: they would be relative to
/// it, which is what indexing the returned slice already says.
///
/// # Errors
/// Returns what [`envelop_payload`] reports.
///
/// # Panics
/// Panics when `limiter_pairs` is empty (see [`flat_deserializer`]).
///
/// # Safety
/// Same contract as the `final_buffer` parameter of
/// [`Deserialize::deserialize`](sdave_codec::Deserialize::deserialize).
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
    let mut deserializer = flat_deserializer(buffer, variant, limiter_pairs, &allocator);
    let (origin, payload) = unsafe { envelop_payload(&mut deserializer, final_buffer)? };
    Ok(&origin[payload.0.get()..payload.1.get()])
}

/// Deframe `buffer` as a mixed channel, returning its logical items (phantom boundary markers
/// excluded) in order.
///
/// A payload produced by a composite [`Serialize`](sdave_codec::Serialize) is exactly such a
/// channel: a sequence of `NonEnvelop` signatures each followed by its value `Envelop`.
///
/// A `Variant::V2` Envelop that ends `buffer` comes back unarchived, so its delimiter is still
/// unconfirmed. Offsets are relative to `buffer`, which is the origin here.
///
/// # Panics
/// Panics when `limiter_pairs` is empty (see [`flat_deserializer`]).
pub fn items_of<'a, 'l, T, A>(
    buffer: &'a [T],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, T, A>],
    allocator: A,
) -> Vec<Item, A>
where
    T: Clone + PartialEq + Sized,
    A: Allocator + Clone,
{
    let mut deserializer = flat_deserializer(buffer, variant, limiter_pairs, &allocator);
    let mut result: Vec<Item, A> = Vec::new_in(allocator.clone());
    for item in deserializer.iter() {
        if !item.is_phantom() {
            result.push(item);
        }
    }
    result
}

/// The element byte-ranges of an iterable *payload*, in order.
///
/// An iterable payload is a leading empty marker Envelop followed by one Envelop per element
/// (optionally preceded by a `Signature` NonEnvelop when the iterable was serialized with
/// `serialize_typed`). The ranges are in `payload`'s own coordinates — there is no buffer in
/// front of it here — so an element can be deserialized from `&payload[head..tail]`, which
/// stays correct for a trailing `Variant::V2` Envelop whose `tail_offset` is still `None`.
///
/// The walk itself is the codec's ([`for_each_element_range`]): this only builds the one
/// deserializer a test owns and collects what the walk reports.
///
/// # Errors
/// Returns what [`for_each_element_range`] reports.
///
/// # Panics
/// Panics when `limiter_pairs` is empty (see [`flat_deserializer`]).
pub fn iterable_element_ranges<'l, A>(
    payload: &[u8],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'l, u8, A>],
    allocator: A,
) -> Result<Vec<(Offset, Offset), A>, Error>
where
    A: Allocator + Clone,
{
    let mut deserializer = flat_deserializer(payload, variant, limiter_pairs, &allocator);
    let mut ranges: Vec<(Offset, Offset), A> = Vec::new_in(allocator);
    unsafe {
        for_each_element_range(
            &mut deserializer,
            // The payload IS this deserializer's origin, so its frame starts at 0.
            (Offset::new(0), Offset::new(payload.len())),
            |head, tail| {
                ranges.push((head, tail));
                Ok(())
            },
        )?;
    }
    Ok(ranges)
}

// ---------------------------------------------------------------------------
// Structure dumps.
//
// A recursive walk of a buffer, so a test can see how SDAVE actually framed a document: every
// Envelop contributes its limiter (`L`), its payload (`E`) and — once archived — its delimiter
// (`D`); every NonEnvelop contributes itself (`N`). Offsets are absolute, so a dump reads
// top-down without knowing the depth it was produced at.
// ---------------------------------------------------------------------------

/// One slice of a dumped document.
///
/// Not `Debug`: [`Offset`] is not either, and a dump is read through [`print_dump`] rather than
/// as pairs of numbers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    /// An Envelop's limiter slice: `head .. payload_head`.
    L(Offset, Offset),
    /// A `NonEnvelop`: `head .. tail`.
    N(Offset, Offset),
    /// An Envelop's payload: `payload_head .. payload_tail`.
    E(Offset, Offset),
    /// An Envelop's delimiter slice: `payload_tail .. tail`.
    ///
    /// Absent while the Envelop is pending, i.e. while its delimiter is unconfirmed.
    D(Offset, Offset),
}

/// Dump `document` into its slices, descending into every Envelop payload.
///
/// `limiter_pairs` is the set the document was framed with; any set carrying the same
/// `(limiter, delimiter)` pairs reads it. `final_buffer` is the promise
/// [`Deserialize::deserialize`] takes — `Some` offset, equal to `document.len()`, for a complete
/// document; `None` dumps it as an open stream, where a `V2` Envelop that ends the buffer has no
/// confirmed delimiter.
pub fn dump(
    document: &[u8],
    variant: Variant,
    limiter_pairs: &[LimiterPair<'static, u8, Global>],
    final_buffer: Option<Offset>,
) -> Vec<SliceType> {
    let mut slice_types: Vec<SliceType> = Vec::new();
    dump_level(
        document,
        0,
        variant,
        limiter_pairs,
        final_buffer,
        &mut slice_types,
    );
    slice_types
}

/// Print the collected slices as one nested s-expression: `L` is `(`, `D` is `)`, and `N`/`E`
/// are the bytes they cover, read as UTF-8. An `E` whose payload did decompose prints nothing:
/// its children spell those bytes out already.
///
/// Sorted by head offset first. Sorting the `Offset`s themselves would come out backwards —
/// `NonMaxUsize`'s `Ord` is deliberately inverted — so sort the `usize`. The sort is stable,
/// which is what keeps an `E` ahead of the first slice of its own payload (they share a head).
pub fn print_dump(title: &str, document: &[u8], slice_types: &mut [SliceType]) {
    slice_types.sort_by_key(|slice| match slice {
        SliceType::L(head, _)
        | SliceType::N(head, _)
        | SliceType::E(head, _)
        | SliceType::D(head, _) => head.get(),
    });
    print!("--- {title} --- ");
    let limiter_fmt = "{";
    let delimiter_fmt = "}";
    for (index, slice) in slice_types.iter().enumerate() {
        match slice {
            SliceType::L(..) => print!("{}", limiter_fmt),
            SliceType::D(..) => print!("{}", delimiter_fmt),
            SliceType::N(head, tail) => print_slice(document, head, tail),
            SliceType::E(head, tail) => {
                if payload_is_undecomposed(&slice_types[..], index, head.get(), tail.get()) {
                    print_slice(document, head, tail);
                }
            }
        }
    }
    println!();
}

/// One level of a dump: `Stack`-collect this channel's items (a native frame each, no heap
/// `Vec<Item>`), convert them into [`SliceType`]s, and recurse into every Envelop's payload — a
/// payload is a channel of its own, which is what makes the nesting visible in one flat `Vec`.
///
/// `base` is where `channel` starts in the document being dumped, so offsets stay absolute.
///
/// `final_buffer` carries [`Deserialize::deserialize`]'s promise that `channel` is complete. A
/// complete channel is walked as a sentinel-confirmed copy, because `iter()` takes no such flag:
/// under `V2` an Envelop that ends the channel has no following non-delimiter to confirm its
/// delimiter, so it would come back unarchived — no `D`, and part of its own delimiter run
/// pushed back into its payload.
fn dump_level(
    channel: &[u8],
    base: usize,
    variant: Variant,
    limiter_pairs: &[LimiterPair<'static, u8, Global>],
    final_buffer: Option<Offset>,
    slice_types: &mut Vec<SliceType>,
) {
    /// Confirms a trailing `V2` delimiter run. Not a byte a printable `(limiter, delimiter)`
    /// pair uses, so it cannot merge into a run of its own.
    const SENTINEL: u8 = 0;

    /// Yield the channel's next item, or `None` at its end — bounded, which is what
    /// [`Stack::with`] requires of a producer.
    fn next_item<'a, 'd, 'l>(
        items: &mut FlatDeserializerIter<'a, 'd, 'l, u8, Global>,
    ) -> Option<Item> {
        items.next()
    }

    let mut padded: Vec<u8> = Vec::new();
    let parse: &[u8] = match final_buffer {
        Some(tail) => {
            assert_eq!(
                tail.get(),
                channel.len(),
                "[SDAVE] final_buffer offset MUST equal the channel length"
            );
            padded.reserve_exact(channel.len() + 1);
            padded.extend_from_slice(channel);
            padded.push(SENTINEL);
            &padded
        }
        None => channel,
    };
    let mut walker = flat_deserializer(parse, variant, limiter_pairs, &Global);

    let source = walker.iter();
    let consumer = |stack: &Stack<Item>, slice_types: &mut Vec<SliceType>| {
        // The sentinel is parsed like any other byte, so the item that ends the channel swallows
        // it: clamp every offset to `channel`, which is the only thing a dump is about.
        let end = channel.len();
        for item in stack.iter() {
            // A phantom item is a zero-width boundary marker, not a slice, and the sentinel's own
            // item starts past the channel: neither belongs in the dump.
            if item.is_phantom() || item.head_offset().get() >= end {
                continue;
            }
            match item {
                Item::N { non_envelop } => slice_types.push(SliceType::N(
                    Offset::new(base + non_envelop.head_offset.get()),
                    Offset::new(base + non_envelop.tail_offset.get().min(end)),
                )),
                Item::E { envelop } => {
                    let payload_head = envelop.payload_head_offset.get();
                    let payload_tail = envelop.payload_tail_offset.get().min(end);
                    // An Envelop is a limiter slice, its payload, and — once archived — a
                    // delimiter slice.
                    slice_types.push(SliceType::L(
                        Offset::new(base + envelop.head_offset.get()),
                        Offset::new(base + payload_head),
                    ));
                    slice_types.push(SliceType::E(
                        Offset::new(base + payload_head),
                        Offset::new(base + payload_tail),
                    ));
                    if let Some(tail_offset) = envelop.tail_offset {
                        slice_types.push(SliceType::D(
                            Offset::new(base + payload_tail),
                            Offset::new(base + tail_offset.get().min(end)),
                        ));
                    }
                    // Recurse. A leaf payload comes back as a single `N`, so the walk never has
                    // to know which payloads are composite.
                    dump_level(
                        &channel[payload_head..payload_tail],
                        base + payload_head,
                        variant,
                        limiter_pairs,
                        // A payload of a complete document is complete as well, so the recursion
                        // inherits the promise instead of guessing it.
                        final_buffer.map(|_| Offset::new(payload_tail - payload_head)),
                        slice_types,
                    );
                }
            }
        }
    };
    // SAFETY: `producer` is bounded by the channel's end, so the recursion terminates.
    unsafe { Stack::with(source, next_item, consumer, slice_types) };
}

/// Print the bytes `head..tail` as UTF-8.
fn print_slice(document: &[u8], head: &Offset, tail: &Offset) {
    // `get` rather than indexing: an offset can run past the document while an Envelop is
    // pending, and a dump MUST NOT panic on the states it exists to show.
    match document.get(head.get()..tail.get()) {
        Some(bytes) => print!("{}", String::from_utf8_lossy(bytes)),
        None => print!("<outside the document>"),
    }
}

/// Whether the payload `head..tail` holds no slice of its own — an empty payload, or one the
/// walk could not decompose. Only then are those bytes printed from the `E`, because only then
/// does nothing else print them: a decomposed payload prints them once through its `N`.
fn payload_is_undecomposed(
    slice_types: &[SliceType],
    itself: usize,
    head: usize,
    tail: usize,
) -> bool {
    !slice_types.iter().enumerate().any(|(index, slice)| {
        if index == itself {
            return false;
        }
        let (slice_head, slice_tail) = match slice {
            SliceType::L(head, tail)
            | SliceType::N(head, tail)
            | SliceType::E(head, tail)
            | SliceType::D(head, tail) => (head.get(), tail.get()),
        };
        slice_head >= head && slice_tail <= tail
    })
}
