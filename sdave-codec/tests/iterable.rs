//! The always-available iterables: slices `[T]` and arrays `[T; N]`.
//!
//! An array carries the iterable wire shape: one outer Envelop whose payload is a
//! leading empty marker Envelop followed by one Envelop per element (see the crate
//! README). A `&[T]` / `Box<[T]>` forwards to the unsized slice `[T]`, which is
//! the same shape. Containers (`Vec`, `VecDeque`, …) are feature-gated and tested
//! in `features.rs`.

mod common;

use std::alloc::Global;

use common::{
    deserialize_one, document, iterable_element_ranges, iterable_pairs, round_trip_with,
    single_item_buffer_as_envelop_payload,
};
use sdave_codec::*;

#[test]
fn array_round_trips() {
    let pairs = iterable_pairs();
    assert_eq!(round_trip_with([1u32, 2, 3], &pairs), [1u32, 2, 3]);

    // A length mismatch is rejected, reporting the iterable payload it read: the
    // element count is what disagreed, so the offsets are not pinned here.
    let mut working = pairs.clone();
    let flat = document(&[1u32, 2, 3], &mut working);
    let err =
        deserialize_one::<NoShortening, [u32; 2], _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap_err();
    assert!(
        matches!(
            err,
            Error::UnexpectedSignature {
                valid_signatures: &["[T; N]"],
                ..
            }
        ),
        "{err:?}",
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
        deserialize_one::<NoShortening, [u32; 3], _>(
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

/// The element ranges of an iterable payload: the split the codec's own element walk
/// (`for_each_element_range`) performs on the way to decoding each element.
#[test]
fn an_iterable_payload_reports_one_range_per_element() {
    let pairs = iterable_pairs();

    let mut working = pairs.clone();
    let flat = document(&[1u32, 2, 3], &mut working);
    let payload =
        unsafe { single_item_buffer_as_envelop_payload(&flat, Variant::V1, &pairs, None, Global) }
            .unwrap();
    let ranges = iterable_element_ranges(payload, Variant::V1, &pairs, Global).unwrap();
    assert_eq!(ranges.len(), 3);
    for (index, (head, tail)) in ranges.iter().enumerate() {
        assert!(
            head.get() < tail.get(),
            "element {index} is a non-empty Envelop, so its range cannot be empty",
        );
    }

    // An empty iterable is its lone marker Envelop, so it has no element range at all —
    // which is what keeps `[]` and `[""]` apart.
    let mut working = pairs.clone();
    let flat = document(&[0u32; 0], &mut working);
    let payload =
        unsafe { single_item_buffer_as_envelop_payload(&flat, Variant::V1, &pairs, None, Global) }
            .unwrap();
    assert!(
        iterable_element_ranges(payload, Variant::V1, &pairs, Global)
            .unwrap()
            .is_empty()
    );
}
