//! Application-configurable type naming for the codec's `Signature`s.
//!
//! Every field signature (named `name: type` / positional `(type)`) and every
//! concrete-type signature (a value's own type, a `dyn Trait` concrete type) is
//! built from a [`TypeName`]. A [`NamePolicy`] decides which type names may be
//! shortened to their last `::` segment.
//!
//! Names are composed **per node**: a type constructor and each of its arguments
//! are resolved independently, so shortening never propagates across a level.
//! `SmallVec<Option<T>>` renders the `SmallVec` node, the `Option` node and the
//! `T` node each through the policy on its own; whether one of them is shortened
//! has no effect on the others.
//!
//! The policy is a *method* parameter of the codec's (de)serializers, so several
//! policies (e.g. wire versions) can coexist in one binary. The codec itself never
//! shortens anything implicitly: the default [`NoShortening`] policy leaves every
//! name at full length.

use core::alloc::Allocator;

use alloc::vec::Vec;

/// A type the codec can render into a `Signature`.
///
/// [`TypeName::FULL`] is the type's full qualified name *ignoring* its generic
/// arguments; [`TypeName::render`] composes them back in, resolving every node
/// through the [`NamePolicy`] independently.
pub trait TypeName {
    /// The type's full qualified name, ignoring generic arguments.
    const FULL: &'static str;

    /// Write this type's wire name through `N` into `f`.
    fn render<N: NamePolicy>(f: &mut dyn core::fmt::Write) -> core::fmt::Result {
        f.write_str(<N as NamePolicy>::shorten(Self::FULL))
    }
}

/// A type that a [`NamePolicy`] may shorten to its last `::` segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShortType {
    full: &'static str,
}

impl ShortType {
    /// Declare `full` as a shortenable type.
    pub const fn new(full: &'static str) -> Self {
        Self { full }
    }

    /// The type's full qualified name.
    pub const fn full(&self) -> &'static str {
        self.full
    }

    /// The short name: the last `::` segment of [`ShortType::full`], which MUST
    /// equal the type's own Rust ident.
    pub fn short(&self) -> &'static str {
        let full: &'static str = self.full;
        match full.rsplit_once("::") {
            Some((_, ident)) => ident,
            None => full,
        }
    }
}

/// An application-defined naming policy.
///
/// Peer serializers/deserializers MUST use the same policy (and the same
/// [`TypeName`] names) to interoperate. Short names MUST be unique inside
/// [`NamePolicy::SHORT_TYPES`]; call [`assert_valid`] to check a policy.
pub trait NamePolicy {
    /// The types whose names this policy shortens.
    const SHORT_TYPES: &'static [ShortType];

    /// `full`, shortened when it is listed in [`NamePolicy::SHORT_TYPES`].
    fn shorten(full: &'static str) -> &'static str {
        let mut i = 0;
        while i < Self::SHORT_TYPES.len() {
            let short_type = &Self::SHORT_TYPES[i];
            if short_type.full() == full {
                return short_type.short();
            }
            i += 1;
        }
        full
    }
}

/// The default [`NamePolicy`]: no type is shortened.
pub struct NoShortening;

impl NamePolicy for NoShortening {
    const SHORT_TYPES: &'static [ShortType] = &[];
}

/// A `core::fmt::Write` sink appending ASCII bytes to a `u8` buffer.
pub struct BufferWriter<'a, A: Allocator>(pub &'a mut Vec<u8, A>);

impl<A: Allocator> core::fmt::Write for BufferWriter<'_, A> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// Render `T`'s wire name under `N` into a fresh `Vec` owned by `allocator`.
///
/// Used to build the `type_describer` of a `Signature` when the name is composed
/// (i.e. for any type with generic arguments).
pub fn render_type<N: NamePolicy, T: TypeName + ?Sized, A: Allocator + Clone>(
    allocator: A,
) -> Vec<u8, A> {
    let mut out: Vec<u8, A> = Vec::new_in(allocator);
    // Infallible: writing into a `Vec` never errors.
    let _ = T::render::<N>(&mut BufferWriter(&mut out));
    out
}

/// Whether `expected` is exactly `T`'s wire name under `N`.
pub fn render_matches<N: NamePolicy, T: TypeName + ?Sized>(expected: &[u8]) -> bool {
    /// `core::fmt::Write` sink that checks what is written against `expected`.
    struct SignatureChecker<'a> {
        expected: &'a [u8],
        pos: usize,
    }

    impl core::fmt::Write for SignatureChecker<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let bytes = s.as_bytes();
            match self.pos.checked_add(bytes.len()) {
                Some(end)
                    if end <= self.expected.len() && &self.expected[self.pos..end] == bytes =>
                {
                    self.pos = end;
                }
                // Poison `pos` so the final length check fails.
                _ => self.pos = usize::MAX,
            }
            Ok(())
        }
    }

    let mut checker = SignatureChecker { expected, pos: 0 };
    if T::render::<N>(&mut checker).is_err() {
        return false;
    }
    checker.pos == expected.len()
}

/// Whether a named field's `Signature` NonEnvelop is exactly
/// `name ++ TYPE_SEPARATOR ++ render::<T>()`.
pub fn named_signature_matches<N: NamePolicy, T: TypeName + ?Sized>(
    signature: &[u8],
    name: &[u8],
) -> bool {
    let Some(rest) = signature.strip_prefix(name) else {
        return false;
    };
    // MUST be the same separator `write_signature` writes, or a derived signature and
    // the one it is matched against could never agree.
    let Some(rest) = rest.strip_prefix(crate::TYPE_SEPARATOR) else {
        return false;
    };
    render_matches::<N, T>(rest)
}

/// Panics if two entries of `short_types` share a full or short name.
///
/// Call it from a test or at application start-up to assert the application's
/// policy is well formed (the codec cannot enforce it at compile time).
pub fn assert_valid(short_types: &[ShortType]) {
    for (i, a) in short_types.iter().enumerate() {
        for b in &short_types[i + 1..] {
            assert!(
                a.full() != b.full(),
                "duplicate ShortType full name {:?}",
                a.full()
            );
            assert!(
                a.short() != b.short(),
                "duplicate ShortType short name {:?}",
                a.short()
            );
        }
    }
}
