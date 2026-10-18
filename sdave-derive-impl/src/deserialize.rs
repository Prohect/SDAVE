//! `#[derive(Deserialize)]` codegen.
//!
//! A generated `deserialize` reads the value's one Envelop into its payload
//! channel, then walks the `NonEnvelop(field signature) ++ Envelop(field value)`
//! pairs. Named fields are matched by their signature (so the order they arrive in
//! is irrelevant), tuple fields by position.
//!
//! Nothing is collected on the heap: the channel items of a generated type are a
//! compile-time-known count (two per encoded field), so they are collected into a
//! fixed-size array, and every field value is then decoded through the enclosing
//! [`FlatDeserializer`]'s recycled storage (`with_deserializer`).

use alloc::{string::ToString, vec::Vec};

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident, LitInt, LitStr};

use crate::{
    FieldInfo, Variants, allocator_ident, impl_generics, policy_ident, readable_field_types,
    self_type, type_param_bounds, union_error, where_predicates_of,
};

/// Deframe the value `#deserializer` is positioned on as one Envelop, binding its
/// payload as `__sdave_payload`.
///
/// `#deserializer` is the deserializer expression (`__sdave_deserializer` at the top
/// level, `__sdave_inner_deserializer` one level down) and `#final_buffer` is the
/// `Option<Offset>` flag matching it.
pub fn envelop_payload(deserializer: &TokenStream2, final_buffer: &TokenStream2) -> TokenStream2 {
    quote! {
        let __sdave_payload = unsafe {
            ::sdave_codec::envelop_payload(#deserializer, #final_buffer)?
        };
    }
}

/// [`envelop_payload`] plus the item channel of that payload, collected into a
/// fixed-size array (`item_slots` entries: two per encoded field for a named shape,
/// one per encoded field for a positional one) instead of a heap `Vec`.
pub fn payload_and_items(
    deserializer: &TokenStream2,
    final_buffer: &TokenStream2,
    item_slots: usize,
) -> TokenStream2 {
    let payload = envelop_payload(deserializer, final_buffer);
    quote! {
        #payload
        let mut __sdave_items: [::core::option::Option<::sdave_codec::Item>; #item_slots] =
            ::core::array::from_fn(|_| ::core::option::Option::None);
        unsafe {
            ::sdave_codec::with_deserializer(
                #deserializer,
                __sdave_payload,
                |__sdave_channel| {
                    let _ = ::sdave_codec::collect_items(__sdave_channel, &mut __sdave_items);
                    ::core::result::Result::Ok(())
                },
            )?;
        }
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

    let deserializer = quote!(__sdave_deserializer);
    let final_buffer = quote!(final_buffer);
    let body = match &input.data {
        Data::Struct(data) => struct_deserialize_body(
            &data.fields,
            &allocator,
            &policy,
            &deserializer,
            &final_buffer,
        )?,
        Data::Enum(data) => enum_deserialize_body(
            &data.variants,
            &allocator,
            &policy,
            &deserializer,
            &final_buffer,
        )?,
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
                #deserializer: &mut ::sdave_codec::FlatDeserializer<'_, '_, u8, #allocator>,
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
    deserializer: &TokenStream2,
    final_buffer: &TokenStream2,
) -> syn::Result<TokenStream2> {
    let infos = FieldInfo::collect(fields)?;
    if infos.is_empty() {
        // A fieldless struct is an Envelop like any other — in practice an empty
        // payload, exactly like `()` — so it only has to deframe cleanly. The value
        // expression still has to spell the struct's own shape: `Self` only names a
        // value for a unit struct, a braced/tuple one needs `Self {}` / `Self()`.
        let payload = envelop_payload(deserializer, final_buffer);
        let value = fieldless_constructor(&quote!(Self), fields);
        return Ok(quote! {
            #payload
            ::core::result::Result::Ok(#value)
        });
    }
    let positional = matches!(fields, Fields::Unnamed(_));
    let parse = parse_fields(&infos, positional, allocator, policy, deserializer);
    let ctor = constructor(&quote!(Self), &infos, !positional);
    let payload = payload_and_items(deserializer, final_buffer, item_slots(&infos, positional));
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
    deserializer: &TokenStream2,
    final_buffer: &TokenStream2,
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
                let positional = matches!(variant.fields, Fields::Unnamed(_));
                let inner_deserializer = quote!(__sdave_inner_deserializer);
                let inner_final = quote!(::core::option::Option::Some(::sdave_codec::Offset::new(
                    __sdave_inner.len()
                )));
                let parse =
                    parse_fields(&infos, positional, allocator, policy, &inner_deserializer);
                let ctor = constructor(&quote!(Self::#name), &infos, !positional);
                let payload = payload_and_items(
                    &inner_deserializer,
                    &inner_final,
                    item_slots(&infos, positional),
                );
                quote! {
                    let ::core::option::Option::Some(::sdave_codec::Item::E { envelop }) =
                        &__sdave_items[1]
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
                    return unsafe {
                        ::sdave_codec::with_deserializer(
                            #deserializer,
                            __sdave_inner,
                            |#inner_deserializer| {
                                #payload
                                #parse
                                ::core::result::Result::Ok(#ctor)
                            },
                        )
                    };
                }
            };
            Ok(quote! {
                if __sdave_name == #name_lit.as_bytes() {
                    #body
                }
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;

    // The variant payload is `NonEnvelop(variant name) ++ Envelop(fields)`.
    let payload = payload_and_items(deserializer, final_buffer, 2);
    Ok(quote! {
        #payload
        let ::core::option::Option::Some(::sdave_codec::Item::N { non_envelop }) =
            &__sdave_items[0]
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

/// How many items a channel of `fields` can hold: one `NonEnvelop` signature plus one `Envelop`
/// value per *encoded* field for a named shape, and exactly one value `Envelop` per *encoded*
/// field for a positional one (there is no per-field signature — the position is the identity).
fn item_slots(fields: &[FieldInfo], positional: bool) -> usize {
    let encoded = fields.iter().filter(|field| field.encoded()).count();
    if positional { encoded } else { 2 * encoded }
}

/// Statements that fill one `Option<FieldTy>` binding per *encoded* field from
/// `__sdave_items` / `__sdave_payload`, decoding each value through `deserializer`.
///
/// `positional` picks the shape, and the two are not variants of one another:
///
/// - a **named** shape writes `NonEnvelop(name: type) ++ Envelop(value)` per field, so the items
///   are read as signature/value pairs and matched **by signature** — the arrival order is
///   irrelevant and a pair this type does not know is ignored;
/// - a **positional** shape writes only the sequential value Envelops, so item `k` is field `k`
///   and the fields are matched **by position** (a `#[skip]`ped field consumes no position).
///
/// A `#[skip]` field takes no part in either: it is never written, so nothing matches it, and the
/// constructor builds it from its fallback expression (see [`FieldInfo::fallback`]).
pub fn parse_fields(
    fields: &[FieldInfo],
    positional: bool,
    allocator: &Ident,
    policy: &Ident,
    deserializer: &TokenStream2,
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
    let pair_pattern = quote!(::sdave_codec::Item::N { non_envelop });

    // One value is decoded through the *lending* deserializer: `with_deserializer` recycles
    // its storage for the nested value instead of allocating a new one. One token stream per
    // encoded field, index-parallel to `bindings`.
    let decodes: Vec<TokenStream2> = wire
        .iter()
        .map(|field| {
            let ty = &field.ty;
            quote! {
                ::core::option::Option::Some(unsafe {
                    ::sdave_codec::with_deserializer(
                        #deserializer,
                        __sdave_value,
                        |__sdave_value_deserializer| {
                            <#ty as ::sdave_codec::Deserialize<u8, #allocator>>::deserialize::<#policy>(
                                __sdave_value_deserializer,
                                ::core::option::Option::Some(::sdave_codec::Offset::new(
                                    __sdave_value.len(),
                                )),
                                allocator.clone(),
                            )
                        },
                    )?
                })
            }
        })
        .collect();

    // With nothing to match there is no scan to make: the payload only has to deframe
    // cleanly, which `payload_and_items` has already checked.
    let scan = if wire.is_empty() {
        quote!()
    } else if positional {
        // A positional shape writes one value Envelop per field, in order, and no
        // `Signature` at all: the position IS the identity, so field `k` is the `k`-th item.
        let indices: Vec<LitInt> = (0..wire.len())
            .map(|index| LitInt::new(&index.to_string(), Span::call_site()))
            .collect();
        quote! {
            #(
                if let ::core::option::Option::Some(::sdave_codec::Item::E { envelop }) =
                    &__sdave_items[#indices]
                {
                    let __sdave_tail =
                        envelop.tail_offset.map_or(__sdave_payload.len(), |tail| tail.get());
                    let __sdave_value =
                        &__sdave_payload[envelop.head_offset.get()..__sdave_tail];
                    #bindings = #decodes;
                }
            )*
        }
    } else {
        let names: Vec<_> = wire.iter().map(|field| &field.name_bytes).collect();
        quote! {
            let mut __sdave_i = 0usize;
            while __sdave_i + 1 < __sdave_items.len() {
                if let (
                    ::core::option::Option::Some(#pair_pattern),
                    ::core::option::Option::Some(::sdave_codec::Item::E { envelop }),
                ) = (&__sdave_items[__sdave_i], &__sdave_items[__sdave_i + 1]) {
                    let __sdave_tail =
                        envelop.tail_offset.map_or(__sdave_payload.len(), |tail| tail.get());
                    let __sdave_value =
                        &__sdave_payload[envelop.head_offset.get()..__sdave_tail];
                    let __sdave_signature = &__sdave_payload[
                        non_envelop.head_offset.get()..non_envelop.tail_offset.get()
                    ];
                    #(
                        if ::sdave_codec::named_signature_matches::<#policy, #types>(
                            __sdave_signature,
                            #names.as_bytes(),
                        ) {
                            #bindings = #decodes;
                        } else
                    )*
                    {}
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
