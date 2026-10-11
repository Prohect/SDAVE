//! The per-type wire shape.

/// The wire shape of a declared type.
///
/// A derive sets this as an associated constant, so the shape — and therefore the framing — is
/// known *before* any value is inspected. That is what makes a unit variant and an empty list
/// decodable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Shape {
    /// A named struct or a unit struct: one envelope of `Name: Type` field runs. A unit struct is
    /// the empty record.
    Record,
    /// A tuple struct, a newtype or a zero-field tuple struct: one envelope holding positional
    /// item envelopes, one per field. Arity is capped at 12.
    Tuple,
    /// An enum or `Option`/`Result`: one variant envelope, plus (when the variant has fields) one
    /// associated-value envelope holding the variant's body. `V`, `V()` and `V {}` keep distinct
    /// framing.
    Enum,
    /// The shape is not statically known: the value is a `dyn` payload, whose bytes carry the
    /// concrete type's marked document and therefore its own shape.
    ///
    /// This variant is this crate's addition to the three shapes a derive can set; a derive never
    /// emits it.
    Dynamic,
}

impl Shape {
    /// Whether the shape is one of the three a derive can set.
    pub const fn is_declared(&self) -> bool {
        !matches!(self, Shape::Dynamic)
    }

    /// Whether a value of this shape is framed as one envelope of sub-items.
    pub const fn is_aggregate(&self) -> bool {
        matches!(self, Shape::Record | Shape::Tuple)
    }

    /// The shape's name, for diagnostics.
    pub const fn name(&self) -> &'static str {
        match self {
            Shape::Record => "Record",
            Shape::Tuple => "Tuple",
            Shape::Enum => "Enum",
            Shape::Dynamic => "Dynamic",
        }
    }
}
