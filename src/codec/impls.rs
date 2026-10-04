use std::num::*;

use crate::{error_new, error_payload};

use super::{
    BasicType, Deserialize, Deserializer, Error, ErrorKind, NamePolicy, Payload, Result, Serialize,
    Serializer, Shape, ValueRun,
};

/// Owned, opaque binary bytes. `Vec<u8>` instead means a homogeneous integer list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ByteBuf(pub Vec<u8>);

/// Borrowed, opaque binary bytes, without UTF-8 interpretation or escaping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bytes<'a>(pub &'a [u8]);

macro_rules! basic_impl {
    ($($ty:ty),* $(,)?) => {$ (
        impl Serialize for $ty {
            const SHAPE: Shape = Shape::Basic;
            fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
                serializer.basic(self)
            }
            fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
                self.encode_basic(serializer)
            }
        }
        impl<'de> Deserialize<'de> for $ty {
            const SHAPE: Shape = Shape::Basic;
            fn deserialize_value<P: NamePolicy>(run: ValueRun<'de>, decoder: &mut Deserializer<P>) -> Result<Self> {
                decoder.basic(run)
            }
            fn deserialize_payload<P: NamePolicy>(payload: Payload<'de>, decoder: &mut Deserializer<P>) -> Result<Self> {
                Self::decode_basic(payload, decoder)
            }
        }
    )*};
}

macro_rules! integers {
    ($signed:expr; $($ty:ty),* $(,)?) => {$ (
        impl BasicType for $ty {
            fn encode_basic<P: NamePolicy>(&self, _serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
                Ok(self.to_string().into_bytes())
            }
            fn decode_basic<P: NamePolicy>(payload: Payload<'_>, _decoder: &mut Deserializer<P>) -> Result<Self> {
                let text = payload.as_str()?;
                let digits = if $signed { text.strip_prefix('-').unwrap_or(text) } else { text };
                if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(error_payload!(concat!("expected decimal ", stringify!($ty))).at(payload.offset()));
                }
                text.parse::<Self>().map_err(|error| error_payload!(error.to_string()).at(payload.offset()))
            }
        }
        basic_impl!($ty);
    )*};
}

integers!(false; u8, u16, u32, u64, u128, usize);
integers!(true; i8, i16, i32, i64, i128, isize);

macro_rules! floats {
    ($($ty:ty),* $(,)?) => {$ (
        impl BasicType for $ty {
            fn encode_basic<P: NamePolicy>(&self, _serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
                if !self.is_finite() {
                    return Err(error_payload!("nonfinite floats are unsupported"));
                }
                Ok(self.to_string().into_bytes())
            }
            fn decode_basic<P: NamePolicy>(payload: Payload<'_>, _decoder: &mut Deserializer<P>) -> Result<Self> {
                let value = payload.as_str()?.parse::<Self>().map_err(|error| error_payload!(error.to_string()).at(payload.offset()))?;
                if !value.is_finite() {
                    return Err(error_payload!("nonfinite floats are unsupported").at(payload.offset()));
                }
                Ok(value)
            }
        }
        basic_impl!($ty);
    )*};
}

floats!(f32, f64);

impl BasicType for bool {
    fn encode_basic<P: NamePolicy>(&self, _serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        Ok(if *self {
            b"true".to_vec()
        } else {
            b"false".to_vec()
        })
    }
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        _decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        match payload.bytes() {
            b"true" => Ok(true),
            b"false" => Ok(false),
            _ => Err(error_payload!("expected true or false").at(payload.offset())),
        }
    }
}

impl BasicType for char {
    fn encode_basic<P: NamePolicy>(&self, _serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        Ok(self.to_string().into_bytes())
    }
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        _decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let mut chars = payload.as_str()?.chars();
        let ch = chars
            .next()
            .ok_or_else(|| error_payload!("expected one Unicode scalar").at(payload.offset()))?;
        if chars.next().is_some() {
            return Err(error_payload!("expected one Unicode scalar").at(payload.offset()));
        }
        Ok(ch)
    }
}

impl BasicType for String {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        serializer.append(&mut output, self.as_bytes())?;
        Ok(output)
    }
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        _decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Ok(payload.as_str()?.to_owned())
    }
}

impl BasicType for ByteBuf {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        serializer.append(&mut output, &self.0)?;
        Ok(output)
    }
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        _decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Ok(Self(payload.bytes().to_vec()))
    }
}

basic_impl!(bool, char, String, ByteBuf);

impl Serialize for str {
    const SHAPE: Shape = Shape::Basic;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        serializer.frame(self.as_bytes())
    }
    fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        serializer.append(&mut output, self.as_bytes())?;
        Ok(output)
    }
}

impl<T: Serialize + ?Sized> Serialize for &T {
    const SHAPE: Shape = T::SHAPE;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        T::serialize_value(self, serializer)
    }
    fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        T::serialize_payload(self, serializer)
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for &'a str {
    const SHAPE: Shape = Shape::Basic;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        _decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        run.single()?.as_str()
    }
    fn deserialize_payload<P: NamePolicy>(
        payload: Payload<'de>,
        _decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        payload.as_str()
    }
}

impl Serialize for Bytes<'_> {
    const SHAPE: Shape = Shape::Basic;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        serializer.frame(self.0)
    }
    fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        serializer.append(&mut output, self.0)?;
        Ok(output)
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Bytes<'a> {
    const SHAPE: Shape = Shape::Basic;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        _decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Ok(Self(run.single()?.bytes()))
    }
    fn deserialize_payload<P: NamePolicy>(
        payload: Payload<'de>,
        _decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Ok(Self(payload.bytes()))
    }
}

macro_rules! nonzero {
    ($($ty:ty => $primitive:ty),* $(,)?) => {$ (
        impl BasicType for $ty {
            fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
                serializer.payload(&self.get())
            }
            fn decode_basic<P: NamePolicy>(payload: Payload<'_>, decoder: &mut Deserializer<P>) -> Result<Self> {
                let value = decoder.payload::<$primitive>(payload)?;
                Self::new(value).ok_or_else(|| error_payload!("nonzero value cannot be zero").at(payload.offset()))
            }
        }
        basic_impl!($ty);
    )*};
}

nonzero!(
    NonZeroU8 => u8, NonZeroU16 => u16, NonZeroU32 => u32,
    NonZeroU64 => u64, NonZeroU128 => u128, NonZeroUsize => usize,
    NonZeroI8 => i8, NonZeroI16 => i16, NonZeroI32 => i32,
    NonZeroI64 => i64, NonZeroI128 => i128, NonZeroIsize => isize,
);

impl BasicType for crate::NonMaxUsize {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        serializer.payload(&self.get())
    }
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let value = decoder.payload::<usize>(payload)?;
        if value == usize::MAX {
            return Err(error_payload!("NonMaxUsize excludes usize::MAX").at(payload.offset()));
        }
        // SAFETY: the excluded value was checked, so new cannot panic.
        Ok(unsafe { Self::new(value) })
    }
}

basic_impl!(crate::NonMaxUsize);

impl<T: Serialize + ?Sized> Serialize for Box<T> {
    const SHAPE: Shape = T::SHAPE;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        T::serialize_value(self, serializer)
    }
    fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        T::serialize_payload(self, serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Box<T> {
    const SHAPE: Shape = T::SHAPE;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Ok(Box::new(T::deserialize_value(run, decoder)?))
    }
    fn deserialize_payload<P: NamePolicy>(
        payload: Payload<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Ok(Box::new(T::deserialize_payload(payload, decoder)?))
    }
}

impl<T: Serialize> Serialize for Vec<T> {
    const SHAPE: Shape = Shape::List;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let mut output = serializer.frame(&[])?;
        for (index, value) in self.iter().enumerate() {
            serializer
                .item(value, &mut output)
                .map_err(|error| error.at_index(index))?;
        }
        Ok(output)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Vec<T> {
    const SHAPE: Shape = Shape::List;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let offset = run.offset();
        let mut frames = run.into_frames().into_iter();
        let header = frames
            .next()
            .ok_or_else(|| error_new!(ErrorKind::MissingListHeader).at(offset))?;
        if !header.bytes().is_empty() {
            return Err(error_new!(ErrorKind::NonemptyListHeader).at(header.offset()));
        }
        frames
            .enumerate()
            .map(|(index, item)| {
                decoder
                    .payload::<T>(item)
                    .map_err(|error| error.at_index(index))
            })
            .collect()
    }
}

impl<T: Serialize> Serialize for Option<T> {
    const SHAPE: Shape = Shape::Enum;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        match self {
            None => serializer.variant("None", None),
            Some(value) => (|| {
                let payload = serializer.payload(value)?;
                serializer.variant("Some", Some(&payload))
            })()
            .map_err(|error: Error| error.in_variant("Some")),
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
            "None" => variant
                .unit()
                .map(|()| None)
                .map_err(|error| error.in_variant("None")),
            "Some" => (|| decoder.payload::<T>(variant.associated()?).map(Some))()
                .map_err(|error: Error| error.in_variant("Some")),
            _ => Err(variant.unknown()),
        }
    }
}

impl<T: Serialize, E: Serialize> Serialize for std::result::Result<T, E> {
    const SHAPE: Shape = Shape::Enum;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        let (name, payload) = match self {
            Ok(value) => (
                "Ok",
                serializer
                    .payload(value)
                    .map_err(|error| error.in_variant("Ok"))?,
            ),
            Err(value) => (
                "Err",
                serializer
                    .payload(value)
                    .map_err(|error| error.in_variant("Err"))?,
            ),
        };
        serializer
            .variant(name, Some(&payload))
            .map_err(|error| error.in_variant(name))
    }
}

impl<'de, T: Deserialize<'de>, E: Deserialize<'de>> Deserialize<'de> for std::result::Result<T, E> {
    const SHAPE: Shape = Shape::Enum;
    fn deserialize_value<P: NamePolicy>(
        run: ValueRun<'de>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let variant = run.variant()?;
        match variant.name() {
            "Ok" => (|| decoder.payload::<T>(variant.associated()?).map(Ok))()
                .map_err(|error: Error| error.in_variant("Ok")),
            "Err" => (|| decoder.payload::<E>(variant.associated()?).map(Err))()
                .map_err(|error: Error| error.in_variant("Err")),
            _ => Err(variant.unknown()),
        }
    }
}

impl Serialize for () {
    const SHAPE: Shape = Shape::Tuple;
    fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        serializer.frame(&[])
    }
    fn serialize_payload<P: NamePolicy>(&self, _serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

impl<'de> Deserialize<'de> for () {
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
        Ok(())
    }
}

macro_rules! tuple {
    ($count:expr; $($ty:ident $value:ident $index:tt),+ $(,)?) => {
        impl<$($ty: Serialize),+> Serialize for ($($ty,)+) {
            const SHAPE: Shape = Shape::Tuple;
            fn serialize_value<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
                let payload = self.serialize_payload(serializer)?;
                serializer.frame(&payload)
            }
            fn serialize_payload<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
                let mut output = Vec::new();
                $(serializer.item(&self.$index, &mut output).map_err(|error| error.at_index($index))?;)+
                Ok(output)
            }
        }
        impl<'de, $($ty: Deserialize<'de>),+> Deserialize<'de> for ($($ty,)+) {
            const SHAPE: Shape = Shape::Tuple;
            fn deserialize_value<P: NamePolicy>(run: ValueRun<'de>, decoder: &mut Deserializer<P>) -> Result<Self> {
                Self::deserialize_payload(run.single()?, decoder)
            }
            fn deserialize_payload<P: NamePolicy>(payload: Payload<'de>, decoder: &mut Deserializer<P>) -> Result<Self> {
                let mut items = decoder.run(payload)?.exact($count)?.into_iter();
                $(let $value = items.next().ok_or_else(|| error_new!(ErrorKind::EnvelopeCount { expected: $count, actual: $index }).at(payload.offset()))?;
                  let $value = decoder.payload::<$ty>($value).map_err(|error| error.at_index($index))?;)+
                Ok(($($value,)+))
            }
        }
    };
}

tuple!(1; A a 0);
tuple!(2; A a 0, B b 1);
tuple!(3; A a 0, B b 1, C c 2);
tuple!(4; A a 0, B b 1, C c 2, D d 3);
tuple!(5; A a 0, B b 1, C c 2, D d 3, E e 4);
tuple!(6; A a 0, B b 1, C c 2, D d 3, E e 4, F f 5);
tuple!(7; A a 0, B b 1, C c 2, D d 3, E e 4, F f 5, G g 6);
tuple!(8; A a 0, B b 1, C c 2, D d 3, E e 4, F f 5, G g 6, H h 7);
tuple!(9; A a 0, B b 1, C c 2, D d 3, E e 4, F f 5, G g 6, H h 7, I i 8);
tuple!(10; A a 0, B b 1, C c 2, D d 3, E e 4, F f 5, G g 6, H h 7, I i 8, J j 9);
tuple!(11; A a 0, B b 1, C c 2, D d 3, E e 4, F f 5, G g 6, H h 7, I i 8, J j 9, K k 10);
tuple!(12; A a 0, B b 1, C c 2, D d 3, E e 4, F f 5, G g 6, H h 7, I i 8, J j 9, K k 10, L l 11);
