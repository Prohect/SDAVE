//! Verbatim payloads: what an envelope holds.
//!
//! A [`Payload`] is the text a leaf contributes to the envelope around it. Leaves are written by
//! exactly one method, so a payload can never diverge from the flow: [`crate::Serialize`]
//! implementations for the same types delegate to it.

use core::fmt::Write as _;
use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize,
};

use crate::{Error, ErrorKind, NonMaxUsize, Sink};

/// A value that contributes a verbatim payload to the envelope around it.
///
/// The payload of a number is its canonical decimal text (`-001` is not produced, and `-1` is); a
/// float is finite only; a `bool` is `true`/`false`; a `char` is its UTF-8 encoding, so one unit of
/// the payload is one encoded byte, not one scalar. `str` and `[u8]` carry their contents verbatim:
/// for a byte unit the bytes are the units, for a scalar unit the bytes are decoded as UTF-8.
pub trait Payload {
    /// Append this value's payload.
    fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error>;
}

/// The exact decimal text of `magnitude`, negated when `negative`.
///
/// `u128::MAX` is 39 digits and a sign can precede them, so 40 digits always suffice.
pub(crate) fn decimal(sink: &mut Sink<'_>, negative: bool, magnitude: u128) -> Result<(), Error> {
    let mut digits = [0u8; 40];
    let mut index = digits.len();
    let mut rest = magnitude;
    loop {
        index -= 1;
        digits[index] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if negative {
        index -= 1;
        digits[index] = b'-';
    }
    sink.text(as_ascii(&digits[index..])?)
}

/// The canonical text of a finite float.
///
/// `Display` is Rust's shortest representation that round-trips, and it never uses an exponent, so
/// the longest `f64` spelling is a sign plus 309 digits and a point: 512 bytes always suffice.
pub(crate) fn finite<F: core::fmt::Display>(sink: &mut Sink<'_>, value: F) -> Result<(), Error> {
    let mut buffer = TextBuffer::new();
    write!(buffer, "{value}").map_err(|_| {
        Error::new(ErrorKind::Invariant(
            "float text overflowed the formatting buffer",
        ))
    })?;
    sink.text(as_ascii(buffer.as_bytes())?)
}

/// Borrow a byte slice that is known to be ASCII as text.
fn as_ascii(bytes: &[u8]) -> Result<&str, Error> {
    core::str::from_utf8(bytes)
        .map_err(|_| Error::new(ErrorKind::Invariant("the codec emits ASCII only")))
}

/// A fixed formatting buffer: numbers are formatted without allocating.
struct TextBuffer {
    bytes: [u8; 512],
    len: usize,
}

impl TextBuffer {
    fn new() -> Self {
        TextBuffer {
            bytes: [0; 512],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl core::fmt::Write for TextBuffer {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let bytes = text.as_bytes();
        if self.len + bytes.len() > self.bytes.len() {
            return Err(core::fmt::Error);
        }
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }
}

/// Every unsigned type widens losslessly into `u128`, which is the decimal writer's input.
macro_rules! unsigned_payload {
    ($($ty:ty),* $(,)?) => {$(
        impl Payload for $ty {
            fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
                decimal(sink, false, *self as u128)
            }
        }
    )*};
}

macro_rules! signed_payload {
    ($($ty:ty),* $(,)?) => {$(
        impl Payload for $ty {
            fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
                decimal(sink, *self < 0, self.unsigned_abs() as u128)
            }
        }
    )*};
}

macro_rules! nonzero_unsigned_payload {
    ($($ty:ty),* $(,)?) => {$(
        impl Payload for $ty {
            fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
                decimal(sink, false, self.get() as u128)
            }
        }
    )*};
}

macro_rules! nonzero_signed_payload {
    ($($ty:ty),* $(,)?) => {$(
        impl Payload for $ty {
            fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
                let value = self.get();
                decimal(sink, value < 0, value.unsigned_abs() as u128)
            }
        }
    )*};
}

unsigned_payload!(u8, u16, u32, u64, u128, usize);
signed_payload!(i8, i16, i32, i64, i128, isize);
nonzero_unsigned_payload!(
    NonZeroU8,
    NonZeroU16,
    NonZeroU32,
    NonZeroU64,
    NonZeroU128,
    NonZeroUsize
);
nonzero_signed_payload!(
    NonZeroI8,
    NonZeroI16,
    NonZeroI32,
    NonZeroI64,
    NonZeroI128,
    NonZeroIsize
);

impl Payload for NonMaxUsize {
    fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        decimal(sink, false, self.get() as u128)
    }
}

impl Payload for bool {
    fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.text(if *self { "true" } else { "false" })
    }
}

impl Payload for char {
    fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut encoded = [0u8; 4];
        sink.utf8(self.encode_utf8(&mut encoded).as_bytes())
    }
}

impl Payload for f32 {
    fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        if !self.is_finite() {
            return Err(Error::new(ErrorKind::NonFiniteFloat));
        }
        finite(sink, *self)
    }
}

impl Payload for f64 {
    fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        if !self.is_finite() {
            return Err(Error::new(ErrorKind::NonFiniteFloat));
        }
        finite(sink, *self)
    }
}

impl Payload for str {
    fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.utf8(self.as_bytes())
    }
}

impl Payload for [u8] {
    fn payload(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        sink.utf8(self)
    }
}

impl Payload for () {
    fn payload(&self, _sink: &mut Sink<'_>) -> Result<(), Error> {
        Ok(())
    }
}
