//! Shared fixtures for the sdave-codec tests.
//!
//! Every expectation in the test files is derived from the codec README (the wire grammar, the
//! marker rules, the profile identity, the one-pass contract) plus the documented behaviour of
//! `can_serialize`; nothing here is read off observed output.

#![allow(dead_code)]
// `Error` carries its fixed-capacity path by value on purpose; see the crate root's note.
#![allow(clippy::result_large_err)]

use core::cell::OnceCell;
use core::num::NonZeroUsize;
use std::alloc::Global;

use sdave_codec::{
    Config, DocumentTarget, Error, ErrorKind, LimiterPair, Limits, NamePolicy, PairSelect,
    Serialize, Sink, Variant,
};

/// A `DocumentTarget` that records what a document would receive, as units.
///
/// Its framing is deliberately trivial — a frame becomes `<payload>` — because the tests that use it
/// pin the *grammar* (which envelopes exist, in which order, holding which text), not the pair
/// selection, which the `[T]`-backed target owns.
#[derive(Default)]
pub struct Recording {
    units: Vec<char>,
    depth: usize,
    max_depth: usize,
}

impl Recording {
    pub fn new() -> Self {
        Recording::default()
    }

    /// Everything appended so far, as text.
    pub fn text(&self) -> String {
        self.units.iter().collect()
    }

    /// The deepest nesting depth reached.
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }
}

impl DocumentTarget for Recording {
    fn cursor(&self) -> usize {
        self.units.len()
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn push_text(&mut self, text: &str) -> Result<(), Error> {
        self.units.extend(text.chars());
        Ok(())
    }

    fn push_utf8(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let text = core::str::from_utf8(bytes).map_err(|_| Error::new(ErrorKind::InvalidUtf8))?;
        self.units.extend(text.chars());
        Ok(())
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        self.max_depth = self.max_depth.max(self.depth);
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn frame(&mut self, base: usize) -> Result<(), Error> {
        let payload: Vec<char> = self.units.split_off(base);
        self.units.push('<');
        self.units.extend(payload);
        self.units.push('>');
        Ok(())
    }

    fn truncate(&mut self, len: usize) -> Result<(), Error> {
        if len > self.units.len() {
            return Err(Error::new(ErrorKind::Invariant("beyond the recording")));
        }
        self.units.truncate(len);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// profiles
// ---------------------------------------------------------------------------

pub type Pair = LimiterPair<'static, char, Global>;
pub type Profile = Config<'static, 'static, char, Global>;

/// The README's pair set:
/// `[('^', '~', 2), ('?', '!', 2), ('%', '%', 2), ('{', '}', 2), ('♦', '♣', 1)]`.
pub const STD: &[(&str, &str, usize)] = &[
    ("^", "~", 2),
    ("?", "!", 2),
    ("%", "%", 2),
    ("{", "}", 2),
    ("♦", "♣", 1),
];

pub fn unit(text: &str) -> &'static Vec<char> {
    Box::leak(Box::new(text.chars().collect::<Vec<char>>()))
}

pub fn pair(least_repeat: usize, limiter: &str, delimiter: &str) -> Pair {
    LimiterPair::new(
        Some(NonZeroUsize::new(least_repeat).unwrap()),
        OnceCell::from(unit(limiter)),
        OnceCell::from(unit(delimiter)),
    )
}

pub fn pairs(specs: &[(&str, &str, usize)]) -> &'static Vec<Pair> {
    Box::leak(Box::new(
        specs
            .iter()
            .map(|(limiter, delimiter, repeat)| pair(*repeat, limiter, delimiter))
            .collect::<Vec<Pair>>(),
    ))
}

pub fn profile_of(variant: Variant, pairs: &'static Vec<Pair>, limits: Limits) -> Profile {
    Config::new(
        variant,
        pairs,
        limits,
        PairSelect::FirstMatch,
        NamePolicy::VERBATIM,
    )
    .expect("a valid profile")
}

/// The fixture profile: V1, the README's pair set, no bounds, first match, verbatim markers.
pub fn demo_profile() -> Profile {
    profile_of(Variant::V1, pairs(STD), Limits::UNBOUNDED)
}

// The same fixtures for a byte document.

pub type Pair8 = LimiterPair<'static, u8, Global>;
pub type Profile8 = Config<'static, 'static, u8, Global>;

pub fn bytes_of(text: &str) -> &'static Vec<u8> {
    Box::leak(Box::new(text.as_bytes().to_vec()))
}

pub fn pair8(least_repeat: usize, limiter: &str, delimiter: &str) -> Pair8 {
    LimiterPair::new(
        Some(NonZeroUsize::new(least_repeat).unwrap()),
        OnceCell::from(bytes_of(limiter)),
        OnceCell::from(bytes_of(delimiter)),
    )
}

pub fn pairs8(specs: &[(&str, &str, usize)]) -> &'static Vec<Pair8> {
    Box::leak(Box::new(
        specs
            .iter()
            .map(|(limiter, delimiter, repeat)| pair8(*repeat, limiter, delimiter))
            .collect::<Vec<Pair8>>(),
    ))
}

pub fn profile8(variant: Variant, pairs: &'static Vec<Pair8>, limits: Limits) -> Profile8 {
    Config::new(
        variant,
        pairs,
        limits,
        PairSelect::FirstMatch,
        NamePolicy::VERBATIM,
    )
    .expect("a valid profile")
}

// ---------------------------------------------------------------------------
// one value's body, through the one append entry
// ---------------------------------------------------------------------------

/// Append one value's *body* into the recording target.
pub fn body_of<V: Serialize + ?Sized>(value: &V) -> Result<String, Error> {
    let limits = Limits::UNBOUNDED;
    let mut recording = Recording::new();
    {
        let mut sink = Sink::new(&mut recording, &NamePolicy::VERBATIM, &limits);
        value.append(&mut sink)?;
    }
    Ok(recording.text())
}

/// The body of a value, asserted to append cleanly.
pub fn body<V: Serialize + ?Sized>(value: &V) -> String {
    body_of(value).expect("appended")
}

/// The error one value's body produces.
pub fn body_error<V: Serialize + ?Sized>(value: &V) -> Error {
    body_of(value).expect_err("must fail")
}

/// Append a whole document — marker and all — into a fresh recording target.
pub fn document_of<V: Serialize + ?Sized>(profile: &Profile, value: &V) -> Result<String, Error> {
    let mut recording = Recording::new();
    profile.append(value, &mut recording)?;
    Ok(recording.text())
}

// ---------------------------------------------------------------------------
// text and slices
// ---------------------------------------------------------------------------

pub fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

pub fn text_of(units: &[char]) -> String {
    units.iter().collect()
}

/// A blank region of `len` filler units, to be written into.
///
/// The filler is a unit no test's document contains, so an untouched unit is recognisable.
pub fn blank(len: usize) -> Vec<char> {
    vec!['\u{0}'; len]
}

/// The filler text a `blank(len)` holds.
pub fn filler(len: usize) -> String {
    "\u{0}".repeat(len)
}
