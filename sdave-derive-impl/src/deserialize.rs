//! `#[derive(Deserialize)]` codegen.
//!
//! A generated `deserialize` reads the value's one Envelop into its payload
//! channel, then walks the `NonEnvelop(field signature) ++ Envelop(field value)`
//! pairs. Named fields are matched by their signature (so the order they arrive in
//! is irrelevant), tuple fields by position.

use alloc::{string::ToString, vec::Vec};

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident, LitInt, LitStr};

use crate::{
    FieldInfo, Variants, allocator_ident, impl_generics, policy_ident, readable_field_types,
    self_type, type_param_bounds, union_error, where_predicates_of,
};

/// The generated preamble of every deserializer body that only needs the value's
/// payload channel: deframe `buffer` as one Envelop and bind its payload as
/// `__sdave_payload`.
///
/// `buffer` is the byte expression to deframe (`buffer` at the top level, the
/// inner Envelop of an enum variant one level down), and `final_buffer` is the
/// `Option<Offset>` flag matching it. The emitted code uses the generated
/// deserializer's own local names (`variant`, `limiter_pairs`, `allocator`).
pub fn envelop_payload(buffer: &TokenStream2, final_buffer: &TokenStream2) -> TokenStream2 {
    quote! {
        let __sdave_payload = unsafe {
            ::sdave_codec::single_item_buffer_as_envelop_payload(
                #buffer,
                variant,
                limiter_pairs,
                #final_buffer,
                allocator.clone(),
            )?
        };
    }
}

/// [`envelop_payload`] plus the item channel of that payload: the
/// `NonEnvelop(field signature) ++ Envelop(field value)` pairs every derived type
/// walks.
pub fn payload_and_items(buffer: &TokenStream2, final_buffer: &TokenStream2) -> TokenStream2 {
    let payload = envelop_payload(buffer, final_buffer);
    quote! {
        #payload
        let __sdave_items = ::sdave_codec::items_of(
            __sdave_payload,
            variant,
            limiter_pairs,
            allocator.clone(),
        )?;
    }
}

/// Generate `impl Deserialize for #ty`.
pub fn expand_deserialize(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let allocator = allocator_ident();
    let policy = policy_ident();
    let ident = &input.ident;
    let this = self_type(ident, &input.generics);
    let generics = impl_generics(&input.generics, &allocator);
    let field_types = readable_field_types(&input.data);
    let bounds: Vec<TokenStream2> = field_types
        .iter()
        .map(|ty| quote!(#ty: ::sdave_codec::Deserialize<u8, #allocator, Type = #ty>))
        .collect();
    let type_name_bounds: Vec<TokenStream2> = field_types
        .iter()
        .map(|ty| quote!(#ty: ::sdave_codec::TypeName))
        .collect();
    let type_param_bounds = type_param_bounds(&input.generics);
    let where_predicates = where_predicates_of(&input.generics);

    let body = match &input.data {
        Data::Struct(data) => struct_deserialize_body(&data.fields, &allocator, &policy)?,
        Data::Enum(data) => enum_deserialize_body(&data.variants, &allocator, &policy)?,
        Data::Union(data) => return Err(union_error(data)),
    };

    Ok(quote! {
        impl #generics ::sdave_codec::Deserialize<u8, #allocator> for #this
        where
            #allocator: ::core::alloc::Allocator + ::core::clone::Clone,
            #(#bounds,)*
            #(#type_name_bounds,)*
            #(#type_param_bounds,)*
            #(#where_predicates,)*
        {
            type Type = #this;

            fn deserialize<#policy: ::sdave_codec::NamePolicy>(
                buffer: &[u8],
                variant: ::sdave_codec::Variant,
                limiter_pairs: &[::sdave_codec::LimiterPair<'_, u8, #allocator>],
                final_buffer: ::core::option::Option<::sdave_codec::Offset>,
                allocator: #allocator,
            ) -> ::core::result::Result<<Self as ::sdave_codec::Deserialize<u8, #allocator>>::Type, ::sdave_codec::Error> {
                #body
            }
        }
    })
}

/// The `deserialize` body of a struct.
///
/// # Errors
/// Returns the `#[default]`/`#[skip]` attribute errors of any field.
pub fn struct_deserialize_body(
    fields: &Fields,
    allocator: &Ident,
    policy: &Ident,
) -> syn::Result<TokenStream2> {
    let infos = FieldInfo::collect(fields)?;
    if infos.is_empty() {
        // A fieldless struct is an Envelop like any other — in practice an empty
        // payload, exactly like `()` — so it only has to deframe cleanly. The value
        // expression still has to spell the struct's own shape: `Self` only names a
        // value for a unit struct, a braced/tuple one needs `Self {}` / `Self()`.
        let payload = envelop_payload(&quote!(buffer), &quote!(final_buffer));
        let value = fieldless_constructor(&quote!(Self), fields);
        return Ok(quote! {
            #payload
            ::core::result::Result::Ok(#value)
        });
    }
    let named = fields.iter().any(|f| f.ident.is_some());
    let parse = parse_fields(&infos, named, allocator, policy);
    let ctor = constructor(&quote!(Self), &infos, named);
    let payload = payload_and_items(&quote!(buffer), &quote!(final_buffer));
    Ok(quote! {
        #payload
        #parse
        ::core::result::Result::Ok(#ctor)
    })
}

/// The `deserialize` body of an enum.
///
/// # Errors
/// Returns the `#[default]`/`#[skip]` attribute errors of any variant's fields.
pub fn enum_deserialize_body(
    variants: &Variants,
    allocator: &Ident,
    policy: &Ident,
) -> syn::Result<TokenStream2> {
    let arms: Vec<TokenStream2> = variants
        .iter()
        .map(|variant| {
            let name = &variant.ident;
            let name_lit = LitStr::new(&name.to_string(), name.span());
            // A variant with no fields — a unit one, or an empty braced/tuple one —
            // is written as the bare name NonEnvelop (see `enum_serialize_arms`), so
            // there is no inner Envelop to deframe: it is built like a unit variant.
            let infos = FieldInfo::collect(&variant.fields)?;
            let body = if infos.is_empty() {
                let value = fieldless_constructor(&quote!(Self::#name), &variant.fields);
                quote! {
                    return ::core::result::Result::Ok(#value);
                }
            } else {
                let named = matches!(variant.fields, Fields::Named(_));
                let parse = parse_fields(&infos, named, allocator, policy);
                let ctor = constructor(&quote!(Self::#name), &infos, named);
                let final_inner = quote!(::core::option::Option::Some(::sdave_codec::Offset::new(
                    __sdave_inner.len()
                )));
                let payload = payload_and_items(&quote!(__sdave_inner), &final_inner);
                quote! {
                    let ::core::option::Option::Some(::sdave_codec::Item::E { envelop }) =
                        __sdave_items.get(1)
                    else {
                        return ::core::result::Result::Err(
                            ::sdave_codec::Error::UnexpectedItem,
                        );
                    };
                    let __sdave_tail = envelop
                        .tail_offset
                        .map_or(__sdave_payload.len(), |tail| tail.get());
                    let __sdave_inner =
                        &__sdave_payload[envelop.head_offset.get()..__sdave_tail];
                    #payload
                    #parse
                    return ::core::result::Result::Ok(#ctor);
                }
            };
            Ok(quote! {
                if __sdave_name == #name_lit.as_bytes() {
                    #body
                }
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let payload = payload_and_items(&quote!(buffer), &quote!(final_buffer));
    Ok(quote! {
        #payload
        let ::core::option::Option::Some(::sdave_codec::Item::N { non_envelop }) =
            __sdave_items.first()
        else {
            return ::core::result::Result::Err(::sdave_codec::Error::UnexpectedItem);
        };
        let __sdave_name =
            &__sdave_payload[non_envelop.head_offset.get()..non_envelop.tail_offset.get()];
        #(#arms)*
        {
            ::core::result::Result::Err(::sdave_codec::Error::UnexpectedItem)
        }
    })
}

/// Statements that fill one `Option<FieldTy>` binding per *encoded* field from
/// `__sdave_items` / `__sdave_payload`.
///
/// Named fields are matched by their `Signature`, so their arrival order is
/// irrelevant; tuple fields match by wire position. A `#[skip]` field takes no part
/// in either: it is never written, so nothing matches it, and the constructor builds
/// it from its fallback expression (see [`FieldInfo::fallback`]).
pub fn parse_fields(
    fields: &[FieldInfo],
    named: bool,
    allocator: &Ident,
    policy: &Ident,
) -> TokenStream2 {
    let wire: Vec<_> = fields.iter().filter(|field| field.encoded()).collect();
    let bindings: Vec<_> = wire.iter().map(|field| &field.binding).collect();
    let types: Vec<_> = wire.iter().map(|field| &field.ty).collect();

    // A field nothing can ever assign to — a `#[skip]` one — is declared without
    // `mut`, so the generated body cannot trip `unused_mut` in the annotated crate.
    let declarations: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let binding = &field.binding;
            let ty = &field.ty;
            if field.encoded() {
                quote!(let mut #binding: ::core::option::Option<#ty> = ::core::option::Option::None;)
            } else {
                quote!(let #binding: ::core::option::Option<#ty> = ::core::option::Option::None;)
            }
        })
        .collect();

    // `non_envelop` is only bound where it is read, so a body with nothing to match
    // cannot leave an unused binding behind.
    let pair_pattern = if named && !wire.is_empty() {
        quote!(::sdave_codec::Item::N { non_envelop })
    } else {
        quote!(::sdave_codec::Item::N { .. })
    };

    let assignments = if wire.is_empty() {
        // Nothing is encoded, so there is nothing to match; emitting an empty
        // `match`/`if` chain would only be dead code in the generated body.
        quote!()
    } else if named {
        let names: Vec<_> = wire.iter().map(|field| &field.name_bytes).collect();
        quote! {
            let __sdave_signature = &__sdave_payload[
                non_envelop.head_offset.get()..non_envelop.tail_offset.get()
            ];
            #(
                if ::sdave_codec::named_signature_matches::<#policy, #types>(
                    __sdave_signature,
                    #names.as_bytes(),
                ) {
                    #bindings = ::core::option::Option::Some(
                        <#types as ::sdave_codec::Deserialize<u8, #allocator>>::deserialize::<#policy>(
                            __sdave_value,
                            variant,
                            limiter_pairs,
                            ::core::option::Option::Some(::sdave_codec::Offset::new(
                                __sdave_value.len(),
                            )),
                            allocator.clone(),
                        )?,
                    );
                } else
            )*
            {}
        }
    } else {
        // Tuple fields match by position, and only the encoded ones occupy a slot.
        let indices: Vec<LitInt> = (0..wire.len())
            .map(|index| LitInt::new(&index.to_string(), Span::call_site()))
            .collect();
        quote! {
            let __sdave_index = __sdave_i / 2;
            match __sdave_index {
                #(
                    #indices => {
                        #bindings = ::core::option::Option::Some(
                            <#types as ::sdave_codec::Deserialize<u8, #allocator>>::deserialize::<#policy>(
                                __sdave_value,
                                variant,
                                limiter_pairs,
                                ::core::option::Option::Some(::sdave_codec::Offset::new(
                                    __sdave_value.len(),
                                )),
                                allocator.clone(),
                            )?,
                        );
                    }
                )*
                _ => {}
            }
        }
    };

    // With nothing to match there is no scan to make: the payload only has to deframe
    // cleanly, which `payload_and_items` has already checked.
    let scan = if wire.is_empty() {
        quote!()
    } else {
        quote! {
            let mut __sdave_i = 0usize;
            while __sdave_i + 1 < __sdave_items.len() {
                if let (
                    #pair_pattern,
                    ::sdave_codec::Item::E { envelop },
                ) = (&__sdave_items[__sdave_i], &__sdave_items[__sdave_i + 1]) {
                    let __sdave_tail =
                        envelop.tail_offset.map_or(__sdave_payload.len(), |tail| tail.get());
                    let __sdave_value =
                        &__sdave_payload[envelop.head_offset.get()..__sdave_tail];
                    #assignments
                }
                __sdave_i += 2;
            }
        }
    };

    quote! {
        #(#declarations)*
        #scan
    }
}

/// The value expression for a shape that carries no fields at all: `path` for a
/// unit shape, `path {}` for a braced one and `path()` for a tuple one.
///
/// A fieldless braced or tuple shape has no `path`-as-a-value form, which is why
/// both the struct and the enum deserializer build them through here.
pub fn fieldless_constructor(path: &TokenStream2, fields: &Fields) -> TokenStream2 {
    match fields {
        Fields::Unit => quote!(#path),
        Fields::Named(_) => constructor(path, &[], true),
        Fields::Unnamed(_) => constructor(path, &[], false),
    }
}

/// The `path { .. }` / `path(..)` constructor expression for `fields`, where
/// `path` is `Self` for a struct or `Self::Variant` for an enum variant.
pub fn constructor(path: &TokenStream2, fields: &[FieldInfo], named: bool) -> TokenStream2 {
    let values: Vec<_> = fields.iter().map(field_value).collect();
    if named {
        let idents: Vec<_> = fields
            .iter()
            .map(|field| field.ident.as_ref().expect("named field"))
            .collect();
        quote! {
            #path {
                #(#idents: #values),*
            }
        }
    } else {
        quote! {
            #path(#(#values),*)
        }
    }
}

/// The expression that yields one field's value in the constructor: the decoded
/// value when the field is on the wire, else its `#[default]`/`#[skip]` expression
/// (or [`Error::UnexpectedItem`] when the field is mandatory).
fn field_value(field: &FieldInfo) -> TokenStream2 {
    let binding = &field.binding;
    match field.fallback() {
        Some(fallback) => quote!(#binding.unwrap_or_else(|| #fallback)),
        None => quote!(#binding.ok_or(::sdave_codec::Error::UnexpectedItem)?),
    }
}
