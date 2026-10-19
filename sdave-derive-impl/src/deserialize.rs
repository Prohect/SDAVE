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

/// Deframe the value `#deserializer` is positioned on as one Envelop, binding the raw
/// buffer it reads as `__sdave_buffer` and the payload's absolute `(head, tail)` frame as
/// `__sdave_payload`.
///
/// Every item offset the generated reader then handles is absolute in `__sdave_buffer`,
/// so slicing an item out is `&__sdave_buffer[item.head_offset.get()..tail]` — the
/// payload frame is what a nested `with_deserializer` call and an `Error` take.
/// Both bindings are `_`-prefixed, so a reader that uses only one of them stays warning
/// free.
///
/// `#deserializer` is the deserializer expression (`__sdave_deserializer` at the top
/// level, `__sdave_inner_deserializer` one level down) and `#final_buffer` is the
/// `Option<Offset>` flag matching it.
pub fn envelop_payload(deserializer: &TokenStream2, final_buffer: &TokenStream2) -> TokenStream2 {
    quote! {
        let (__sdave_buffer, __sdave_payload) = unsafe {
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

/// The `Error::UnexpectedSignature` the generated code reports: `valid_signatures` names what it
/// would have accepted and `caught` is the range that stood where one of them belongs.
pub fn unexpected_signature(
    valid_signatures: &TokenStream2,
    caught: &TokenStream2,
) -> TokenStream2 {
    quote! {
        ::sdave_codec::Error::UnexpectedSignature {
            valid_signatures: #valid_signatures,
            caught: #caught,
        }
    }
}

/// The `Error::MissingEnvelop` the generated code reports: `required_by` is the signature that
/// demands the absent Envelop(s) (`None` when nothing did) and `caught` is the range searched for
/// them.
pub fn missing_envelop(required_by: &TokenStream2, caught: &TokenStream2) -> TokenStream2 {
    quote! {
        ::sdave_codec::Error::MissingEnvelop {
            required_by: #required_by,
            caught: #caught,
        }
    }
}

/// The range of the signature `non_envelop` is bound to, as a `caught`/`required_by` value.
pub fn signature_range() -> TokenStream2 {
    quote!((non_envelop.head_offset, non_envelop.tail_offset))
}

/// Generate `impl Deserialize for #ty`.
///
/// # Errors
/// Returns the union rejection, or the `#[default]`/`#[skip]` attribute errors of any
/// field of any variant.
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
    let ctor = constructor(&quote!(Self), &infos, !positional, &quote!(__sdave_payload));
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
    // Every variant name is a valid signature of this channel, so a tag matching none of them
    // reports them all; an absent tag is caught on the payload, an unmatched one on itself.
    let names: Vec<LitStr> = variants
        .iter()
        .map(|variant| {
            let name = &variant.ident;
            LitStr::new(&name.to_string(), name.span())
        })
        .collect();
    let valid_signatures = quote!(&[#(#names),*]);
    let missing_tag = unexpected_signature(&valid_signatures, &quote!(__sdave_payload));
    let unmatched_tag = unexpected_signature(&valid_signatures, &signature_range());
    // A data variant's tag requires its inner Envelop right behind it.
    let missing_inner = missing_envelop(
        &quote!(::core::option::Option::Some((
            non_envelop.head_offset,
            non_envelop.tail_offset,
        ))),
        &quote!((non_envelop.tail_offset, __sdave_payload.1)),
    );
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
                let inner_final = quote!(::core::option::Option::Some(__sdave_inner_final));
                let parse =
                    parse_fields(&infos, positional, allocator, policy, &inner_deserializer);
                let ctor = constructor(
                    &quote!(Self::#name),
                    &infos,
                    !positional,
                    &quote!(__sdave_payload),
                );
                let payload = payload_and_items(
                    &inner_deserializer,
                    &inner_final,
                    item_slots(&infos, positional),
                );
                quote! {
                    let ::core::option::Option::Some(::sdave_codec::Item::E { envelop }) =
                        &__sdave_items[1]
                    else {
                        return ::core::result::Result::Err(#missing_inner);
                    };
                    // The variant's own Envelop, absolute in `__sdave_buffer`: from its
                    // limiter slice to its tail, which is this payload's tail while a
                    // `Variant::V2` delimiter is still unconfirmed.
                    let __sdave_inner_final = envelop.tail_offset.unwrap_or(__sdave_payload.1);
                    let __sdave_inner = (envelop.head_offset, __sdave_inner_final);
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
            return ::core::result::Result::Err(#missing_tag);
        };
        let __sdave_name =
            &__sdave_buffer[non_envelop.head_offset.get()..non_envelop.tail_offset.get()];
        #(#arms)*
        {
            ::core::result::Result::Err(#unmatched_tag)
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
/// `__sdave_items` / `__sdave_buffer`, decoding each value through `deserializer`.
///
/// `positional` picks the shape, and the two are not variants of one another:
///
/// - a **named** shape writes `NonEnvelop(name: type) ++ Envelop(value)` per field, so the items
///   are read as signature/value pairs and matched **by signature** — the arrival order is
///   irrelevant, and a signature this type does not know is reported as
///   `Error::UnexpectedSignature` at itself;
/// - a **positional** shape writes only the sequential value Envelops, so item `k` is field `k`
///   and the fields are matched **by position** (a `#[skip]`ped field consumes no position).
///
/// A `#[skip]` field takes no part in either: it is never written, so nothing matches it, and the
/// constructor builds it from its fallback expression (see [`FieldInfo::fallback`]).
///
/// A signature that matches a field **requires** its value Envelop right behind it, else
/// `MissingEnvelop`; a positional slot holding a signature is `UnexpectedSignature` (no signature
/// is valid there). A *required* field that never arrived is left to the constructor, which
/// reports `UnexpectedSignature` with the signatures it would have accepted.
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

    // One value is decoded through the *lending* deserializer: `with_deserializer` recycles
    // its storage for the nested value instead of allocating a new one. One token stream per
    // encoded field, index-parallel to `bindings`. `__sdave_value` is the value Envelop's
    // absolute frame, so its tail is also the final length of the frame the child reads.
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
                                ::core::option::Option::Some(__sdave_value.1),
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
        // No signature is valid in a positional slot.
        let slot_signature = unexpected_signature(&quote!(&[]), &signature_range());
        quote! {
            #(
                match &__sdave_items[#indices] {
                    ::core::option::Option::Some(::sdave_codec::Item::E { envelop }) => {
                        // The value Envelop's own frame, absolute in `__sdave_buffer`.
                        let __sdave_value =
                            (envelop.head_offset, envelop.tail_offset.unwrap_or(__sdave_payload.1));
                        #bindings = #decodes;
                    }
                    ::core::option::Option::Some(::sdave_codec::Item::N { non_envelop }) => {
                        // A positional slot is the value Envelop alone: no signature is valid
                        // there.
                        return ::core::result::Result::Err(#slot_signature);
                    }
                    // No Envelop at this position: the constructor reports the missing value.
                    _ => {}
                }
            )*
        }
    } else {
        let names: Vec<_> = wire.iter().map(|field| &field.name_bytes).collect();
        let valid_signatures = quote!(&[#(#names),*]);
        let missing_value = missing_envelop(
            &quote!(::core::option::Option::Some((
                non_envelop.head_offset,
                non_envelop.tail_offset,
            ))),
            &quote!((non_envelop.tail_offset, __sdave_payload.1)),
        );
        let unknown_signature = unexpected_signature(&valid_signatures, &signature_range());
        let stray_item = unexpected_signature(
            &valid_signatures,
            &quote!((item.head_offset(), item.semantical_tail_offset())),
        );
        quote! {
            let mut __sdave_i = 0usize;
            while __sdave_i + 1 < __sdave_items.len() {
                match &__sdave_items[__sdave_i] {
                    // The channel ended: the constructor reports what never arrived.
                    ::core::option::Option::None => break,
                    ::core::option::Option::Some(::sdave_codec::Item::N { non_envelop }) => {
                        let __sdave_signature = &__sdave_buffer[
                            non_envelop.head_offset.get()..non_envelop.tail_offset.get()
                        ];
                        #(
                            if ::sdave_codec::named_signature_matches::<#policy, #types>(
                                __sdave_signature,
                                #names.as_bytes(),
                            ) {
                                // A field signature requires its value Envelop right behind it.
                                let ::core::option::Option::Some(::sdave_codec::Item::E { envelop }) =
                                    &__sdave_items[__sdave_i + 1]
                                else {
                                    return ::core::result::Result::Err(#missing_value);
                                };
                                let __sdave_value = (
                                    envelop.head_offset,
                                    envelop.tail_offset.unwrap_or(__sdave_payload.1),
                                );
                                #bindings = #decodes;
                            } else
                        )*
                        {
                            // A signature this type does not accept: reported at itself, so
                            // the caller sees which text was wrong.
                            return ::core::result::Result::Err(#unknown_signature);
                        }
                    }
                    ::core::option::Option::Some(item) => {
                        // A value Envelop where a signature belongs: this channel is not a
                        // sequence of `signature ++ Envelop` pairs.
                        return ::core::result::Result::Err(#stray_item);
                    }
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
    // No field can be missing, so the report `caught` is never used.
    let caught = quote!(__sdave_payload);
    match fields {
        Fields::Unit => quote!(#path),
        Fields::Named(_) => constructor(path, &[], true, &caught),
        Fields::Unnamed(_) => constructor(path, &[], false, &caught),
    }
}

/// The `path { .. }` / `path(..)` constructor expression for `fields`, where
/// `path` is `Self` for a struct or `Self::Variant` for an enum variant.
///
/// `caught` is the binding holding the absolute `(head, tail)` frame of the channel the
/// fields were read from: a mandatory field that channel did not hold is reported there,
/// together with the field names that would have matched.
pub fn constructor(
    path: &TokenStream2,
    fields: &[FieldInfo],
    named: bool,
    caught: &TokenStream2,
) -> TokenStream2 {
    // A positional field has no signature of its own (its position is its identity), so only
    // a named shape can name the signatures it would have accepted -- and only the ones that
    // reach the wire: a `#[skip]`ped field is never written, so no document can carry it.
    let valid_signatures = if named {
        let names: Vec<_> = fields
            .iter()
            .filter(|field| field.encoded())
            .map(|field| &field.name_bytes)
            .collect();
        quote!(&[#(#names),*])
    } else {
        quote!(&[])
    };
    let values: Vec<_> = fields
        .iter()
        .map(|field| field_value(field, &valid_signatures, caught))
        .collect();
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
/// (or `Error::UnexpectedSignature` when the field is mandatory).
fn field_value(
    field: &FieldInfo,
    valid_signatures: &TokenStream2,
    caught: &TokenStream2,
) -> TokenStream2 {
    let binding = &field.binding;
    match field.fallback() {
        Some(fallback) => quote!(#binding.unwrap_or_else(|| #fallback)),
        None => {
            let missing = unexpected_signature(valid_signatures, caught);
            quote!(#binding.ok_or(#missing)?)
        }
    }
}
