//! The always-available iterables: slices `[T]` and arrays `[T; N]`.
//!
//! An array carries the iterable wire shape: one outer Envelop whose payload is a
//! leading empty marker Envelop followed by one Envelop per element (see the crate
//! README). A `&[T]` / `Box<[T]>` forwards to the unsized slice `[T]`, which is
//! the same shape. Containers (`Vec`, `VecDeque`, …) are feature-gated and tested
//! in `features.rs`.

mod common;

use std::alloc::Global;

use common::{document, iterable_pairs, round_trip_with};
use sdave_codec::*;

#[test]
fn array_round_trips() {
    let pairs = iterable_pairs();
    assert_eq!(round_trip_with([1u32, 2, 3], &pairs), [1u32, 2, 3]);

    // A length mismatch is rejected.
    let mut working = pairs.clone();
    let flat = document(&[1u32, 2, 3], &mut working);
    assert_eq!(
        <[u32; 2] as Deserialize<u8, Global>>::deserialize::<NoShortening>(
            &flat,
            Variant::V1,
            &pairs,
            None,
            Global,
        )
        .unwrap_err(),
        Error::UnexpectedItem,
    );

    // The empty array round-trips through the lone marker Envelop.
    assert_eq!(round_trip_with([0u32; 0], &pairs), [0u32; 0]);
}

#[test]
fn slice_and_boxed_slice_forward_to_the_array_shape() {
    let pairs = iterable_pairs();
    let array = [4u32, 5, 6];
    let mut working = pairs.clone();
    let from_array = document(&array, &mut working);

    // `&[T]` and `Box<[T]>` serialize to exactly the array's document.
    let slice: &[u32] = &array;
    let mut working = pairs.clone();
    assert_eq!(document(&slice, &mut working), from_array);

    let boxed: Box<[u32]> = array.to_vec().into_boxed_slice();
    let mut working = pairs.clone();
    assert_eq!(document(&boxed, &mut working), from_array);

    // …and that document deserializes back into the array.
    assert_eq!(
        <[u32; 3] as Deserialize<u8, Global>>::deserialize::<NoShortening>(
            &from_array,
            Variant::V1,
            &pairs,
            None,
            Global,
        )
        .unwrap(),
        array,
    );
}
