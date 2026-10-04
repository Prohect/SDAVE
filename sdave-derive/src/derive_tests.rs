//! Compile and behavior tests against a protocol-independent core API double.
//! The real codec's delimiter, metadata, and resource-limit tests belong to the
//! sdave crate; this double checks derives without a circular dev-dependency.
#![allow(dead_code)]

extern crate self as sdave;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::marker::PhantomData;

use sdave_derive::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, PartialEq, Eq)]
pub enum ErrorKind {
    EnvelopeCount { expected: usize, actual: usize },
    MissingField(String),
    UnknownField(String),
    DuplicateField(String),
    UnknownVariant(String),
    MissingListHeader,
    NonemptyListHeader,
    InvalidPayload(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub path: Vec<String>,
    pub rust_source: String,
}

#[macro_export]
macro_rules! error_new {
    ($kind:expr $(,)?) => {
        $crate::Error::with_source($kind, file!(), line!())
    };
}

#[macro_export]
macro_rules! error_payload {
    ($message:expr $(,)?) => {
        $crate::Error::with_source(
            $crate::ErrorKind::InvalidPayload($message.into()),
            file!(),
            line!(),
        )
    };
}

impl Error {
    pub fn with_source(kind: ErrorKind, file: &'static str, line: u32) -> Self {
        Self {
            kind,
            path: Vec::new(),
            rust_source: format!("{file}:{line}"),
        }
    }

    pub fn payload(message: impl ToString) -> Self {
        {
            let kind = ErrorKind::InvalidPayload(message.to_string());
            Self {
                kind,
                path: Vec::new(),
                rust_source: "".to_string(),
            }
        }
    }

    pub fn in_field(mut self, name: &str) -> Self {
        self.path.insert(0, format!("field:{name}"));
        self
    }

    pub fn in_variant(mut self, name: &str) -> Self {
        self.path.insert(0, format!("variant:{name}"));
        self
    }

    pub fn at_index(mut self, index: usize) -> Self {
        self.path.insert(0, format!("index:{index}"));
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Basic,
    Tuple,
    Record,
    List,
    Enum,
}

pub trait NamePolicy {}

pub struct TestPolicy;
impl NamePolicy for TestPolicy {}

pub trait Serialize {
    const SHAPE: Shape;

    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>>;

    fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        self.serialize_value(serializer)
    }
}

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

pub trait BasicType: Sized {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>>;
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self>;
}

#[derive(Clone, Copy)]
pub struct Payload<'de>(&'de [u8]);

pub struct ValueRun<'de> {
    items: Vec<Payload<'de>>,
}

impl<'de> ValueRun<'de> {
    pub fn single(self) -> Result<Payload<'de>> {
        self.exact(1)?
            .into_iter()
            .next()
            .ok_or_else(|| error_payload!("missing frame"))
    }

    pub fn exact(self, count: usize) -> Result<Vec<Payload<'de>>> {
        if self.items.len() != count {
            return Err(error_new!(ErrorKind::EnvelopeCount {
                expected: count,
                actual: self.items.len(),
            }));
        }
        Ok(self.items)
    }

    pub fn variant(self) -> Result<VariantRun<'de>> {
        let mut items = self.items.into_iter();
        let selector = items.next().ok_or_else(|| {
            error_new!(ErrorKind::EnvelopeCount {
                expected: 1,
                actual: 0,
            })
        })?;
        let name = std::str::from_utf8(selector.0).map_err(Error::payload)?;
        Ok(VariantRun {
            name,
            associated: items.collect(),
        })
    }
}

pub struct VariantRun<'de> {
    name: &'de str,
    associated: Vec<Payload<'de>>,
}

impl<'de> VariantRun<'de> {
    pub fn name(&self) -> &'de str {
        self.name
    }

    pub fn unit(self) -> Result<()> {
        if self.associated.is_empty() {
            Ok(())
        } else {
            Err(error_new!(ErrorKind::EnvelopeCount {
                expected: 0,
                actual: self.associated.len(),
            }))
        }
    }

    pub fn associated(self) -> Result<Payload<'de>> {
        ValueRun {
            items: self.associated,
        }
        .single()
    }

    pub fn unknown(&self) -> Error {
        error_new!(ErrorKind::UnknownVariant(self.name.to_owned()))
    }
}

pub struct Record<'de> {
    fields: BTreeMap<&'de str, ValueRun<'de>>,
}

impl<'de> Record<'de> {
    pub fn take<T: Deserialize<'de>, P: NamePolicy>(
        &mut self,
        name: &str,
        decoder: &mut Deserializer<P>,
    ) -> Result<Option<T>> {
        match self.fields.remove(name) {
            Some(run) => T::deserialize_value(run, decoder)
                .map(Some)
                .map_err(|error| error.in_field(name)),
            None => Ok(None),
        }
    }

    pub fn finish(&self) -> Result<()> {
        match self.fields.keys().next() {
            Some(name) => Err(error_new!(ErrorKind::UnknownField((*name).to_owned()))),
            None => Ok(()),
        }
    }

    pub fn require_present(&self, name: &str, present: bool) -> Result<()> {
        if present {
            Ok(())
        } else {
            Err(self.missing_field(name))
        }
    }

    pub fn missing_field(&self, name: &str) -> Error {
        error_new!(ErrorKind::MissingField(name.to_owned())).in_field(name)
    }
}

pub struct Serializer<P: NamePolicy>(PhantomData<P>);

impl<P: NamePolicy> Serializer<P> {
    fn new() -> Self {
        Self(PhantomData)
    }

    pub fn field<T: Serialize + ?Sized>(
        &mut self,
        name: &str,
        value: &T,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let header = format!("!{}:{name}", name.len());
        self.append(out, header.as_bytes())?;
        let run = value
            .serialize_value(self)
            .map_err(|error| error.in_field(name))?;
        self.append(out, &run)
    }

    // Length-prefixed frames are deliberately not an implementation of SDAVE.
    pub fn frame(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut out = format!("[{}:", payload.len()).into_bytes();
        out.extend_from_slice(payload);
        out.extend_from_slice(b"]\n");
        Ok(out)
    }

    pub fn payload<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<Vec<u8>> {
        value.serialize_payload(self)
    }

    pub fn item<T: Serialize + ?Sized>(&mut self, value: &T, out: &mut Vec<u8>) -> Result<()> {
        let payload = self.payload(value)?;
        let framed = self.frame(&payload)?;
        self.append(out, &framed)
    }

    pub fn append(&self, out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
        out.extend_from_slice(bytes);
        Ok(())
    }

    pub fn variant(&mut self, name: &str, payload: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut out = self.frame(name.as_bytes())?;
        if let Some(payload) = payload {
            let associated = self.frame(payload)?;
            self.append(&mut out, &associated)?;
        }
        Ok(out)
    }

    pub fn basic<T: BasicType>(&mut self, value: &T) -> Result<Vec<u8>> {
        let payload = value.encode_basic(self)?;
        self.frame(&payload)
    }
}

fn skip_formatting(bytes: &[u8], position: &mut usize) {
    while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
        *position += 1;
    }
}

fn read_length(bytes: &[u8], position: &mut usize) -> Result<usize> {
    let start = *position;
    while bytes.get(*position).is_some_and(u8::is_ascii_digit) {
        *position += 1;
    }
    if bytes.get(*position) != Some(&b':') {
        return Err(error_payload!("expected length separator"));
    }
    let length = std::str::from_utf8(&bytes[start..*position])
        .map_err(Error::payload)?
        .parse::<usize>()
        .map_err(Error::payload)?;
    *position += 1;
    Ok(length)
}

fn read_frame<'de>(bytes: &'de [u8], position: &mut usize) -> Result<Payload<'de>> {
    if bytes.get(*position) != Some(&b'[') {
        return Err(error_payload!("meaningful metadata in a value run"));
    }
    *position += 1;
    let length = read_length(bytes, position)?;
    let end = (*position)
        .checked_add(length)
        .ok_or_else(|| error_payload!("length overflow"))?;
    let payload = bytes
        .get(*position..end)
        .ok_or_else(|| error_payload!("truncated payload"))?;
    if bytes.get(end) != Some(&b']') {
        return Err(error_payload!("truncated frame"));
    }
    *position = end + 1;
    Ok(Payload(payload))
}

pub struct Deserializer<P: NamePolicy>(PhantomData<P>);

impl<P: NamePolicy> Deserializer<P> {
    fn new() -> Self {
        Self(PhantomData)
    }

    pub fn run<'de>(&mut self, payload: Payload<'de>) -> Result<ValueRun<'de>> {
        let mut position = 0;
        let mut items = Vec::new();
        skip_formatting(payload.0, &mut position);
        while position < payload.0.len() {
            items.push(read_frame(payload.0, &mut position)?);
            skip_formatting(payload.0, &mut position);
        }
        Ok(ValueRun { items })
    }

    pub fn record<'de>(&mut self, run: ValueRun<'de>) -> Result<Record<'de>> {
        let payload = run.single()?;
        let bytes = payload.0;
        let mut position = 0;
        let mut fields = BTreeMap::new();
        skip_formatting(bytes, &mut position);
        while position < bytes.len() {
            if bytes.get(position) != Some(&b'!') {
                return Err(error_payload!("expected field metadata"));
            }
            position += 1;
            let length = read_length(bytes, &mut position)?;
            let end = position
                .checked_add(length)
                .ok_or_else(|| error_payload!("length overflow"))?;
            let name = std::str::from_utf8(
                bytes
                    .get(position..end)
                    .ok_or_else(|| error_payload!("truncated field name"))?,
            )
            .map_err(Error::payload)?;
            position = end;
            let mut items = Vec::new();
            skip_formatting(bytes, &mut position);
            while bytes.get(position) == Some(&b'[') {
                items.push(read_frame(bytes, &mut position)?);
                skip_formatting(bytes, &mut position);
            }
            if fields.insert(name, ValueRun { items }).is_some() {
                return Err(error_new!(ErrorKind::DuplicateField(name.to_owned())));
            }
        }
        Ok(Record { fields })
    }

    pub fn payload<'de, T: Deserialize<'de>>(&mut self, payload: Payload<'de>) -> Result<T> {
        T::deserialize_payload(payload, self)
    }

    pub fn basic<'de, T: BasicType>(&mut self, run: ValueRun<'de>) -> Result<T> {
        T::decode_basic(run.single()?, self)
    }
}

impl BasicType for u32 {
    fn encode_basic<P: NamePolicy>(&self, _: &mut Serializer<P>) -> Result<Vec<u8>> {
        Ok(self.to_string().into_bytes())
    }

    fn decode_basic<P: NamePolicy>(payload: Payload<'_>, _: &mut Deserializer<P>) -> Result<Self> {
        std::str::from_utf8(payload.0)
            .map_err(Error::payload)?
            .parse()
            .map_err(Error::payload)
    }
}

impl BasicType for String {
    fn encode_basic<P: NamePolicy>(&self, _: &mut Serializer<P>) -> Result<Vec<u8>> {
        Ok(self.as_bytes().to_vec())
    }

    fn decode_basic<P: NamePolicy>(payload: Payload<'_>, _: &mut Deserializer<P>) -> Result<Self> {
        std::str::from_utf8(payload.0)
            .map(str::to_owned)
            .map_err(Error::payload)
    }
}

macro_rules! basic_codec {
    ($ty:ty) => {
        impl Serialize for $ty {
            const SHAPE: Shape = Shape::Basic;
            fn serialize_value<P: NamePolicy>(
                &self,
                serializer: &mut Serializer<P>,
            ) -> Result<Vec<u8>> {
                serializer.basic(self)
            }
            fn serialize_payload<P: NamePolicy>(
                &self,
                serializer: &mut Serializer<P>,
            ) -> Result<Vec<u8>> {
                self.encode_basic(serializer)
            }
        }
        impl<'de> Deserialize<'de> for $ty {
            const SHAPE: Shape = Shape::Basic;
            fn deserialize_value<P: NamePolicy>(
                run: ValueRun<'de>,
                decoder: &mut Deserializer<P>,
            ) -> Result<Self> {
                decoder.basic(run)
            }
            fn deserialize_payload<P: NamePolicy>(
                payload: Payload<'de>,
                decoder: &mut Deserializer<P>,
            ) -> Result<Self> {
                Self::decode_basic(payload, decoder)
            }
        }
    };
}
basic_codec!(u32);
basic_codec!(String);

impl Serialize for &str {
    const SHAPE: Shape = Shape::Basic;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        serializer.frame(self.as_bytes())
    }
    fn serialize_payload<P: NamePolicy>(&self, _: &mut Serializer<P>) -> Result<Vec<u8>> {
        Ok(self.as_bytes().to_vec())
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for &'a str {
    const SHAPE: Shape = Shape::Basic;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Self::deserialize_payload(run.single()?, decoder)
    }
    fn deserialize_payload<P: NamePolicy>(
        payload: Payload<'de>,
        _: &mut Deserializer<P>,
    ) -> Result<Self> {
        std::str::from_utf8(payload.0).map_err(Error::payload)
    }
}

impl<T: Serialize + ?Sized> Serialize for Box<T> {
    const SHAPE: Shape = T::SHAPE;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        self.as_ref().serialize_value(serializer)
    }
    fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        self.as_ref().serialize_payload(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Box<T> {
    const SHAPE: Shape = T::SHAPE;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        T::deserialize_value(run, decoder).map(Box::new)
    }
    fn deserialize_payload<P: NamePolicy>(
        payload: Payload<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        decoder.payload(payload).map(Box::new)
    }
}

impl<T: Serialize> Serialize for Option<T> {
    const SHAPE: Shape = Shape::Enum;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        match self {
            Some(value) => {
                let payload = serializer.payload(value)?;
                serializer.variant("Some", Some(&payload))
            }
            None => serializer.variant("None", None),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Option<T> {
    const SHAPE: Shape = Shape::Enum;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let variant = run.variant()?;
        match variant.name() {
            "None" => {
                variant.unit()?;
                Ok(None)
            }
            "Some" => decoder.payload(variant.associated()?).map(Some),
            _ => Err(variant.unknown()),
        }
    }
}

impl<T: Serialize> Serialize for Vec<T> {
    const SHAPE: Shape = Shape::List;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let mut out = serializer.frame(&[])?;
        for (index, value) in self.iter().enumerate() {
            serializer
                .item(value, &mut out)
                .map_err(|error| error.at_index(index))?;
        }
        Ok(out)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Vec<T> {
    const SHAPE: Shape = Shape::List;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let mut items = run.items.into_iter();
        let header = items
            .next()
            .ok_or_else(|| error_new!(ErrorKind::MissingListHeader))?;
        if !header.0.is_empty() {
            return Err(error_new!(ErrorKind::NonemptyListHeader));
        }
        items
            .enumerate()
            .map(|(index, item)| decoder.payload(item).map_err(|error| error.at_index(index)))
            .collect()
    }
}

impl<T> Serialize for PhantomData<T> {
    const SHAPE: Shape = Shape::Tuple;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        serializer.frame(&[])
    }
    fn serialize_payload<P: NamePolicy>(&self, _: &mut Serializer<P>) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

impl<'de, T> Deserialize<'de> for PhantomData<T> {
    const SHAPE: Shape = Shape::Tuple;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Self::deserialize_payload(run.single()?, decoder)
    }
    fn deserialize_payload<P: NamePolicy>(
        payload: Payload<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        decoder.run(payload)?.exact(0)?;
        Ok(PhantomData)
    }
}

fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    value
        .serialize_value(&mut Serializer::<TestPolicy>::new())
        .unwrap()
}

fn decode<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T> {
    let mut decoder = Deserializer::<TestPolicy>::new();
    let run = decoder.run(Payload(bytes))?;
    T::deserialize_value(run, &mut decoder)
}

fn frames(bytes: &[u8]) -> Vec<Payload<'_>> {
    Deserializer::<TestPolicy>::new()
        .run(Payload(bytes))
        .unwrap()
        .items
}

fn record(build: impl FnOnce(&mut Serializer<TestPolicy>, &mut Vec<u8>)) -> Vec<u8> {
    let mut serializer = Serializer::<TestPolicy>::new();
    let mut body = Vec::new();
    build(&mut serializer, &mut body);
    serializer.frame(&body).unwrap()
}

fn roundtrip<T: Serialize + for<'de> Deserialize<'de> + Debug + PartialEq>(value: T) {
    let bytes = encode(&value);
    assert_eq!(decode::<T>(&bytes).unwrap(), value);
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Named {
    value: u32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Unit;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct EmptyNamed {}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct EmptyTuple();

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Newtype(Named);

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Tuple(Named, Option<u32>);

thread_local! {
    static DEFAULT_CALLS: Cell<usize> = const { Cell::new(0) };
    static DECODE_CALLS: Cell<usize> = const { Cell::new(0) };
}

fn fallback() -> Option<u32> {
    DEFAULT_CALLS.set(DEFAULT_CALLS.get() + 1);
    Some(99)
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Defaults {
    #[sdave(default = fallback)]
    optional: Option<u32>,
    #[sdave(default)]
    values: Vec<u32>,
    first: u32,
    last: u32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
enum Variants {
    Unit,
    EmptyTuple(),
    EmptyRecord {},
    Scalar(u32),
    Newtype(Named),
    Many(Named, u32),
    Named {
        #[sdave(default = fallback)]
        optional: Option<u32>,
        required: u32,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Node<T> {
    value: T,
    next: Option<Box<Node<T>>>,
    children: Vec<Self>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
enum Chain<T> {
    End(T),
    Next(Box<Self>),
}

#[derive(Serialize, Deserialize)]
struct Borrowed<'__sdave_de> {
    text: &'__sdave_de str,
}

#[derive(Serialize, Deserialize)]
struct PolicyCollision<__SdavePolicy = u32> {
    value: __SdavePolicy,
}

#[allow(non_upper_case_globals)]
#[derive(Serialize, Deserialize)]
struct ConstCollision<const __sdave_record: usize = 3> {
    value: u32,
}

fn __sdave_field_0() -> u32 {
    12
}

#[derive(Serialize, Deserialize)]
struct LocalCollision {
    #[sdave(default = __sdave_field_0)]
    value: u32,
}

trait HasItem {
    type Item;
}
struct ItemOwner;
impl HasItem for ItemOwner {
    type Item = u32;
}

#[derive(Serialize, Deserialize)]
struct Associated<T: HasItem> {
    item: T::Item,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct NoDefault(u32);

#[derive(Serialize, Deserialize)]
struct GenericDefaults<T> {
    #[sdave(default)]
    values: Vec<T>,
}

struct NoCodec;

#[derive(Serialize, Deserialize)]
struct Marker<T> {
    marker: PhantomData<T>,
}

#[derive(Serialize, Deserialize)]
#[sdave(basic)]
struct Opaque<T> {
    value: u32,
    storage: NoCodec,
    generic_storage: PhantomData<T>,
}

impl<T> BasicType for Opaque<T> {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        self.value.encode_basic(serializer)
    }
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let value = u32::decode_basic(payload, decoder)?;
        Ok(Self {
            value,
            storage: NoCodec,
            generic_storage: PhantomData,
        })
    }
}

struct Probe;
impl<'de> Deserialize<'de> for Probe {
    const SHAPE: Shape = Shape::Basic;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Self::deserialize_payload(run.single()?, decoder)
    }
    fn deserialize_payload<P: NamePolicy>(
        _: Payload<'de>,
        _: &mut Deserializer<P>,
    ) -> Result<Self> {
        DECODE_CALLS.set(DECODE_CALLS.get() + 1);
        Ok(Self)
    }
}

#[derive(Deserialize)]
enum Validation {
    Newtype(Probe),
    Many(Probe, Probe),
    Named { probe: Probe },
}

#[derive(Serialize, Deserialize)]
enum Never {}

#[allow(non_camel_case_types)]
#[derive(Serialize, Deserialize, Debug, PartialEq)]
enum RawNames {
    r#match { r#type: u32 },
}

#[test]
fn structs_and_variants_roundtrip_and_preserve_envelope_boundaries() {
    roundtrip(Unit);
    roundtrip(EmptyNamed {});
    roundtrip(EmptyTuple());
    roundtrip(Named { value: 1 });
    roundtrip(Newtype(Named { value: 2 }));
    roundtrip(Tuple(Named { value: 3 }, Some(4)));
    roundtrip(Variants::Unit);
    roundtrip(Variants::EmptyTuple());
    roundtrip(Variants::EmptyRecord {});
    roundtrip(Variants::Scalar(5));
    roundtrip(Variants::Newtype(Named { value: 6 }));
    roundtrip(Variants::Many(Named { value: 7 }, 8));
    roundtrip(Variants::Named {
        optional: None,
        required: 9,
    });

    let mut serializer = Serializer::<TestPolicy>::new();
    assert!(!Unit.serialize_payload(&mut serializer).unwrap().is_empty());
    assert!(
        !EmptyNamed {}
            .serialize_payload(&mut serializer)
            .unwrap()
            .is_empty()
    );
    assert!(
        EmptyTuple()
            .serialize_payload(&mut serializer)
            .unwrap()
            .is_empty()
    );
    assert_eq!(<Unit as Serialize>::SHAPE, Shape::Record);
    assert_eq!(<Newtype as Serialize>::SHAPE, Shape::Tuple);

    let named = encode(&Named { value: 10 });
    let newtype = encode(&Newtype(Named { value: 10 }));
    let outer = frames(&newtype);
    let items = frames(outer[0].0);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, named.as_slice());

    let newtype_variant = encode(&Variants::Newtype(Named { value: 10 }));
    let run = frames(&newtype_variant);
    assert_eq!(run.len(), 2);
    assert_eq!(run[1].0, named.as_slice());

    let scalar = encode(&Variants::Scalar(10));
    assert_eq!(frames(&scalar)[1].0, b"10");

    let empty_tuple = encode(&Variants::EmptyTuple());
    assert_eq!(frames(&empty_tuple).len(), 2);
    assert!(frames(&empty_tuple)[1].0.is_empty());
    let empty_record = encode(&Variants::EmptyRecord {});
    assert_eq!(frames(&empty_record).len(), 2);
    let associated = frames(&empty_record)[1];
    assert_eq!(frames(associated.0).len(), 1);
    assert!(frames(associated.0)[0].0.is_empty());
    assert_eq!(frames(&encode(&Variants::Unit)).len(), 1);

    let many = encode(&Variants::Many(Named { value: 10 }, 11));
    let items = frames(frames(&many)[1].0);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].0, named.as_slice());
    assert_eq!(items[1].0, b"11");
}

#[test]
fn missing_fields_default_once_but_explicit_none_is_present() {
    DEFAULT_CALLS.set(0);
    let absent = record(|serializer, body| {
        serializer.field("last", &2u32, body).unwrap();
        serializer.field("first", &1u32, body).unwrap();
    });
    let decoded = decode::<Defaults>(&absent).unwrap();
    assert_eq!(
        decoded,
        Defaults {
            optional: Some(99),
            values: Vec::new(),
            first: 1,
            last: 2
        }
    );
    assert_eq!(DEFAULT_CALLS.get(), 1);
    roundtrip(decoded);
    assert_eq!(DEFAULT_CALLS.get(), 1);

    DEFAULT_CALLS.set(0);
    let explicit = record(|serializer, body| {
        serializer.field("optional", &None::<u32>, body).unwrap();
        serializer.field("last", &2u32, body).unwrap();
        serializer.field("first", &1u32, body).unwrap();
        serializer
            .field("values", &Vec::<u32>::new(), body)
            .unwrap();
    });
    assert_eq!(decode::<Defaults>(&explicit).unwrap().optional, None);
    assert_eq!(DEFAULT_CALLS.get(), 0);
}

#[test]
fn failed_record_validation_never_calls_a_default_provider() {
    let cases = [
        record(|serializer, body| {
            serializer.field("first", &1u32, body).unwrap();
        }),
        record(|serializer, body| {
            serializer.field("first", &1u32, body).unwrap();
            serializer.field("last", &2u32, body).unwrap();
            serializer.field("unknown", &3u32, body).unwrap();
        }),
        record(|serializer, body| {
            serializer.field("first", &1u32, body).unwrap();
            serializer.field("first", &2u32, body).unwrap();
            serializer.field("last", &3u32, body).unwrap();
        }),
        record(|serializer, body| {
            serializer.field("first", &1u32, body).unwrap();
            serializer
                .field("last", &String::from("invalid"), body)
                .unwrap();
        }),
    ];
    for bytes in cases {
        DEFAULT_CALLS.set(0);
        assert!(decode::<Defaults>(&bytes).is_err());
        assert_eq!(DEFAULT_CALLS.get(), 0);
    }
}

#[test]
fn named_variant_defaults_follow_record_rules_and_errors_get_variant_context() {
    DEFAULT_CALLS.set(0);
    let inner = record(|serializer, body| {
        serializer.field("required", &7u32, body).unwrap();
    });
    let bytes = Serializer::<TestPolicy>::new()
        .variant("Named", Some(&inner))
        .unwrap();
    assert_eq!(
        decode::<Variants>(&bytes).unwrap(),
        Variants::Named {
            optional: Some(99),
            required: 7
        }
    );
    assert_eq!(DEFAULT_CALLS.get(), 1);

    let invalid_records = [
        record(|_, _| {}),
        record(|serializer, body| {
            serializer.field("required", &7u32, body).unwrap();
            serializer.field("unknown", &8u32, body).unwrap();
        }),
    ];
    for inner in invalid_records {
        DEFAULT_CALLS.set(0);
        let bytes = Serializer::<TestPolicy>::new()
            .variant("Named", Some(&inner))
            .unwrap();
        let error = decode::<Variants>(&bytes).unwrap_err();
        assert_eq!(error.path.first().unwrap(), "variant:Named");
        assert_eq!(DEFAULT_CALLS.get(), 0);
    }
    DEFAULT_CALLS.set(0);
    let missing_associated = Serializer::<TestPolicy>::new()
        .variant("Named", None)
        .unwrap();
    assert!(decode::<Variants>(&missing_associated).is_err());
    assert_eq!(DEFAULT_CALLS.get(), 0);
}

#[test]
fn enum_and_tuple_arity_is_validated_before_decoding_any_field() {
    for name in ["Newtype", "Many", "Named"] {
        let mut serializer = Serializer::<TestPolicy>::new();
        let missing = serializer.variant(name, None).unwrap();
        DECODE_CALLS.set(0);
        let error = decode::<Validation>(&missing).err().unwrap();
        assert_eq!(DECODE_CALLS.get(), 0);
        assert_eq!(error.path.first().unwrap(), &format!("variant:{name}"));

        let mut extra = serializer.variant(name, Some(b"payload")).unwrap();
        extra.extend(serializer.frame(b"extra").unwrap());
        DECODE_CALLS.set(0);
        assert!(decode::<Validation>(&extra).is_err());
        assert_eq!(DECODE_CALLS.get(), 0);
    }

    let mut serializer = Serializer::<TestPolicy>::new();
    let one_item = serializer.frame(b"item").unwrap();
    let wrong_tuple = serializer.variant("Many", Some(&one_item)).unwrap();
    DECODE_CALLS.set(0);
    let error = decode::<Validation>(&wrong_tuple).err().unwrap();
    assert!(matches!(
        error.kind,
        ErrorKind::EnvelopeCount {
            expected: 2,
            actual: 1
        }
    ));
    assert_eq!(DECODE_CALLS.get(), 0);

    let unit_with_data = serializer.variant("Unit", Some(b"")).unwrap();
    assert!(decode::<Variants>(&unit_with_data).is_err());
    let tuple_without_data = serializer.variant("EmptyTuple", None).unwrap();
    assert!(decode::<Variants>(&tuple_without_data).is_err());
    let named_without_record = serializer.variant("EmptyRecord", Some(b"")).unwrap();
    assert!(decode::<Variants>(&named_without_record).is_err());

    #[derive(Deserialize)]
    struct ProbeTuple(Probe, Probe);
    let bytes = serializer.frame(&one_item).unwrap();
    DECODE_CALLS.set(0);
    assert!(decode::<ProbeTuple>(&bytes).is_err());
    assert_eq!(DECODE_CALLS.get(), 0);
}

#[test]
fn tuple_field_errors_add_index_and_variant_context() {
    let mut serializer = Serializer::<TestPolicy>::new();
    let mut body = Vec::new();
    serializer.item(&Named { value: 1 }, &mut body).unwrap();
    serializer
        .item(&String::from("invalid"), &mut body)
        .unwrap();
    let bytes = serializer.variant("Many", Some(&body)).unwrap();
    let error = decode::<Variants>(&bytes).unwrap_err();
    assert_eq!(error.path, ["variant:Many", "index:1"]);
}

#[test]
fn recursive_generic_box_option_and_vec_fields_compile_and_roundtrip() {
    roundtrip(Node {
        value: 1u32,
        next: Some(Box::new(Node {
            value: 2,
            next: None,
            children: Vec::new(),
        })),
        children: vec![Node {
            value: 3,
            next: None,
            children: Vec::new(),
        }],
    });
    roundtrip(Chain::Next(Box::new(Chain::Next(Box::new(Chain::End(
        4u32,
    ))))));
}

#[test]
fn basic_storage_and_phantom_parameters_need_no_codec_traits() {
    let opaque = Opaque::<NoCodec> {
        value: 42,
        storage: NoCodec,
        generic_storage: PhantomData,
    };
    let bytes = encode(&opaque);
    assert_eq!(frames(&bytes)[0].0, b"42");
    assert_eq!(decode::<Opaque<NoCodec>>(&bytes).unwrap().value, 42);
    assert_eq!(<Opaque<NoCodec> as Serialize>::SHAPE, Shape::Basic);
    let invalid = Serializer::<TestPolicy>::new()
        .frame(b"not a number")
        .unwrap();
    assert!(decode::<Opaque<NoCodec>>(&invalid).is_err());
    let option = Some(opaque);
    let bytes = encode(&option);
    assert_eq!(frames(&bytes)[1].0, b"42");
    assert_eq!(
        decode::<Option<Opaque<NoCodec>>>(&bytes)
            .unwrap()
            .unwrap()
            .value,
        42
    );
    let marker = Marker::<NoCodec> {
        marker: PhantomData,
    };
    assert!(decode::<Marker<NoCodec>>(&encode(&marker)).is_ok());
}

#[test]
fn associated_types_and_field_defaults_do_not_overbound_generic_parameters() {
    let value = Associated::<ItemOwner> { item: 7 };
    assert_eq!(
        decode::<Associated<ItemOwner>>(&encode(&value))
            .unwrap()
            .item,
        7
    );
    let empty = record(|_, _| {});
    assert!(
        decode::<GenericDefaults<NoDefault>>(&empty)
            .unwrap()
            .values
            .is_empty()
    );
    let nonempty = GenericDefaults {
        values: vec![NoDefault(8)],
    };
    assert_eq!(
        decode::<GenericDefaults<NoDefault>>(&encode(&nonempty))
            .unwrap()
            .values,
        nonempty.values
    );
}

#[test]
fn borrowed_fields_and_internal_name_collisions_compile() {
    let bytes = encode(&Borrowed {
        text: "borrowed verbatim",
    });
    let value = decode::<Borrowed<'_>>(&bytes).unwrap();
    assert_eq!(value.text, "borrowed verbatim");
    let start = bytes.as_ptr() as usize;
    let end = start + bytes.len();
    assert!((start..end).contains(&(value.text.as_ptr() as usize)));

    let value = PolicyCollision { value: 5u32 };
    assert_eq!(
        decode::<PolicyCollision<u32>>(&encode(&value))
            .unwrap()
            .value,
        5
    );
    let value = ConstCollision::<3> { value: 6 };
    assert_eq!(
        decode::<ConstCollision<3>>(&encode(&value)).unwrap().value,
        6
    );
    let empty = record(|_, _| {});
    assert_eq!(decode::<LocalCollision>(&empty).unwrap().value, 12);
}

#[test]
fn raw_identifiers_are_unraw_on_the_wire_and_variant_selectors_are_verbatim() {
    let value = RawNames::r#match { r#type: 7 };
    let bytes = encode(&value);
    assert_eq!(frames(&bytes)[0].0, b"match");
    assert!(!String::from_utf8_lossy(&bytes).contains("r#"));
    roundtrip(value);

    let unknown = Serializer::<TestPolicy>::new()
        .variant(" match ", None)
        .unwrap();
    assert!(
        matches!(decode::<RawNames>(&unknown).unwrap_err().kind, ErrorKind::UnknownVariant(name) if name == " match ")
    );
    let unknown = Serializer::<TestPolicy>::new()
        .variant("Unknown", None)
        .unwrap();
    assert!(matches!(
        decode::<Never>(&unknown).err().unwrap().kind,
        ErrorKind::UnknownVariant(_)
    ));
}
