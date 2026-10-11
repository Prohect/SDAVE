//! Markers: `NamePolicy`, `ShortType`, and the marker comparison rules.

use core::fmt;

use crate::{Error, ErrorKind, Unit};

/// A type *exemplar* that opts one constructor into short (last-identifier) marker spelling.
///
/// An entry names a constructor by naming *a type built from it*:
///
/// ```
/// use sdave_codec::ShortType;
///
/// let entry = ShortType::of::<Vec<()>>();
/// assert_eq!(entry.own_path(), "alloc::vec::Vec");
/// assert_eq!(entry.short_name(), "Vec");
/// ```
///
/// Because lookup is by the constructor's own fully-qualified path (with generic arguments
/// stripped), the one entry above covers `Vec<T>` for every `T` — but never shortens a different
/// constructor that merely ends in `Vec`, and never reaches into a *nested* spelling.
#[derive(Clone, Copy)]
pub struct ShortType {
    /// The exemplar's compiler spelling, fetched lazily so that the entry itself can live in a
    /// `const`/`static` initializer.
    name: fn() -> &'static str,
}

impl ShortType {
    /// The exemplar for the constructor of `X`.
    pub const fn of<X: ?Sized>() -> Self {
        ShortType {
            name: core::any::type_name::<X> as fn() -> &'static str,
        }
    }

    /// The exemplar's verbatim compiler spelling, e.g. `alloc::vec::Vec<()>`.
    pub fn full_name(&self) -> &'static str {
        (self.name)()
    }

    /// The exemplar's own constructor path, generic arguments stripped, e.g. `alloc::vec::Vec`.
    pub fn own_path(&self) -> &'static str {
        own_path(self.full_name())
    }

    /// The name this entry emits: the last `::`-segment of [`ShortType::own_path`], e.g. `Vec`.
    pub fn short_name(&self) -> &'static str {
        short_name(self.own_path())
    }
}

impl fmt::Debug for ShortType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.full_name(), f)
    }
}

/// The application's compile-time constant naming authority.
///
/// A marker is the compiler-reported type name ([`core::any::type_name`]) **filtered** by this
/// policy, and verbatim otherwise. `NamePolicy::VERBATIM` (no entries, the default) shortens
/// nothing. The rules are:
///
/// - each constructor's *own* fully-qualified path is looked up, never a suffix;
/// - a whitelisted constructor emits its last identifier, everything else keeps its full path;
/// - shortening does not propagate across nesting;
/// - entries cannot supply replacement names;
/// - two distinct constructors that would abbreviate to the same short name make the policy
///   invalid ([`ErrorKind::InvalidNamePolicy`]) rather than resolving by order;
/// - the policy is a `const`, immutable for the life of a serializer.
#[derive(Clone, Copy, Default)]
pub struct NamePolicy {
    entries: &'static [ShortType],
}

impl NamePolicy {
    /// Shorten nothing: every marker is the verbatim compiler spelling.
    pub const VERBATIM: NamePolicy = NamePolicy { entries: &[] };

    /// A policy whose whitelist is `entries`.
    pub const fn new(entries: &'static [ShortType]) -> Self {
        NamePolicy { entries }
    }

    /// The whitelist, in declaration order.
    pub const fn entries(&self) -> &'static [ShortType] {
        self.entries
    }

    /// Whether the policy shortens nothing.
    pub const fn is_verbatim(&self) -> bool {
        self.entries.is_empty()
    }

    /// Render a compiler spelling.
    ///
    /// A whitelisted constructor's own path becomes its last identifier; everything *after* that
    /// path — generic arguments and nested spellings — is kept verbatim, so
    /// `alloc::vec::Vec<alloc::string::String>` renders as `Vec<alloc::string::String>`, the inner
    /// constructor keeps its own full path, and no two instantiations of a whitelisted constructor
    /// collide on one marker.
    ///
    /// The result is a subslice of `full` (the last identifier of the constructor path, followed
    /// by the verbatim remainder, which is contiguous because the identifier ends where the path
    /// does), so rendering never allocates.
    pub fn render<'n>(&self, full: &'n str) -> &'n str {
        let own = own_path(full);
        for entry in self.entries {
            if own_path(entry.full_name()) == own {
                let short = short_name(own);
                return &full[own.len() - short.len()..];
            }
        }
        full
    }

    /// Reject a policy whose entries abbreviate two distinct constructors to the same name.
    ///
    /// Duplicate entries for the *same* constructor are harmless and accepted.
    pub fn validate(&self) -> Result<(), Error> {
        for (index, entry) in self.entries.iter().enumerate() {
            for other in self.entries.iter().skip(index + 1) {
                let (own, other_own) = (entry.own_path(), other.own_path());
                if own != other_own && entry.short_name() == other.short_name() {
                    return Err(Error::new(ErrorKind::InvalidNamePolicy));
                }
            }
        }
        Ok(())
    }

    /// Whether an incoming marker `incoming` is acceptable for the expected compiler spelling
    /// `expected`.
    ///
    /// Three comparisons are tried, each under the comma-padding equivalence and with no other
    /// normalisation:
    ///
    /// 1. the sender emitted exactly what this policy renders for `expected`;
    /// 2. the sender emitted the verbatim `expected` (a sender whose policy does not shorten it);
    /// 3. both spellings render to the same marker under this policy.
    pub fn accepts(&self, expected: &str, incoming: &str) -> bool {
        padded_eq(self.render(expected), incoming)
            || padded_eq(expected, incoming)
            || padded_eq(self.render(expected), self.render(incoming))
    }

    /// [`NamePolicy::accepts`] for an incoming marker still held as document units.
    ///
    /// The incoming side is compared unit-by-unit against ASCII text, so this needs no `&str`
    /// view of the input and cannot allocate. Comparisons 1 and 2 of [`NamePolicy::accepts`]
    /// apply here; comparison 3 cannot, because the incoming side is not a `&str`.
    pub fn accepts_units<T: Unit>(&self, expected: &str, incoming: &[T]) -> bool {
        marker_eq_ascii(incoming, self.render(expected)) || marker_eq_ascii(incoming, expected)
    }
}

impl fmt::Debug for NamePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.entries.iter()).finish()
    }
}

/// The constructor spelling of `full`: everything up to the first generic argument list, when the
/// spelling starts with a path; otherwise the whole spelling.
///
/// `alloc::vec::Vec<u8>` becomes `alloc::vec::Vec`; `(alloc::vec::Vec<u8>, u8)` and `[u8; 4]`
/// are returned unchanged, because their own constructor is the whole spelling.
pub fn own_path(full: &str) -> &str {
    let mut depth = 0i32;
    for (index, byte) in full.as_bytes().iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'<' if depth == 0 && is_path(&full[..index]) => return &full[..index],
            _ => {}
        }
    }
    full
}

/// The emitted short name for a constructor path: its last `::`-segment.
///
/// `alloc::vec::Vec` becomes `Vec`; a spelling with no `::` is returned unchanged.
pub fn short_name(own: &str) -> &str {
    match own.rfind("::") {
        Some(index) => &own[index + 2..],
        None => own,
    }
}

/// Whether `text` is a plain Rust path (identifiers joined by `::`), and non-empty.
fn is_path(text: &str) -> bool {
    !text.is_empty()
        && text
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b':')
}

/// Compare a unit span with ASCII text, byte-exact and with no padding equivalence.
pub fn units_eq_ascii<T: Unit>(units: &[T], text: &str) -> bool {
    units.len() == text.len()
        && units
            .iter()
            .zip(text.bytes())
            .all(|(unit, byte)| unit.to_ascii() == Some(byte))
}

/// Compare a unit span with an expected marker, under the comma-padding equivalence.
///
/// A single `,` immediately followed by `)` is padding and is ignored on either side; nothing else
/// is normalised.
pub fn marker_eq_ascii<T: Unit>(units: &[T], expected: &str) -> bool {
    UnitCodes::new(units).eq(TextCodes::new(expected))
}

/// Text comparison under the comma-padding equivalence.
fn padded_eq(left: &str, right: &str) -> bool {
    TextCodes::new(left).eq(TextCodes::new(right))
}

/// The code points of `text`, with the comma-padding equivalence applied.
struct TextCodes<'a> {
    chars: core::iter::Peekable<core::str::Chars<'a>>,
}

impl<'a> TextCodes<'a> {
    fn new(text: &'a str) -> Self {
        TextCodes {
            chars: text.chars().peekable(),
        }
    }
}

impl Iterator for TextCodes<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        let unit = self.chars.next()?;
        if unit == ',' && self.chars.peek() == Some(&')') {
            return self.chars.next().map(|close| close as u32);
        }
        Some(unit as u32)
    }
}

/// The `not_ascii` sentinel never equals a `char` code point, so a non-ASCII unit can never match
/// ASCII marker text.
const NOT_ASCII: u32 = u32::MAX;

/// The codes of `units`, with the comma-padding equivalence applied.
struct UnitCodes<'a, T: Unit> {
    units: core::iter::Peekable<core::slice::Iter<'a, T>>,
}

impl<'a, T: Unit> UnitCodes<'a, T> {
    fn new(units: &'a [T]) -> Self {
        UnitCodes {
            units: units.iter().peekable(),
        }
    }

    fn code(unit: &T) -> u32 {
        unit.to_ascii().map_or(NOT_ASCII, u32::from)
    }
}

impl<T: Unit> Iterator for UnitCodes<'_, T> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        let unit = self.units.next()?;
        let code = Self::code(unit);
        if code == u32::from(b',')
            && self.units.peek().and_then(|peek| peek.to_ascii()) == Some(b')')
        {
            let close = self.units.next()?;
            return Some(Self::code(close));
        }
        Some(code)
    }
}
