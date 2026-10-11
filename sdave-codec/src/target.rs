//! The document target: the fixed `[T]` the pass appends into.
//!
//! A target is where a level's frames are *inserted*: the level's payload is already in the region,
//! so closing a level moves that payload up by the limiter's width and fills the gap before it and
//! the tail after it. That is the whole of the one-pass framing — there is no staging area, and the
//! codec never grows a target itself.

use core::alloc::Allocator;

use crate::{Chosen, Config, DocumentTarget, Error, ErrorKind, Limit, Unit};

/// Insert a frame's limiter slice and delimiter slice around `units[base..cursor]`.
///
/// `units[..base]` is the content the frame follows, and `units[base..cursor]` is the level's
/// payload — the two sides [`crate::Config::choose`] was asked about. The payload is moved up by
/// the limiter's width, backwards so that nothing is overwritten before it is read, because the
/// limiter slice must precede the payload and the payload's position is only known once the payload
/// itself has decided the repeat.
///
/// Returns the new cursor.
///
/// # Errors
///
/// [`ErrorKind::Invariant`] when `units` has no room left for the slices: a caller driving its own
/// storage must make room for [`Chosen::frame_len`] units first.
pub fn insert_frame<T: Unit>(
    chosen: &Chosen<'_, T>,
    units: &mut [T],
    base: usize,
    cursor: usize,
) -> Result<usize, Error> {
    let repeat = chosen.repeat.get();
    let gap = repeat * chosen.limiter.len();
    let close = repeat * chosen.delimiter.len();
    if cursor.saturating_add(gap).saturating_add(close) > units.len() {
        return Err(Error::new(ErrorKind::Invariant(
            "the frame's slices do not fit in the target",
        )));
    }

    let mut index = cursor;
    while index > base {
        index -= 1;
        let unit = units[index].clone();
        units[index + gap] = unit;
    }

    let mut position = base;
    for _ in 0..repeat {
        for unit in chosen.limiter {
            units[position] = unit.clone();
            position += 1;
        }
    }

    let mut position = cursor + gap;
    for _ in 0..repeat {
        for unit in chosen.delimiter {
            units[position] = unit.clone();
            position += 1;
        }
    }

    Ok(cursor + gap + close)
}

/// A fixed `[T]` document target.
///
/// `units[..context_len]` is the caller's pre-existing content — the left-hand side every framing
/// decision is taken against — and `units[context_len..]` is the room the pass may use. Only that
/// room is ever written, so a failed pass leaves the caller's own content untouched.
///
/// The codec never grows a target. When the room runs out the pass stops with
/// [`ErrorKind::TargetExhausted`], which names how much room there was and how far the pass reached
/// — a *lower bound* on the document's length, because a single inner-to-outer pass learns the
/// length only as it produces it. A caller that wants the document anyway then retries with a
/// target it can grow through its own allocator, or with a larger slice.
pub struct SliceTarget<'a, 'p, 'l, 'v, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    units: &'a mut [T],
    start: usize,
    cursor: usize,
    depth: usize,
    profile: &'p Config<'l, 'v, T, A>,
}

impl<'a, 'p, 'l, 'v, T, A> SliceTarget<'a, 'p, 'l, 'v, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    /// Append into `units` after its first `context_len` units.
    pub fn new(units: &'a mut [T], context_len: usize, profile: &'p Config<'l, 'v, T, A>) -> Self {
        SliceTarget {
            units,
            start: context_len,
            cursor: context_len,
            depth: 0,
            profile,
        }
    }

    /// The length of the caller's pre-existing content.
    pub fn context_len(&self) -> usize {
        self.start
    }

    /// The document's end offset.
    pub fn end(&self) -> usize {
        self.cursor
    }

    /// The document's length, so far.
    pub fn len(&self) -> usize {
        self.cursor.saturating_sub(self.start)
    }

    /// Whether nothing has been appended.
    pub fn is_empty(&self) -> bool {
        self.cursor == self.start
    }

    /// The room the caller gave, in units.
    fn capacity(&self) -> usize {
        self.units.len().saturating_sub(self.start)
    }

    /// Reserve `needed` more units, or report why the target cannot offer them.
    fn room(&self, needed: usize) -> Result<(), Error> {
        let end = self.cursor.saturating_add(needed);
        let limits = self.profile.limits();
        let length = end.saturating_sub(self.start);
        if limits.max_len != 0 && length > limits.max_len {
            return Err(Error::new(ErrorKind::LimitExceeded {
                limit: Limit::Length,
                value: length,
                max: limits.max_len,
            }));
        }
        if end > self.units.len() {
            return Err(Error::new(ErrorKind::TargetExhausted {
                available: self.capacity(),
                reached: length,
            }));
        }
        Ok(())
    }

    /// Append one unit.
    fn push(&mut self, unit: T) -> Result<(), Error> {
        self.room(1)?;
        self.units[self.cursor] = unit;
        self.cursor += 1;
        Ok(())
    }
}

impl<T, A> DocumentTarget for SliceTarget<'_, '_, '_, '_, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    fn cursor(&self) -> usize {
        self.cursor
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn push_text(&mut self, text: &str) -> Result<(), Error> {
        for byte in text.bytes() {
            self.push(T::from_ascii(byte))?;
        }
        Ok(())
    }

    fn push_utf8(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let (unit, used) = T::from_utf8(rest)?;
            self.push(unit)?;
            rest = &rest[used..];
        }
        Ok(())
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        let limits = self.profile.limits();
        if limits.max_depth != 0 && self.depth > limits.max_depth {
            return Err(Error::new(ErrorKind::LimitExceeded {
                limit: Limit::Depth,
                value: self.depth,
                max: limits.max_depth,
            }));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn frame(&mut self, base: usize) -> Result<(), Error> {
        let cursor = self.cursor;
        // Copy the profile reference out first: the chosen slices belong to the profile, never to
        // the region being written.
        let profile = self.profile;
        let chosen = {
            let (head, tail) = self.units.split_at(base);
            let payload = &tail[..cursor - base];
            profile.choose(head, payload)?
        };

        let limits = profile.limits();
        let repeat = chosen.repeat.get();
        if limits.max_repeat != 0 && repeat > limits.max_repeat {
            return Err(Error::new(ErrorKind::LimitExceeded {
                limit: Limit::Repeat,
                value: repeat,
                max: limits.max_repeat,
            }));
        }

        self.room(chosen.frame_len())?;
        self.cursor = insert_frame(&chosen, self.units, base, cursor)?;
        Ok(())
    }

    fn truncate(&mut self, len: usize) -> Result<(), Error> {
        if len < self.start || len > self.cursor {
            return Err(Error::new(ErrorKind::Invariant(
                "a target can only be truncated inside what the pass appended",
            )));
        }
        self.cursor = len;
        Ok(())
    }
}
