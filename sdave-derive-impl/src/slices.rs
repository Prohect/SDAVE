//! The compile-time `SLICES` count of a generated `Serialize` impl.
//!
//! Every slice count in this codec is independent of the allocator, so the derive
//! evaluates each field's `<FieldTy as Serialize<u8, _>>::SLICES` through the
//! concrete [`global_allocator`](fn@global_allocator) while the impl itself stays
//! generic over the allocator parameter (a projection off a generic parameter is
//! not a constant).

use alloc::vec::Vec;

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::{FieldInfo, Variants, global_allocator};

/// The `SLICES` count of a struct: the outer frame plus one signature and one
/// value Envelop per *encoded* field.
///
/// A `#[skip(..)]` field never reaches the wire, so it contributes nothing.
pub fn struct_slices(fields: &[FieldInfo]) -> TokenStream2 {
    let types: Vec<_> = fields
        .iter()
        .filter(|field| field.encoded())
        .map(|field| &field.ty)
        .collect();
    let allocator = global_allocator();
    quote!(2usize #( + 1 + <#types as ::sdave_codec::Serialize<u8, #allocator>>::SLICES )*)
}

/// The `SLICES` count of an enum: the maximum over all variants.
///
/// A variant with fields costs a struct's count ([`struct_slices`]) plus the 3
/// slices of the inner Envelop that wraps those fields. A variant with *no* fields
/// — a unit variant, or an empty braced/tuple one — is a name-only Envelop, i.e.
/// just 3, which is also how `serialize` writes it.
///
/// # Errors
/// Returns the `#[default]`/`#[skip]` attribute errors of any variant's fields.
pub fn enum_slices(variants: &Variants) -> syn::Result<TokenStream2> {
    let sizes: Vec<TokenStream2> = variants
        .iter()
        .map(|variant| {
            let fields = FieldInfo::collect(&variant.fields)?;
            Ok(if fields.is_empty() {
                quote!(3usize)
            } else {
                let fields = struct_slices(&fields);
                quote!((#fields) + 3)
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(quote!({
        let mut __sdave_max = 0usize;
        #(
            {
                let __sdave_variant = #sizes;
                if __sdave_variant > __sdave_max {
                    __sdave_max = __sdave_variant;
                }
            }
        )*
        __sdave_max
    }))
}
