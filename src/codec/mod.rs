//! Native, statically typed Rust serialization above SDAVE's flat framing.
//!
//! A document is one type-marked value, not an arbitrary mixed channel. Child
//! codecs inherit the immutable naming policy, framing profile and limits.

mod config;
mod de;
mod error;
mod framing;
mod grammar;
mod impls;
mod names;
mod ser;

pub use config::{Config, ConfigFingerprint, Limits};
pub use de::{Deserializer, Payload, Record, ValueRun, VariantRun};
pub use error::{Error, ErrorKind, PathSegment, Result};
pub use grammar::{
    FieldHeader, is_formatting, parse_field_header, trim_metadata, trim_metadata_range,
};
pub use impls::{ByteBuf, Bytes};
pub use names::{DefaultNames, NamePolicy, ShortType, type_marker};
pub use ser::Serializer;

/// A type's declared wire shape, available without inspecting a value.
///
/// Shape does not imply dynamic type construction or payload inference. In
/// particular, empty lists and unit enum variants retain their static types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Basic,
    Tuple,
    Record,
    List,
    Enum,
}

/// Native serialization; independent of Serde.
///
/// `serialize_value` writes a context-known run of real envelopes. Metadata is
/// supplied by the caller. `serialize_payload` writes the body of a separate
/// item/associated-value envelope. Records retain their record envelope there;
/// basics and tuples override the method to avoid a redundant value wrapper.
/// Implementations must use the context's child operations to inherit limits.
pub trait Serialize {
    const SHAPE: Shape;

    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>>;

    fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        self.serialize_value(serializer)
    }
}

/// Native decoding from a complete, bounded input. Leaf strings/bytes can borrow.
pub trait Deserialize<'de>: Sized {
    const SHAPE: Shape;

    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self>;

    fn deserialize_payload<P: NamePolicy>(
        payload: Payload<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let run = decoder.run(payload)?;
        Self::deserialize_value(run, decoder)
    }
}

/// A fallible semantic payload codec for an explicitly basic/opaque type.
///
/// Helpers handle raw payloads, not an outer type marker or value envelope.
/// They must not panic on invalid input, invalid domain values or ordinary
/// output failures. Use [`Serializer::payload`] and [`Deserializer::payload`]
/// for nested semantics; they preserve this object's policy/profile/limits.
/// A `#[sdave(basic)]` derive requires this trait without traversing storage.
/// This trait decodes owned types; borrowed leaves can implement the native
/// traits directly instead.
pub trait BasicType: Sized {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>>;

    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self>;
}

/// Encode one type-marked document using the default V2 profile and names.
pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    Serializer::<DefaultNames>::new(Config::default())?.to_vec(value)
}

/// Encode one UTF-8 document. Binary basic payloads may require [`to_vec`].
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Serializer::<DefaultNames>::new(Config::default())?.to_string(value)
}

/// Buffer one complete document and then write it, propagating output failures.
pub fn to_writer<W: std::io::Write, T: Serialize + ?Sized>(writer: W, value: &T) -> Result<()> {
    Serializer::<DefaultNames>::new(Config::default())?.to_writer(writer, value)
}

/// Decode exactly one complete type-marked document, rejecting trailing values.
pub fn from_slice<'de, T: Deserialize<'de>>(input: &'de [u8]) -> Result<T> {
    Deserializer::<DefaultNames>::new(Config::default())?.from_slice(input)
}

pub fn from_str<'de, T: Deserialize<'de>>(input: &'de str) -> Result<T> {
    from_slice(input.as_bytes())
}
