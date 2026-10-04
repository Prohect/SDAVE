use std::{io::Write, marker::PhantomData};

use crate::error_payload;

use super::{
    BasicType, Config, DefaultNames, Error, NamePolicy, Result, Serialize, config::Context, framing,
};

/// A buffered serializer with a fixed compile-time naming policy and profile.
///
/// Every document method resets resource accounting, but never the policy or
/// framing configuration. Child/payload operations are for native trait and
/// semantic helper implementations; they share the active document's budget.
pub struct Serializer<P: NamePolicy = DefaultNames> {
    context: Context,
    policy: PhantomData<fn() -> P>,
}

impl<P: NamePolicy> Serializer<P> {
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

    pub fn to_vec<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<Vec<u8>> {
        self.context.reset();
        let marker = self.type_marker::<T>()?;
        let mut output = framing::marker(&mut self.context, &marker)?;
        let run = self.value(value)?;
        self.append(&mut output, &run)?;
        Ok(output)
    }

    pub fn to_string<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<String> {
        String::from_utf8(self.to_vec(value)?)
            .map_err(|_| error_payload!("document contains non-UTF-8 basic bytes; use to_vec"))
    }

    pub fn to_writer<W: Write, T: Serialize + ?Sized>(
        &mut self,
        mut writer: W,
        value: &T,
    ) -> Result<()> {
        let output = self.to_vec(value)?;
        writer.write_all(&output)?;
        Ok(())
    }

    /// Encode a context-known value run, without repeating its type marker.
    pub fn value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<Vec<u8>> {
        self.context.enter()?;
        let result = value.serialize_value(self);
        self.context.leave();
        let bytes = result?;
        self.context.output(bytes.len())?;
        Ok(bytes)
    }

    /// Encode only an item/associated-value body, not its enclosing envelope.
    /// Primitive and basic helpers use this to encode semantic representations.
    pub fn payload<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<Vec<u8>> {
        self.context.enter()?;
        let result = value.serialize_payload(self);
        self.context.leave();
        let bytes = result?;
        self.context.output(bytes.len())?;
        Ok(bytes)
    }

    pub fn frame(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        framing::frame(&mut self.context, payload)
    }

    /// Append bytes only after checking the enclosing output-size bound.
    pub fn append(&self, output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
        let length = output.len().checked_add(bytes.len()).ok_or_else(|| {
            self.context.limit(
                "output bytes",
                self.context.config.limits().max_output_bytes,
            )
        })?;
        self.context.output(length)?;
        output.extend_from_slice(bytes);
        Ok(())
    }

    /// Emit one named field in declaration order. Its header supplies the type;
    /// a structural child therefore writes its record envelope directly.
    pub fn field<T: Serialize + ?Sized>(
        &mut self,
        name: &str,
        value: &T,
        output: &mut Vec<u8>,
    ) -> Result<()> {
        (|| {
            let type_marker = self.type_marker::<T>()?;
            self.context.output(
                name.len()
                    .saturating_add(2)
                    .saturating_add(type_marker.len())
                    .saturating_add(1),
            )?;
            let marker = format!("{name}: {type_marker}");
            let header = framing::marker(&mut self.context, &marker)?;
            let run = self.value(value)?;
            self.append(output, &header)?;
            self.append(output, &run)
        })()
        .map_err(|error: Error| error.in_field(name))
    }

    /// A positional child always has exactly one enclosing item envelope.
    pub fn item<T: Serialize + ?Sized>(&mut self, value: &T, output: &mut Vec<u8>) -> Result<()> {
        let payload = self.payload(value)?;
        let envelope = self.frame(&payload)?;
        self.append(output, &envelope)
    }

    pub fn variant(&mut self, name: &str, associated: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut output = self.frame(name.as_bytes())?;
        if let Some(payload) = associated {
            let envelope = self.frame(payload)?;
            self.append(&mut output, &envelope)?;
        }
        Ok(output)
    }

    pub fn basic<T: BasicType>(&mut self, value: &T) -> Result<Vec<u8>> {
        let payload = value.encode_basic(self)?;
        self.frame(&payload)
    }
}
