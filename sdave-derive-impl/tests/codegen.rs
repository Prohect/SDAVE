//! Codegen tests for `sdave-derive-impl`: the data helpers ([`FieldInfo`],
//! [`is_any_bound`], …) and the token streams the derive macros emit.
//!
//! Token streams are compared with [`compact`], which strips the whitespace
//! `TokenStream`'s `Display` inserts. That pins the *generator's output*; the generated
//! programs themselves are pinned at run time, in `sdave-codec/tests/derive.rs`.

extern crate alloc;

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, parse_quote, punctuated::Punctuated, token::Comma};

use sdave_derive_impl::*;

/// `tokens` rendered with every space removed, so an assertion does not depend on
/// `TokenStream`'s exact spacing.
fn compact(tokens: TokenStream2) -> String {
    tokens.to_string().replace(' ', "")
}

/// Parse `input` and return its struct fields.
fn struct_fields(input: &DeriveInput) -> syn::Fields {
    let Data::Struct(data) = &input.data else {
        panic!("expected a struct");
    };
    data.fields.clone()
}

fn field_infos(input: &DeriveInput) -> Vec<FieldInfo> {
    FieldInfo::collect(&struct_fields(input)).unwrap()
}

#[test]
fn identifiers_are_stable() {
    assert_eq!(allocator_ident().to_string(), "__SDAVE_A");
    assert_eq!(policy_ident().to_string(), "__SDAVE_N");
}

#[test]
fn slices_type_is_an_option_array() {
    assert_eq!(
        compact(slices_type(&quote!(3))),
        "[::core::option::Option<(::sdave_codec::Offset,::sdave_codec::Offset)>;3]",
    );
}

#[test]
fn any_bound_is_detected() {
    let any: syn::TypeParamBound = parse_quote!(Any);
    let core_any: syn::TypeParamBound = parse_quote!(::core::any::Any);
    let send: syn::TypeParamBound = parse_quote!(Send);
    let lifetime: syn::TypeParamBound = parse_quote!('static);
    assert!(is_any_bound(&any));
    assert!(is_any_bound(&core_any));
    assert!(!is_any_bound(&send));
    assert!(!is_any_bound(&lifetime));
}

#[test]
fn unions_are_rejected() {
    let input: DeriveInput = parse_quote!(union U { a: u8 });
    let Data::Union(data) = &input.data else {
        panic!("expected a union");
    };
    assert!(
        union_error(data)
            .to_string()
            .contains("does not support unions")
    );
    assert!(expand_serialize(&input).is_err());
    assert!(expand_deserialize(&input).is_err());
}

#[test]
fn field_info_collects_named_fields() {
    let infos = field_infos(&parse_quote!(
        struct S {
            x: u32,
            y: bool,
        }
    ));
    assert_eq!(infos.len(), 2);
    assert_eq!(infos[0].ident.as_ref().unwrap(), "x");
    assert_eq!(infos[0].name_bytes.value(), "x");
    assert_eq!(infos[0].signature_prefix.value(), "x: ");
    assert_eq!(infos[0].index.index, 0);
    assert_eq!(infos[0].binding.to_string(), "__sdave_field_0");
    assert_eq!(infos[1].index.index, 1);
}

#[test]
fn field_info_collects_tuple_fields() {
    let infos = field_infos(&parse_quote!(
        struct S(u32, bool);
    ));
    assert_eq!(infos.len(), 2);
    assert!(infos[0].ident.is_none());
    assert_eq!(infos[0].name_bytes.value(), "");
    // A positional field writes no `Signature` of its own: the position is the identity,
    // and the type text that names the composite as a whole is `TypeName`'s job.
    assert_eq!(infos[0].signature_prefix.value(), "");
    assert_eq!(infos[0].index.index, 0);
}

#[test]
fn field_info_collects_unit_structs() {
    assert!(
        field_infos(&parse_quote!(
            struct S;
        ))
        .is_empty()
    );
}

#[test]
fn struct_slices_counts_every_field() {
    let infos = field_infos(&parse_quote!(
        struct S {
            x: u32,
            y: bool,
        }
    ));
    let tokens = compact(struct_slices(&infos));
    assert!(tokens.starts_with("2usize"), "{tokens}");
    // One `<Ty as Serialize>::SLICES` per field.
    assert_eq!(tokens.matches("::SLICES").count(), 2);
}

#[test]
fn enum_slices_takes_the_maximum() {
    let input: DeriveInput = parse_quote!(
        enum E {
            A,
            B(u32, u32),
        }
    );
    let Data::Enum(data) = &input.data else {
        panic!("expected an enum");
    };
    let tokens = compact(enum_slices(&data.variants).unwrap());
    assert!(tokens.contains("__sdave_max"), "{tokens}");
    // The unit variant alone is a 3-slice Envelop; the positional one records one value
    // Envelop per field and no signature, so it projects each field's `SLICES`.
    assert!(tokens.contains("3usize"), "{tokens}");
    assert_eq!(tokens.matches("::SLICES").count(), 2, "{tokens}");
}

#[test]
fn type_name_impl_composes_type_parameters() {
    let input: DeriveInput = parse_quote!(
        struct G<T> {
            value: T,
        }
    );
    let tokens = compact(type_name_impl(&input, &policy_ident()));
    assert!(
        tokens.contains("::sdave_codec::TypeNameforG<T>"),
        "{tokens}"
    );
    assert!(
        tokens.contains("<Tas::sdave_codec::TypeName>::render"),
        "{tokens}"
    );
}

#[test]
fn self_type_carries_generic_arguments() {
    let input: DeriveInput = parse_quote!(
        struct G<'a, T, const N: usize> {
            value: &'a T,
        }
    );
    assert_eq!(
        compact(self_type(&input.ident, &input.generics)),
        "G<'a,T,N>"
    );
}

#[test]
fn monomorphic_struct_sizes_its_slices() {
    let input: DeriveInput = parse_quote!(
        struct S {
            x: u32,
        }
    );
    let tokens = compact(expand_serialize(&input).unwrap());
    assert!(tokens.contains("2usize+1+"), "{tokens}");
    assert!(
        tokens.contains("::sdave_codec::Serialize<u8,__SDAVE_A>forS"),
        "{tokens}"
    );
    assert!(tokens.contains("fnserialize_typed"), "{tokens}");
}

#[test]
fn generic_struct_takes_the_flattened_path() {
    let input: DeriveInput = parse_quote!(
        struct G<T> {
            value: T,
        }
    );
    let tokens = compact(expand_serialize(&input).unwrap());
    assert!(tokens.contains("constSLICES:usize=3usize"), "{tokens}");
    assert!(tokens.contains("frame_region"), "{tokens}");
}

#[test]
fn monomorphic_tuple_struct_never_flattens() {
    // A positional struct knows its slice count statically (no signature slots), so `serialize`
    // records its fields' slices in one array and frames that array: no in-place flatten.
    let input: DeriveInput = parse_quote!(
        struct P(u32, bool);
    );
    let tokens = compact(expand_serialize(&input).unwrap());
    // The `SLICES` const is `2 + Σ SLICES`, two fields and no `1 +` signature slots. (The
    // same expression is repeated for `Slices`/`SlicesTyped`, hence the narrow slice.)
    let (_, after) = tokens
        .split_once("constSLICES:usize=")
        .expect("a SLICES const");
    let (slices, _) = after.split_once(';').expect("a complete SLICES statement");
    assert!(slices.starts_with("(2usize+<u32as"), "{slices}");
    assert_eq!(slices.matches("::SLICES").count(), 2, "{slices}");
    assert!(tokens.contains("frame_payload"), "{tokens}");
    assert!(!tokens.contains("flatten_in_place"), "{tokens}");
}

#[test]
fn generic_tuple_struct_takes_the_flattened_path() {
    // The one shape that still has to flatten: a type parameter's `SLICES` is not a constant.
    let input: DeriveInput = parse_quote!(
        struct G<T>(T);
    );
    let tokens = compact(expand_serialize(&input).unwrap());
    assert!(tokens.contains("constSLICES:usize=3usize"), "{tokens}");
    assert!(tokens.contains("flatten_in_place"), "{tokens}");
}

#[test]
fn deserialize_impl_is_emitted() {
    let input: DeriveInput = parse_quote!(
        struct S {
            x: u32,
        }
    );
    let tokens = compact(expand_deserialize(&input).unwrap());
    assert!(
        tokens.contains("::sdave_codec::Deserialize<u8,__SDAVE_A>forS"),
        "{tokens}"
    );
}

#[test]
fn parse_fields_matches_named_by_signature_and_tuple_by_position() {
    let named = field_infos(&parse_quote!(
        struct S {
            x: u32,
        }
    ));
    let named = compact(parse_fields(
        &named,
        false,
        &allocator_ident(),
        &policy_ident(),
        &quote!(__sdave_deserializer),
    ));
    assert!(named.contains("named_signature_matches"), "{named}");

    let tuple = field_infos(&parse_quote!(
        struct S(u32, bool);
    ));
    let tuple = compact(parse_fields(
        &tuple,
        true,
        &allocator_ident(),
        &policy_ident(),
        &quote!(__sdave_deserializer),
    ));
    // No `(T)` signatures at all: the k-th value Envelop is the k-th field.
    assert!(!tuple.contains("named_signature_matches"), "{tuple}");
    assert!(tuple.contains("__sdave_items[0]"), "{tuple}");
    assert!(tuple.contains("__sdave_items[1]"), "{tuple}");
}

#[test]
fn constructor_builds_named_and_tuple_values() {
    let named = field_infos(&parse_quote!(
        struct S {
            x: u32,
        }
    ));
    let named = compact(constructor(&quote!(Self), &named, true));
    assert!(named.starts_with("Self{x:__sdave_field_0.ok_or"), "{named}");

    let tuple = field_infos(&parse_quote!(
        struct S(u32, bool);
    ));
    let tuple = compact(constructor(&quote!(Self), &tuple, false));
    assert!(tuple.starts_with("Self(__sdave_field_0.ok_or"), "{tuple}");
}

#[test]
fn sdave_dyn_rejects_empty_and_generic_traits() {
    let item: syn::ItemTrait = parse_quote!(
        trait Tagged {}
    );
    assert!(expand_sdave_dyn(&Punctuated::new(), item.clone()).is_err());

    let generic: syn::ItemTrait = parse_quote!(
        trait Tagged<X> {}
    );
    let one: Punctuated<syn::Type, Comma> = parse_quote!(u32);
    assert!(expand_sdave_dyn(&one, generic).is_err());
}

#[test]
fn sdave_dyn_seals_the_trait() {
    let item: syn::ItemTrait = parse_quote!(
        trait Tagged {}
    );
    let types: Punctuated<syn::Type, Comma> = parse_quote!(u32);
    let tokens = compact(expand_sdave_dyn(&types, item).unwrap());
    assert!(tokens.contains("__SdaveDynClaim_Tagged"), "{tokens}");
    assert!(tokens.contains("::core::any::Any"), "{tokens}");
}

// ---------------------------------------------------------------------------
// Fieldless shapes and generic defaults.
// ---------------------------------------------------------------------------

#[test]
fn fieldless_structs_construct_their_own_shape() {
    // `Self` only names a value for a unit struct; a braced or tuple one needs its
    // own braces/parens, or the generated body does not type-check.
    let unit: DeriveInput = parse_quote!(
        struct U;
    );
    assert!(
        compact(expand_deserialize(&unit).unwrap()).contains("Ok(Self)"),
        "unit struct",
    );

    let braced: DeriveInput = parse_quote!(
        struct B {}
    );
    assert!(
        compact(expand_deserialize(&braced).unwrap()).contains("Ok(Self{})"),
        "braced struct",
    );

    let tuple: DeriveInput = parse_quote!(
        struct T();
    );
    assert!(
        compact(expand_deserialize(&tuple).unwrap()).contains("Ok(Self())"),
        "tuple struct",
    );
}

#[test]
fn fieldless_variants_are_name_only_envelops() {
    let input: DeriveInput = parse_quote!(
        enum E {
            Braced {},
            Tuple(),
        }
    );
    let Data::Enum(data) = &input.data else {
        panic!("expected an enum");
    };

    // `serialize` writes a name-only Envelop for both, so neither contributes the
    // 3 slices of an inner Envelop to the count.
    let sizes = compact(enum_slices(&data.variants).unwrap());
    assert!(!sizes.contains("::SLICES"), "{sizes}");
    assert!(sizes.contains("3usize"), "{sizes}");

    // …and `deserialize` must not look for an inner Envelop that is not there.
    let tokens = compact(expand_deserialize(&input).unwrap());
    assert!(tokens.contains("Ok(Self::Braced{})"), "{tokens}");
    assert!(tokens.contains("Ok(Self::Tuple())"), "{tokens}");
    assert!(!tokens.contains("__sdave_items.get(1)"), "{tokens}");
}

#[test]
fn named_enum_arms_bind_through_the_generated_binding() {
    // A `#[skip]` named field is not read by the arm body; binding through the
    // underscore-prefixed `__sdave_field_*` name keeps that from tripping
    // `unused_variables` in the annotated crate.
    let input: DeriveInput = parse_quote!(
        enum E {
            V {
                x: u32,
                #[skip(0)]
                y: u32,
            },
        }
    );
    let tokens = compact(expand_serialize(&input).unwrap());
    assert!(
        tokens.contains("Self::V{x:__sdave_field_0,y:__sdave_field_1}"),
        "{tokens}",
    );
}

#[test]
fn generic_defaults_are_stripped_from_impl_headers() {
    // A default is legal on the type definition but rejected on an `impl` header.
    let type_default: DeriveInput = parse_quote!(
        struct G<T = u8> {
            value: T,
        }
    );
    let tokens = compact(expand_serialize(&type_default).unwrap());
    assert!(!tokens.contains("T=u8"), "{tokens}");
    assert!(tokens.contains("TypeNameforG<T>"), "{tokens}");

    let const_default: DeriveInput = parse_quote!(
        struct C<const N: usize = 3> {
            items: [u8; N],
        }
    );
    let tokens = compact(expand_deserialize(&const_default).unwrap());
    assert!(!tokens.contains("N:usize=3"), "{tokens}");
    assert!(tokens.contains("impl<constN:usize,"), "{tokens}");
}

// ---------------------------------------------------------------------------
// The `#[default(..)]` / `#[skip(..)]` field attributes.
// ---------------------------------------------------------------------------

#[test]
fn field_info_reads_the_optional_attributes() {
    let infos = field_infos(&parse_quote!(
        struct S {
            plain: u32,
            #[default(8080)]
            port: u32,
            #[skip(String::new())]
            cache: String,
        }
    ));

    // A plain field is mandatory and on the wire.
    assert!(infos[0].encoded());
    assert!(infos[0].fallback().is_none());

    // `#[default]` keeps the field on the wire and adds a fallback.
    assert!(infos[1].encoded());
    let port_fallback = infos[1].fallback().unwrap();
    assert_eq!(compact(quote!(#port_fallback)), "8080");

    // `#[skip]` takes the field off the wire and keeps the fallback.
    assert!(!infos[2].encoded());
    let cache_fallback = infos[2].fallback().unwrap();
    assert_eq!(compact(quote!(#cache_fallback)), "String::new()");
    assert!(infos[2].default.is_none());
}

#[test]
fn optional_attributes_must_be_well_formed() {
    // An argument is mandatory: it is the field's value.
    let bare: DeriveInput = parse_quote!(
        struct S {
            #[default]
            x: u32,
        }
    );
    assert!(expand_serialize(&bare).is_err());
    assert!(expand_deserialize(&bare).is_err());

    // Neither attribute may be repeated.
    let twice: DeriveInput = parse_quote!(
        struct S {
            #[skip(1)]
            #[skip(2)]
            x: u32,
        }
    );
    assert!(expand_serialize(&twice).is_err());

    let twice_default: DeriveInput = parse_quote!(
        struct S {
            #[default(1)]
            #[default(2)]
            x: u32,
        }
    );
    assert!(expand_deserialize(&twice_default).is_err());
}

#[test]
fn a_field_may_carry_both_attributes() {
    // `#[skip]` is the write side and `#[default]` the read side, so one field may
    // carry both; `#[default]` then supplies the fallback.
    let input: DeriveInput = parse_quote!(
        struct S {
            #[skip(1)]
            #[default(2)]
            x: u32,
        }
    );
    assert!(expand_serialize(&input).is_ok());
    assert!(expand_deserialize(&input).is_ok());

    let infos = field_infos(&input);
    assert!(!infos[0].encoded(), "`#[skip]` still takes it off the wire");
    let fallback = infos[0].fallback().unwrap();
    assert_eq!(
        compact(quote!(#fallback)),
        "2",
        "`#[default]` wins as the read fallback",
    );
}

#[test]
fn a_positional_field_rejects_default() {
    // `#[default]` on a positional field would be ambiguous: a short document cannot say
    // *which* position is missing, so the derive refuses it — on both halves of the codec,
    // because both go through `FieldInfo::collect`.
    let input: DeriveInput = parse_quote!(
        struct S(u32, #[default(7)] u32);
    );
    for result in [
        expand_serialize(&input).map(|_| ()),
        expand_deserialize(&input).map(|_| ()),
    ] {
        let error = match result {
            Ok(()) => panic!("a positional `#[default]` MUST be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("#[default]"), "{error}");
    }
}

#[test]
fn a_positional_field_may_be_skipped() {
    // Removing a position is well defined (the remaining fields keep their order), so
    // `#[skip]` stays legal on a positional field — any number of times.
    let infos = field_infos(&parse_quote!(
        struct S(u32, #[skip(9)] u32, u32);
    ));
    let tokens = compact(constructor(&quote!(Self), &infos, false));
    assert!(
        tokens.contains("__sdave_field_1.unwrap_or_else(||9)"),
        "{tokens}"
    );

    // The skipped slot is never assigned, so its binding is declared without
    // `mut` — otherwise the generated code would trip `unused_mut`.
    let tokens = compact(parse_fields(
        &infos,
        true,
        &allocator_ident(),
        &policy_ident(),
        &quote!(__sdave_deserializer),
    ));
    assert!(
        tokens.contains("letmut__sdave_field_0:"),
        "the encoded slot stays mutable: {tokens}"
    );
    assert!(
        tokens.contains("let__sdave_field_1:"),
        "the skipped slot does not: {tokens}"
    );
}

#[test]
fn skipped_fields_do_not_count_toward_slices() {
    let infos = field_infos(&parse_quote!(
        struct S {
            x: u32,
            #[skip(0)]
            y: u32,
        }
    ));
    let tokens = compact(struct_slices(&infos));
    assert_eq!(tokens.matches("::SLICES").count(), 1, "{tokens}");
}

#[test]
fn skipped_fields_are_not_serialized() {
    let input: DeriveInput = parse_quote!(
        struct S {
            x: u32,
            #[skip(0)]
            y: u32,
        }
    );
    // `compact` strips the spaces, so the signature literals read `"x:"`/`"y:"`.
    let tokens = compact(expand_serialize(&input).unwrap());
    assert!(tokens.contains("\"x:\""), "{tokens}");
    assert!(!tokens.contains("\"y:\""), "{tokens}");
}

#[test]
fn a_missing_field_takes_its_fallback_in_the_constructor() {
    let infos = field_infos(&parse_quote!(
        struct S {
            id: u32,
            #[default(8080)]
            port: u32,
            #[skip(0)]
            secret: u32,
        }
    ));
    let tokens = compact(constructor(&quote!(Self), &infos, true));
    // The mandatory field still errors when absent.
    assert!(tokens.contains("id:__sdave_field_0.ok_or("), "{tokens}");
    // Both attribute kinds fall back instead.
    assert!(
        tokens.contains("port:__sdave_field_1.unwrap_or_else(||8080)"),
        "{tokens}"
    );
    assert!(
        tokens.contains("secret:__sdave_field_2.unwrap_or_else(||0)"),
        "{tokens}"
    );
}

#[test]
fn a_skipped_field_is_matched_by_neither_shape() {
    let infos = field_infos(&parse_quote!(
        struct S {
            x: u32,
            #[skip(0)]
            y: u32,
        }
    ));
    // The attribute is local to its field: `y` is off the codec, so only `x` is
    // matched — nothing reaches into `y`'s type.
    let named = compact(parse_fields(
        &infos,
        false,
        &allocator_ident(),
        &policy_ident(),
        &quote!(__sdave_deserializer),
    ));
    assert_eq!(
        named.matches("named_signature_matches").count(),
        1,
        "{named}"
    );
    assert!(named.contains("\"x\".as_bytes()"), "{named}");
    assert!(!named.contains("\"y\".as_bytes()"), "{named}");

    let tuple = compact(parse_fields(
        &infos,
        true,
        &allocator_ident(),
        &policy_ident(),
        &quote!(__sdave_deserializer),
    ));
    assert!(tuple.contains("__sdave_items[0]"), "{tuple}");
    assert!(!tuple.contains("__sdave_items[1]"), "{tuple}");
}

#[test]
fn a_skipped_fields_type_needs_no_codec_bounds() {
    // `NoCodec` implements none of the codec traits, so any bound on it would make
    // the generated impl unsound to compile.
    let input: DeriveInput = parse_quote!(
        struct S {
            id: u32,
            #[skip(NoCodec)]
            opaque: NoCodec,
        }
    );

    let serialize = compact(expand_serialize(&input).unwrap());
    assert!(!serialize.contains("NoCodec:"), "{serialize}");
    assert!(!serialize.contains("NoCodec"), "{serialize}");

    let deserialize = compact(expand_deserialize(&input).unwrap());
    assert!(!deserialize.contains("NoCodec:"), "{deserialize}");
    // …but it is still named, so the constructor can build it from `#[skip]`.
    assert!(deserialize.contains("Option<NoCodec>"), "{deserialize}");
}

#[test]
fn skipped_tuple_fields_do_not_consume_a_wire_position() {
    let infos = field_infos(&parse_quote!(
        struct S(u32, #[skip(0u32)] u32, u32);
    ));
    let tokens = compact(parse_fields(
        &infos,
        true,
        &allocator_ident(),
        &policy_ident(),
        &quote!(__sdave_deserializer),
    ));
    // Only the two encoded fields take a position: 0 and 1, never 2.
    assert!(tokens.contains("__sdave_items[0]"), "{tokens}");
    assert!(tokens.contains("__sdave_items[1]"), "{tokens}");
    assert!(!tokens.contains("__sdave_items[2]"), "{tokens}");
    // The skipped field keeps its (unfilled) declaration for the constructor, but
    // without `mut`: nothing can assign to it, so a `mut` would trip `unused_mut`
    // in the annotated crate.
    assert!(
        tokens.contains("let__sdave_field_1:::core::option::Option<u32>"),
        "{tokens}"
    );
    assert!(
        tokens.contains("letmut__sdave_field_0:"),
        "an encoded slot stays mutable: {tokens}"
    );
}
