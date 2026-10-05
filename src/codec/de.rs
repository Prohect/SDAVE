use std::{collections::BTreeSet, marker::PhantomData};

use super::{
    BasicType, Config, DefaultNames, Deserialize, Error, ErrorKind, NamePolicy, Result,
    config::Context,
    framing::{self, Token},
    grammar::parse_field_header,
};
use crate::{error_new, error_payload};

/// A byte-identical leaf/body slice, with an absolute input location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Payload<'de> {
    bytes: &'de [u8],
    offset: usize,
}

impl<'de> Payload<'de> {
    pub fn new(bytes: &'de [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub fn with_offset(bytes: &'de [u8], offset: usize) -> Self {
        Self { bytes, offset }
    }

    pub fn bytes(self) -> &'de [u8] {
        self.bytes
    }

    pub fn offset(self) -> usize {
        self.offset
    }

    pub fn as_str(self) -> Result<&'de str> {
        std::str::from_utf8(self.bytes).map_err(|error| {
            error_payload!("expected UTF-8").at(self.offset.saturating_add(error.valid_up_to()))
        })
    }
}

/// Completed sibling value envelopes after one meaningful type/field marker.
/// Formatting-only gaps have already been removed; real empty frames remain.
#[derive(Debug)]
pub struct ValueRun<'de> {
    frames: Vec<Payload<'de>>,
    offset: usize,
}

impl<'de> ValueRun<'de> {
    fn new(frames: Vec<Payload<'de>>, offset: usize) -> Self {
        Self { frames, offset }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn into_frames(self) -> Vec<Payload<'de>> {
        self.frames
    }

    pub fn exact(self, expected: usize) -> Result<Vec<Payload<'de>>> {
        if self.frames.len() != expected {
            return Err(error_new!(ErrorKind::EnvelopeCount {
                expected,
                actual: self.frames.len(),
            })
            .at(self.offset));
        }
        Ok(self.frames)
    }

    pub fn single(self) -> Result<Payload<'de>> {
        let offset = self.offset;
        self.exact(1)?.into_iter().next().ok_or_else(|| {
            error_new!(ErrorKind::EnvelopeCount {
                expected: 1,
                actual: 0,
            })
            .at(offset)
        })
    }

    pub fn variant(self) -> Result<VariantRun<'de>> {
        let offset = self.offset;
        let count = self.frames.len();
        let mut frames = self.frames.into_iter();
        let selector = frames.next().ok_or_else(|| {
            error_new!(ErrorKind::EnvelopeCount {
                expected: 1,
                actual: count,
            })
            .at(offset)
        })?;
        let name = selector.as_str()?;
        Ok(VariantRun {
            name,
            rest: frames.collect(),
            offset: selector.offset(),
        })
    }
}

/// An enum selector and the as-yet-uninterpreted associated-value envelopes.
/// Selector payloads are never trimmed. The selected schema decides the arity.
#[derive(Debug)]
pub struct VariantRun<'de> {
    name: &'de str,
    rest: Vec<Payload<'de>>,
    offset: usize,
}

impl<'de> VariantRun<'de> {
    pub fn name(&self) -> &'de str {
        self.name
    }

    pub fn unit(self) -> Result<()> {
        if !self.rest.is_empty() {
            return Err(error_new!(ErrorKind::EnvelopeCount {
                expected: 1,
                actual: self.rest.len() + 1,
            })
            .at(self.offset));
        }
        Ok(())
    }

    pub fn associated(self) -> Result<Payload<'de>> {
        if self.rest.len() != 1 {
            return Err(error_new!(ErrorKind::EnvelopeCount {
                expected: 2,
                actual: self.rest.len() + 1,
            })
            .at(self.offset));
        }
        self.rest.into_iter().next().ok_or_else(|| {
            error_new!(ErrorKind::EnvelopeCount {
                expected: 2,
                actual: 1,
            })
            .at(self.offset)
        })
    }

    pub fn unknown(&self) -> Error {
        error_new!(ErrorKind::UnknownVariant(self.name.to_owned())).at(self.offset)
    }
}

struct Field<'de> {
    name: &'de str,
    marker: &'de str,
    run: Option<ValueRun<'de>>,
    offset: usize,
}

/// A confirmed named record. Presence is separate from decoded values, even
/// for `None`, empty strings/vectors and fields with default providers.
pub struct Record<'de> {
    fields: Vec<Field<'de>>,
    offset: usize,
}

impl<'de> Record<'de> {
    pub fn take<T: Deserialize<'de>, P: NamePolicy>(
        &mut self,
        name: &str,
        decoder: &mut Deserializer<P>,
    ) -> Result<Option<T>> {
        let Some(field) = self.fields.iter_mut().find(|field| field.name == name) else {
            return Ok(None);
        };
        let result = (|| {
            decoder.check_marker::<T>(field.marker, field.offset)?;
            let run = field.run.take().ok_or_else(|| {
                error_new!(ErrorKind::DuplicateField(name.into())).at(field.offset)
            })?;
            decoder.value::<T>(run).map(Some)
        })();
        result.map_err(|error: Error| error.in_field(name))
    }

    /// Reject every supplied field that was not in the selected static schema.
    pub fn finish(&self) -> Result<()> {
        if let Some(field) = self.fields.iter().find(|field| field.run.is_some()) {
            return Err(error_new!(ErrorKind::UnknownField(field.name.into()))
                .at(field.offset)
                .in_field(field.name));
        }
        Ok(())
    }

    pub fn require_present(&self, name: &str, present: bool) -> Result<()> {
        if present {
            Ok(())
        } else {
            Err(self.missing_field(name))
        }
    }

    pub fn missing_field(&self, name: &str) -> Error {
        error_new!(ErrorKind::MissingField(name.into()))
            .at(self.offset)
            .in_field(name)
    }
}

/// Whole-document decoder with immutable policy/profile and shared limits.
/// No unfinished framing is considered a successful record or a default.
pub struct Deserializer<P: NamePolicy = DefaultNames> {
    context: Context,
    policy: PhantomData<fn() -> P>,
}

impl<P: NamePolicy> Deserializer<P> {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            context: Context::new::<P>(config)?,
            policy: PhantomData,
        })
    }

    pub fn config(&self) -> &Config {
        &self.context.config
    }

    pub fn type_marker<T: ?Sized>(&self) -> Result<String> {
        self.context.names.render::<T>()
    }

    /// Validate that `marker` is the policy-rendered type marker for `T`.
    ///
    /// Neither side is abbreviated here, so a qualified spelling cannot bypass a
    /// policy that requires the short spelling. Tuple-comma padding is treated as
    /// equivalent, exactly as during decoding. No outer metadata trimming is
    /// performed, so pass an already-trimmed marker.
    pub fn check_marker<T: ?Sized>(&self, marker: &str, offset: usize) -> Result<()> {
        let expected = self.type_marker::<T>().map_err(|error| error.at(offset))?;
        if self
            .context
            .names
            .equivalent(marker, &expected)
            .map_err(|error| error.at(offset))?
        {
            Ok(())
        } else {
            Err(error_new!(ErrorKind::TypeMismatch {
                expected,
                found: marker.into(),
            })
            .at(offset))
        }
    }

    pub fn from_slice<'de, T: Deserialize<'de>>(&mut self, input: &'de [u8]) -> Result<T> {
        self.context.reset();
        let tokens = framing::parse(&mut self.context, Payload::new(input), true)?;
        let mut tokens = tokens.into_iter();
        let marker = match tokens.next() {
            Some(Token::Metadata(marker)) => marker,
            _ => return Err(error_new!(ErrorKind::MissingMarker).at(0)),
        };
        self.check_marker::<T>(marker.as_str()?, marker.offset())?;
        let mut frames = Vec::new();
        for token in tokens {
            match token {
                Token::Envelope(payload) => frames.push(payload),
                Token::Metadata(metadata) => {
                    return Err(error_new!(ErrorKind::UnexpectedMetadata(
                        metadata.as_str()?.into(),
                    ))
                    .at(metadata.offset()));
                }
            }
        }
        self.value(ValueRun::new(frames, marker.offset()))
    }

    pub fn from_str<'de, T: Deserialize<'de>>(&mut self, input: &'de str) -> Result<T> {
        self.from_slice(input.as_bytes())
    }

    pub fn value<'de, T: Deserialize<'de>>(&mut self, run: ValueRun<'de>) -> Result<T> {
        let offset = run.offset;
        self.context.enter().map_err(|error| error.at(offset))?;
        let result = T::deserialize_value(run, self);
        self.context.leave();
        result.map_err(|error| error.at(offset))
    }

    /// Interpret a body in its supplied type context, without a repeated marker.
    pub fn payload<'de, T: Deserialize<'de>>(&mut self, payload: Payload<'de>) -> Result<T> {
        self.context
            .input(payload.bytes().len())
            .map_err(|error| error.at(payload.offset()))?;
        self.context
            .enter()
            .map_err(|error| error.at(payload.offset()))?;
        let result = T::deserialize_payload(payload, self);
        self.context.leave();
        result.map_err(|error| error.at(payload.offset()))
    }

    /// Parse immediate sibling envelopes only; meaningful metadata is illegal
    /// in an item/tuple body, but ASCII formatting gaps are insignificant.
    pub fn run<'de>(&mut self, payload: Payload<'de>) -> Result<ValueRun<'de>> {
        let mut frames = Vec::new();
        for token in framing::parse(&mut self.context, payload, true)? {
            match token {
                Token::Envelope(item) => frames.push(item),
                Token::Metadata(metadata) => {
                    return Err(error_new!(ErrorKind::UnexpectedMetadata(
                        metadata.as_str()?.into(),
                    ))
                    .at(metadata.offset()));
                }
            }
        }
        Ok(ValueRun::new(frames, payload.offset()))
    }

    pub fn basic<'de, T: BasicType>(&mut self, run: ValueRun<'de>) -> Result<T> {
        let payload = run.single()?;
        T::decode_basic(payload, self).map_err(|error| error.at(payload.offset()))
    }

    pub fn record<'de>(&mut self, run: ValueRun<'de>) -> Result<Record<'de>> {
        let payload = run.single()?;
        let mut fields = Vec::<Field<'de>>::new();
        let mut names = BTreeSet::new();
        for token in framing::parse(&mut self.context, payload, true)? {
            match token {
                Token::Metadata(header) => {
                    let (name, marker) = parse_field_header(header.as_str()?)
                        .map_err(|error| error.at(header.offset()))?
                        .into_parts();
                    if !names.insert(name) {
                        return Err(error_new!(ErrorKind::DuplicateField(name.into()))
                            .at(header.offset())
                            .in_field(name));
                    }
                    fields.push(Field {
                        name,
                        marker,
                        run: Some(ValueRun::new(Vec::new(), header.offset())),
                        offset: header.offset(),
                    });
                }
                Token::Envelope(item) => {
                    let field = fields.last_mut().ok_or_else(|| {
                        error_new!(ErrorKind::InvalidFieldHeader(
                            "value without field metadata".into(),
                        ))
                        .at(item.offset())
                    })?;
                    if let Some(run) = &mut field.run {
                        run.frames.push(item);
                    }
                }
            }
        }
        Ok(Record {
            fields,
            offset: payload.offset(),
        })
    }
}
