//! The document unit `T`: the element type of the `[T]` buffers SDAVE frames.
//!
//! `sdave`'s buffers are `[T]` for any `T: Clone + PartialEq`; the codec additionally writes
//! *text* into them (markers, field headers, canonical decimal integers) and reads that text back,
//! so it bounds `T` by [`Unit`]. The document's text is UTF-8 in every case; the unit decides
//! whether the buffer holds one byte per unit (`u8`) or one scalar per unit (`char`).

use crate::{Error, ErrorKind};

/// The document unit: the `T` of the `[T]` buffers SDAVE frames.
///
/// Two unit types are implemented, and they are the two sensible ones:
///
/// - `u8`: a unit *is* a byte, so the buffer is a UTF-8 (or, for byte payloads, arbitrary) byte
///   string;
/// - `char`: a unit is a Unicode scalar value, so the buffer is text stored one scalar per unit.
pub trait Unit: Clone + PartialEq + Sized {
    /// The unit for an ASCII byte. Every ASCII byte is representable in every unit type.
    fn from_ascii(byte: u8) -> Self;
    /// The ASCII byte for this unit, or `None` when the unit is not ASCII.
    fn to_ascii(&self) -> Option<u8>;
    /// Decode one unit from the head of a UTF-8 fragment, returning it and its byte length.
    ///
    /// A leading byte that cannot start a UTF-8 sequence is [`ErrorKind::InvalidUtf8`]; a
    /// fragment that holds only a prefix of a sequence is [`ErrorKind::UnexpectedEnd`].
    fn from_utf8(bytes: &[u8]) -> Result<(Self, usize), Error>;
    /// The number of bytes [`Unit::from_utf8`] consumes for this unit: always 1 for a byte unit,
    /// the scalar's encoded length for a scalar unit.
    fn utf8_len(&self) -> usize;
    /// A stable numeric identity, used only by the profile fingerprint.
    fn code(&self) -> u32;
    /// The units as UTF-8 text, when a unit *is* a byte.
    ///
    /// `Some` lets a leaf borrow its payload verbatim from the input (`&str`, `&[u8]`); a scalar
    /// unit has no byte view of the document, so it answers `None` and those leaves report
    /// [`ErrorKind::NotBorrowable`].
    fn as_text(units: &[Self]) -> Option<&str>;
    /// The units as bytes, when a unit *is* a byte.
    fn as_bytes(units: &[Self]) -> Option<&[u8]>;
}

impl Unit for u8 {
    fn from_ascii(byte: u8) -> Self {
        byte
    }

    fn to_ascii(&self) -> Option<u8> {
        Some(*self)
    }

    fn from_utf8(bytes: &[u8]) -> Result<(Self, usize), Error> {
        match bytes.first() {
            Some(byte) => Ok((*byte, 1)),
            None => Err(Error::new(ErrorKind::UnexpectedEnd)),
        }
    }

    fn utf8_len(&self) -> usize {
        1
    }

    fn code(&self) -> u32 {
        u32::from(*self)
    }

    fn as_text(units: &[Self]) -> Option<&str> {
        core::str::from_utf8(units).ok()
    }

    fn as_bytes(units: &[Self]) -> Option<&[u8]> {
        Some(units)
    }
}

impl Unit for char {
    fn from_ascii(byte: u8) -> Self {
        char::from(byte)
    }

    fn to_ascii(&self) -> Option<u8> {
        if self.is_ascii() {
            Some(*self as u8)
        } else {
            None
        }
    }

    fn from_utf8(bytes: &[u8]) -> Result<(Self, usize), Error> {
        let Some(lead) = bytes.first() else {
            return Err(Error::new(ErrorKind::UnexpectedEnd));
        };
        let needed = match lead {
            0x00..=0x7F => 1,
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            // 0x80..=0xC1 and 0xF5.. are neither valid lead bytes nor standalone units:
            // continuation bytes, overlong starts and out-of-range starts.
            _ => return Err(Error::new(ErrorKind::InvalidUtf8)),
        };
        if bytes.len() < needed {
            return Err(Error::new(ErrorKind::UnexpectedEnd));
        }
        match core::str::from_utf8(&bytes[..needed]) {
            Ok(text) => match text.chars().next() {
                Some(unit) => Ok((unit, needed)),
                None => Err(Error::new(ErrorKind::InvalidUtf8)),
            },
            Err(_) => Err(Error::new(ErrorKind::InvalidUtf8)),
        }
    }

    fn utf8_len(&self) -> usize {
        self.len_utf8()
    }

    fn code(&self) -> u32 {
        *self as u32
    }

    fn as_text(_units: &[Self]) -> Option<&str> {
        None
    }

    fn as_bytes(_units: &[Self]) -> Option<&[u8]> {
        None
    }
}
