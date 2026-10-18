//! Helpers shared by the `sdave-codec` integration tests.
//!
//! Every test file is organised around one concern (`primitive`, `iterable`,
//! `composite`, `derive`, `boundary`, `features`); the small pieces they share —
//! limiter-pair factories, the type-name renderer, flattening and round-trip drivers —
//! live here.

#![allow(dead_code)]

use std::{alloc::Global, num::NonZeroUsize};

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
