//! The error type of the SDAVE codec.

/// Errors raised by the SDAVE codec.
///
/// The codec is static: every type it can serialize/deserialize is decided at
/// compile time, so these errors only describe malformed *data*, never a
/// missing/unsupported runtime capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The deserializer was given an empty `limiter_pairs` set whereas at least
    /// one well formed pair is required to frame/deframe an Envelop.
    EmptyLimiterPairs,
    /// No limiter pair of the provided set could frame the payload under the
    /// current [`Strategy`](crate::Strategy).
    NoSerializableLimiterPair,
    /// The buffer structure did not match what the target type expects, e.g. a
    /// signature was not followed by its value Envelop.
    UnexpectedItem,
    /// A `Variant::V2` Envelop ran to the end of the buffer while its delimiter
    /// slice was still unconfirmed: no following non-delimiter byte terminated it.
    ///
    /// Such an Envelop can only be accepted when the caller declares the buffer
    /// final (see the `final_buffer` argument of
    /// [`Deserialize::deserialize`](crate::Deserialize::deserialize)); otherwise
    /// the buffer could still grow and turn this Envelop into a NonEnvelop, so its
    /// payload MUST NOT be handed out.
    UnconfirmedDelimiter,
    /// A payload could not be interpreted as the target type, e.g. an integer
    /// payload was not a valid decimal.
    InvalidPayload,
    /// A payload was a well-formed decimal but its value overflowed the target
    /// integer type.
    ///
    /// Only the readable primitives (the `primitive` feature) make this distinction, so
    /// without that feature the variant is never produced.
    ArithmeticOverflow,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Error::EmptyLimiterPairs => "the deserializer was given no limiter pair",
            Error::NoSerializableLimiterPair => "no limiter pair could frame the payload",
            Error::UnexpectedItem => "the buffer structure did not match the target type",
            Error::UnconfirmedDelimiter => {
                "a V2 Envelop's delimiter slice was not confirmed by a following item"
            }
            Error::InvalidPayload => "the payload could not be interpreted as the target type",
            Error::ArithmeticOverflow => "the payload's value overflowed the target integer type",
        };
        f.write_str(message)
    }
}

impl core::error::Error for Error {}
