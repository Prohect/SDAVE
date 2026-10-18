//! The public codegen internals of `sdave-derive`.
//!
//! [`sdave-derive`] is a `proc-macro` crate, which may only export proc macros,
//! so all of its token-generation logic lives here instead — in a normal library
//! crate that can be public, documented and tested.
//!
//! The module map, the conventions the generated code follows (absolute
//! `::sdave_codec` paths, the `__SDAVE_A`/`__SDAVE_N` parameters, the `__sdave_*`
//! locals), the `SLICES` accounting and the test layout are documented once in the
//! crate `README.md`. The wire format those impls produce is `sdave-codec`'s
//! subject, and the macro surface the user sees is `sdave-derive`'s.
//!
//! [`sdave-derive`]: https://docs.rs/sdave-derive

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Ident, Variant, punctuated::Punctuated, token::Comma};

pub mod deserialize;
pub mod dyn_trait;
pub mod field;
pub mod generics;
pub mod serialize;
pub mod slices;

pub use crate::{deserialize::*, dyn_trait::*, field::*, generics::*, serialize::*, slices::*};

/// The variant list of an enum.
pub type Variants = Punctuated<Variant, Comma>;

/// The allocator parameter generated impls are generic over.
pub fn allocator_ident() -> Ident {
    format_ident!("__SDAVE_A")
}

/// The name-policy generic parameter of generated (de)serializers.
pub fn policy_ident() -> Ident {
    format_ident!("__SDAVE_N")
}

/// A concrete allocator used only to *evaluate* the field types' `SLICES`
/// associated consts.
///
/// The array lengths of `Slices`/`SlicesTyped` must be concrete, but a trait
/// projection over the generic allocator parameter
/// (`<T as Serialize<u8, A>>::SLICES`) cannot be evaluated in a const context.
/// Every slice count in this codec is independent of the allocator, so the
/// derive evaluates them through `Global` while the impl itself stays generic
/// over `A`.
pub fn global_allocator() -> TokenStream2 {
    quote!(::sdave_codec::__private::Global)
}

/// The associated slice-array type used by every generated impl:
/// `[Option<(Offset, Offset)>; len]`.
pub fn slices_type(len: &TokenStream2) -> TokenStream2 {
    quote!([::core::option::Option<(::sdave_codec::Offset, ::sdave_codec::Offset)>; #len])
}

/// The compile error emitted for a union, which the codec cannot (de)serialize
/// without a hand-written impl.
pub fn union_error(data: &syn::DataUnion) -> syn::Error {
    syn::Error::new_spanned(data.union_token, "sdave derive does not support unions")
}
