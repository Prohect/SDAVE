//! LOCAL PROTOTYPE (not committed, not published): accepting `dyn Trait` spellings and
//! serializing a `Box<dyn Trait>` field so the maintainer can read the real bytes.
//!
//! It prints the markers and the full documents; run with `-- --nocapture` to see them.
//! Would ship as **0.3.2** (purely additive: `dyn` spellings previously errored, and no
//! non-`dyn` marker changes).

#![cfg(feature = "derive")]

use sdave::*;

// ---- concrete implementors ----

#[derive(Debug, Serialize)]
struct Circle {
    radius: u32,
}

#[derive(Debug, Serialize)]
struct Square {
    side: u32,
}

// ---- application object traits ----

/// The object trait whose `dyn` field is serialized. `DynSerialize` is its object-safe hook.
trait Spike: DynSerialize {}

/// A second, unrelated trait: its `dyn` marker must differ from `Spike`'s.
trait Blob {}

// ---- per-implementor erased hooks (delegate to that type's Serialize) ----

impl DynSerialize for Circle {
    fn serialize_erased(&self, serializer: &mut Serializer<DynNames>) -> Result<Vec<u8>> {
        self.serialize_value(serializer)
    }
}

impl DynSerialize for Square {
    fn serialize_erased(&self, serializer: &mut Serializer<DynNames>) -> Result<Vec<u8>> {
        self.serialize_value(serializer)
    }
}

impl Spike for Circle {}
impl Spike for Square {}

// ---- the per-trait `Serialize for dyn Trait` impl (application-supplied) ----
//
// The vtable cannot be generic over the caller's `NamePolicy`, so the concrete value is
// rendered with the fixed `DynNames` policy, sharing the caller's framing config.

impl Serialize for dyn Spike {
    const SHAPE: Shape = Shape::Record;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let mut erased = Serializer::<DynNames>::new(serializer.config().clone())?;
        self.serialize_erased(&mut erased)
    }
}

// ---- a holder with a `dyn` field ----

#[derive(Serialize)]
struct SpikeHolder {
    shape: Box<dyn Spike>,
}

fn encode(holder: &SpikeHolder) -> String {
    Serializer::<DefaultNames>::new(Config::default())
        .unwrap()
        .to_string(holder)
        .unwrap()
}

#[test]
fn dyn_field_prototype_prints_real_bytes() {
    let circle = SpikeHolder {
        shape: Box::new(Circle { radius: 7 }),
    };
    let square = SpikeHolder {
        shape: Box::new(Square { side: 9 }),
    };
    let circle_bytes = encode(&circle);
    let square_bytes = encode(&square);

    let bare = type_marker::<dyn Spike, DefaultNames>().unwrap();
    let reference = type_marker::<&dyn Spike, DefaultNames>().unwrap();
    let boxed = type_marker::<Box<dyn Spike>, DefaultNames>().unwrap();
    let nested = type_marker::<Vec<Box<dyn Spike>>, DefaultNames>().unwrap();
    let other_trait = type_marker::<dyn Blob, DefaultNames>().unwrap();
    let concrete = type_marker::<Box<Circle>, DefaultNames>().unwrap();
    let header = format!("shape: {boxed}");

    println!("=== SDAVE `dyn` prototype: real bytes ===");
    println!("marker dyn Spike           = {bare}");
    println!("marker &dyn Spike          = {reference}");
    println!("marker Box<dyn Spike>      = {boxed}");
    println!("marker Vec<Box<dyn Spike>> = {nested}");
    println!("marker dyn Blob            = {other_trait}");
    println!("marker Box<Circle>         = {concrete}");
    println!("shared field header        = {header}");
    println!("holder<Circle>             = {circle_bytes}");
    println!("holder<Square>             = {square_bytes}");

    // (1) Erasure: one field header per trait, regardless of the concrete implementor.
    assert!(circle_bytes.contains(&header), "Circle: {circle_bytes}");
    assert!(square_bytes.contains(&header), "Square: {square_bytes}");

    // (2) Each implementor's own document is inside the value bytes.
    assert!(circle_bytes.contains("radius: u32"), "Circle: {circle_bytes}");
    assert!(square_bytes.contains("side: u32"), "Square: {square_bytes}");
    assert_ne!(circle_bytes, square_bytes);

    // (3) Two traits do not collide.
    assert_ne!(bare, other_trait);
    assert_ne!(boxed, other_trait);

    // (4) A `dyn` marker differs from a concrete marker.
    assert_ne!(boxed, concrete);
    assert_ne!(bare, concrete);

    // (5) Deterministic.
    assert_eq!(bare, type_marker::<dyn Spike, DefaultNames>().unwrap());

    // (6) Non-`dyn` spellings are byte-identical to today.
    assert_eq!(type_marker::<u32, DefaultNames>().unwrap(), "u32");
    assert_eq!(
        type_marker::<Vec<String>, DefaultNames>().unwrap(),
        "Vec<String>"
    );
    assert_eq!(
        type_marker::<Box<Circle>, DefaultNames>().unwrap(),
        format!("Box<{}>", std::any::type_name::<Circle>())
    );
}
