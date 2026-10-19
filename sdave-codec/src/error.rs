//! The error type of the SDAVE codec.

use sdave::Offset;

/// Errors raised by the SDAVE codec.
///
/// The codec is static: every type it can serialize/deserialize is decided at
/// compile time, so these errors only describe malformed *data*, never a
/// missing/unsupported runtime capability.
///
/// Every offset an error carries is absolute in the buffer the failing call was
/// given, and a nested call reads a raw *prefix* of that very buffer (see
/// [`with_deserializer`](crate::with_deserializer)), so the offsets a nested value
/// reports are directly comparable with the ones its parent reports.
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// No limiter pair of the provided set could frame the payload under the
    /// current [`Strategy`](crate::Strategy).
    NoSerializableLimiterPair {
        /// Length of the total limiter pairs the selection was given.
        len: Offset,
    },
    /// The buffer structure did not match what the target type expects.
    ///
    /// Three shapes produce it: a signature this channel would have accepted is not there
    /// at all (a field, an enum variant or a `dyn Trait` concrete type that never arrived);
    /// a signature is there but identifies nothing the type accepts — a discriminator
    /// matching none of the valid signatures, or a `NonEnvelop` in a slot whose wire form is
    /// the value Envelop alone (a positional field, an iterable element); and an item of the
    /// wrong kind sitting where the reader needs a signature-shaped marker.
    UnexpectedSignature {
        /// The signatures that were valid where the signature was caught, e.g. the field
        /// names of a composite, the variant names of an enum, or the concrete type
        /// names of a `dyn Trait`. Empty when no signature was acceptable there.
        valid_signatures: &'static [&'static str],
        /// What was provided instead, as absolute `(head, tail)` offsets.
        caught: (Offset, Offset),
    },
    /// An Envelop that must be there is missing.
    MissingEnvelop {
        /// `None` when `deserialize` was not fed a valid Envelop at all; the range of the
        /// signature that requires the missing Envelop(s) otherwise.
        required_by: Option<(Offset, Offset)>,
        /// What is caught: the whole range of the item that stood where the Envelop was
        /// required when `required_by` is `None`; the range the reader searched for the
        /// missing Envelop(s) — from the requiring signature's tail to the end of the
        /// channel it was read from — when it is `Some`.
        caught: (Offset, Offset),
    },
    /// A `Variant::V2` Envelop ran to the end of the buffer while its delimiter
    /// slice was still unconfirmed: no following non-delimiter byte terminated it.
    ///
    /// Such an Envelop can only be accepted when the caller declares the buffer
    /// final (see the `final_buffer` argument of
    /// [`Deserialize::deserialize`](crate::Deserialize::deserialize)); otherwise
    /// the buffer could still grow and turn this Envelop into a NonEnvelop, so its
    /// payload MUST NOT be handed out.
    UnconfirmedDelimiter {
        /// The limiter slice that was determined, as absolute `(head, tail)` offsets.
        limiter_slice: (Offset, Offset),
        /// The delimiter slice that stayed unconfirmed, as absolute `(head, tail)`
        /// offsets: the bytes the scan did reach.
        unconfirmed: (Offset, Offset),
    },
    /// A payload could not be interpreted as the target type, e.g. an integer
    /// payload was not a valid decimal.
    InvalidPayload {
        /// The type the payload was expected to hold, e.g. `u32`.
        r#type: &'static str,
        /// What was provided instead, as absolute `(head, tail)` offsets.
        caught: (Offset, Offset),
    },
    /// A payload was a well-formed decimal but its value overflowed the target
    /// integer type.
    ///
    /// Only the readable primitives (the `primitive` feature) make this distinction, so
    /// without that feature the variant is never produced.
    ArithmeticOverflow,
}

impl core::fmt::Debug for Error {
    // Written by hand: `Offset` is deliberately not `Debug` (an offset is only ever read as
    // a number), so every range is spelled out here.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoSerializableLimiterPair { len } => {
                write!(f, "NoSerializableLimiterPair {{ len: {} }}", len.get())
            }
            Error::UnexpectedSignature {
                valid_signatures,
                caught,
            } => write!(
                f,
                "UnexpectedSignature {{ valid_signatures: {:?}, caught: [{}..{}) }}",
                valid_signatures,
                caught.0.get(),
                caught.1.get()
            ),
            Error::MissingEnvelop {
                required_by,
                caught,
            } => match required_by {
                Some(required_by) => write!(
                    f,
                    "MissingEnvelop {{ required_by: [{}..{}), caught: [{}..{}) }}",
                    required_by.0.get(),
                    required_by.1.get(),
                    caught.0.get(),
                    caught.1.get()
                ),
                None => write!(
                    f,
                    "MissingEnvelop {{ required_by: None, caught: [{}..{}) }}",
                    caught.0.get(),
                    caught.1.get()
                ),
            },
            Error::UnconfirmedDelimiter {
                limiter_slice,
                unconfirmed,
            } => write!(
                f,
                "UnconfirmedDelimiter {{ limiter_slice: [{}..{}), unconfirmed: [{}..{}) }}",
                limiter_slice.0.get(),
                limiter_slice.1.get(),
                unconfirmed.0.get(),
                unconfirmed.1.get()
            ),
            Error::InvalidPayload { r#type, caught } => write!(
                f,
                "InvalidPayload {{ type: {}, caught: [{}..{}) }}",
                r#type,
                caught.0.get(),
                caught.1.get()
            ),
            Error::ArithmeticOverflow => f.write_str("ArithmeticOverflow"),
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoSerializableLimiterPair { len } => write!(
                f,
                "none of the {} limiter pair(s) provided could frame the payload",
                len.get()
            ),
            Error::UnexpectedSignature {
                valid_signatures,
                caught,
            } => write!(
                f,
                "the buffer structure did not match the target type: expected one of {:?}, \
                 caught [{}..{})",
                valid_signatures,
                caught.0.get(),
                caught.1.get()
            ),
            Error::MissingEnvelop {
                required_by,
                caught,
            } => match required_by {
                Some(required_by) => write!(
                    f,
                    "the signature [{}..{}) requires an Envelop that is missing from [{}..{})",
                    required_by.0.get(),
                    required_by.1.get(),
                    caught.0.get(),
                    caught.1.get()
                ),
                None => write!(
                    f,
                    "the buffer does not hold a valid Envelop at [{}..{})",
                    caught.0.get(),
                    caught.1.get()
                ),
            },
            Error::UnconfirmedDelimiter {
                limiter_slice,
                unconfirmed,
            } => write!(
                f,
                "a V2 Envelop's delimiter slice was not confirmed by a following item: \
                 limiter slice [{}..{}), unconfirmed delimiter slice [{}..{})",
                limiter_slice.0.get(),
                limiter_slice.1.get(),
                unconfirmed.0.get(),
                unconfirmed.1.get()
            ),
            Error::InvalidPayload { r#type, caught } => write!(
                f,
                "the payload [{}..{}) could not be interpreted as {}",
                caught.0.get(),
                caught.1.get(),
                r#type
            ),
            Error::ArithmeticOverflow => {
                f.write_str("the payload's value overflowed the target integer type")
            }
        }
    }
}

impl core::error::Error for Error {}

/// An [`Error`] rendered against the buffer its offsets index.
///
/// Built by [`fmt_error`]; it implements [`core::fmt::Display`], so a diagnostic is
/// `eprintln!("{}", fmt_error(error, buffer))` and needs no allocation.
///
/// Every range an [`Error`] carries is absolute in the buffer that was deserialized (see
/// the [`Error`] documentation), which is exactly what the caller still owns when a
/// `deserialize` call returns `Err`.
#[cfg(feature = "primitive")]
pub struct ErrorDisplay<'a> {
    error: Error,
    buffer: &'a [u8],
}

/// Render `error` against `buffer`: the bytes the offsets it carries index.
///
/// The wire form is only text when the readable primitives wrote it, so `buffer` is *not*
/// assumed to be UTF-8. A slice that happens to be valid UTF-8 is quoted as text (with
/// `"`, `\` and control characters escaped); anything else is quoted byte by byte as
/// `\xNN`. A range that the buffer no longer holds — e.g. after
/// [`FlatDeserializer::advance`](sdave::FlatDeserializer::advance) moved the offsets out
/// of it — is reported as outside the buffer instead of panicking.
///
/// ```
/// # use sdave_codec::*;
/// // A `u32` reader that found `9i` at `[2, 4)` in `^^9i~~`.
/// let error = Error::InvalidPayload {
///     r#type: "u32",
///     caught: (Offset::new(2), Offset::new(4)),
/// };
/// assert_eq!(
///     fmt_error(error, b"^^9i~~").to_string(),
///     r#"InvalidPayload { type: u32, caught: [2..4) "9i" }"#,
/// );
/// ```
#[cfg(feature = "primitive")]
pub fn fmt_error<'a>(error: Error, buffer: &'a [u8]) -> ErrorDisplay<'a> {
    ErrorDisplay { error, buffer }
}

#[cfg(feature = "primitive")]
impl core::fmt::Display for ErrorDisplay<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.error {
            Error::NoSerializableLimiterPair { len } => {
                write!(f, "NoSerializableLimiterPair {{ len: {} }}", len.get())
            }
            Error::UnexpectedSignature {
                valid_signatures,
                caught,
            } => {
                write!(
                    f,
                    "UnexpectedSignature {{ valid_signatures: {:?}, caught: ",
                    valid_signatures
                )?;
                write_range(f, self.buffer, caught)?;
                f.write_str(" }")
            }
            Error::MissingEnvelop {
                required_by,
                caught,
            } => {
                f.write_str("MissingEnvelop { required_by: ")?;
                match required_by {
                    Some(required_by) => write_range(f, self.buffer, required_by)?,
                    None => f.write_str("None")?,
                }
                f.write_str(", caught: ")?;
                write_range(f, self.buffer, caught)?;
                f.write_str(" }")
            }
            Error::UnconfirmedDelimiter {
                limiter_slice,
                unconfirmed,
            } => {
                f.write_str("UnconfirmedDelimiter { limiter_slice: ")?;
                write_range(f, self.buffer, limiter_slice)?;
                // Only a deframer over a flattened buffer raises this, and it settled the
                // payload between the two slices it reports: `payload_head` is the limiter
                // slice's tail, `payload_tail` is the unconfirmed delimiter's head.
                f.write_str(", payload: ")?;
                write_range(f, self.buffer, (limiter_slice.1, unconfirmed.0))?;
                f.write_str(", unconfirmed: ")?;
                write_range(f, self.buffer, unconfirmed)?;
                f.write_str(" }")
            }
            Error::InvalidPayload { r#type, caught } => {
                write!(f, "InvalidPayload {{ type: {}, caught: ", r#type)?;
                write_range(f, self.buffer, caught)?;
                f.write_str(" }")
            }
            Error::ArithmeticOverflow => f.write_str("ArithmeticOverflow"),
        }
    }
}

/// Write `range` as `[head..tail)` followed by the bytes it covers in `buffer`, quoted.
#[cfg(feature = "primitive")]
fn write_range(
    f: &mut core::fmt::Formatter<'_>,
    buffer: &[u8],
    range: (Offset, Offset),
) -> core::fmt::Result {
    let (head, tail) = (range.0.get(), range.1.get());
    write!(f, "[{}..{})", head, tail)?;
    match buffer.get(head..tail) {
        Some(bytes) => {
            f.write_str(" \"")?;
            write_quoted(f, bytes)?;
            f.write_str("\"")
        }
        // A diagnostic MUST NOT panic because the buffer moved under the offsets (or
        // because a caller paired the error with a different buffer).
        None => f.write_str(" <outside the buffer>"),
    }
}

/// Write `bytes` as always-printable, quoted text: UTF-8 as itself (with `"`, `\` and
/// control characters escaped), anything else byte by byte as `\xNN`.
#[cfg(feature = "primitive")]
fn write_quoted(f: &mut core::fmt::Formatter<'_>, bytes: &[u8]) -> core::fmt::Result {
    match core::str::from_utf8(bytes) {
        Ok(text) => {
            for character in text.chars() {
                match character {
                    '"' => f.write_str("\\\"")?,
                    '\\' => f.write_str("\\\\")?,
                    control if control.is_control() => write!(f, "\\u{{{:x}}}", control as u32)?,
                    character => write!(f, "{}", character)?,
                }
            }
            Ok(())
        }
        Err(_) => {
            for &byte in bytes {
                match byte {
                    b'"' => f.write_str("\\\"")?,
                    b'\\' => f.write_str("\\\\")?,
                    0x20..=0x7e => write!(f, "{}", byte as char)?,
                    byte => write!(f, "\\x{:02x}", byte)?,
                }
            }
            Ok(())
        }
    }
}
