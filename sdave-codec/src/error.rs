//! The codec's typed error vocabulary.

use core::fmt;

/// How many context steps an [`Error`] carries.
///
/// Deeper paths keep their outermost [`PATH_CAPACITY`] steps and set [`Path::is_truncated`].
pub const PATH_CAPACITY: usize = 8;

/// One step of an error's outer→inner context path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The named field of a record.
    Field(&'static str),
    /// The named variant of an enum.
    Variant(&'static str),
    /// The positional item of a tuple, list or variant body.
    Index(usize),
}

/// An error's context path, outer→inner, with a fixed non-allocating capacity.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Path {
    steps: [Step; PATH_CAPACITY],
    len: usize,
    truncated: bool,
}

impl Path {
    /// The empty path.
    pub const EMPTY: Path = Path {
        steps: [Step::Index(0); PATH_CAPACITY],
        len: 0,
        truncated: false,
    };

    /// How many steps are recorded.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether no step is recorded.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether steps were dropped because the capacity was reached.
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The recorded steps, outer→inner.
    pub fn steps(&self) -> impl Iterator<Item = Step> + '_ {
        self.steps[..self.len].iter().copied()
    }

    /// A path with `step` as its new outermost step.
    ///
    /// When the capacity is reached the *innermost* step is dropped and
    /// [`Path::is_truncated`] becomes `true`.
    pub fn pushed(mut self, step: Step) -> Self {
        if self.len == PATH_CAPACITY {
            self.truncated = true;
        } else {
            self.len += 1;
        }
        let mut index = self.len;
        while index > 1 {
            self.steps[index - 1] = self.steps[index - 2];
            index -= 1;
        }
        self.steps[0] = step;
        self
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, step) in self.steps().enumerate() {
            match step {
                Step::Field(name) | Step::Variant(name) => {
                    if position != 0 {
                        f.write_str(".")?;
                    }
                    f.write_str(name)?;
                }
                Step::Index(index) => write!(f, "[{index}]")?,
            }
        }
        if self.truncated {
            f.write_str("...")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Which [`Limits`] bound was exceeded.
///
/// [`Limits`]: crate::Limits
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Limit {
    /// The nesting depth of frames.
    Depth,
    /// The total document length in units.
    Length,
    /// The delimiter/limiter repeat of one frame.
    Repeat,
}

impl Limit {
    /// The bound's name.
    pub const fn name(&self) -> &'static str {
        match self {
            Limit::Depth => "nesting depth",
            Limit::Length => "document length",
            Limit::Repeat => "frame repeat",
        }
    }
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What went wrong, typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    /// Two distinct constructors in the naming policy abbreviate to the same short name.
    InvalidNamePolicy,
    /// A value's text is not representable in the document's unit type.
    NotRepresentable,
    /// A UTF-8 fragment is not valid UTF-8.
    InvalidUtf8,
    /// Input ended in the middle of a value.
    UnexpectedEnd,
    /// No limiter pair of the profile can frame this payload in this context.
    Unframable,
    /// A fixed-size document target ran out of room.
    ///
    /// A single inner-to-outer pass learns a document's length only as it produces it, so the
    /// document's true length is *not* known when this is reported: `reached` is how far the pass
    /// got, which is a lower bound on the final length. A target that may grow never reports this.
    TargetExhausted {
        /// How many units the target offered, from the document's start.
        available: usize,
        /// How many units the pass had produced, or was about to produce, when it ran out.
        reached: usize,
    },
    /// A document target that may grow could not: its allocator refused the extension.
    Allocation,
    /// A [`Limit`] bound was exceeded.
    LimitExceeded {
        /// The bound that was exceeded.
        limit: Limit,
        /// The value that was reached.
        value: usize,
        /// The bound.
        max: usize,
    },
    /// Floats are serializable only when finite.
    NonFiniteFloat,
    /// A frame is truncated or still pending.
    Truncated,
    /// A marker does not match the expected compiler spelling.
    UnexpectedMarker {
        /// The expected spelling.
        expected: &'static str,
    },
    /// A record field name is not declared by the type.
    UnknownField,
    /// A record field name appeared twice.
    DuplicateField,
    /// A required record field is absent.
    MissingField {
        /// The field's declared name.
        name: &'static str,
    },
    /// A variant name is not declared by the enum.
    UnknownVariant,
    /// The number of envelopes does not match the declared arity.
    WrongArity {
        /// The declared arity.
        expected: usize,
        /// The arity found.
        actual: usize,
    },
    /// A list's mandatory header envelope is absent.
    MissingListHeader,
    /// A list's header envelope is not empty.
    NonEmptyListHeader,
    /// A canonical decimal integer is malformed.
    InvalidNumber,
    /// A canonical decimal integer does not fit the target type.
    NumberOutOfRange,
    /// A character payload is not exactly one scalar.
    InvalidCharacter,
    /// A boolean payload is neither `true` nor `false`.
    InvalidBoolean,
    /// The document unit cannot be borrowed as the requested text or bytes.
    NotBorrowable,
    /// An internal invariant was violated: a bug, or an entry used outside its contract.
    Invariant(&'static str),
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::InvalidNamePolicy => {
                f.write_str("two whitelisted constructors abbreviate to the same short name")
            }
            ErrorKind::NotRepresentable => {
                f.write_str("the value's text is not representable in the document unit")
            }
            ErrorKind::InvalidUtf8 => f.write_str("invalid UTF-8"),
            ErrorKind::UnexpectedEnd => f.write_str("unexpected end of input"),
            ErrorKind::Unframable => {
                f.write_str("no limiter pair of the profile can frame this payload")
            }
            ErrorKind::TargetExhausted { available, reached } => write!(
                f,
                "the document target ran out of room: reached {reached} of {available} units"
            ),
            ErrorKind::Allocation => f.write_str("the document target's allocator refused to grow"),
            ErrorKind::LimitExceeded { limit, value, max } => {
                write!(f, "{limit} exceeded: {value} > {max}")
            }
            ErrorKind::NonFiniteFloat => f.write_str("only finite floats are serializable"),
            ErrorKind::Truncated => f.write_str("a frame is truncated or still pending"),
            ErrorKind::UnexpectedMarker { expected } => {
                write!(f, "unexpected marker: expected `{expected}`")
            }
            ErrorKind::UnknownField => f.write_str("unknown record field"),
            ErrorKind::DuplicateField => f.write_str("duplicate record field"),
            ErrorKind::MissingField { name } => write!(f, "missing record field `{name}`"),
            ErrorKind::UnknownVariant => f.write_str("unknown variant"),
            ErrorKind::WrongArity { expected, actual } => {
                write!(f, "wrong item count: expected {expected}, got {actual}")
            }
            ErrorKind::MissingListHeader => f.write_str("missing list header envelope"),
            ErrorKind::NonEmptyListHeader => f.write_str("non-empty list header envelope"),
            ErrorKind::InvalidNumber => f.write_str("invalid canonical decimal integer"),
            ErrorKind::NumberOutOfRange => {
                f.write_str("decimal integer out of range for the target type")
            }
            ErrorKind::InvalidCharacter => {
                f.write_str("character payload is not exactly one scalar")
            }
            ErrorKind::InvalidBoolean => f.write_str("boolean payload is not `true` or `false`"),
            ErrorKind::NotBorrowable => {
                f.write_str("the document unit cannot be borrowed as the requested text or bytes")
            }
            ErrorKind::Invariant(what) => write!(f, "internal invariant violated: {what}"),
        }
    }
}

/// A coded error: a typed [`ErrorKind`], an absolute input offset where one exists, and the
/// outer→inner context path.
///
/// With the `trace` feature the host source location (`file!()`/`line!()`) of the error's
/// construction is recorded. A trace is never part of the observable contract: it is absent from
/// [`Display`] and from [`PartialEq`]/[`Eq`] in every build.
#[derive(Clone, Debug)]
pub struct Error {
    kind: ErrorKind,
    offset: Option<usize>,
    path: Path,
    #[cfg(feature = "trace")]
    file: &'static str,
    #[cfg(feature = "trace")]
    line: u32,
}

impl Error {
    /// An error without an input offset.
    #[cfg_attr(feature = "trace", track_caller)]
    pub fn new(kind: ErrorKind) -> Self {
        Self::build(kind, None)
    }

    /// An error at `offset` of the input.
    #[cfg_attr(feature = "trace", track_caller)]
    pub fn at(kind: ErrorKind, offset: usize) -> Self {
        Self::build(kind, Some(offset))
    }

    #[cfg(feature = "trace")]
    #[track_caller]
    fn build(kind: ErrorKind, offset: Option<usize>) -> Self {
        let location = core::panic::Location::caller();
        Error {
            kind,
            offset,
            path: Path::EMPTY,
            file: location.file(),
            line: location.line(),
        }
    }

    #[cfg(not(feature = "trace"))]
    fn build(kind: ErrorKind, offset: Option<usize>) -> Self {
        Error {
            kind,
            offset,
            path: Path::EMPTY,
        }
    }

    /// What went wrong.
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// The absolute input offset, where the error has one.
    pub fn offset(&self) -> Option<usize> {
        self.offset
    }

    /// The outer→inner context path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record `name` as this error's new outermost context step.
    pub fn in_field(self, name: &'static str) -> Self {
        self.push(Step::Field(name))
    }

    /// Record the variant `name` as this error's new outermost context step.
    pub fn in_variant(self, name: &'static str) -> Self {
        self.push(Step::Variant(name))
    }

    /// Record `index` as this error's new outermost context step.
    pub fn at_index(self, index: usize) -> Self {
        self.push(Step::Index(index))
    }

    /// Fill in an input offset, if the error does not have one yet.
    pub fn with_offset(mut self, offset: usize) -> Self {
        if self.offset.is_none() {
            self.offset = Some(offset);
        }
        self
    }

    /// The host source location of this error's construction, with the `trace` feature.
    #[cfg(feature = "trace")]
    pub fn trace(&self) -> (&'static str, u32) {
        (self.file, self.line)
    }

    fn push(mut self, step: Step) -> Self {
        self.path = self.path.pushed(step);
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.kind, f)?;
        match (self.offset, self.path.is_empty()) {
            (Some(offset), false) => write!(f, " [at offset {offset}, path `{}`]", self.path),
            (Some(offset), true) => write!(f, " [at offset {offset}]"),
            (None, false) => write!(f, " [path `{}`]", self.path),
            (None, true) => Ok(()),
        }?;
        #[cfg(feature = "trace")]
        write!(f, " [trace {}:{}]", self.file, self.line)?;
        Ok(())
    }
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.offset == other.offset && self.path == other.path
    }
}

impl Eq for Error {}

impl core::error::Error for Error {}
