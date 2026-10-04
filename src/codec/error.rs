use std::fmt;

/// The result type used by the native Rust codec.
pub type Result<T> = std::result::Result<T, Error>;

/// A typed-codec failure. Framing errors never request a field default.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidProfile(String),
    InvalidNamePolicy(String),
    InvalidTypeExpression(String),
    MissingMarker,
    TypeMismatch {
        expected: String,
        found: String,
    },
    UnexpectedMetadata(String),
    InvalidFieldHeader(String),
    MissingField(String),
    DuplicateField(String),
    UnknownField(String),
    UnknownVariant(String),
    EnvelopeCount {
        expected: usize,
        actual: usize,
    },
    MissingListHeader,
    NonemptyListHeader,
    TruncatedEnvelope,
    InvalidPayload(String),
    NoDelimiter,
    LimitExceeded {
        resource: &'static str,
        limit: usize,
    },
    Io(String),
}

/// A location within a statically typed value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSegment {
    Field(String),
    Variant(String),
    Index(usize),
}

/// An error with an absolute input byte offset and an outer-to-inner value path.
///
/// Encoding failures generally have no byte offset. Application basic helpers
/// can use [`error_payload!()`]; their enclosing decode operation supplies a location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: Option<usize>,
    pub path: Vec<PathSegment>,
    /// The `file:line` in the Rust source where this error was constructed.
    pub rust_source: String,
}

#[macro_export]
macro_rules! error_new {
    ($kind:expr $(,)?) => {
        $crate::Error::with_source($kind, file!(), line!())
    };
}

#[macro_export]
macro_rules! error_payload {
    ($message:expr $(,)?) => {
        $crate::Error::with_source(
            $crate::ErrorKind::InvalidPayload($message.into()),
            file!(),
            line!(),
        )
    };
}

impl Error {
    pub fn with_source(kind: ErrorKind, file: &'static str, line: u32) -> Self {
        Self {
            kind,
            offset: None,
            path: Vec::new(),
            rust_source: format!("{file}:{line}"),
        }
    }

    /// Set a location if a more specific child location has not already been set.
    pub fn at(mut self, offset: usize) -> Self {
        if self.offset.is_none() {
            self.offset = Some(offset);
        }
        self
    }

    pub fn in_field(mut self, field: impl Into<String>) -> Self {
        self.path.insert(0, PathSegment::Field(field.into()));
        self
    }

    pub fn in_variant(mut self, variant: impl Into<String>) -> Self {
        self.path.insert(0, PathSegment::Variant(variant.into()));
        self
    }

    pub fn at_index(mut self, index: usize) -> Self {
        self.path.insert(0, PathSegment::Index(index));
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SDAVE: ")?;
        match &self.kind {
            ErrorKind::InvalidProfile(message) => write!(f, "invalid framing profile: {message}")?,
            ErrorKind::InvalidNamePolicy(message) => write!(f, "invalid naming policy: {message}")?,
            ErrorKind::InvalidTypeExpression(message) => {
                write!(f, "invalid type expression: {message}")?
            }
            ErrorKind::MissingMarker => f.write_str("missing type marker")?,
            ErrorKind::TypeMismatch { expected, found } => {
                write!(f, "expected type {expected}, found {found}")?
            }
            ErrorKind::UnexpectedMetadata(message) => write!(f, "unexpected metadata {message:?}")?,
            ErrorKind::InvalidFieldHeader(message) => write!(
                f,
                "invalid field header {message:?} (expected `name: Type`)"
            )?,
            ErrorKind::MissingField(field) => write!(f, "missing field {field}")?,
            ErrorKind::DuplicateField(field) => write!(f, "duplicate field {field}")?,
            ErrorKind::UnknownField(field) => write!(f, "unknown field {field}")?,
            ErrorKind::UnknownVariant(variant) => write!(f, "unknown variant {variant:?}")?,
            ErrorKind::EnvelopeCount { expected, actual } => {
                write!(f, "expected {expected} envelopes, found {actual}")?
            }
            ErrorKind::MissingListHeader => {
                f.write_str("missing real empty list-header envelope")?
            }
            ErrorKind::NonemptyListHeader => f.write_str("list-header envelope is not empty")?,
            ErrorKind::TruncatedEnvelope => {
                f.write_str("unfinished limiter or unconfirmed/truncated envelope")?
            }
            ErrorKind::InvalidPayload(message) => write!(f, "invalid payload: {message}")?,
            ErrorKind::NoDelimiter => {
                f.write_str("no configured pair can frame this payload verbatim")?
            }
            ErrorKind::LimitExceeded { resource, limit } => {
                write!(f, "{resource} limit exceeded ({limit})")?
            }
            ErrorKind::Io(message) => write!(f, "output failed: {message}")?,
        }
        if let Some(offset) = self.offset {
            write!(f, " at byte {offset}")?;
        }
        for segment in &self.path {
            match segment {
                PathSegment::Field(field) => write!(f, ".{field}")?,
                PathSegment::Variant(variant) => write!(f, "::{variant}")?,
                PathSegment::Index(index) => write!(f, "[{index}]")?,
            }
        }
        if !self.rust_source.is_empty() {
            write!(f, " [at: {}]", self.rust_source)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self {
            kind: (ErrorKind::Io(error.to_string())),
            offset: None,
            path: Vec::new(),
            rust_source: "".to_string(),
        }
    }
}
