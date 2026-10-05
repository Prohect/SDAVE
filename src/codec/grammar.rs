//! Public grammar primitives shared by the typed codec and its applications.
//!
//! These functions are the single source of truth for the `NonEnvelop` metadata
//! grammar described in the SDAVE struct-serialization specification. An
//! application that re-scans streaming payloads — for example to surface a
//! partially received record before its enclosing envelope is confirmed — MUST
//! use them rather than re-deriving the rules, so that its view cannot drift from
//! what the codec serializes and validates.

use super::{ErrorKind, Result};
use crate::error_new;

/// Whether `byte` is ASCII formatting (` `, `\t`, `\r`, `\n`), the only bytes
/// [`trim_metadata`] removes from `NonEnvelop` metadata.
#[inline]
pub const fn is_formatting(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

/// Trim leading and trailing ASCII formatting from a raw `NonEnvelop` metadata
/// region, returning the surviving `[head, tail)` byte range.
///
/// `head == tail` means the region is a formatting-only gap rather than a marker.
/// Interior formatting is preserved verbatim; this is not whitespace
/// normalization.
pub fn trim_metadata_range(metadata: &[u8]) -> (usize, usize) {
    let mut head = 0;
    let mut tail = metadata.len();
    while head < tail && is_formatting(metadata[head]) {
        head += 1;
    }
    while tail > head && is_formatting(metadata[tail - 1]) {
        tail -= 1;
    }
    (head, tail)
}

/// The trimmed `NonEnvelop` metadata region. An empty result is a formatting gap,
/// not a type or field marker.
pub fn trim_metadata(metadata: &[u8]) -> &[u8] {
    let (head, tail) = trim_metadata_range(metadata);
    &metadata[head..tail]
}

/// A named-field metadata header (`name: Type`) split into its two halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldHeader<'a> {
    name: &'a str,
    ty: &'a str,
}

impl<'a> FieldHeader<'a> {
    /// The field's Rust source identifier, before the separator.
    pub fn name(&self) -> &'a str {
        self.name
    }

    /// The type expression after the separator. Namespace separators `"::"` and
    /// nested syntax (generics, tuples, references) remain part of it.
    pub fn ty(&self) -> &'a str {
        self.ty
    }

    /// Split into `(name, ty)` without carrying the wrapper.
    pub fn into_parts(self) -> (&'a str, &'a str) {
        (self.name, self.ty)
    }
}

/// Split a trimmed named-field metadata region (`name: Type`) at the exact
/// two-byte `": "` separator — never a bare `':'`.
///
/// This keeps `"::"` namespace separators and nested type expressions inside the
/// type, and rejects the `field:Type` spelling rather than rewriting it. The
/// input is expected to be already trimmed (see [`trim_metadata`]).
pub fn parse_field_header(text: &str) -> Result<FieldHeader<'_>> {
    text.split_once(": ")
        .filter(|(name, ty)| !name.is_empty() && !ty.is_empty())
        .map(|(name, ty)| FieldHeader { name, ty })
        .ok_or_else(|| error_new!(ErrorKind::InvalidFieldHeader(text.into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_predicate_matches_the_agreed_set() {
        for byte in [b' ', b'\t', b'\r', b'\n'] {
            assert!(is_formatting(byte));
        }
        for byte in [b'a', b'0', b':', b'\0', 0xFF] {
            assert!(!is_formatting(byte));
        }
    }

    #[test]
    fn trimming_is_ascii_only_and_interior_preserving() {
        assert_eq!(trim_metadata(b""), b"");
        assert_eq!(trim_metadata(b" \t\r\n"), b"");
        assert_eq!(trim_metadata(b"x: u32"), b"x: u32");
        assert_eq!(trim_metadata(b"  x: u32\r\n"), b"x: u32");
        assert_eq!(trim_metadata(b" pair: (u32, u64) "), b"pair: (u32, u64)");
        assert_eq!(trim_metadata_range(b"  x: u32\r\n"), (2, 8));
    }

    #[test]
    fn headers_split_at_the_exact_separator() {
        let header =
            parse_field_header("inner: my_app::Vec<my_app::Map<u32, my_app::Entity<SharedString>>>")
                .unwrap();
        assert_eq!(header.name(), "inner");
        assert_eq!(
            header.ty(),
            "my_app::Vec<my_app::Map<u32, my_app::Entity<SharedString>>>"
        );

        let (name, ty) = parse_field_header("pair: (u32, u64)").unwrap().into_parts();
        assert_eq!((name, ty), ("pair", "(u32, u64)"));

        for malformed in ["", "field:Type", "x::y", ": Type", "name: ", "name:"] {
            assert!(
                matches!(
                    parse_field_header(malformed).unwrap_err().kind,
                    ErrorKind::InvalidFieldHeader(_)
                ),
                "expected {malformed:?} to be rejected"
            );
        }
    }
}
