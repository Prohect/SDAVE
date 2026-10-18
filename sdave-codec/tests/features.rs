//! The feature-gated impls, one feature per concrete type: the heap containers,
//! `Option`, `Result`, `String` and the pointer/cell wrappers. (`primitive` is gated
//! too, but it is the subject of `primitive.rs`.)
//!
//! Every test is `#[cfg]`-gated on the feature that provides its wire shape, so
//! the file compiles under any feature combination; run it with `--all-features`
//! to exercise everything. The always-available shapes are tested in
//! `primitive.rs`, `iterable.rs` and `composite.rs`.

// The feature matrix means some imports and helpers are unused under some
// combinations (e.g. `round_trip_no_eq` without `unsafe_cell`); that is expected.
#![allow(unused_imports, dead_code)]

mod common;

use std::alloc::Global;

use common::{flatten, many_pairs, round_trip_no_eq, round_trip_with};
use sdave_codec::*;
use sdave_derive::{Deserialize, Serialize};

/// Round-trip `value` under the deep pair set.
fn round_trip<T>(value: T) -> T
where
    T: Serialize<u8, Global> + Deserialize<u8, Global, Type = T> + PartialEq + core::fmt::Debug,
{
    round_trip_with(value, &many_pairs())
}

// ---------------------------------------------------------------------------
// Tagged enums.
// ---------------------------------------------------------------------------

#[cfg(feature = "option")]
#[test]
fn option_round_trips() {
    assert_eq!(round_trip(Some(5u32)), Some(5));
    assert_eq!(round_trip(None::<u32>), None);
    assert_eq!(round_trip(Some(Some(1u32))), Some(Some(1)));
}

#[cfg(feature = "result")]
#[test]
fn result_round_trips() {
    assert_eq!(round_trip(Ok::<u32, bool>(1)), Ok(1));
    assert_eq!(round_trip(Err::<u32, bool>(true)), Err(true));
    assert_eq!(round_trip(Ok::<u32, u32>(7)), Ok(7));
}

// ---------------------------------------------------------------------------
// Pointer wrappers.
// ---------------------------------------------------------------------------

#[cfg(feature = "boxed")]
#[test]
fn box_round_trips() {
    assert_eq!(round_trip(Box::new(5u32)), Box::new(5));
}

#[cfg(feature = "rc")]
#[test]
fn rc_round_trips() {
    assert_eq!(round_trip(std::rc::Rc::new(5u32)), std::rc::Rc::new(5));
}

#[cfg(feature = "arc")]
#[test]
fn arc_round_trips() {
    assert_eq!(
        round_trip(std::sync::Arc::new(5u32)),
        std::sync::Arc::new(5)
    );
}

#[cfg(all(feature = "cow", feature = "vec"))]
#[test]
fn cow_bytes_round_trip_to_the_owned_form() {
    use std::borrow::Cow;

    // `Cow<'_, [u8]>` deserializes via `Vec<u8>`.
    let borrowed: Cow<'static, [u8]> = Cow::Borrowed(&[1u8, 2, 3]);
    let expected: Cow<'static, [u8]> = Cow::Owned(vec![1u8, 2, 3]);
    assert_eq!(round_trip(borrowed), expected);
}

#[cfg(all(feature = "cow", feature = "string"))]
#[test]
fn cow_str_round_trips_to_the_owned_form() {
    use std::borrow::Cow;

    // `Cow<'_, str>` deserializes via `String`.
    let borrowed: Cow<'static, str> = Cow::Borrowed("hi");
    let expected: Cow<'static, str> = Cow::Owned(String::from("hi"));
    assert_eq!(round_trip(borrowed), expected);
}

// ---------------------------------------------------------------------------
// Cell wrappers.
// ---------------------------------------------------------------------------

#[cfg(feature = "cell")]
#[test]
fn cell_round_trips() {
    assert_eq!(
        round_trip(std::cell::Cell::new(5u32)),
        std::cell::Cell::new(5)
    );
}

#[cfg(feature = "unsafe_cell")]
#[test]
fn unsafe_cell_round_trips() {
    // `UnsafeCell<T>` is not `PartialEq`, so compare the inner values.
    let decoded = round_trip_no_eq(&std::cell::UnsafeCell::new(5u32), &many_pairs());
    // SAFETY: the cell is not aliased here.
    assert_eq!(unsafe { *decoded.get() }, 5);
}

// ---------------------------------------------------------------------------
// The owned string.
// ---------------------------------------------------------------------------

#[cfg(feature = "string")]
#[test]
fn string_round_trips() {
    assert_eq!(round_trip(String::from("hello")), String::from("hello"));
    assert_eq!(round_trip(String::new()), String::new());
    assert_eq!(round_trip(String::from("🦀")), String::from("🦀"));
}

#[cfg(feature = "string")]
#[test]
fn string_and_str_share_a_wire_shape() {
    // A document written from a `&str` deserializes into a `String` (a `String`
    // field renders the `str` signature, so the two interoperate).
    let pairs = many_pairs();
    let mut working = pairs.clone();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        "hi".serialize::<NoShortening>(&mut buffer, Variant::V1, Strategy::FirstMatch, &mut working)
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);
    let decoded =
        deserialize_one::<NoShortening, String, _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!(decoded, "hi");
}

// ---------------------------------------------------------------------------
// Heap containers.
// ---------------------------------------------------------------------------

#[cfg(feature = "vec")]
#[test]
fn vec_round_trips() {
    for values in [vec![], vec![42u32], vec![1u32, 2, 3], vec![u32::MAX, 0, 7]] {
        assert_eq!(round_trip(values.clone()), values);
    }
}

#[cfg(feature = "vec")]
#[test]
fn vec_of_vec_round_trips() {
    // `Vec<Vec<u8>>` makes the `[]` vs `[""]` distinction observable.
    let cases: [Vec<Vec<u8>>; 6] = [
        vec![],
        vec![vec![]],
        vec![vec![], vec![]],
        vec![vec![1, 2, 3]],
        vec![vec![9], vec![], vec![7, 8]],
        vec![vec![0; 300]],
    ];
    for values in cases {
        assert_eq!(round_trip(values.clone()), values);
    }
}

#[cfg(feature = "vec")]
#[test]
fn vec_typed_round_trips() {
    let pairs = many_pairs();
    let mut working = pairs.clone();
    let values: Vec<u32> = vec![7, 8, 9];
    let mut buffer: Vec<u8> = Vec::new();
    let name_head = buffer.len();
    buffer.extend_from_slice(b"v");
    let name = Name {
        head_offset: Offset::new(name_head),
        tail_offset: Offset::new(buffer.len()),
    };
    let slices = unsafe {
        values.serialize_typed::<NoShortening>(
            &mut buffer,
            name,
            Variant::V1,
            Strategy::FirstMatch,
            &mut working,
        )
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);

    // Payload = Signature NonEnvelop ++ empty marker Envelop ++ one Envelop per element.
    let payload =
        unsafe { single_item_buffer_as_envelop_payload(&flat, Variant::V1, &pairs, None, Global) }
            .unwrap();
    let items = items_of(payload, Variant::V1, &pairs, Global).unwrap();
    assert_eq!(items.len(), 2 + values.len());
    match &items[0] {
        Item::N { non_envelop } => {
            let signature = &payload[non_envelop.head_offset.get()..non_envelop.tail_offset.get()];
            assert!(signature.starts_with(b"v: "), "signature = {signature:?}");
        }
        _ => panic!("expected a leading signature NonEnvelop"),
    }

    let decoded =
        deserialize_one::<NoShortening, Vec<u32>, _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!(values, decoded);
}

#[cfg(feature = "vec_deque")]
#[test]
fn vec_deque_round_trips() {
    use std::collections::VecDeque;

    assert_eq!(
        round_trip(VecDeque::from([1u32, 2, 3])),
        VecDeque::from([1, 2, 3])
    );
}

#[cfg(feature = "linked_list")]
#[test]
fn linked_list_round_trips() {
    use std::collections::LinkedList;

    assert_eq!(
        round_trip(LinkedList::from([1u32, 2, 3])),
        LinkedList::from([1, 2, 3])
    );
}

#[cfg(feature = "btree_set")]
#[test]
fn btree_set_round_trips() {
    use std::collections::BTreeSet;

    assert_eq!(
        round_trip(BTreeSet::from([3u32, 1, 2])),
        BTreeSet::from([1, 2, 3])
    );
}

// ---------------------------------------------------------------------------
// The derive on the gated shapes.
// ---------------------------------------------------------------------------

#[cfg(feature = "linked_list")]
#[test]
fn derive_accepts_a_linked_list_field() {
    use std::collections::LinkedList;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Tagged {
        id: u32,
        items: LinkedList<u32>,
    }

    let value = Tagged {
        id: 1,
        items: LinkedList::from([4u32, 5, 6]),
    };
    assert_eq!(round_trip(value.clone()), value);

    let empty = Tagged {
        id: 2,
        items: LinkedList::new(),
    };
    assert_eq!(round_trip(empty.clone()), empty);
}

#[cfg(all(feature = "option", feature = "boxed"))]
#[test]
fn derive_accepts_an_owning_box_field() {
    // A non-recursive owning indirection: `Option<Box<Leaf>>`.
    #[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
    struct Leaf {
        value: u32,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Holder {
        id: u32,
        leaf: Option<Box<Leaf>>,
    }

    assert_eq!(
        round_trip(Holder {
            id: 1,
            leaf: Some(Box::new(Leaf { value: 2 })),
        }),
        Holder {
            id: 1,
            leaf: Some(Box::new(Leaf { value: 2 })),
        }
    );
    assert_eq!(
        round_trip(Holder { id: 3, leaf: None }),
        Holder { id: 3, leaf: None }
    );
}

#[cfg(feature = "string")]
#[test]
fn derive_accepts_a_string_field() {
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Named {
        id: u32,
        name: String,
    }

    let value = Named {
        id: 1,
        name: String::from("sdave"),
    };
    assert_eq!(round_trip(value.clone()), value);
}
