//! Field metadata shared by the serialize and deserialize codegen.
//!
//! A [`FieldInfo`] captures everything codegen needs about one struct field or
//! enum-variant field: how to name it in a `Signature`, how to bind it in a
//! pattern, how to refer to it from a value expression, and whether — and how — it
//! reaches the wire.
//!
//! # Field attributes
//!
//! `#[default(<expr>)]` and `#[skip(<expr>)]` are parsed here, but what they *mean* is
//! documented once in `sdave-derive`'s `README.md`: that is the crate an application
//! depends on, so the user-facing statement lives there and this module implements it.
//!
//! What this module owns is the codegen consequence of each attribute — [`FieldInfo::encoded`]
//! (`#[skip]` takes the field off the wire entirely), [`FieldInfo::fallback`] (which
//! expression builds a field that was not read) — and [`FieldInfo::is_skipped`], the coarse
//! form the bound-only paths use, where the attribute's expression is not needed.

use alloc::{format, string::ToString, vec::Vec};

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{Fields, Ident, Index, LitStr};

/// A single field of a struct or enum variant, with everything codegen needs.
pub struct FieldInfo {
    /// `Some(name)` for a named field, `None` for a tuple field.
    pub ident: Option<Ident>,
    /// The positional index of the field.
    pub index: Index,
    /// A unique binding used in patterns and value expressions.
    pub binding: Ident,
    /// The declared field type.
    pub ty: syn::Type,
    /// The bytes written before the field's rendered type in its `Signature`
    /// (`"{name}: "` for a named field, `""` for a positional one).
    ///
    /// The `": "` of a named field MUST stay equal to `sdave_codec::TYPE_SEPARATOR`,
    /// because the generated deserializer matches a peer's signature with
    /// `sdave_codec::named_signature_matches`, which splits on exactly that separator.
    /// A positional field writes no `Signature` at all: its position is its identity,
    /// and the type text that names a positional composite as a whole (`P(f32, f32)`)
    /// is built by `TypeName`, not here. Nothing is written *after* the rendered type:
    /// the type describer closes the signature.
    pub signature_prefix: LitStr,
    /// The field `name` as it appears in a named `Signature` (`""` for a tuple
    /// field).
    pub name_bytes: LitStr,
    /// `Some(expr)` when the field is optional on the wire (`#[default(expr)]`): a
    /// document that omits it deserializes with `expr` as the value.
    ///
    /// Only a **named** field can carry it: a positional field is matched by position, so a
    /// document that ends early cannot say which position is missing. A positional `#[default]`
    /// is rejected by [`FieldInfo::collect`].
    pub default: Option<syn::Expr>,
    /// `Some(expr)` when the field is kept out of the codec (`#[skip(expr)]`): it is
    /// neither written nor read, and `expr` always builds it. Its type therefore
    /// needs no codec impl — unless a `#[default(expr)]` is given too, which then
    /// wins as the value (see [`FieldInfo::fallback`]).
    pub skip: Option<syn::Expr>,
}

impl FieldInfo {
    /// Collect the fields of `fields`.
    ///
    /// # Errors
    /// Returns the `#[default]`/`#[skip]` attribute errors of any field.
    pub fn collect(fields: &Fields) -> syn::Result<Vec<Self>> {
        fields
            .iter()
            .enumerate()
            .map(|(position, field)| {
                let ty = field.ty.clone();
                let (ident, name_bytes, signature_prefix) = match &field.ident {
                    Some(name) => (
                        Some(name.clone()),
                        LitStr::new(&name.to_string(), name.span()),
                        LitStr::new(&format!("{name}: "), name.span()),
                    ),
                    None => (
                        None,
                        LitStr::new("", Span::call_site()),
                        LitStr::new("", Span::call_site()),
                    ),
                };
                let (default, skip) = optional_attributes(field)?;
                Ok(Self {
                    ident,
                    index: Index::from(position),
                    binding: format_ident!("__sdave_field_{position}"),
                    ty,
                    signature_prefix,
                    name_bytes,
                    default,
                    skip,
                })
            })
            .collect()
    }

    /// Whether the field reaches the wire at all, i.e. is not `#[skip(..)]`.
    ///
    /// A skipped field takes no part in either derive: no signature, no value
    /// Envelop, no signature matching (see [`crate::parse_fields`]).
    pub fn encoded(&self) -> bool {
        self.skip.is_none()
    }

    /// The expression that builds a skipped field: the `#[default]` one first (it is
    /// the explicit value), then the `#[skip]` one, which carries it only when no
    /// `#[default]` is given.
    ///
    /// A positional field can only ever have the `#[skip]` half (see
    /// [`FieldInfo::default`]).
    pub fn fallback(&self) -> Option<&syn::Expr> {
        self.default.as_ref().or(self.skip.as_ref())
    }

    /// Whether `field` carries `#[skip(..)]`, without parsing its expression.
    ///
    /// Used where only the bounds are being derived, e.g. [`crate::wire_field_types`].
    pub fn is_skipped(field: &syn::Field) -> bool {
        field.attrs.iter().any(|attr| attr.path().is_ident("skip"))
    }

    /// The value expression used by a struct's `serialize`, i.e. `&self.<field>`.
    pub fn struct_access(&self) -> TokenStream2 {
        match &self.ident {
            Some(name) => quote!(&self.#name),
            None => {
                let index = &self.index;
                quote!(&self.#index)
            }
        }
    }

    /// The value expression used inside an enum `match`: always the field's
    /// [`FieldInfo::binding`], whether the field is named or positional.
    ///
    /// Every arm pattern binds through the generated `__sdave_field_*` name (see
    /// [`crate::enum_serialize_arms`]) so that a `#[skip(..)]` field — whose binding
    /// an arm never reads — stays underscore-prefixed and cannot trigger
    /// `unused_variables` in the annotated crate.
    pub fn enum_value(&self) -> TokenStream2 {
        let binding = &self.binding;
        quote!(#binding)
    }
}

/// Read the `#[default(expr)]` / `#[skip(expr)]` pair off one field.
///
/// Both may be present on a *named* field: `#[skip]` takes the field out of the codec and
/// `#[default]` supplies the value built in its place, so the two describe different aspects of
/// the field rather than alternatives.
///
/// `#[default]` is rejected on a *positional* field. Positional fields are matched by position,
/// so a document that ends early cannot say *which* position is missing: with two optional
/// positions the reader could not tell them apart, and the value would be ambiguous. Removing a
/// position is well defined, which is why `#[skip]` stays legal, any number of times.
///
/// # Errors
/// Returns an error for a missing expression, a duplicated attribute, or a `#[default]` on a
/// positional field.
fn optional_attributes(field: &syn::Field) -> syn::Result<(Option<syn::Expr>, Option<syn::Expr>)> {
    let mut default: Option<syn::Expr> = None;
    let mut skip: Option<syn::Expr> = None;
    for attr in &field.attrs {
        let slot = if attr.path().is_ident("skip") {
            &mut skip
        } else if attr.path().is_ident("default") {
            if field.ident.is_none() {
                return Err(syn::Error::new_spanned(
                    attr,
                    "`#[default]` on a positional field is ambiguous: fields are matched by \
                     position, so a document that ends early cannot say which position is \
                     missing. Use `#[skip]` to remove the position, or move the field into a \
                     named-field struct/variant",
                ));
            }
            &mut default
        } else {
            // Not ours: `#[cfg]`, doc comments, another derive's helper.
            continue;
        };
        if slot.is_some() {
            return Err(syn::Error::new_spanned(
                attr,
                "duplicate field attribute for this field",
            ));
        }
        let syn::Meta::List(_) = &attr.meta else {
            return Err(syn::Error::new_spanned(
                attr,
                "expected an expression, e.g. `#[default(42)]` or `#[skip(0)]`",
            ));
        };
        *slot = Some(attr.parse_args()?);
    }
    Ok((default, skip))
}
