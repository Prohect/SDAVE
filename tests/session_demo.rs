//! Demo (local prototype, not shipped): a session-shaped struct with a `meta` layer plus a
//! `dyn` payload, taken through a full serialize -> deserialize round-trip.
//!
//! The `dyn` field's value bytes are self-identifying: the concrete type's own document
//! (marker + record), wrapped in one envelope. Decoding reads that document's marker and
//! constructs the concrete type through a demo-local registry.

#![cfg(feature = "derive")]

use sdave::*;
use std::any::Any;
use std::fmt;

// ---- concrete implementors ----

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Circle {
    radius: u32,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Square {
    side: u32,
}

// ---- object trait: erased serialization plus a comparison hook ----

trait Spike: DynSerialize + fmt::Debug + 'static {
    fn as_any(&self) -> &dyn Any;

    /// Trait-level equality across the erased field. Not vacuous: it downcasts to the
    /// concrete type and uses that type's own `PartialEq`, so a different implementor or a
    /// different field value compares false.
    fn eq_dyn(&self, other: &dyn Spike) -> bool;
}

impl Spike for Circle {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_dyn(&self, other: &dyn Spike) -> bool {
        other.as_any().downcast_ref::<Circle>() == Some(self)
    }
}

impl Spike for Square {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_dyn(&self, other: &dyn Spike) -> bool {
        other.as_any().downcast_ref::<Square>() == Some(self)
    }
}

// ---- erased serialization: a self-identifying document wrapped in one envelope ----

impl DynSerialize for Circle {
    fn serialize_erased(&self, serializer: &mut Serializer<DynNames>) -> Result<Vec<u8>> {
        // `to_vec` writes the concrete type's marker and its record envelope; wrapping
        // that whole document in one envelope keeps it a single value run for the outer
        // record, while the concrete marker stays inside for the decoder to read.
        let document = serializer.to_vec(self)?;
        serializer.frame(&document)
    }
}

impl DynSerialize for Square {
    fn serialize_erased(&self, serializer: &mut Serializer<DynNames>) -> Result<Vec<u8>> {
        let document = serializer.to_vec(self)?;
        serializer.frame(&document)
    }
}

impl Serialize for dyn Spike {
    const SHAPE: Shape = Shape::Record;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let mut erased = Serializer::<DynNames>::new(serializer.config().clone())?;
        self.serialize_erased(&mut erased)
    }
}

// ---- the session shape: a meta layer plus a dyn payload ----

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Meta {
    id: u32,
    title: String,
}

#[derive(Debug, Serialize)]
struct Session {
    meta: Meta,
    payload: Box<dyn Spike>,
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.meta == other.meta && self.payload.eq_dyn(other.payload.as_ref())
    }
}

impl<'de> Deserialize<'de> for Session {
    const SHAPE: Shape = Shape::Record;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let mut record = decoder.record(run)?;
        let meta = record
            .take::<Meta, P>("meta", decoder)?
            .ok_or_else(|| record.missing_field("meta"))?;
        let (marker, payload_run) = record
            .take_raw("payload")?
            .ok_or_else(|| record.missing_field("payload"))?;
        // the field's static marker is still the erased `Box<dyn Spike>`.
        decoder.check_marker::<Box<dyn Spike>>(marker, payload_run.offset())?;
        let payload = decode_payload(payload_run, decoder.config().clone())?;
        record.finish()?;
        Ok(Session { meta, payload })
    }
}

/// Demo-local registry: read the erased value's own document marker and construct the
/// concrete type. SDAVE cannot build a trait object, so the application owns this map.
fn decode_payload(wrapper: ValueRun<'_>, config: Config) -> Result<Box<dyn Spike>> {
    let document = wrapper.single()?;
    let mut inner = Deserializer::<DynNames>::new(config)?;
    let (marker, run) = inner.document(document)?;
    let circle = type_marker::<Circle, DynNames>()?;
    let square = type_marker::<Square, DynNames>()?;
    if marker == circle.as_str() {
        Ok(Box::new(inner.value::<Circle>(run)?))
    } else if marker == square.as_str() {
        Ok(Box::new(inner.value::<Square>(run)?))
    } else {
        Err(Error::with_source(
            ErrorKind::UnknownVariant(marker.to_owned()),
            file!(),
            line!(),
        ))
    }
}

// ---- the round-trip ----

fn config() -> Config {
    Config::default()
}

fn roundtrip(label: &str, origin: Session) {
    let wire = Serializer::<DefaultNames>::new(config())
        .unwrap()
        .to_string(&origin)
        .unwrap();
    let decoded: Session = Deserializer::<DefaultNames>::new(config())
        .unwrap()
        .from_slice(wire.as_bytes())
        .unwrap();

    println!("=== session demo ({label}) ===");
    println!("origin       = {origin:?}");
    println!("serialized   = {wire}");
    println!("deserialized = {decoded:?}");

    assert_eq!(origin, decoded);
}

#[test]
fn session_with_meta_and_dyn_payload_roundtrips() {
    roundtrip(
        "Circle",
        Session {
            meta: Meta {
                id: 1,
                title: "first".into(),
            },
            payload: Box::new(Circle { radius: 7 }),
        },
    );
    roundtrip(
        "Square",
        Session {
            meta: Meta {
                id: 2,
                title: "second".into(),
            },
            payload: Box::new(Square { side: 9 }),
        },
    );

    // The comparison is not vacuous: a different implementor is not equal.
    let circle: Box<dyn Spike> = Box::new(Circle { radius: 7 });
    let other_circle: Box<dyn Spike> = Box::new(Circle { radius: 7 });
    let square: Box<dyn Spike> = Box::new(Square { side: 9 });
    assert!(circle.eq_dyn(other_circle.as_ref()));
    assert!(!circle.eq_dyn(square.as_ref()));
}
