//! `#[derive(Serialize)]` codegen.
//!
//! A generated `serialize` writes `NonEnvelop(field signature) ++
//! Envelop(field value)` for every field, wrapped in one outer Envelop; a
//! monomorphic type keeps those slices in one fixed-size array, while a generic
//! type (whose `Slices` cannot be sized from its fields' compile-time `SLICES`)
//! flattens them in place like an iterable / `dyn Trait` and always reports a
//! fixed-size result.
//!
//! [`serialize_typed_method`] is the second half of every generated impl, and is
//! shared with `#[sdave_dyn]`'s `dyn Trait` impl.

use alloc::{string::ToString, vec::Vec};

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Fields, GenericParam, Ident, LitStr};

use crate::{
    FieldInfo, Variants, allocator_ident, enum_slices, impl_generics, policy_ident,
    positional_struct_slices, self_type, slices_type, struct_slices, type_name_impl,
    type_param_bounds, union_error, where_predicates_of, wire_field_types,
};

/// Generate `impl Serialize for #ty`.
pub fn expand_serialize(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let allocator = allocator_ident();
    let policy = policy_ident();
    let ident = &input.ident;
    let this = self_type(ident, &input.generics);
    let generics = impl_generics(&input.generics, &allocator);
    let field_types = wire_field_types(&input.data);
    let bounds: Vec<TokenStream2> = field_types
        .iter()
        .map(|ty| quote!(#ty: ::sdave_codec::Serialize<u8, #allocator>))
        .collect();
    let type_name_bounds: Vec<TokenStream2> = field_types
        .iter()
        .map(|ty| quote!(#ty: ::sdave_codec::TypeName))
        .collect();
    let type_param_bounds = type_param_bounds(&input.generics);
    let where_predicates = where_predicates_of(&input.generics);
    // A type with type/const parameters cannot size `Slices` from its fields'
    // compile-time `SLICES` (a projection off a generic parameter is not a
    // constant), so it flattens in place like an iterable / `dyn Trait` and always
    // reports a fixed-size result. Monomorphic types keep the unflattened path.
    let generic = input
        .generics
        .params
        .iter()
        .any(|param| !matches!(param, GenericParam::Lifetime(_)));

    let (slices_len, slices_typed_len, body) = match &input.data {
        Data::Union(data) => return Err(union_error(data)),
        Data::Struct(data) => {
            let fields = FieldInfo::collect(&data.fields)?;
            let positional = matches!(data.fields, Fields::Unnamed(_));
            if generic {
                // A type parameter's `SLICES` is not a constant, so the slice array cannot be
                // sized: a generic type is the only shape that still flattens in place.
                let body =
                    flattened_struct_serialize_body(&fields, &allocator, &policy, positional);
                (quote!(3usize), quote!(6usize), body)
            } else {
                let slices = if positional {
                    positional_struct_slices(&fields)
                } else {
                    struct_slices(&fields)
                };
                let body = struct_serialize_body(&fields, &allocator, &policy, positional);
                (quote!((#slices)), quote!((#slices) + 3), body)
            }
        }
        Data::Enum(data) if generic => {
            let body = flattened_enum_serialize_body(&data.variants, &allocator, &policy)?;
            (quote!(3usize), quote!(6usize), body)
        }
        Data::Enum(data) => {
            let slices = enum_slices(&data.variants)?;
            let body = enum_serialize_body(&data.variants, &allocator, &policy)?;
            (quote!((#slices)), quote!((#slices) + 3), body)
        }
    };
    let slices = slices_type(&slices_len);
    let slices_typed = slices_type(&slices_typed_len);
    let serialize_typed = serialize_typed_method(&allocator, &policy);
    let type_name = type_name_impl(input, &policy);

    Ok(quote! {
        #type_name

        impl #generics ::sdave_codec::Serialize<u8, #allocator> for #this
        where
            #allocator: ::core::alloc::Allocator + ::core::clone::Clone,
            #(#bounds,)*
            #(#type_name_bounds,)*
            #(#type_param_bounds,)*
            #(#where_predicates,)*
        {
            const SLICES: usize = #slices_len;
            type Slices = #slices;
            type SlicesTyped = #slices_typed;

            unsafe fn serialize<#policy: ::sdave_codec::NamePolicy>(
                &self,
                buffer: &mut ::sdave_codec::__private::Vec<u8, #allocator>,
                variant: ::sdave_codec::Variant,
                strategy: ::sdave_codec::Strategy,
                limiter_pairs: &mut [::sdave_codec::LimiterPair<'_, u8, #allocator>],
            ) -> ::core::result::Result<<Self as ::sdave_codec::Serialize<u8, #allocator>>::Slices, ::sdave_codec::Error> {
                #body
            }

            #serialize_typed
        }
    })
}

/// The generated `serialize_typed` method, shared verbatim by every `Serialize`
/// impl this crate emits (the derives and `#[sdave_dyn]`): prefix the `name: type`
/// signature NonEnvelop, serialize the value through the type's own `serialize`,
/// then wrap both in one outer Envelop via `sdave_codec::frame_typed_envelop`.
///
/// The type describer is `Self`'s name rendered through the `#policy` generic.
pub fn serialize_typed_method(allocator: &syn::Ident, policy: &syn::Ident) -> TokenStream2 {
    quote! {
        unsafe fn serialize_typed<#policy: ::sdave_codec::NamePolicy>(
            &self,
            buffer: &mut ::sdave_codec::__private::Vec<u8, #allocator>,
            name: ::sdave_codec::Name,
            variant: ::sdave_codec::Variant,
            strategy: ::sdave_codec::Strategy,
            limiter_pairs: &mut [::sdave_codec::LimiterPair<'_, u8, #allocator>],
        ) -> ::core::result::Result<<Self as ::sdave_codec::Serialize<u8, #allocator>>::SlicesTyped, ::sdave_codec::Error> {
            let __sdave_signature =
                ::sdave_codec::write_type_signature::<#policy, Self, _>(buffer, name)?;
            let __sdave_inner = unsafe {
                <Self as ::sdave_codec::Serialize<u8, #allocator>>::serialize::<#policy>(
                    self,
                    buffer,
                    variant,
                    strategy,
                    limiter_pairs,
                )?
            };
            let mut __sdave_slices: <Self as ::sdave_codec::Serialize<u8, #allocator>>::SlicesTyped =
                ::core::array::from_fn(|_| ::core::option::Option::None);
            unsafe {
                ::sdave_codec::frame_typed_envelop(
                    buffer,
                    __sdave_signature,
                    __sdave_inner,
                    variant,
                    strategy,
                    limiter_pairs,
                    &mut __sdave_slices,
                )?;
            }
            ::core::result::Result::Ok(__sdave_slices)
        }
    }
}

/// The statements that append one field's `Signature` NonEnvelop to `buffer` and
/// bind its `(head, tail)` offsets as `__sdave_signature`.
///
/// Both per-field loops below start from this, so a field signature is written in
/// exactly one place. A positional field writes no `Signature` at all (the caller skips
/// this helper), because its position is its identity.
fn field_signature(field: &FieldInfo, policy: &Ident) -> TokenStream2 {
    let ty = &field.ty;
    let prefix = &field.signature_prefix;
    quote! {
        let __sdave_signature_head = buffer.len();
        buffer.extend_from_slice(#prefix.as_bytes());
        <#ty as ::sdave_codec::TypeName>::render::<#policy>(
            &mut ::sdave_codec::BufferWriter(&mut *buffer),
        )
        .map_err(|_| ::sdave_codec::Error::UnexpectedItem)?;
        let __sdave_signature = (
            ::sdave_codec::Offset::new(__sdave_signature_head),
            ::sdave_codec::Offset::new(buffer.len()),
        );
    }
}

/// The per-field loop shared by the (non-flattened) `serialize` bodies: for each *encoded*
/// field (i.e. not `#[skip]`), optionally append its `Signature` NonEnvelop, serialize the
/// field value (one of `accesses`, i.e. `&self.field` or the matched binding), then copy the
/// field's logical slices right after. `__sdave_cursor` MUST be initialized by the caller.
///
/// `positional` selects the shape: a positional field (tuple struct / tuple variant) writes
/// **no** `Signature`, so only its value's slices are recorded. This is the *static* path — the
/// caller sizes the slice array from the fields' compile-time `SLICES`, so nothing is flattened.
pub fn field_serialize_loop(
    fields: &[FieldInfo],
    accesses: &[TokenStream2],
    allocator: &Ident,
    policy: &Ident,
    positional: bool,
) -> TokenStream2 {
    let bodies: Vec<TokenStream2> = fields
        .iter()
        .zip(accesses)
        .filter(|(field, _)| field.encoded())
        .map(|(field, access)| {
            let ty = &field.ty;
            let signature = if positional {
                quote!()
            } else {
                let signature = field_signature(field, policy);
                quote! {
                    #signature
                    __sdave_slices[__sdave_cursor] =
                        ::core::option::Option::Some(__sdave_signature);
                    __sdave_cursor += 1;
                }
            };
            quote! {
                {
                    #signature
                    let __sdave_field_slices = unsafe {
                        <#ty as ::sdave_codec::Serialize<u8, #allocator>>::serialize::<#policy>(
                            #access,
                            buffer,
                            variant,
                            strategy,
                            limiter_pairs,
                        )?
                    };
                    for __sdave_slice in __sdave_field_slices {
                        __sdave_slices[__sdave_cursor] = __sdave_slice;
                        __sdave_cursor += 1;
                    }
                }
            }
        })
        .collect();
    quote! {
        #(#bodies)*
    }
}

/// The `serialize` body of a struct: one value Envelop per field — preceded by its
/// `Signature` NonEnvelop unless the shape is positional — wrapped in one outer Envelop.
pub fn struct_serialize_body(
    fields: &[FieldInfo],
    allocator: &Ident,
    policy: &Ident,
    positional: bool,
) -> TokenStream2 {
    let accesses: Vec<_> = fields.iter().map(FieldInfo::struct_access).collect();
    let loop_body = field_serialize_loop(fields, &accesses, allocator, policy, positional);
    quote! {
        let __sdave_head = buffer.len();
        let mut __sdave_slices: <Self as ::sdave_codec::Serialize<u8, #allocator>>::Slices = ::core::array::from_fn(|_| ::core::option::Option::None);
        let mut __sdave_cursor = 1usize;
        #loop_body
        let (__sdave_limiter, __sdave_delimiter) = unsafe {
            ::sdave_codec::frame_payload(
                buffer,
                __sdave_head,
                &__sdave_slices,
                1..__sdave_cursor,
                variant,
                strategy,
                limiter_pairs,
            )?
        };
        __sdave_slices[0] = ::core::option::Option::Some(__sdave_limiter);
        __sdave_slices[__sdave_cursor] = ::core::option::Option::Some(__sdave_delimiter);
        ::core::result::Result::Ok(__sdave_slices)
    }
}

/// The `match self` arms of an enum's `serialize` body: one arm per variant,
/// introducing the variant's bindings and delegating the arm's statements to
/// `arm_body`, which is called with the variant's name literal and its parsed
/// fields.
///
/// # Errors
/// Returns the `#[default]`/`#[skip]` attribute errors of any variant's fields.
pub fn enum_serialize_arms(
    variants: &Variants,
    mut arm_body: impl FnMut(&LitStr, &[FieldInfo]) -> syn::Result<TokenStream2>,
) -> syn::Result<Vec<TokenStream2>> {
    variants
        .iter()
        .map(|variant| {
            let name = &variant.ident;
            let name_lit = LitStr::new(&name.to_string(), name.span());
            let fields = FieldInfo::collect(&variant.fields)?;
            let body = arm_body(&name_lit, &fields)?;
            Ok(match &variant.fields {
                Fields::Unit => quote! {
                    Self::#name => {
                        #body
                    }
                },
                Fields::Unnamed(_) => {
                    let bindings: Vec<_> = fields.iter().map(|field| &field.binding).collect();
                    quote! {
                        Self::#name(#(#bindings),*) => {
                            #body
                        }
                    }
                }
                Fields::Named(_) => {
                    // Bind through the generated `__sdave_field_*` name rather than
                    // the field's own ident: a `#[skip]` field is not read by the arm
                    // body, and an underscore-prefixed binding keeps that unused
                    // binding out of the `unused_variables` lint.
                    let patterns: Vec<_> = fields
                        .iter()
                        .map(|field| {
                            let field_name = field.ident.as_ref().expect("named field");
                            let binding = &field.binding;
                            quote!(#field_name: #binding)
                        })
                        .collect();
                    quote! {
                        Self::#name { #(#patterns),* } => {
                            #body
                        }
                    }
                }
            })
        })
        .collect()
}

/// The `serialize` statements of a fieldless enum variant — `V`, `V {}` or `V()`:
/// its name NonEnvelop as the outer Envelop's sole payload, framed in place.
pub fn unit_serialize_body(name_lit: &LitStr) -> TokenStream2 {
    quote! {
        let __sdave_name_head = buffer.len();
        buffer.extend_from_slice(#name_lit.as_bytes());
        let __sdave_name_tail = buffer.len();
        __sdave_slices[1] = ::core::option::Option::Some((
            ::sdave_codec::Offset::new(__sdave_name_head),
            ::sdave_codec::Offset::new(__sdave_name_tail),
        ));
        let (__sdave_limiter, __sdave_delimiter) = unsafe {
            ::sdave_codec::frame_payload(
                buffer,
                __sdave_head,
                &__sdave_slices,
                1..2,
                variant,
                strategy,
                limiter_pairs,
            )?
        };
        __sdave_slices[0] = ::core::option::Option::Some(__sdave_limiter);
        __sdave_slices[2] = ::core::option::Option::Some(__sdave_delimiter);
    }
}

/// The `serialize` body of an enum.
///
/// # Errors
/// Returns the `#[default]`/`#[skip]` attribute errors of any variant's fields.
pub fn enum_serialize_body(
    variants: &Variants,
    allocator: &Ident,
    policy: &Ident,
) -> syn::Result<TokenStream2> {
    let arms = enum_serialize_arms(variants, |name_lit, fields| {
        Ok(if fields.is_empty() {
            unit_serialize_body(name_lit)
        } else {
            let positional = fields.iter().any(|field| field.ident.is_none());
            data_variant_serialize_body(name_lit, fields, allocator, policy, positional)
        })
    })?;
    Ok(quote! {
        let __sdave_head = buffer.len();
        let mut __sdave_slices: <Self as ::sdave_codec::Serialize<u8, #allocator>>::Slices = ::core::array::from_fn(|_| ::core::option::Option::None);
        match self {
            #(#arms)*
        }
        ::core::result::Result::Ok(__sdave_slices)
    })
}

/// The body shared by tuple-like and struct-like enum variants: the variant name
/// NonEnvelop followed by an inner Envelop containing the fields, all wrapped in
/// the outer Envelop. A positional variant records its fields' slices without signature
/// slots (see [`field_serialize_loop`]), which is why it costs `5 + Σ SLICES` and no flatten.
pub fn data_variant_serialize_body(
    name_lit: &LitStr,
    fields: &[FieldInfo],
    allocator: &Ident,
    policy: &Ident,
    positional: bool,
) -> TokenStream2 {
    let values: Vec<_> = fields.iter().map(FieldInfo::enum_value).collect();
    let loop_body = field_serialize_loop(fields, &values, allocator, policy, positional);
    quote! {
        let __sdave_name_head = buffer.len();
        buffer.extend_from_slice(#name_lit.as_bytes());
        let __sdave_name_tail = buffer.len();
        __sdave_slices[1] = ::core::option::Option::Some((
            ::sdave_codec::Offset::new(__sdave_name_head),
            ::sdave_codec::Offset::new(__sdave_name_tail),
        ));
        let __sdave_inner_head = buffer.len();
        let mut __sdave_cursor = 3usize;
        #loop_body
        let (__sdave_inner_limiter, __sdave_inner_delimiter) = unsafe {
            ::sdave_codec::frame_payload(
                buffer,
                __sdave_inner_head,
                &__sdave_slices,
                3..__sdave_cursor,
                variant,
                strategy,
                limiter_pairs,
            )?
        };
        __sdave_slices[2] = ::core::option::Option::Some(__sdave_inner_limiter);
        __sdave_slices[__sdave_cursor] = ::core::option::Option::Some(__sdave_inner_delimiter);
        __sdave_cursor += 1;
        let (__sdave_limiter, __sdave_delimiter) = unsafe {
            ::sdave_codec::frame_payload(
                buffer,
                __sdave_head,
                &__sdave_slices,
                1..__sdave_cursor,
                variant,
                strategy,
                limiter_pairs,
            )?
        };
        __sdave_slices[0] = ::core::option::Option::Some(__sdave_limiter);
        __sdave_slices[__sdave_cursor] = ::core::option::Option::Some(__sdave_delimiter);
    }
}

/// The per-field statements of the flattened `serialize` bodies: for each *encoded* field,
/// serialize its value and immediately flatten its logical slices in place, so the region
/// becomes the field's bytes.
///
/// This is only for shapes whose slice count cannot be known statically — a type with
/// type/const parameters, whose fields' `SLICES` cannot be projected in a const context. A
/// monomorphic type (named or positional) uses the static array path
/// ([`field_serialize_loop`]) instead and never flattens.
///
/// `positional` selects the shape: a positional field (tuple struct / tuple variant) writes
/// no `Signature` at all, only the value Envelop.
pub fn field_flatten_loop(
    fields: &[FieldInfo],
    accesses: &[TokenStream2],
    allocator: &Ident,
    policy: &Ident,
    positional: bool,
) -> TokenStream2 {
    let bodies: Vec<TokenStream2> = fields
        .iter()
        .zip(accesses)
        .filter(|(field, _)| field.encoded())
        .map(|(field, access)| {
            let ty = &field.ty;
            let signature = if positional {
                quote!()
            } else {
                field_signature(field, policy)
            };
            let slices = if positional {
                quote!(__sdave_field)
            } else {
                quote!(
                    [::core::option::Option::Some(__sdave_signature)]
                        .into_iter()
                        .chain(__sdave_field)
                )
            };
            quote! {
                {
                    let __sdave_region = buffer.len();
                    #signature
                    let __sdave_field = unsafe {
                        <#ty as ::sdave_codec::Serialize<u8, #allocator>>::serialize::<#policy>(
                            #access,
                            buffer,
                            variant,
                            strategy,
                            limiter_pairs,
                        )?
                    };
                    unsafe {
                        ::sdave_codec::flatten_in_place(buffer, __sdave_region, #slices);
                    }
                }
            }
        })
        .collect();
    quote! {
        #(#bodies)*
    }
}

/// The flattened `serialize` body of a struct: one value per field (a `Signature` too, unless
/// the shape is positional), flattened in place, wrapped in one Envelop.
pub fn flattened_struct_serialize_body(
    fields: &[FieldInfo],
    allocator: &Ident,
    policy: &Ident,
    positional: bool,
) -> TokenStream2 {
    let accesses: Vec<_> = fields.iter().map(FieldInfo::struct_access).collect();
    let loop_body = field_flatten_loop(fields, &accesses, allocator, policy, positional);
    quote! {
        let __sdave_head = buffer.len();
        #loop_body
        unsafe {
            ::sdave_codec::frame_region(buffer, __sdave_head, variant, strategy, limiter_pairs)
        }
    }
}

/// The flattened `serialize` tail of a fieldless enum variant (`V`, `V {}` or
/// `V()`): its name NonEnvelop framed as one Envelop, returned directly.
pub fn flattened_unit_serialize_body(name_lit: &LitStr) -> TokenStream2 {
    quote! {
        buffer.extend_from_slice(#name_lit.as_bytes());
        unsafe {
            ::sdave_codec::frame_region(buffer, __sdave_head, variant, strategy, limiter_pairs)
        }
    }
}

/// The flattened `serialize` body of an enum.
///
/// # Errors
/// Returns the `#[default]`/`#[skip]` attribute errors of any variant's fields.
pub fn flattened_enum_serialize_body(
    variants: &Variants,
    allocator: &Ident,
    policy: &Ident,
) -> syn::Result<TokenStream2> {
    let arms = enum_serialize_arms(variants, |name_lit, fields| {
        Ok(if fields.is_empty() {
            flattened_unit_serialize_body(name_lit)
        } else {
            let positional = fields.iter().any(|field| field.ident.is_none());
            flattened_data_variant_serialize_body(name_lit, fields, allocator, policy, positional)
        })
    })?;
    Ok(quote! {
        let __sdave_head = buffer.len();
        match self {
            #(#arms)*
        }
    })
}

/// The flattened body of a tuple-like / struct-like enum variant: the variant
/// name NonEnvelop followed by an inner Envelop containing the flattened fields,
/// both flattened in place as the outer Envelop's payload.
pub fn flattened_data_variant_serialize_body(
    name_lit: &LitStr,
    fields: &[FieldInfo],
    allocator: &Ident,
    policy: &Ident,
    positional: bool,
) -> TokenStream2 {
    let values: Vec<_> = fields.iter().map(FieldInfo::enum_value).collect();
    let loop_body = field_flatten_loop(fields, &values, allocator, policy, positional);
    quote! {
        let __sdave_name_head = buffer.len();
        buffer.extend_from_slice(#name_lit.as_bytes());
        let __sdave_name = ::core::option::Option::Some((
            ::sdave_codec::Offset::new(__sdave_name_head),
            ::sdave_codec::Offset::new(buffer.len()),
        ));
        let __sdave_inner_head = buffer.len();
        #loop_body
        let [__sdave_inner_limiter, __sdave_inner_payload, __sdave_inner_delimiter] = unsafe {
            ::sdave_codec::frame_region(buffer, __sdave_inner_head, variant, strategy, limiter_pairs)?
        };
        unsafe {
            ::sdave_codec::flatten_in_place(
                buffer,
                __sdave_head,
                [
                    __sdave_name,
                    __sdave_inner_limiter,
                    __sdave_inner_payload,
                    __sdave_inner_delimiter,
                ]
                .into_iter(),
            );
        }
        unsafe {
            ::sdave_codec::frame_region(buffer, __sdave_head, variant, strategy, limiter_pairs)
        }
    }
}
