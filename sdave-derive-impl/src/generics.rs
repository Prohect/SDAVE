//! The annotated type's own generics, and how the generated impls echo them.
//!
//! A generated impl carries the annotated type's lifetime/type/const parameters
//! (plus the allocator `A`), keeps its `where`-clause, and — for the `TypeName`
//! impl — adds a `TypeName` bound on each of the type's type parameters so they
//! can be rendered.

use alloc::vec::Vec;

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, GenericParam, Ident, Type};

use crate::FieldInfo;

/// One generic parameter as it may appear in an `impl` header: the parameter
/// itself with any default stripped.
///
/// A default (`struct G<T = u8>`) is only legal in the type's own definition; an
/// `impl` header that echoed it would be rejected, so the derives MUST NOT carry it
/// over. Clearing `default` is enough: `syn` renders the `=` from it, there is no
/// separate `eq_token` to clear.
fn impl_param(param: &GenericParam) -> TokenStream2 {
    match param {
        GenericParam::Type(param) => {
            let mut param = param.clone();
            param.default = None;
            quote!(#param)
        }
        GenericParam::Const(param) => {
            let mut param = param.clone();
            param.default = None;
            quote!(#param)
        }
        GenericParam::Lifetime(_) => quote!(#param),
    }
}

/// The impl generics: the type's own generics (defaults stripped), then the
/// allocator `A`.
pub fn impl_generics(generics: &syn::Generics, allocator: &Ident) -> TokenStream2 {
    let mut params: Vec<TokenStream2> = generics.params.iter().map(impl_param).collect();
    params.push(quote!(#allocator));
    quote!(<#(#params),*>)
}

/// The self type: `Name`, `Name<'a>`, or `Name<T, N>` — every one of the type's
/// own generic arguments.
pub fn self_type(ident: &Ident, generics: &syn::Generics) -> TokenStream2 {
    let args: Vec<TokenStream2> = generics
        .params
        .iter()
        .map(|param| match param {
            GenericParam::Lifetime(param) => {
                let lifetime = &param.lifetime;
                quote!(#lifetime)
            }
            GenericParam::Type(param) => {
                let ident = &param.ident;
                quote!(#ident)
            }
            GenericParam::Const(param) => {
                let ident = &param.ident;
                quote!(#ident)
            }
        })
        .collect();
    if args.is_empty() {
        quote!(#ident)
    } else {
        quote!(#ident<#(#args),*>)
    }
}

/// The annotated type's own `where`-predicates, to append to the generated impl's
/// clause so the impl stays as constrained as the type it targets.
pub fn where_predicates_of(generics: &syn::Generics) -> Vec<&syn::WherePredicate> {
    generics
        .where_clause
        .iter()
        .flat_map(|clause| clause.predicates.iter())
        .collect()
}

/// The field types `Serialize` needs bounds for: the ones that reach the wire, i.e.
/// every field except a `#[skip]` one.
///
/// A `#[skip]` field is local to its own field: neither side of the codec may reach
/// into its type, so it needs no `Serialize`, `Deserialize` or `TypeName` impl at
/// all — it is always built from its fallback expression.
pub fn wire_field_types(data: &Data) -> Vec<Type> {
    all_fields(data)
        .into_iter()
        .filter(|field| !FieldInfo::is_skipped(field))
        .map(|field| field.ty.clone())
        .collect()
}

/// The field types `Deserialize` needs bounds for.
///
/// This is [`wire_field_types`]: a `#[skip]` field is absent from the wire in *both*
/// directions, so the read side needs bounds on exactly the fields the write side
/// does. The two names are kept apart because the derives reason about different
/// halves of the contract, and only one of them may come to need something extra.
pub fn readable_field_types(data: &Data) -> Vec<Type> {
    wire_field_types(data)
}

/// Every field of the type, in declaration order.
fn all_fields(data: &Data) -> Vec<&syn::Field> {
    match data {
        Data::Struct(data) => data.fields.iter().collect(),
        Data::Enum(data) => data
            .variants
            .iter()
            .flat_map(|variant| variant.fields.iter())
            .collect(),
        Data::Union(_) => Vec::new(),
    }
}

/// The type's own generics (without the allocator, defaults stripped), as an
/// `impl` parameter list.
pub fn type_generics(generics: &syn::Generics) -> TokenStream2 {
    let params: Vec<TokenStream2> = generics.params.iter().map(impl_param).collect();
    if params.is_empty() {
        quote!()
    } else {
        quote!(<#(#params),*>)
    }
}

/// `TypeName` bounds for the type's own type parameters, which `render` needs in
/// order to render each of them.
pub fn type_param_bounds(generics: &syn::Generics) -> Vec<TokenStream2> {
    generics
        .params
        .iter()
        .filter_map(|param| match param {
            GenericParam::Type(param) => {
                let ident = &param.ident;
                Some(quote!(#ident: ::sdave_codec::TypeName))
            }
            _ => None,
        })
        .collect()
}

/// The `TypeName` impl for `#this`.
///
/// `FULL` is the type's module path + ident (generic arguments ignored). `render`
/// writes `FULL` (shortened by the policy) then composes every type/const
/// argument through the same policy, one node at a time, so no argument's
/// shortening can affect another node.
pub fn type_name_impl(input: &DeriveInput, policy: &Ident) -> TokenStream2 {
    let ident = &input.ident;
    let this = self_type(ident, &input.generics);
    let generics = type_generics(&input.generics);
    let type_param_bounds = type_param_bounds(&input.generics);
    let where_predicates = where_predicates_of(&input.generics);

    let mut writes: Vec<TokenStream2> = Vec::new();
    for param in input.generics.params.iter() {
        let write = match param {
            GenericParam::Lifetime(_) => continue,
            GenericParam::Type(param) => {
                let param = &param.ident;
                quote!(<#param as ::sdave_codec::TypeName>::render::<#policy>(f)?;)
            }
            GenericParam::Const(param) => {
                let param = &param.ident;
                quote!(::core::fmt::Write::write_fmt(f, ::core::format_args!("{}", #param))?;)
            }
        };
        if !writes.is_empty() {
            writes.push(quote!(::core::fmt::Write::write_str(f, ", ")?;));
        }
        writes.push(write);
    }
    let args = if writes.is_empty() {
        quote!()
    } else {
        quote! {
            ::core::fmt::Write::write_str(f, "<")?;
            #(#writes)*
            ::core::fmt::Write::write_str(f, ">")?;
        }
    };

    quote! {
        impl #generics ::sdave_codec::TypeName for #this
        where
            #(#type_param_bounds,)*
            #(#where_predicates,)*
        {
            const FULL: &'static str =
                ::core::concat!(::core::module_path!(), "::", ::core::stringify!(#ident));

            fn render<#policy: ::sdave_codec::NamePolicy>(
                f: &mut dyn ::core::fmt::Write,
            ) -> ::core::fmt::Result {
                ::core::fmt::Write::write_str(
                    f,
                    <#policy as ::sdave_codec::NamePolicy>::shorten(
                        <Self as ::sdave_codec::TypeName>::FULL,
                    ),
                )?;
                #args
                ::core::result::Result::Ok(())
            }
        }
    }
}
