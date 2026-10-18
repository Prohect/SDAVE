//! Heap-free formatting for primitive payloads.
//!
//! A readable primitive's payload is its `Display` output, so it is formatted through a
//! fixed-size stack buffer here and never touches the heap.
//!
//! Nothing here is behind the `primitive` feature: a hand-written impl may want the same
//! buffer for its own textual payload, and the helper costs nothing until something
//! calls it.

use core::fmt::Write;

use crate::Error;

/// A [`core::fmt::Write`] sink over a fixed-size stack buffer, so formatting
/// never has to touch the heap.
///
/// A write that would exceed `N` bytes fails with [`core::fmt::Error`] and leaves
/// the bytes accepted so far readable through [`StackBuf::as_bytes`].
pub struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    /// A new, empty buffer.
    pub const fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    /// The bytes written so far.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// The number of bytes written so far.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Default for StackBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let end = self.len + bytes.len();
        if end > N {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

/// Format `value` into a stack buffer sized for up to `N` bytes.
///
/// A too-small buffer is reported as [`Error::InvalidPayload`]; it can only
/// happen if the reserved bound for the type is wrong.
pub fn format_display<D, const N: usize>(value: &D) -> Result<StackBuf<N>, Error>
where
    D: core::fmt::Display + ?Sized,
{
    let mut formatted = StackBuf::<N>::new();
    write!(formatted, "{}", value).map_err(|_| Error::InvalidPayload)?;
    Ok(formatted)
}
