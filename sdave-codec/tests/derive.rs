//! Tests for the `sdave-derive` derive macros.
//!
//! The derive lives in `sdave-derive`, but the generated impls target
//! `sdave_codec`, so the tests live here where that crate is in scope.

mod common;

use std::alloc::{AllocError, Allocator, Global, Layout};
use std::any::Any;
use std::num::NonZeroUsize;

use common::{
    deserialize_one, document, dump, flat_deserializer, flatten, items_of, many_pairs, pair, pairs,
    print_dump, render, round_trip_with, single_item_buffer_as_envelop_payload,
};
use sdave_codec::*;
use sdave_derive::{Deserialize, Serialize, sdave_dyn};

/// The heap allocations of a [`FlatDeserializer`], kept alive between buffers.
///
/// [`FlatDeserializer::decouple`] hands the `repeat_map`/`limiter_pairs`/
/// `archived_boundaries` vectors back, so the next `FlatDeserializer` can be built
/// from them instead of allocating again. The only thing that MUST happen in
/// between is [`update_internal_cache`] with `None`, because a `LimiterPair::bsize`
/// bitmap only means anything relative to the buffer that produced it. All of this
/// is cheap because the state's shallow `clone` never drags those vectors along.
#[derive(Default)]
struct ParseResources {
    limiter_pairs: Vec<LimiterPair<'static, u8, Global>>,
    repeat_map: Vec<Option<NonMaxUsize>, Global>,
    archived_boundaries: Vec<Envelop, Global>,
}

impl ParseResources {
    fn new(limiter_pairs: Vec<LimiterPair<'static, u8, Global>>) -> Self {
        Self {
            limiter_pairs,
            ..Self::default()
        }
    }

    /// The resource-reusing equivalent of `items_of`: parse `buffer` and return its
    /// non-phantom items together with the final [`InternalState`].
    fn items(&mut self, buffer: &[u8], variant: Variant) -> (Vec<Item>, InternalState) {
        // Move the allocations into the deserializer...
        let mut limiter_pairs = core::mem::take(&mut self.limiter_pairs);
        let mut repeat_map = core::mem::take(&mut self.repeat_map);
        // ...after resetting the per-pair cache, which is relative to the buffer it
        // was built on.
        unsafe { update_internal_cache(&mut repeat_map, &mut limiter_pairs, None) };
        let mut deserializer = FlatDeserializer::new(
            buffer,
            variant,
            limiter_pairs,
            core::mem::take(&mut self.archived_boundaries),
            repeat_map,
        );
        let items = deserializer
            .iter()
            .filter(|item| !item.is_phantom())
            .collect();
        // ...and take them back out, leaving valid-but-empty ones behind.
        let (_, archived_boundaries, _, mut state) = deserializer.decouple();
        let internal = state.internal().clone();
        // The shallow `clone` is what keeps this cheap: it carries none of the
        // allocations over.
        let shallow = unsafe { state.clone() };
        assert!(unsafe { shallow.repeat_map() }.is_none());
        assert!(shallow.limiter_pairs().is_none());
        let repeat_map = unsafe { state.swap_repeat_map(Some(Vec::new())) };
        let limiter_pairs = unsafe { state.swap_limiter_pairs(Some(Vec::new())) };
        self.archived_boundaries = archived_boundaries.unwrap();
        self.repeat_map = repeat_map.unwrap_or_default();
        self.limiter_pairs = limiter_pairs.unwrap_or_default();
        (items, internal)
    }
}

/// Serialize `$value`, flatten it, deserialize into `$ty` and compare.
///
/// Every buffer here is parsed through one reused [`ParseResources`] set, so the
/// `FlatDeserializer` allocations survive from one parse to the next instead of
/// being re-made.
macro_rules! assert_round_trip {
    ($ty:ty, $value:expr) => {{
        let value: $ty = $value;
        let mut pairs = pairs();
        let mut resources = ParseResources::new(pairs.clone());
        let mut buffer: Vec<u8> = Vec::new();

        let slices = unsafe {
            value.serialize::<NoShortening>(
                &mut buffer,
                Variant::V1,
                Strategy::FirstMatch,
                &mut pairs,
            )
        }
        .unwrap();
        let flat = flatten(&buffer, &slices);
        assert!(!flat.is_empty());
        let decoded = deserialize_one::<NoShortening, $ty, _>(
            &flat,
            Variant::V1,
            &pairs,
            None,
            Global::default(),
        )
        .unwrap();
        assert_eq!(value, decoded);

        let len = buffer.len();
        buffer.extend(b"name");
        let slices = unsafe {
            value.serialize_typed::<NoShortening>(
                &mut buffer,
                Name {
                    head_offset: Offset::new(len),
                    tail_offset: Offset::new(len + 4),
                },
                Variant::V1,
                Strategy::FirstMatch,
                &mut pairs,
            )
        }
        .unwrap();
        let flat = flatten(&buffer, &slices);
        assert!(!flat.is_empty());
        let (items, _) = resources.items(&flat, Variant::V1);
        assert_eq!(items.len(), 1);
        let Item::E { envelop } = &items[0] else {
            panic!("expect Envelop")
        };
        let body = &flat[envelop.payload_head_offset.get()..envelop.payload_tail_offset.get()];
        // Offsets of `body_items` are relative to `body`, not to `flat`.
        let (body_items, _) = resources.items(body, Variant::V1);
        assert_eq!(body_items.len(), 2);
        let Item::E { envelop } = &body_items[1] else {
            panic!("expect Envelop")
        };
        // V1 has no delimiter-confirmation latency, so this nested Envelop is
        // already archived and its `tail_offset` is final.
        let value_bytes = &body[envelop.head_offset.get()..envelop.tail_offset.unwrap().get()];
        let decoded = deserialize_one::<NoShortening, $ty, _>(
            value_bytes,
            Variant::V1,
            &pairs,
            None,
            Global::default(),
        )
        .unwrap();
        assert_eq!(value, decoded);

        // NOTE: this macro round-trips under `Variant::V1`, whose Envelops are
        // closed by their delimiter itself. A `Variant::V2` Envelop that ends the
        // buffer stays unterminated, so its whole-buffer deframing needs the
        // `final_buffer` flag; that path is covered in `boundary.rs`.
    }};
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Named {
    x: u32,
    y: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Tuple(u32, bool);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Unit;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Nested {
    a: u32,
    inner: Named,
}

// ---------------------------------------------------------------------------
// Enums.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Plain {
    V0,
    V1(u32),
    V2 { x: u32, y: usize },
}

// ---------------------------------------------------------------------------
// Generic (type-parameterised) types. Their `Slices` can't be sized from their
// fields' compile-time `SLICES`, so they take the flattened iterable/`dyn` path.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Generic<T> {
    id: u32,
    value: T,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GenericTuple<T>(u32, T);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum GenericEnum<T> {
    Empty,
    One(T),
    Two { id: u32, value: T },
}

// ---------------------------------------------------------------------------
// dyn: `#[sdave_dyn]`.
//
// The trait lists the concrete types it can deserialize; the macro adds `Any` as
// a supertrait so serialization can recover the concrete value from the trait
// object.
// ---------------------------------------------------------------------------

#[sdave_dyn(Unit, Named, Tuple, Nested)]
trait MyTrait {}

impl MyTrait for Unit {}
impl MyTrait for Named {}
impl MyTrait for Tuple {}
impl MyTrait for Nested {}

#[derive(Serialize, Deserialize)]
struct Container {
    id: u32,
    tr: Box<dyn MyTrait>,
}

// ---------------------------------------------------------------------------
// `#[derive(Serialize)]` / `#[derive(Deserialize)]`.
// ---------------------------------------------------------------------------

#[test]
fn named_round_trip() {
    assert_round_trip!(Named, Named { x: 42, y: true });
}

#[test]
fn tuple_round_trip() {
    assert_round_trip!(Tuple, Tuple(7, false));
}

#[test]
fn unit_round_trip() {
    assert_round_trip!(Unit, Unit);
}

#[test]
fn nested_round_trip() {
    assert_round_trip!(
        Nested,
        Nested {
            a: 1,
            inner: Named { x: 2, y: false },
        }
    );
}

#[test]
fn enum_round_trip() {
    assert_round_trip!(Plain, Plain::V0);
    assert_round_trip!(Plain, Plain::V1(9));
    assert_round_trip!(Plain, Plain::V2 { x: 3, y: 4 });
}

#[test]
fn generic_round_trip() {
    assert_round_trip!(Generic<u32>, Generic { id: 1, value: 42 });
    assert_round_trip!(GenericTuple<bool>, GenericTuple(7, true));
    assert_round_trip!(GenericEnum<u32>, GenericEnum::Empty);
    assert_round_trip!(GenericEnum<u32>, GenericEnum::One(9));
    assert_round_trip!(GenericEnum<u32>, GenericEnum::Two { id: 3, value: 4 });
}

// ---------------------------------------------------------------------------
// `#[sdave_dyn]`.
// ---------------------------------------------------------------------------

/// Four distinct pairs, which is the minimum that always frames: under `V1` one frame can
/// disable at most three of them — its payload may start with one pair's limiter and end with
/// another pair's delimiter, and the buffer before it may end with a third pair's limiter — so a
/// fourth always survives.
fn dyn_pairs() -> Vec<LimiterPair<'static, u8, Global>> {
    vec![
        pair(2, b"^", b"~"),
        pair(2, b"?", b"!"),
        pair(2, b"#", b"$"),
        pair(2, b"%", b"&"),
    ]
}

/// Serialize `value`, flatten it, then deserialize it back.
fn dyn_round_trip(value: Container) -> Container {
    let mut pairs = dyn_pairs();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        value.serialize::<NoShortening>(&mut buffer, Variant::V1, Strategy::FirstMatch, &mut pairs)
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);
    deserialize_one::<NoShortening, Container, _>(&flat, Variant::V1, &pairs, None, Global).unwrap()
}

/// The `Any` view of a `#[sdave_dyn]` trait object: the trait has no accessor of its own, so
/// this is how a test gets the concrete value back out.
fn as_any<'a>(value: &'a (dyn MyTrait + 'static)) -> &'a (dyn Any + 'static) {
    value
}

/// The deepest `dyn` payload the tests use: a [`Nested`], whose [`Named`] adds two more fields.
fn nested_container() -> Container {
    Container {
        id: 7,
        tr: Box::new(nested_value()),
    }
}

/// What [`nested_container`] carries in its `dyn` field.
fn nested_value() -> Nested {
    Nested {
        a: 8,
        inner: Named { x: 9, y: true },
    }
}

#[test]
fn dyn_trait_round_trip() {
    let decoded = dyn_round_trip(Container {
        id: 3,
        tr: Box::new(Named { x: 5, y: true }),
    });
    assert_eq!(decoded.id, 3);
    assert_eq!(
        *as_any(&*decoded.tr).downcast_ref::<Named>().expect("Named"),
        Named { x: 5, y: true },
    );

    let decoded = dyn_round_trip(Container {
        id: 4,
        tr: Box::new(Unit),
    });
    assert_eq!(decoded.id, 4);
    assert!(as_any(&*decoded.tr).is::<Unit>());

    let decoded = dyn_round_trip(Container {
        id: 5,
        tr: Box::new(Tuple(6, false)),
    });
    assert_eq!(
        *as_any(&*decoded.tr).downcast_ref::<Tuple>().expect("Tuple"),
        Tuple(6, false),
    );

    let decoded = dyn_round_trip(nested_container());
    assert_eq!(decoded.id, 7);
    assert_eq!(
        *as_any(&*decoded.tr)
            .downcast_ref::<Nested>()
            .expect("Nested"),
        nested_value(),
    );
}

/// A named field is matched by its `Signature`, so the order the fields arrive in is irrelevant
/// — including for the innermost field of a `dyn` value, which is as deep as a foreign writer
/// can reach. This is the codec's orderless contract (`sdave-codec/README.md`).
///
/// The document is the one `dyn_round_trip` round-trips, with two of its segments swapped.
/// `deserialize` takes **one value Envelop**: the typed wrapper `serialize_typed` writes
/// (`Envelop(Signature ++ Envelop(value))`) is not its input and is stripped first — feeding it
/// whole is an `UnexpectedSignature` before any field is looked at.
#[test]
fn named_fields_deserialize_in_any_order_through_a_dyn_field() {
    // The same pair set `dyn_round_trip` uses, so the only difference between the two documents
    // below is the order of `Named`'s fields.
    let pairs = dyn_pairs();
    let container = nested_container();
    let mut working = pairs.clone();
    let ordered = document(&container, &mut working);

    // The innermost fields exactly as `pairs` + `Strategy::FirstMatch` writes them: each
    // field's `Signature` NonEnvelop followed by its leaf's Envelop (`^^` limiter, `~~`
    // delimiter, the pair's 2 repeats). Swapping the two segments leaves every length and
    // every frame intact, so only the arrival order differs.
    let written: &[u8] = b"x: u32^^9~~y: bool^^true~~";
    let swapped: &[u8] = b"y: bool^^true~~x: u32^^9~~";
    let at = ordered
        .windows(written.len())
        .position(|window| window == written)
        .expect("the innermost `Named` writes both fields, in declaration order");
    let mut orderless = ordered[..at].to_vec();
    orderless.extend_from_slice(swapped);
    orderless.extend_from_slice(&ordered[at + written.len()..]);

    // `document` produced ONE value Envelop, which is exactly what `deserialize` takes.
    let decoded = deserialize_one::<NoShortening, Container, _>(
        &orderless,
        Variant::V1,
        &pairs,
        None,
        Global,
    )
    .unwrap();
    assert_eq!(decoded.id, container.id);
    assert_eq!(
        *as_any(&*decoded.tr)
            .downcast_ref::<Nested>()
            .expect("Nested"),
        nested_value(),
    );
    let orderless = b"??id: u32^^7~~tr: derive::dyn MyTrait??derive::Nested??a: u32^^8~~inner: derive::Named??y: bool^^true~~x: u32^^9~~!!!!!!!!";
    let de = Container::deserialize::<NoShortening>(
        &mut flat_deserializer(orderless, Variant::V2, &dyn_pairs(), &Global),
        Some(Offset::new(orderless.len())),
        Global,
    )
    .unwrap();
    assert_eq!(de.id, nested_container().id);
    assert_eq!(
        *as_any(&*de.tr).downcast_ref::<Nested>().expect("Nested"),
        Nested {
            a: 8,
            inner: Named { x: 9, y: true },
        },
    );
    let malformed_payload = b"??id: u32^^7~~tr: derive::dyn MyTrait??derive::Nested??a: u32^^8~~inner: derive::Named??y: bool^^true~~x: u32^^9i~~!!!!!!!!";
    let malformed_signature = b"??id: u32^^7~~tr: derive::dyn MyTrait??derive::Nested??a: u32^^8~~inner: derive::Named??y: bool^^true~~x: u31^^9~~!!!!!!!!";
    let missing_envelop_expected = b"??id: u32^^7~~tr: derive::dyn MyTrait??derive::Nested??a: u32^^8~~inner: derive::Named??y: bool^^true~~x: u32!!!!!!!!";

    // The three ways this document can go wrong, deserialized and rendered in place: the buffer
    // they were read from is still here, so `fmt_error` quotes exactly what each report points at.
    // Run the test with `--nocapture` to read the renderings.
    let malformed: [&[u8]; 3] = [
        malformed_payload,   // the innermost `x: u32` payload is `9i`, which is not a `u32`
        malformed_signature, // the innermost signature is `x: u31`, which no field accepts
        missing_envelop_expected, // the innermost signature has no Envelop behind it
    ];
    let mut rendered: Vec<String> = Vec::new();
    for document in malformed {
        let error = deserialize_one::<NoShortening, Container, _>(
            document,
            Variant::V2,
            &dyn_pairs(),
            Some(Offset::new(document.len())),
            Global,
        )
        .err()
        .unwrap();
        let line = fmt_error(error, document).to_string();
        eprintln!("{line}");
        rendered.push(line);
    }
    // A bad leaf payload, a signature this type does not accept (reported at `x: u31` itself), and
    // a signature whose Envelop is missing.
    assert!(
        rendered[0].starts_with("InvalidPayload { type: u32, caught: ["),
        "{}",
        rendered[0],
    );
    assert!(
        rendered[1].starts_with(r#"UnexpectedSignature { valid_signatures: ["x", "y"], caught: ["#),
        "{}",
        rendered[1],
    );
    assert!(rendered[1].ends_with(r#") "x: u31" }"#), "{}", rendered[1],);
    assert!(
        rendered[2].starts_with("MissingEnvelop { required_by: ["),
        "{}",
        rendered[2],
    );
}

/// A *typed* document — `Envelop(NonEnvelop(signature) ++ Envelop(value))`, which is what
/// `serialize_typed` writes and a parent's field reader consumes — is read through the value
/// Envelop inside it, under `V2`, with the field order it arrives in.
#[test]
fn a_typed_document_reads_through_its_value_envelop() {
    let container = nested_container();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        container
            .serialize_typed::<NoShortening>(
                &mut buffer,
                Name {
                    head_offset: Offset::new(0),
                    tail_offset: Offset::new(0),
                },
                Variant::V2,
                Strategy::LeastRepeat,
                &mut dyn_pairs(),
            )
            .unwrap()
    };
    // What `serialize_typed` writes with this pair set, then the same document with `Named`'s two
    // fields swapped.
    let ordered = b"??derive::Container??id: u32^^7~~tr: derive::dyn MyTrait??derive::Nested??a: u32^^8~~inner: derive::Named??x: u32^^9~~y: bool^^true~~!!!!!!!!!!";
    let orderless = b"??derive::Container??id: u32^^7~~tr: derive::dyn MyTrait??derive::Nested??a: u32^^8~~inner: derive::Named??y: bool^^true~~x: u32^^9~~!!!!!!!!!!";
    assert_eq!(flatten(&buffer, &slices), ordered);
    println!("ok");

    // Both are complete buffers, so both dumps are given `final_buffer`: the two structures differ
    // only in `Named`'s innermost pair of groups.
    let mut ordered_slices = dump(
        ordered,
        Variant::V2,
        &dyn_pairs(),
        Some(Offset::new(ordered.len())),
    );
    print_dump("ordered:  ", ordered, &mut ordered_slices);
    let mut orderless_slices = dump(
        orderless,
        Variant::V2,
        &dyn_pairs(),
        Some(Offset::new(orderless.len())),
    );
    print_dump("orderless:", orderless, &mut orderless_slices);
}

// ---------------------------------------------------------------------------
// Custom allocator.
//
// The generated impls are generic over `A: Allocator + Clone`, so they work with
// any allocator, not just `Global`.
// ---------------------------------------------------------------------------

/// A cloneable custom allocator that just delegates to `Global`.
#[derive(Clone, Copy, Default)]
struct DelegatingAllocator;

unsafe impl Allocator for DelegatingAllocator {
    fn allocate(&self, layout: Layout) -> Result<std::ptr::NonNull<[u8]>, AllocError> {
        Global.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: std::ptr::NonNull<u8>, layout: Layout) {
        unsafe { Global.deallocate(ptr, layout) }
    }
}

/// Build a `'static` byte pair whose slices are owned by `allocator`.
fn pair_in<A: Allocator + Clone>(
    allocator: A,
    least_repeat: usize,
    limiter: &[u8],
    delimiter: &[u8],
) -> LimiterPair<'static, u8, A> {
    let mut limiter_vec: Vec<u8, A> = Vec::new_in(allocator.clone());
    limiter_vec.extend_from_slice(limiter);
    let mut delimiter_vec: Vec<u8, A> = Vec::new_in(allocator.clone());
    delimiter_vec.extend_from_slice(delimiter);
    let limiter: &'static Vec<u8, A> = Box::leak(Box::new_in(limiter_vec, allocator.clone()));
    let delimiter: &'static Vec<u8, A> = Box::leak(Box::new_in(delimiter_vec, allocator));
    LimiterPair::new(NonZeroUsize::new(least_repeat), limiter, delimiter)
}

#[test]
fn custom_allocator_round_trip() {
    let allocator = DelegatingAllocator;
    let value = Nested {
        a: 5,
        inner: Named { x: 6, y: true },
    };

    let mut pairs: Vec<LimiterPair<'static, u8, DelegatingAllocator>, DelegatingAllocator> =
        Vec::new_in(allocator);
    pairs.push(pair_in(allocator, 2, b"^", b"~"));
    pairs.push(pair_in(allocator, 2, b"?", b"!"));

    let mut buffer: Vec<u8, DelegatingAllocator> = Vec::new_in(allocator);
    let slices = unsafe {
        value.serialize::<NoShortening>(&mut buffer, Variant::V1, Strategy::FirstMatch, &mut pairs)
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);

    let decoded =
        deserialize_one::<NoShortening, Nested, _>(&flat, Variant::V1, &pairs, None, allocator)
            .unwrap();
    assert_eq!(value, decoded);
}

// ---------------------------------------------------------------------------
// Capability: iterable fields, generics, orderless fields and the name policy.
// ---------------------------------------------------------------------------

/// A struct mixing a scalar field with an iterable one.
#[cfg(feature = "vec")]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WithVec {
    id: u32,
    items: Vec<u32>,
}

#[cfg(feature = "vec")]
#[test]
fn struct_with_an_iterable_field_round_trips() {
    let pairs = many_pairs();
    let value = WithVec {
        id: 7,
        items: vec![1, 2, 3],
    };
    assert_eq!(round_trip_with(value.clone(), &pairs), value);
    // The empty iterable keeps its marker Envelop, so it round-trips distinctly.
    let empty = WithVec {
        id: 9,
        items: vec![],
    };
    assert_eq!(round_trip_with(empty.clone(), &pairs), empty);
}

/// A const-generic type takes the flattened path and still round-trips.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ConstArray<const N: usize> {
    items: [u32; N],
}

#[test]
fn const_generic_struct_round_trips() {
    let pairs = many_pairs();
    let value = ConstArray {
        items: [4u32, 5, 6],
    };
    assert_eq!(round_trip_with(value.clone(), &pairs), value);
}

/// Writes its two fields in the opposite order to [`ReadsXY`].
#[derive(Serialize)]
struct WritesYX {
    y: usize,
    x: u32,
}

/// Declares the same two fields in the natural order.
#[derive(Deserialize)]
struct ReadsXY {
    x: u32,
    y: usize,
}

#[test]
fn fields_deserialize_regardless_of_order() {
    // `WritesYX` writes `y` before `x`; `ReadsXY` matches by `Signature`, so the
    // order fields arrive in is irrelevant (the README's orderless contract).
    let mut pairs = pairs();
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        WritesYX { y: 9, x: 4 }.serialize::<NoShortening>(
            &mut buffer,
            Variant::V1,
            Strategy::FirstMatch,
            &mut pairs,
        )
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);
    let decoded =
        deserialize_one::<NoShortening, ReadsXY, _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!((decoded.x, decoded.y), (4, 9));
}

/// A policy that shortens this test crate's [`Named`].
struct ShortNamed;

impl NamePolicy for ShortNamed {
    const SHORT_TYPES: &'static [ShortType] = &[ShortType::new(concat!(module_path!(), "::Named"))];
}

#[test]
fn derived_type_name_honours_the_policy() {
    // The derive's `TypeName::FULL` is the module path + ident.
    assert_eq!(
        render::<NoShortening, Named>().as_slice(),
        concat!(module_path!(), "::Named").as_bytes(),
    );
    // A policy shortening it renders (and matches) the short name.
    assert_eq!(render::<ShortNamed, Named>(), b"Named");
    assert!(named_signature_matches::<ShortNamed, Named>(
        b"n: Named",
        b"n"
    ));
    assert!(!named_signature_matches::<NoShortening, Named>(
        b"n: Named",
        b"n"
    ));
}

#[test]
fn deserializing_a_non_envelop_is_a_missing_envelop() {
    let pairs = pairs();
    // `garbage` holds no limiter, so the whole 7-byte buffer is a NonEnvelop: the Envelop
    // `deserialize` was fed is missing, and that is what the report says.
    assert_eq!(
        deserialize_one::<NoShortening, Named, _>(b"garbage", Variant::V1, &pairs, None, Global,)
            .err(),
        Some(Error::MissingEnvelop {
            required_by: None,
            caught: (Offset::new(0), Offset::new(7)),
        }),
    );
}

/// A heap container whose element is itself a "not serialization sized" type.
#[cfg(feature = "vec")]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ListOfLists {
    rows: Vec<Vec<u8>>,
}

#[cfg(feature = "vec")]
#[test]
fn heap_container_of_iterables_round_trips() {
    let pairs = many_pairs();
    let value = ListOfLists {
        rows: vec![vec![1, 2], vec![], vec![3]],
    };
    assert_eq!(round_trip_with(value.clone(), &pairs), value);
}

/// A heap container of `dyn Trait` values — the derive carries the `dyn` bounds
/// from its element type, and each element self-describes its concrete type.
#[cfg(feature = "vec")]
#[derive(Serialize, Deserialize)]
struct TaggedList {
    tagged: Vec<Box<dyn MyTrait>>,
}

#[cfg(feature = "vec")]
#[test]
fn heap_container_of_dyn_round_trips() {
    let mut pairs = dyn_pairs();
    let value = TaggedList {
        tagged: vec![Box::new(Named { x: 1, y: true }), Box::new(Unit)],
    };
    let mut buffer: Vec<u8> = Vec::new();
    let slices = unsafe {
        value.serialize::<NoShortening>(&mut buffer, Variant::V1, Strategy::FirstMatch, &mut pairs)
    }
    .unwrap();
    let flat = flatten(&buffer, &slices);
    let decoded =
        deserialize_one::<NoShortening, TaggedList, _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!(decoded.tagged.len(), 2);
    assert_eq!(
        *as_any(&*decoded.tagged[0])
            .downcast_ref::<Named>()
            .expect("Named"),
        Named { x: 1, y: true },
    );
    assert!(as_any(&*decoded.tagged[1]).is::<Unit>());
}

// ---------------------------------------------------------------------------
// `#[default(..)]` / `#[skip(..)]` field attributes.
// ---------------------------------------------------------------------------

/// A field that may be absent from the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Config {
    id: u32,
    #[default(8080)]
    port: u32,
}

/// The same shape without `port` — what a peer that never heard of it writes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SparseConfig {
    id: u32,
}

#[test]
fn a_defaulted_field_may_be_missing_from_the_wire() {
    let pairs = many_pairs();

    // A document written without `port` still deserializes, through the fallback.
    let mut working = pairs.clone();
    let flat = document(&SparseConfig { id: 7 }, &mut working);
    let decoded =
        deserialize_one::<NoShortening, Config, _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!(decoded, Config { id: 7, port: 8080 });

    // A written `port` still wins over it, so the field really is on the wire.
    assert_eq!(
        round_trip_with(Config { id: 1, port: 42 }, &pairs),
        Config { id: 1, port: 42 }
    );
}

/// Deliberately implements none of the codec traits.
#[derive(Clone, Debug, Default, PartialEq)]
struct Opaque(u8);

/// `#[skip]` is local to its own field, so its type needs no codec impl at all —
/// this type derives *both* traits with `Opaque` as a field.
#[allow(dead_code)]
#[derive(Serialize, Deserialize)]
struct OpaqueField {
    id: u32,
    #[skip(Opaque(7))]
    opaque: Opaque,
}

/// The attribute must not leak into the field's own (nested) layer either: a skipped
/// field of tuple type needs nothing of `(u32, u8)`.
#[allow(dead_code)]
#[derive(Serialize, Deserialize)]
struct SkippedTuple {
    id: u32,
    #[skip((0, 0))]
    tuple_field: (u32, u8),
}

/// `note` is written by nobody here, and a peer's `note` is ignored on the way back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Session {
    id: u32,
    #[skip(0)]
    note: u32,
}

/// The peer that always writes `note`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct VerboseSession {
    id: u32,
    note: u32,
}

#[test]
fn skipped_fields_are_out_of_the_codec() {
    let pairs = many_pairs();
    let value = Session { id: 3, note: 42 };

    // Nothing carries `note`, so the fallback is what comes back…
    assert_eq!(
        round_trip_with(value.clone(), &pairs),
        Session { id: 3, note: 0 }
    );

    // …and the document is byte-identical to the one an `id`-only type writes.
    let mut working = pairs.clone();
    let skipped = document(&value, &mut working);
    let mut working = pairs.clone();
    assert_eq!(skipped, document(&SparseConfig { id: 3 }, &mut working));

    // The same holds for a field type with none of the codec traits: the attribute is
    // confined to its own field, so neither derive reaches into `Opaque`.
    let mut working = pairs.clone();
    let opaque = document(
        &OpaqueField {
            id: 9,
            opaque: Opaque(2),
        },
        &mut working,
    );
    let mut working = pairs.clone();
    assert_eq!(opaque, document(&SparseConfig { id: 9 }, &mut working));

    let decoded =
        deserialize_one::<NoShortening, OpaqueField, _>(&opaque, Variant::V1, &pairs, None, Global)
            .unwrap();
    // The field is never read, so `#[skip]`'s expression is what builds it.
    assert_eq!(decoded.id, 9);
    assert_eq!(decoded.opaque, Opaque(7));

    // A skipped field of tuple type writes and reads the same way.
    let mut working = pairs.clone();
    let nested = document(
        &SkippedTuple {
            id: 4,
            tuple_field: (1, 2),
        },
        &mut working,
    );
    let mut working = pairs.clone();
    assert_eq!(nested, document(&SparseConfig { id: 4 }, &mut working));
    let decoded = deserialize_one::<NoShortening, SkippedTuple, _>(
        &nested,
        Variant::V1,
        &pairs,
        None,
        Global,
    )
    .unwrap();
    assert_eq!(decoded.id, 4);
    assert_eq!(decoded.tuple_field, (0, 0));
}

#[test]
fn a_peer_that_sends_a_skipped_field_is_ignored() {
    let pairs = many_pairs();

    // `#[skip]` takes the field off the codec, so whatever a peer sends for it is
    // ignored — the fallback is used, exactly as if this type did not know the field.
    let mut working = pairs.clone();
    let flat = document(&VerboseSession { id: 3, note: 42 }, &mut working);
    let decoded =
        deserialize_one::<NoShortening, Session, _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!(decoded, Session { id: 3, note: 0 });
}

// ---------------------------------------------------------------------------
// Positional (tuple) fields.
//
// A positional field is matched by position, so `#[skip]` is well defined on it — a removed
// position just shifts the rest — while `#[default]` is not: a short document cannot say
// *which* position is missing. The derive rejects `#[default]` there at compile time; that
// rejection is pinned at the macro level, in `sdave-derive-impl/tests/codegen.rs`
// (`a_positional_field_rejects_default`).
// ---------------------------------------------------------------------------

/// A skipped field in the middle of a tuple struct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Row(u32, #[skip(0u32)] u32, i16);

#[test]
fn a_skipped_tuple_field_keeps_the_positions_of_the_rest() {
    let pairs = many_pairs();

    // `(1, 2, 3)` is written as two `u32`s: the middle slot comes back from the
    // fallback, and the last one keeps its wire position.
    assert_eq!(round_trip_with(Row(1, 2, 3), &pairs), Row(1, 0, 3));

    // A peer that skips the middle field too writes exactly two positional values, i.e.
    // the same wire shape as a two-field tuple struct.
    #[derive(Serialize)]
    struct Two(u32, u32);
    let mut working = pairs.clone();
    let row_document = document(&Row(1, 2, 3), &mut working);
    let mut working = pairs.clone();
    assert_eq!(row_document, document(&Two(1, 3), &mut working));
}

/// Every field is skipped, so nothing is left but the frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AllSkipped {
    #[skip(1)]
    a: u32,
}

#[test]
fn a_type_whose_fields_are_all_skipped_is_an_empty_envelop() {
    let pairs = many_pairs();
    assert_eq!(
        round_trip_with(AllSkipped { a: 5 }, &pairs),
        AllSkipped { a: 1 }
    );

    // …which is `()`'s wire shape.
    let mut working = pairs.clone();
    let all_skipped = document(&AllSkipped { a: 5 }, &mut working);
    let mut working = pairs.clone();
    assert_eq!(all_skipped, document(&(), &mut working));
}

/// A variant whose `level` field may be absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Message {
    Ping,
    Data {
        id: u32,
        #[default(1)]
        level: u32,
    },
    Pair(u32, u32, u32),
}

/// The same `Data` variant without `level`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum SparseMessage {
    Data { id: u32 },
}

#[test]
fn optional_field_attributes_work_in_enum_variants() {
    let pairs = many_pairs();

    assert_eq!(round_trip_with(Message::Ping, &pairs), Message::Ping);
    assert_eq!(
        round_trip_with(Message::Data { id: 2, level: 9 }, &pairs),
        Message::Data { id: 2, level: 9 }
    );
    // The tuple variant's fields are matched by position; none of them is optional.
    let enum_tuple_variant = Message::Pair(4, 5, 6);
    assert_eq!(
        round_trip_with(enum_tuple_variant, &pairs),
        Message::Pair(4, 5, 6)
    );

    // A `Data` document written without `level` fills it from the default.
    let mut working = pairs.clone();
    let flat = document(&SparseMessage::Data { id: 2 }, &mut working);
    let decoded =
        deserialize_one::<NoShortening, Message, _>(&flat, Variant::V1, &pairs, None, Global)
            .unwrap();
    assert_eq!(decoded, Message::Data { id: 2, level: 1 });
}

/// `#[skip]` (the write side) and `#[default]` (the read side) on one field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct BothAttributes {
    id: u32,
    #[skip(1u32)]
    #[default(7u32)]
    port: u32,
}

#[test]
fn a_field_may_be_skipped_and_defaulted_at_once() {
    let pairs = many_pairs();

    // Never written, and `#[default]`'s value is what comes back — not `#[skip]`'s.
    assert_eq!(
        round_trip_with(BothAttributes { id: 4, port: 42 }, &pairs,),
        BothAttributes { id: 4, port: 7 }
    );

    // The document is `id`'s alone, exactly as for an `id`-only type.
    let mut working = pairs.clone();
    let both = document(&BothAttributes { id: 4, port: 42 }, &mut working);
    let mut working = pairs.clone();
    assert_eq!(both, document(&SparseConfig { id: 4 }, &mut working));

    // A peer that *does* send the field is ignored: `#[skip]` took it off the codec,
    // so the fallback is still what is used.
    let mut working = pairs.clone();
    let verbose = document(&Config { id: 4, port: 5 }, &mut working);
    let decoded = deserialize_one::<NoShortening, BothAttributes, _>(
        &verbose,
        Variant::V1,
        &pairs,
        None,
        Global,
    )
    .unwrap();
    assert_eq!(decoded, BothAttributes { id: 4, port: 7 });
}

// ---------------------------------------------------------------------------
// Fieldless shapes: a struct or variant that carries no field at all.
//
// The three spellings of a fieldless struct are all one empty-payload Envelop,
// like `()`; the three spellings of a fieldless variant are all one bare-name
// Envelop.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct UnitStruct;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct BracedStruct {}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TupleStruct();

#[test]
fn fieldless_structs_round_trip_and_share_units_wire_shape() {
    let pairs = many_pairs();
    assert_eq!(round_trip_with(UnitStruct, &pairs), UnitStruct);
    assert_eq!(round_trip_with(BracedStruct {}, &pairs), BracedStruct {});
    assert_eq!(round_trip_with(TupleStruct(), &pairs), TupleStruct());

    let mut working = pairs.clone();
    let braced = document(&BracedStruct {}, &mut working);
    let mut working = pairs.clone();
    assert_eq!(braced, document(&TupleStruct(), &mut working));
    let mut working = pairs.clone();
    assert_eq!(braced, document(&(), &mut working));
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum PadWidths {
    Narrow,
    Wide(u32, u32),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PaddedEnumField {
    e: PadWidths,
    tail: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct EmptyPayloadField {
    a: u32,
    u: (),
}

#[test]
fn a_narrow_variant_that_leaves_padding_mid_payload_round_trips() {
    // `Slices` is sized by the widest variant, so `Narrow` leaves `None` padding between its
    // own slices and the field behind it: framing this struct scans a payload view with
    // holes in it, and a hole is not a resting place for a cursor.
    let pairs = many_pairs();
    for value in [
        PaddedEnumField {
            e: PadWidths::Narrow,
            tail: 7,
        },
        PaddedEnumField {
            e: PadWidths::Wide(1, 2),
            tail: 7,
        },
    ] {
        assert_eq!(round_trip_with(value.clone(), &pairs), value);
    }
}

#[test]
fn an_empty_payload_slice_mid_payload_round_trips() {
    // `()`'s Envelop has an empty payload slice, so the enclosing struct's slice list holds a
    // `Some(empty)` entry in the middle of the payload it hands to the framer.
    let pairs = many_pairs();
    let value = EmptyPayloadField { a: 5, u: () };
    assert_eq!(round_trip_with(value.clone(), &pairs), value);
}

#[test]
fn v2_framing_of_a_nested_enum_round_trips() {
    // V2 commits a delimiter run from its tail, and a nested Envelop's delimiter merges into its
    // parent's run. A data-carrying variant nests three Envelops deep and a narrow variant leaves
    // `None` padding mid-payload, so this covers both at once.
    let pairs = many_pairs();
    for value in [
        PaddedEnumField {
            e: PadWidths::Narrow,
            tail: 7,
        },
        PaddedEnumField {
            e: PadWidths::Wide(1, 2),
            tail: 7,
        },
    ] {
        let mut working = pairs.clone();
        let mut buffer: Vec<u8> = Vec::new();
        let slices = unsafe {
            value.serialize::<NoShortening>(
                &mut buffer,
                Variant::V2,
                Strategy::FirstMatch,
                &mut working,
            )
        }
        .unwrap();
        let flat = flatten(&buffer, &slices);
        let decoded = deserialize_one::<NoShortening, PaddedEnumField, _>(
            &flat,
            Variant::V2,
            &pairs,
            Some(Offset::new(flat.len())),
            Global,
        )
        .unwrap();
        assert_eq!(decoded, value);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum EmptyVariants {
    Unit,
    Braced {},
    Tuple(),
    Data { id: u32 },
}

#[test]
fn fieldless_enum_variants_round_trip() {
    let pairs = many_pairs();
    for value in [
        EmptyVariants::Unit,
        EmptyVariants::Braced {},
        EmptyVariants::Tuple(),
        EmptyVariants::Data { id: 3 },
    ] {
        assert_eq!(round_trip_with(value.clone(), &pairs), value);
    }

    // A fieldless variant is a bare-name Envelop: its payload is the name NonEnvelop
    // alone, not the name plus an inner Envelop the way a data-carrying variant is
    // written.
    for value in [
        EmptyVariants::Unit,
        EmptyVariants::Braced {},
        EmptyVariants::Tuple(),
    ] {
        let mut working = pairs.clone();
        let flat = document(&value, &mut working);
        let payload = unsafe {
            single_item_buffer_as_envelop_payload(&flat, Variant::V1, &pairs, None, Global)
        }
        .unwrap();
        let items = items_of(payload, Variant::V1, &pairs, Global);
        assert_eq!(items.len(), 1, "{value:?}");
        assert_eq!(items[0].head_offset().get(), 0, "{value:?}");
        assert!(
            matches!(items[0], Item::N { .. }),
            "{value:?}: the payload is a bare name NonEnvelop",
        );
    }
}
