//! The codec derive layer of SDAVE: `#[derive(Serialize)]` / `#[derive(Deserialize)]`
//! and the `#[sdave_dyn]` attribute.
//!
//! This crate is a thin `proc-macro` wrapper. A `proc-macro` crate may only export
//! proc macros, so no generator can live here; every one is provided by
//! [`sdave_derive_impl`], a normal library crate that can be public, documented and
//! tested. See that crate for the shape of the generated impls.
//!
//! See the crate `README.md` for what the macros accept, what they emit and what they
//! leave out.

#![no_std]
#![warn(missing_docs)]

use proc_macro::TokenStream;

use syn::{DeriveInput, ItemTrait, Type, punctuated::Punctuated, token::Comma};

/// Derive `sdave_codec::Serialize` for the annotated type.
///
/// A union is rejected: the codec cannot know which member is live, so a union's
/// (de)serialization MUST be hand-written.
///
/// # Field attributes
///
/// Two optional attributes are read off each field, both shared with
/// [`macro@Deserialize`]: `#[default(<expr>)]` (a *named* field may be missing from a
/// document) and `#[skip(<expr>)]` (the field is local to this type and never reaches
/// the wire). What they mean, how they combine, and why `#[default]` is rejected on a
/// positional field are documented once in the crate `README.md`.
#[proc_macro_derive(Serialize, attributes(default, skip))]
pub fn derive_serialize(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    sdave_derive_impl::expand_serialize(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive `sdave_codec::Deserialize` for the annotated type.
///
/// Reads the same `#[default(<expr>)]` / `#[skip(<expr>)]` field attributes as
/// [`macro@Serialize`], with the same meaning; see the crate `README.md`.
#[proc_macro_derive(Deserialize, attributes(default, skip))]
pub fn derive_deserialize(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    sdave_derive_impl::expand_deserialize(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// `#[sdave_dyn(ConcreteA, ConcreteB, ...)]`: enable `dyn Trait` SDAVE support.
///
/// Applied to a trait definition, this emits `TypeName` and `Serialize` for
/// `dyn Trait`, `Deserialize` for `Box<dyn Trait, A>`, and a sealing supertrait that is
/// implemented only for the listed types, so `impl Trait for Other` is a compile error
/// rather than a value that can be built but never (de)serialized.
///
/// The exact impls, what serialization writes, and the requirements are documented once
/// in the crate `README.md`.
#[proc_macro_attribute]
pub fn sdave_dyn(attr: TokenStream, item: TokenStream) -> TokenStream {
    let types = syn::parse_macro_input!(attr with Punctuated::<Type, Comma>::parse_terminated);
    let item = syn::parse_macro_input!(item as ItemTrait);
    sdave_derive_impl::expand_sdave_dyn(&types, item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
