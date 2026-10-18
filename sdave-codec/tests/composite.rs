//! Hand-written composites: the `Serialize`/`Deserialize` scaffolds a codec user
//! builds by hand (the README's "manual impl" path).
//!
//! Three shapes, each covered end to end:
//!
//! - `Point`: the orderless composite — one `Signature` NonEnvelop + value Envelop
//!   per field, read back by matching signatures. It frames with the public
//!   primitives (`select_limiter_pair`, `append_frame`,
//!   `single_item_buffer_as_envelop_payload`, `items_of`) rather than the derive.
//! - `Word`: a `union`, which the codec refuses to derive — the application picks
//!   a canonical member and writes both impls itself.
//! - `ByteOffset`: a newtype whose wire form and invariant are its own, not its
//!   inner `u32`'s.
//!
//! The last test shows that once those impls exist, the container impls compose
//! with them for free.
//!
//! # A sized composite never flattens
//!
//! Every impl here is *sized*: its slice count is known at compile time, so
//! `serialize` returns that many `(head, tail)` slots and leaves the buffer exactly
//! as it appended it. The unflattened slice array IS the result — its layout is
//! `[limiter, payload, delimiter]`, or `[limiter, signature, value…, delimiter]`
//! for a typed value — and the tests below read the piece they care about straight
//! out of the array.
//!
//! Flattening in place is the *serialization-unsized* escape hatch: only an
//! iterable or a `dyn Trait` flattens inside its own impl, because its per-element
//! slice count is not known statically. A caller may still flatten once before
//! handing a value to a deserializer, since a deserializer takes one contiguous
//! document — but that step belongs to the caller, never to the impl.

mod common;

use std::alloc::Global;
use std::fmt::Write as _;

use common::{flatten, from_text, many_pairs, pair, round_trip_no_eq, round_trip_with};
use sdave_codec::*;
use sdave_derive::{Deserialize, Serialize};

/// The `Signature` NonEnvelop of the `x` field.
const X_SIGNATURE: &[u8] = b"x: u32";
/// The `Signature` NonEnvelop of the `y` field.
const Y_SIGNATURE: &[u8] = b"y: u32";

#[derive(Clone, Debug, PartialEq)]
struct Point {
    x: u32,
    y: u32,
}

impl Serialize<u8, Global> for Point {
    const SLICES: usize = 10;
    // [outer limiter, x signature, x value(3), y signature, y value(3), outer delimiter]
    type Slices = [Option<(Offset, Offset)>; 10];
    // `Slices` + 3 (outer limiter, signature, outer delimiter).
    type SlicesTyped = [Option<(Offset, Offset)>; 13];

    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, Global>],
    ) -> Result<Self::Slices, Error> {
        let head = buffer.len();
        let mut slices: Self::Slices = [None; 10];

        let signature_head = buffer.len();
        buffer.extend_from_slice(X_SIGNATURE);
        slices[1] = Some((Offset::new(signature_head), Offset::new(buffer.len())));
        let x = unsafe {
            self.x
                .serialize::<N>(buffer, variant, strategy, limiter_pairs)?
        };
        slices[2] = x[0];
        slices[3] = x[1];
        slices[4] = x[2];

        let signature_head = buffer.len();
        buffer.extend_from_slice(Y_SIGNATURE);
        slices[5] = Some((Offset::new(signature_head), Offset::new(buffer.len())));
        let y = unsafe {
            self.y
                .serialize::<N>(buffer, variant, strategy, limiter_pairs)?
        };
        slices[6] = y[0];
        slices[7] = y[1];
        slices[8] = y[2];

        let mut payload: [Option<&[u8]>; 8] = [None; 8];
        for (i, slot) in payload.iter_mut().enumerate() {
            *slot = slices[i + 1].map(|(head, tail)| &buffer[head.get()..tail.get()]);
        }
        let (index, repeats) = unsafe {
            select_limiter_pair(&buffer[..head], &payload, variant, strategy, limiter_pairs)?
        };
        let (limiter, delimiter) = append_frame(buffer, &limiter_pairs[index], repeats);
        slices[0] = Some(limiter);
        slices[9] = Some(delimiter);
        Ok(slices)
    }

    unsafe fn serialize_typed<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8>,
        name: Name,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, Global>],
    ) -> Result<Self::SlicesTyped, Error> {
        let outer_head = name.head_offset.get();
        if name.tail_offset.get() > outer_head {
            buffer.extend_from_slice(TYPE_SEPARATOR);
        }
        buffer.extend_from_slice(b"Point");
        let signature_tail = buffer.len();
        let inner = unsafe { self.serialize::<N>(buffer, variant, strategy, limiter_pairs)? };

        let mut payload: [Option<&[u8]>; 11] = [None; 11];
        payload[0] = Some(&buffer[outer_head..signature_tail]);
        for (i, slot) in payload[1..].iter_mut().enumerate() {
            *slot = inner[i].map(|(head, tail)| &buffer[head.get()..tail.get()]);
        }
        let (index, repeats) = unsafe {
            select_limiter_pair(
                &buffer[..outer_head],
                &payload,
                variant,
                strategy,
                limiter_pairs,
            )?
        };
        let (limiter, delimiter) = append_frame(buffer, &limiter_pairs[index], repeats);

        let mut slices: Self::SlicesTyped = [None; 13];
        slices[0] = Some(limiter);
        slices[1] = Some((Offset::new(outer_head), Offset::new(signature_tail)));
        slices[2..12].copy_from_slice(&inner);
        slices[12] = Some(delimiter);
        Ok(slices)
    }
}

impl Deserialize<u8, Global> for Point {
    type Type = Point;

    fn deserialize<N: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, u8, Global>,
        final_buffer: Option<Offset>,
        allocator: Global,
    ) -> Result<Self::Type, Error> {
        let payload = unsafe { envelop_payload(deserializer, final_buffer)? };
        // Two items per field, so the whole channel fits in a fixed array.
        let mut items: [Option<Item>; 4] = [None, None, None, None];
        unsafe {
            with_deserializer(deserializer, payload, |channel| {
                let _ = collect_items(channel, &mut items);
                Ok(())
            })?;
        }

        let mut x: Option<u32> = None;
        let mut y: Option<u32> = None;
        let mut index = 0;
        while index + 1 < items.len() {
            if let (Some(Item::N { non_envelop }), Some(Item::E { envelop })) =
                (&items[index], &items[index + 1])
            {
                let name = &payload[non_envelop.head_offset.get()..non_envelop.tail_offset.get()];
                let value_tail = envelop
                    .tail_offset
                    .unwrap_or(envelop.payload_tail_offset)
                    .get();
                let value = &payload[envelop.head_offset.get()..value_tail];
                let final_value = Some(Offset::new(value.len()));
                if name == X_SIGNATURE {
                    x = Some(unsafe {
                        with_deserializer(deserializer, value, |channel| {
                            u32::deserialize::<N>(channel, final_value, allocator)
                        })?
                    });
                } else if name == Y_SIGNATURE {
                    y = Some(unsafe {
                        with_deserializer(deserializer, value, |channel| {
                            u32::deserialize::<N>(channel, final_value, allocator)
                        })?
                    });
                }
            }
            index += 2;
        }
        Ok(Point {
            x: x.ok_or(Error::UnexpectedItem)?,
            y: y.ok_or(Error::UnexpectedItem)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Reading a serialize result without flattening.
// ---------------------------------------------------------------------------

/// The bytes one slot of a serialize result points at.
///
/// A sized composite returns its slices **unflattened**, so the piece a test cares
/// about is read straight out of the array — no deframing, no flattening.
fn bytes(buffer: &[u8], slice: Option<(Offset, Offset)>) -> &[u8] {
    let (head, tail) = slice.expect("this slot holds real bytes");
    &buffer[head.get()..tail.get()]
}

#[test]
fn composite_round_trip() {
    // Two distinct pairs let the outer Envelop and the nested field Envelops use
    // different frames, so no delimiter run is ever ambiguous.
    let pairs = vec![pair(2, b"^", b"~"), pair(2, b"?", b"!")];
    let point = Point { x: 42, y: 69 };

    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        point.serialize::<NoShortening>(
            &mut buffer,
            Variant::V1,
            Strategy::FirstMatch,
            &mut pairs.clone(),
        )
    }
    .unwrap();
    // The caller's step: `deserialize` takes one contiguous document, so the
    // unflattened result is read out in logical order first.
    let flat = flatten(&buffer, &slices);
    assert!(!flat.is_empty());

    let decoded =
        deserialize_one::<NoShortening, Point, _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!(point, decoded);
}

#[test]
fn composite_round_trip_typed() {
    let pairs = vec![pair(2, b"^", b"~"), pair(2, b"?", b"!")];
    let point = Point { x: 7, y: 8 };

    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    // The caller has already appended the field name `p`.
    let head = buffer.len();
    buffer.extend_from_slice(b"p");
    let name = Name {
        head_offset: Offset::new(head),
        tail_offset: Offset::new(buffer.len()),
    };
    let slices = unsafe {
        point.serialize_typed::<NoShortening>(
            &mut buffer,
            name,
            Variant::V1,
            Strategy::FirstMatch,
            &mut working,
        )
    }
    .unwrap();

    // `Point::serialize_typed` returns `[limiter, signature, inner…, delimiter]`, so
    // the signature it wrote is the second slot — still unflattened.
    assert_eq!(bytes(&buffer, slices[1]), b"p: Point");

    // The inner slots are in logical order but not contiguous (SDAVE appends the
    // frame *after* the payload), so the caller flattens them into the one
    // contiguous document `deserialize` takes. This step is the caller's, not the
    // impl's.
    let value = flatten(&buffer, &slices[2..slices.len() - 1]);
    let decoded =
        deserialize_one::<NoShortening, Point, _>(&value, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!(point, decoded);
}

#[test]
fn pointer_forwarding_keeps_the_referent_slices() {
    // `&T` / `Box<T, A>` MUST forward the concrete type's `SLICES`, including for
    // unsized referents such as `[U]`.
    assert_eq!(<&u32 as Serialize<u8, Global>>::SLICES, 3);
    assert_eq!(<&[u32] as Serialize<u8, Global>>::SLICES, 3);
    assert_eq!(<&Point as Serialize<u8, Global>>::SLICES, 10);
    assert_eq!(<Box<Point> as Serialize<u8, Global>>::SLICES, 10);
}

// ---------------------------------------------------------------------------
// A union: the wire design belongs to the application.
//
// The codec refuses to derive a union because nothing knows which member is live,
// so this is the hand-written shape the README asks for: pick a canonical member,
// write it, and rebuild the union from it.
// ---------------------------------------------------------------------------

/// A union the codec never derives.
///
/// Canonical design: every `Word` is written as its `as_u32` member, so its wire
/// shape is exactly `u32`'s and `as_float` is an in-memory view that never reaches
/// it. Reading `as_u32` is well defined even when the application last wrote
/// another member, because every `u32` bit pattern is a valid `u32`.
#[repr(C)]
union Word {
    as_u32: u32,
    as_float: f32,
}

impl TypeName for Word {
    const FULL: &'static str = concat!(module_path!(), "::Word");
}

impl Serialize<u8, Global> for Word {
    const SLICES: usize = <u32 as Serialize<u8, Global>>::SLICES;
    type Slices = <u32 as Serialize<u8, Global>>::Slices;
    type SlicesTyped = [Option<(Offset, Offset)>; 6];

    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, Global>],
    ) -> Result<Self::Slices, Error> {
        // SAFETY: `as_u32` is the canonical member (see the type doc), and reading
        // it needs no further invariant than that.
        let value = unsafe { self.as_u32 };
        unsafe {
            <u32 as Serialize<u8, Global>>::serialize::<N>(
                &value,
                buffer,
                variant,
                strategy,
                limiter_pairs,
            )
        }
    }

    unsafe fn serialize_typed<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8>,
        name: Name,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, Global>],
    ) -> Result<Self::SlicesTyped, Error> {
        let mut out: Self::SlicesTyped = [None; 6];
        unsafe {
            frame_typed_generic::<N, Self, Global>(
                buffer,
                name,
                self,
                variant,
                strategy,
                limiter_pairs,
                &mut out,
            )?;
        }
        Ok(out)
    }
}

impl Deserialize<u8, Global> for Word {
    type Type = Word;

    fn deserialize<N: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, u8, Global>,
        final_buffer: Option<Offset>,
        allocator: Global,
    ) -> Result<Self::Type, Error> {
        let value = u32::deserialize::<N>(deserializer, final_buffer, allocator)?;
        // A union value names exactly one member, and the canonical one is `as_u32`.
        Ok(Word { as_u32: value })
    }
}

#[test]
fn a_union_round_trips_through_its_canonical_member() {
    let pairs = many_pairs();

    // The canonical member IS the wire shape, and a sized composite hands back its
    // slice array unflattened, so the payload is the second slot.
    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let word = Word {
        as_u32: 0xDEAD_BEEF,
    };
    let slices = unsafe {
        word.serialize::<NoShortening>(&mut buffer, Variant::V1, Strategy::FirstMatch, &mut working)
    }
    .unwrap();
    assert_eq!(bytes(&buffer, slices[1]), b"3735928559");

    // Built through the other member, it is those same bits that reach the wire.
    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        Word { as_float: 1.0 }.serialize::<NoShortening>(
            &mut buffer,
            Variant::V1,
            Strategy::FirstMatch,
            &mut working,
        )
    }
    .unwrap();
    // `1.0f32.to_bits()` is `0x3F80_0000`, so those are the bits written here.
    assert_eq!(bytes(&buffer, slices[1]), b"1065353216");

    let decoded = round_trip_no_eq(&word, &pairs);
    // SAFETY: `decoded` was rebuilt from the canonical member.
    assert_eq!(unsafe { decoded.as_u32 }, 0xDEAD_BEEF);
}

#[test]
fn a_union_typed_signature_names_the_union() {
    let pairs = many_pairs();
    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let head = buffer.len();
    buffer.extend_from_slice(b"w");
    let name = Name {
        head_offset: Offset::new(head),
        tail_offset: Offset::new(buffer.len()),
    };
    let slices = unsafe {
        Word { as_u32: 7 }.serialize_typed::<NoShortening>(
            &mut buffer,
            name,
            Variant::V1,
            Strategy::FirstMatch,
            &mut working,
        )
    }
    .unwrap();

    // Typed layout: `[outer limiter, signature, value…, outer delimiter]`. The
    // describer is the hand-written `TypeName::FULL` — `Word`, not `u32`.
    assert_eq!(bytes(&buffer, slices[1]), b"w: composite::Word");
}

// ---------------------------------------------------------------------------
// A newtype with its own wire semantics.
// ---------------------------------------------------------------------------

/// A byte offset, written `@12` rather than the `12` its inner `u32` would write.
///
/// This is the case where a domain type must NOT reuse its inner type's impl: the
/// wire form carries a marker, and deserialization enforces an invariant the inner
/// type cannot express — `u32::MAX` is the "no offset" sentinel, so no `ByteOffset`
/// is ever built from it. `sdave::Offset` (a non-max `usize`, built through
/// `Offset::new`) is the same idea.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByteOffset(u32);

impl ByteOffset {
    /// `Some` for a real offset, `None` for the `u32::MAX` sentinel.
    fn new(offset: u32) -> Option<Self> {
        (offset < u32::MAX).then_some(Self(offset))
    }
}

impl TypeName for ByteOffset {
    const FULL: &'static str = concat!(module_path!(), "::ByteOffset");
}

/// The marker that makes an offset payload self-describing.
const OFFSET_MARKER: u8 = b'@';

impl Serialize<u8, Global> for ByteOffset {
    const SLICES: usize = 3;
    type Slices = [Option<(Offset, Offset)>; 3];
    type SlicesTyped = [Option<(Offset, Offset)>; 6];

    unsafe fn serialize<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8>,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, Global>],
    ) -> Result<Self::Slices, Error> {
        // The marker plus at most 10 decimal digits of a `u32`; a `ByteOffset`
        // never holds `u32::MAX`, so 11 bytes always suffice.
        let mut text = StackBuf::<11>::new();
        text.write_char(OFFSET_MARKER as char)
            .map_err(|_| Error::InvalidPayload)?;
        write!(text, "{}", self.0).map_err(|_| Error::InvalidPayload)?;
        unsafe { frame_slice(buffer, text.as_bytes(), variant, strategy, limiter_pairs) }
    }

    unsafe fn serialize_typed<N: NamePolicy>(
        &self,
        buffer: &mut Vec<u8>,
        name: Name,
        variant: Variant,
        strategy: Strategy,
        limiter_pairs: &mut [LimiterPair<'_, u8, Global>],
    ) -> Result<Self::SlicesTyped, Error> {
        let mut out: Self::SlicesTyped = [None; 6];
        unsafe {
            frame_typed_generic::<N, Self, Global>(
                buffer,
                name,
                self,
                variant,
                strategy,
                limiter_pairs,
                &mut out,
            )?;
        }
        Ok(out)
    }
}

impl Deserialize<u8, Global> for ByteOffset {
    type Type = ByteOffset;

    fn deserialize<N: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, u8, Global>,
        final_buffer: Option<Offset>,
        _allocator: Global,
    ) -> Result<Self::Type, Error> {
        let payload = unsafe { envelop_payload(deserializer, final_buffer)? };
        let text = core::str::from_utf8(payload).map_err(|_| Error::InvalidPayload)?;
        let digits = text
            .strip_prefix(OFFSET_MARKER as char)
            .ok_or(Error::InvalidPayload)?;
        let offset: u32 = digits.parse().map_err(|_| Error::InvalidPayload)?;
        ByteOffset::new(offset).ok_or(Error::InvalidPayload)
    }
}

#[test]
fn a_newtype_writes_its_own_payload_and_validates_it() {
    let pairs = many_pairs();
    let offset = ByteOffset::new(12).unwrap();

    // Its own payload — `@12`, not the `12` a bare `u32` writes — read out of the
    // unflattened slice array.
    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        offset.serialize::<NoShortening>(
            &mut buffer,
            Variant::V1,
            Strategy::FirstMatch,
            &mut working,
        )
    }
    .unwrap();
    assert_eq!(bytes(&buffer, slices[1]), b"@12");

    // It round-trips at both ends of its range...
    assert_eq!(round_trip_with(offset, &pairs), ByteOffset(12));
    assert_eq!(
        round_trip_with(ByteOffset(u32::MAX - 1), &pairs),
        ByteOffset(u32::MAX - 1),
    );

    // ...and enforces on the way in the invariant the inner type cannot express.
    assert_eq!(from_text::<ByteOffset>("@7", &pairs), Ok(ByteOffset(7)));
    assert_eq!(
        from_text::<ByteOffset>("@4294967295", &pairs),
        Err(Error::InvalidPayload),
        "`u32::MAX` is the no-offset sentinel",
    );
    assert_eq!(
        from_text::<ByteOffset>("12", &pairs),
        Err(Error::InvalidPayload),
        "the marker is part of the wire form",
    );
}

#[test]
fn a_newtype_typed_signature_names_the_newtype() {
    let pairs = many_pairs();
    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let head = buffer.len();
    buffer.extend_from_slice(b"off");
    let name = Name {
        head_offset: Offset::new(head),
        tail_offset: Offset::new(buffer.len()),
    };
    let slices = unsafe {
        ByteOffset(3).serialize_typed::<NoShortening>(
            &mut buffer,
            name,
            Variant::V1,
            Strategy::FirstMatch,
            &mut working,
        )
    }
    .unwrap();

    assert_eq!(bytes(&buffer, slices[1]), b"off: composite::ByteOffset");
}

// ---------------------------------------------------------------------------
// Composition: nothing else has to be written by hand.
// ---------------------------------------------------------------------------

/// Once a type has `Serialize` + `TypeName` + `Deserialize`, the container impls
/// pick it up on their own — the codec's iterable impl only asks for those bounds,
/// so `Vec<Word>`, `Vec<ByteOffset>` and a positional struct holding both work
/// without another line of codegen. Gated on `vec`, which is the feature that
/// provides `Vec<T, A>`.
#[cfg(feature = "vec")]
#[test]
fn hand_written_impls_compose_with_the_containers() {
    let pairs = many_pairs();

    // `Vec<ByteOffset>` — a plain (and `PartialEq`) element type.
    let offsets = vec![ByteOffset(0), ByteOffset(12), ByteOffset(u32::MAX - 1)];
    assert_eq!(round_trip_with(offsets.clone(), &pairs), offsets);

    // `Vec<Word>` — no `PartialEq`/`Debug`, hence the field-by-field comparison.
    let words = vec![
        Word { as_u32: 1 },
        Word {
            as_u32: 0xDEAD_BEEF,
        },
        Word { as_u32: 0 },
    ];
    let decoded = round_trip_no_eq(&words, &pairs);
    assert_eq!(decoded.len(), words.len());
    for (decoded, original) in decoded.iter().zip(&words) {
        // SAFETY: every element was rebuilt from the canonical member.
        assert_eq!(unsafe { decoded.as_u32 }, unsafe { original.as_u32 });
    }

    // Nesting composes just as freely: a `Vec` of a positional struct holding both
    // hand-written types, still with no impl written for the container. (A positional
    // composite is a struct — bare tuples have no codec impl.)
    #[derive(Serialize, Deserialize)]
    struct Pair(ByteOffset, Word);

    let nested = vec![
        Pair(ByteOffset(1), Word { as_u32: 2 }),
        Pair(ByteOffset(3), Word { as_u32: 4 }),
    ];
    let decoded = round_trip_no_eq(&nested, &pairs);
    assert_eq!(decoded.len(), nested.len());
    for (decoded, original) in decoded.iter().zip(&nested) {
        assert_eq!(decoded.0, original.0);
        // SAFETY: every element was rebuilt from the canonical member.
        assert_eq!(unsafe { decoded.1.as_u32 }, unsafe { original.1.as_u32 });
    }
}
