//! The SDAVE macro crate: `#[derive(Serialize)]`, `#[derive(Deserialize)]`, and the `dyn` pair.
//!
//! This crate is **host-only**: it depends on `syn` / `quote` / `proc-macro2` and on nothing of ours,
//! so it never enters a runtime build graph. Every expansion names the runtime through exactly one
//! path — `::sdave_codec` by default, redirectable with `#[sdave(crate = …)]` — and emits **no**
//! `Vec`, `String`, `Box` or `format!` of its own.
//!
//! # The derives
//!
//! The derive only maps a *declaration* to a shape: it emits `SHAPE` and `DEPTH`, announces the
//! fields to the codec (which owns their order, their headers and their markers), and reads them
//! back by name in any order. It never composes a header, compares a marker or touches a leaf byte.
//!
//! ```
//! use sdave_macro::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct ToolCall {
//!     name: u8,
//!     #[sdave(default)]
//!     timeout_ms: Option<u64>,
//! }
//! ```
//!
//! # The `dyn` pair
//!
//! ```ignore
//! #[sdave_macro::dyn_payload]
//! trait ToolInput: sdave_codec::DynSerialize + core::fmt::Debug + 'static {
//!     fn as_any(&self) -> &dyn core::any::Any;
//!     fn eq_dyn(&self, other: &dyn ToolInput) -> bool;
//! }
//!
//! #[derive(sdave_macro::DynPayload)]
//! #[dyn_payload_impl(dyn_trait = ToolInput)]
//! struct ReadFileToolInput { path: u8 }
//! ```
//!
//! The trait-side macro emits the trait unchanged plus the one `impl Serialize for dyn ToolInput`
//! the codec requires; the type-side derive emits the concrete type's type-erased `DynSerialize`
//! delegation — the concrete type's **marked** document in the value bytes — and its own `as_any` /
//! `eq_dyn`.
//!
//! # Rejections
//!
//! Anything the codec does not specify is rejected at compile time rather than guessed at: an
//! unknown `sdave` attribute, `rename`, `skip`, `#[sdave(basic = …)]`, a field-level `basic`, a
//! container-level `default`, a `default` on a positional field, and a `union`.
//!
//! The snippets below only prove that the build *fails*; a typo would pass for a rejection. The
//! targeted messages are asserted message-for-message by this crate's own unit tests (`mod tests`),
//! which call the very expansion functions the derives call.
//!
//! ```compile_fail
//! use sdave_macro::Serialize;
//! #[derive(Serialize)]
//! #[sdave(rename = "other")]
//! struct S { a: u8 }
//! ```
//!
//! ```compile_fail
//! use sdave_macro::Serialize;
//! #[derive(Serialize)]
//! #[sdave(skip)]
//! struct S { a: u8 }
//! ```
//!
//! ```compile_fail
//! use sdave_macro::Serialize;
//! #[derive(Serialize)]
//! #[sdave(nonsense)]
//! struct S { a: u8 }
//! ```
//!
//! ```compile_fail
//! use sdave_macro::Serialize;
//! #[derive(Serialize)]
//! #[sdave(basic = something)]
//! struct S { a: u8 }
//! ```
//!
//! ```compile_fail
//! use sdave_macro::Serialize;
//! #[derive(Serialize)]
//! union U { a: u8, b: u8 }
//! ```
//!
//! ```compile_fail
//! use sdave_macro::Serialize;
//! #[derive(Serialize)]
//! struct S(#[sdave(default)] u8);
//! ```
//!
//! ```compile_fail
//! use sdave_macro::Serialize;
//! #[derive(Serialize)]
//! #[sdave(default)]
//! struct S { a: u8 }
//! ```
//!
//! ```compile_fail
//! use sdave_macro::Deserialize;
//! #[derive(Deserialize)]
//! enum E { V(#[sdave(default)] u8) }
//! ```
//!
//! ```compile_fail
//! #[sdave_macro::dyn_payload]
//! trait NotDyn: core::fmt::Debug + 'static {
//!     fn as_any(&self) -> &dyn core::any::Any;
//! }
//! ```

extern crate proc_macro;

use std::collections::HashSet;

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2, TokenTree};
use quote::{ToTokens, quote};
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Fields, FieldsNamed, GenericParam, Ident,
    Lifetime, LifetimeParam, Path, Type, parse_macro_input, parse_quote,
};

// ---------------------------------------------------------------------------
// entry points
// ---------------------------------------------------------------------------

/// Derive the codec's `Serialize`.
#[proc_macro_derive(Serialize, attributes(sdave))]
pub fn derive_serialize(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_serialize(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the codec's `Deserialize`.
#[proc_macro_derive(Deserialize, attributes(sdave))]
pub fn derive_deserialize(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_deserialize(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Pair a hand-written object trait with the codec's `dyn` machinery.
#[proc_macro_attribute]
pub fn dyn_payload(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    let item = TokenStream2::from(item);
    expand_dyn_payload(attr, item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the concrete implementor's half of the `dyn` pair.
#[proc_macro_derive(DynPayload, attributes(sdave, dyn_payload_impl))]
pub fn derive_dyn_payload(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_dyn_payload_impl(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

// ---------------------------------------------------------------------------
// attribute parsing
// ---------------------------------------------------------------------------

/// A container's `#[sdave(...)]` configuration.
struct Container {
    krate: Option<Path>,
    basic: bool,
}

fn container_cfg(attrs: &[Attribute]) -> syn::Result<Container> {
    let mut out = Container {
        krate: None,
        basic: false,
    };
    for attr in attrs {
        if !attr.path().is_ident("sdave") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                out.krate = Some(meta.value()?.parse::<Path>()?);
                Ok(())
            } else if meta.path.is_ident("basic") {
                if meta.input.peek(syn::Token![=]) {
                    return Err(meta.error("`#[sdave(basic)]` does not take a value"));
                }
                out.basic = true;
                Ok(())
            } else if meta.path.is_ident("rename") {
                Err(meta.error(
                    "`#[sdave(rename)]` is not supported: a wire name is the Rust source name",
                ))
            } else if meta.path.is_ident("skip") {
                Err(meta.error(
                    "`#[sdave(skip)]` is not supported: serialization writes every field",
                ))
            } else if meta.path.is_ident("default") {
                Err(meta.error(
                    "`#[sdave(default)]` applies to a named field, not to a container",
                ))
            } else {
                Err(meta.error("unknown `sdave` attribute"))
            }
        })?;
    }
    Ok(out)
}

/// A field's default provider.
enum Default {
    /// `#[sdave(default)]`: `<FieldType as Default>::default()`.
    Type,
    /// `#[sdave(default = path)]`: `path()`.
    Path(Path),
}

/// A field's `#[sdave(...)]` configuration.
struct FieldCfg {
    default: Option<Default>,
}

fn field_cfg(attrs: &[Attribute]) -> syn::Result<FieldCfg> {
    let mut out = FieldCfg { default: None };
    for attr in attrs {
        if !attr.path().is_ident("sdave") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default") {
                if meta.input.peek(syn::Token![=]) {
                    out.default = Some(Default::Path(meta.value()?.parse::<Path>()?));
                } else {
                    out.default = Some(Default::Type);
                }
                Ok(())
            } else if meta.path.is_ident("crate") {
                Err(meta.error("`#[sdave(crate = ...)]` applies to a container"))
            } else if meta.path.is_ident("basic") {
                Err(meta.error("`#[sdave(basic)]` applies to a type, not to a field"))
            } else if meta.path.is_ident("rename") {
                Err(meta.error(
                    "`#[sdave(rename)]` is not supported: a wire name is the Rust source name",
                ))
            } else if meta.path.is_ident("skip") {
                Err(meta.error(
                    "`#[sdave(skip)]` is not supported: serialization writes every field",
                ))
            } else {
                Err(meta.error("unknown `sdave` attribute"))
            }
        })?;
    }
    Ok(out)
}

/// Reject every field-level `#[sdave(...)]` a field's position cannot carry.
fn validate_fields(data: &Data) -> syn::Result<()> {
    let check = |fields: &Fields| -> syn::Result<()> {
        let positional = matches!(fields, Fields::Unnamed(_));
        for field in fields.iter() {
            let cfg = field_cfg(&field.attrs)?;
            if positional && cfg.default.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "`#[sdave(default)]` applies to a named field only",
                ));
            }
        }
        Ok(())
    };
    match data {
        Data::Struct(data) => check(&data.fields)?,
        Data::Enum(data) => {
            for variant in &data.variants {
                check(&variant.fields)?;
            }
        }
        Data::Union(_) => {}
    }
    Ok(())
}

/// The wire spelling of a name, whether or not it was written as a raw identifier.
fn field_name(ident: &Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").map(str::to_owned).unwrap_or(text)
}

/// The codec path an expansion names.
fn crate_path(cfg: &Container) -> Path {
    cfg.krate
        .clone()
        .unwrap_or_else(|| parse_quote!(::sdave_codec))
}

// ---------------------------------------------------------------------------
// generics
// ---------------------------------------------------------------------------

/// Every generic parameter's own name (types, lifetimes and consts alike).
fn param_names(generics: &syn::Generics) -> HashSet<String> {
    let mut names = HashSet::new();
    for param in &generics.params {
        match param {
            GenericParam::Type(ty) => {
                names.insert(ty.ident.to_string());
            }
            GenericParam::Lifetime(lt) => {
                names.insert(lt.lifetime.ident.to_string());
            }
            GenericParam::Const(c) => {
                names.insert(c.ident.to_string());
            }
        }
    }
    names
}

/// Every lifetime parameter.
fn lifetimes(generics: &syn::Generics) -> Vec<Lifetime> {
    generics
        .lifetimes()
        .map(|lt| lt.lifetime.clone())
        .collect()
}

/// Whether a type's spelling mentions any of `names` (walking nested token groups).
fn mentions(ty: &Type, names: &HashSet<String>) -> bool {
    fn walk(stream: TokenStream2, names: &HashSet<String>) -> bool {
        for tree in stream {
            let hit = match tree {
                TokenTree::Ident(ident) => names.contains(&ident.to_string()),
                TokenTree::Group(group) => walk(group.stream(), names),
                _ => false,
            };
            if hit {
                return true;
            }
        }
        false
    }
    walk(ty.to_token_stream(), names)
}

/// Whether a field's type spells `Self` or the container's own name: a recursive field.
fn mentions_self(ty: &Type, container: &Ident) -> bool {
    let mut names = HashSet::new();
    names.insert("Self".to_owned());
    names.insert(container.to_string());
    mentions(ty, &names)
}

/// Every field type the container declares, struct fields or all variant fields alike.
fn field_types(data: &Data) -> Vec<&Type> {
    let mut out = Vec::new();
    match data {
        Data::Struct(data) => {
            for field in &data.fields {
                out.push(&field.ty);
            }
        }
        Data::Enum(data) => {
            for variant in &data.variants {
                for field in &variant.fields {
                    out.push(&field.ty);
                }
            }
        }
        Data::Union(_) => {}
    }
    out
}

/// The wire shape a declaration maps to.
fn shape_of(data: &Data) -> TokenStream2 {
    match data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(_) | Fields::Unit => quote!(Record),
            Fields::Unnamed(_) => quote!(Tuple),
        },
        Data::Enum(_) | Data::Union(_) => quote!(Enum),
    }
}

/// Reject a union before anything else looks at it.
fn reject_union(data: &Data, ident: &Ident) -> syn::Result<()> {
    if matches!(data, Data::Union(_)) {
        return Err(syn::Error::new_spanned(
            ident,
            "sdave does not support unions",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `#[derive(Serialize)]`
// ---------------------------------------------------------------------------

fn expand_serialize(input: DeriveInput) -> syn::Result<TokenStream2> {
    let cfg = container_cfg(&input.attrs)?;
    let krate = crate_path(&cfg);
    let ident = input.ident;
    reject_union(&input.data, &ident)?;

    if cfg.basic {
        let (ig, tg, wc) = input.generics.split_for_impl();
        // The generated body returns `Result<_, sdave_codec::Error>`, and that error is deliberately
        // non-allocating, so the emitted impl carries the matching lint allowance: a derive must not
        // make its consumer's build noisy about the runtime's design.
        return Ok(quote! {
            #[allow(clippy::result_large_err)]
            impl #ig #krate::Serialize for #ident #tg #wc {
                const SHAPE: #krate::Shape = #krate::Shape::Tuple;
                const DEPTH: u16 = 0;

                fn append(&self, sink: &mut #krate::Sink<'_>) -> ::core::result::Result<(), #krate::Error> {
                    #krate::BasicType::append_basic(self, sink)
                }
            }
        });
    }

    validate_fields(&input.data)?;

    let shape = shape_of(&input.data);
    let children = field_types(&input.data);

    let depths: Vec<TokenStream2> = children
        .iter()
        .map(|ty| {
            if mentions_self(ty, &ident) {
                quote!(u16::MAX)
            } else {
                quote!(<#ty as #krate::Serialize>::DEPTH)
            }
        })
        .collect();

    let body = match &input.data {
        Data::Struct(data) => struct_append(data, &krate),
        Data::Enum(data) => enum_append(data, &krate),
        Data::Union(_) => unreachable!("rejected above"),
    };

    let mut generics = input.generics.clone();
    {
        let params = param_names(&input.generics);
        let mut predicates: Vec<syn::WherePredicate> = Vec::new();
        for ty in &children {
            if mentions_self(ty, &ident) {
                continue;
            }
            if mentions(ty, &params) {
                predicates.push(parse_quote!(#ty: #krate::Serialize));
            }
        }
        if !predicates.is_empty() {
            generics.make_where_clause().predicates.extend(predicates);
        }
    }
    let (ig, tg, wc) = generics.split_for_impl();

    // See the note on the `basic` branch: the expansion's `Result<_, sdave_codec::Error>` trips
    // clippy's size heuristic, so the emitted impl carries the allowance itself.
    Ok(quote! {
        #[allow(clippy::result_large_err)]
        impl #ig #krate::Serialize for #ident #tg #wc {
            const SHAPE: #krate::Shape = #krate::Shape::#shape;
            const DEPTH: u16 = #krate::depth_of(&[#(#depths),*]);

            fn append(&self, sink: &mut #krate::Sink<'_>) -> ::core::result::Result<(), #krate::Error> {
                #body
            }
        }
    })
}

fn struct_append(data: &DataStruct, krate: &Path) -> TokenStream2 {
    match &data.fields {
        Fields::Named(fields) => {
            let count = fields.named.len();
            let mut names = Vec::new();
            let mut types = Vec::new();
            let mut accessors = Vec::new();
            for field in &fields.named {
                let name = field.ident.clone().expect("a named field");
                names.push(syn::LitStr::new(&field_name(&name), name.span()));
                types.push(field.ty.clone());
                accessors.push(name);
            }
            quote! {
                let mut __sdave_slots = [#krate::FieldSlot::EMPTY; #count];
                sink.record(&mut __sdave_slots, |__sdave_record| {
                    #( __sdave_record.field(#names, ::core::any::type_name::<#types>(), &self.#accessors)?; )*
                    ::core::result::Result::Ok(())
                })
            }
        }
        Fields::Unnamed(fields) => {
            let indices: Vec<syn::Index> =
                (0..fields.unnamed.len()).map(syn::Index::from).collect();
            quote! {
                #( sink.item_run(&self.#indices)?; )*
                ::core::result::Result::Ok(())
            }
        }
        Fields::Unit => quote! {
            let mut __sdave_slots = [#krate::FieldSlot::EMPTY; 0];
            sink.record(&mut __sdave_slots, |_| ::core::result::Result::Ok(()))
        },
    }
}

fn enum_append(data: &DataEnum, krate: &Path) -> TokenStream2 {
    let mut arms = Vec::new();
    for variant in &data.variants {
        let vident = &variant.ident;
        let vname = syn::LitStr::new(&field_name(vident), vident.span());
        let arm = match &variant.fields {
            Fields::Unit => quote! {
                Self::#vident => sink.variant(#vname),
            },
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => quote! {
                Self::#vident(__sdave_value) => sink.variant_with(#vname, __sdave_value),
            },
            Fields::Unnamed(fields) => {
                let locals: Vec<Ident> = (0..fields.unnamed.len())
                    .map(|index| Ident::new(&format!("__sdave_value_{index}"), Span::call_site()))
                    .collect();
                quote! {
                    Self::#vident(#(#locals),*) => sink.variant_fields(#vname, |__sdave_sink| {
                        #( __sdave_sink.item_run(#locals)?; )*
                        ::core::result::Result::Ok(())
                    }),
                }
            }
            Fields::Named(fields) => {
                let count = fields.named.len();
                let mut names = Vec::new();
                let mut types = Vec::new();
                let mut bindings = Vec::new();
                for field in &fields.named {
                    let name = field.ident.clone().expect("a named field");
                    names.push(syn::LitStr::new(&field_name(&name), name.span()));
                    types.push(field.ty.clone());
                    bindings.push(name);
                }
                quote! {
                    Self::#vident { #(#bindings),* } => sink.variant_fields(#vname, |__sdave_sink| {
                        let mut __sdave_slots = [#krate::FieldSlot::EMPTY; #count];
                        __sdave_sink.record(&mut __sdave_slots, |__sdave_record| {
                            #( __sdave_record.field(#names, ::core::any::type_name::<#types>(), #bindings)?; )*
                            ::core::result::Result::Ok(())
                        })
                    }),
                }
            }
        };
        arms.push(arm);
    }
    quote! {
        match self {
            #(#arms)*
        }
    }
}

// ---------------------------------------------------------------------------
// `#[derive(Deserialize)]`
// ---------------------------------------------------------------------------

fn expand_deserialize(input: DeriveInput) -> syn::Result<TokenStream2> {
    let cfg = container_cfg(&input.attrs)?;
    let krate = crate_path(&cfg);
    let ident = input.ident;
    reject_union(&input.data, &ident)?;
    let de: Lifetime = parse_quote!('__sdave_de);

    // The fresh lifetime belongs to the *impl's* own generics. The deriving type keeps its own
    // argument list untouched, so `Point` stays `Point` and `Text<'a>` stays `Text<'a>` — never
    // `Point<'__sdave_de>`.
    let (_, ty_generics, _) = input.generics.split_for_impl();

    if cfg.basic {
        let mut generics = input.generics.clone();
        generics
            .params
            .insert(0, GenericParam::Lifetime(LifetimeParam::new(de.clone())));
        let (ig, _, wc) = generics.split_for_impl();
        // See the note in `expand_serialize`.
        return Ok(quote! {
            #[allow(clippy::result_large_err)]
            impl #ig #krate::Deserialize<#de> for #ident #ty_generics #wc {
                const SHAPE: #krate::Shape = #krate::Shape::Tuple;

                fn deserialize(source: &mut #krate::DecodeSource<#de, '_>) -> ::core::result::Result<Self, #krate::Error> {
                    <Self as #krate::BasicType>::read_basic(source)
                }
            }
        });
    }

    validate_fields(&input.data)?;

    let shape = shape_of(&input.data);
    let children = field_types(&input.data);

    let body = match &input.data {
        Data::Struct(data) => struct_deserialize(data, &krate, &de),
        Data::Enum(data) => enum_deserialize(data, &krate, &de),
        Data::Union(_) => unreachable!("rejected above"),
    };

    let mut generics = input.generics.clone();
    generics
        .params
        .insert(0, GenericParam::Lifetime(LifetimeParam::new(de.clone())));
    {
        let params = param_names(&input.generics);
        let declared = lifetimes(&input.generics);
        let mut predicates: Vec<syn::WherePredicate> = Vec::new();
        for lt in &declared {
            predicates.push(parse_quote!(#de: #lt));
        }
        for ty in &children {
            if mentions_self(ty, &ident) {
                continue;
            }
            if mentions(ty, &params) {
                predicates.push(parse_quote!(#ty: #krate::Deserialize<#de>));
            }
        }
        if !predicates.is_empty() {
            generics.make_where_clause().predicates.extend(predicates);
        }
    }
    let (ig, _, wc) = generics.split_for_impl();

    Ok(quote! {
        #[allow(clippy::result_large_err)]
        impl #ig #krate::Deserialize<#de> for #ident #ty_generics #wc {
            const SHAPE: #krate::Shape = #krate::Shape::#shape;

            fn deserialize(source: &mut #krate::DecodeSource<#de, '_>) -> ::core::result::Result<Self, #krate::Error> {
                #body
            }
        }
    })
}

fn struct_deserialize(data: &DataStruct, krate: &Path, de: &Lifetime) -> TokenStream2 {
    match &data.fields {
        Fields::Named(fields) => {
            let (decls, values, loop_body) = named_fields(fields, krate, de);
            let idents: Vec<&Ident> = fields
                .named
                .iter()
                .map(|field| field.ident.as_ref().expect("a named field"))
                .collect();
            quote! {
                #decls
                #loop_body
                ::core::result::Result::Ok(Self { #( #idents: #values ),* })
            }
        }
        Fields::Unnamed(fields) => {
            let count = fields.unnamed.len();
            let locals: Vec<Ident> = (0..count)
                .map(|index| Ident::new(&format!("__sdave_value_{index}"), Span::call_site()))
                .collect();
            let types: Vec<&Type> = fields.unnamed.iter().map(|field| &field.ty).collect();
            let indices: Vec<syn::Index> = (0..count).map(syn::Index::from).collect();
            quote! {
                let __sdave_arity = source.item_count();
                if __sdave_arity != #count {
                    return ::core::result::Result::Err(#krate::Error::new(
                        #krate::ErrorKind::WrongArity { expected: #count, actual: __sdave_arity },
                    ));
                }
                #(
                    let #locals = source
                        .next_value::<#types>()
                        .map_err(|__sdave_error| __sdave_error.at_index(#indices))?;
                )*
                ::core::result::Result::Ok(Self(#(#locals),*))
            }
        }
        Fields::Unit => quote! {
            source.expect_empty_body()?;
            ::core::result::Result::Ok(Self)
        },
    }
}

/// The record loop a named-field body (or a named-field variant's body) runs.
///
/// Returns the per-field `Option` declarations, the identifiers holding the finished values, and the
/// loop itself. Defaults are applied *after* the loop, so a provider runs only once the record is
/// confirmed complete, and at most once.
fn named_fields(fields: &FieldsNamed, krate: &Path, _de: &Lifetime) -> (TokenStream2, Vec<Ident>, TokenStream2) {
    let mut declares = Vec::new();
    let mut matches = Vec::new();
    let mut values = Vec::new();
    let mut finishes = Vec::new();

    for (index, field) in fields.named.iter().enumerate() {
        let ident = field.ident.clone().expect("a named field");
        let name = syn::LitStr::new(&field_name(&ident), ident.span());
        let ty = field.ty.clone();
        let local = Ident::new(&format!("__sdave_field_{index}"), Span::call_site());
        let value = Ident::new(&format!("__sdave_value_{index}"), Span::call_site());

        declares.push(quote! {
            let mut #local: ::core::option::Option<#ty> = ::core::option::Option::None;
        });
        matches.push(quote! {
            if !__sdave_matched && __sdave_header.name_is(#name) {
                __sdave_matched = true;
                __sdave_header
                    .expect_marker(::core::any::type_name::<#ty>())
                    .map_err(|__sdave_error| __sdave_error.in_field(#name))?;
                if ::core::option::Option::is_some(&#local) {
                    return ::core::result::Result::Err(
                        #krate::Error::new(#krate::ErrorKind::DuplicateField).in_field(#name),
                    );
                }
                #local = ::core::option::Option::Some(
                    source
                        .field_value::<#ty>()
                        .map_err(|__sdave_error| __sdave_error.in_field(#name))?,
                );
            }
        });

        let cfg = field_cfg(&field.attrs).expect("validated before");
        match cfg.default {
            Some(Default::Type) => finishes.push(quote! {
                let #value = #krate::option_or_default(#local);
            }),
            Some(Default::Path(path)) => finishes.push(quote! {
                let #value = #krate::option_or(#local, #path);
            }),
            None => finishes.push(quote! {
                let #value = match #local {
                    ::core::option::Option::Some(__sdave_present) => __sdave_present,
                    ::core::option::Option::None => {
                        return ::core::result::Result::Err(
                            #krate::Error::new(#krate::ErrorKind::MissingField { name: #name })
                                .in_field(#name),
                        );
                    }
                };
            }),
        }
        values.push(value);
    }

    let loop_body = quote! {
        while let ::core::option::Option::Some(__sdave_header) = source.next_field()? {
            let mut __sdave_matched = false;
            #(#matches)*
            if !__sdave_matched {
                return ::core::result::Result::Err(
                    #krate::Error::new(#krate::ErrorKind::UnknownField),
                );
            }
        }
        #(#finishes)*
    };
    (quote!( #(#declares)* ), values, loop_body)
}

fn enum_deserialize(data: &DataEnum, krate: &Path, de: &Lifetime) -> TokenStream2 {
    let mut chain: TokenStream2 = quote! {
        ::core::result::Result::Err(#krate::Error::new(#krate::ErrorKind::UnknownVariant))
    };

    for variant in data.variants.iter().rev() {
        let vident = &variant.ident;
        let vname = syn::LitStr::new(&field_name(vident), vident.span());
        let cond = quote!(__sdave_variant.payload_eq_ascii(#vname));
        let body = variant_deserialize(variant, krate, de);
        chain = quote! {
            if #cond {
                #body
            } else {
                #chain
            }
        };
    }

    quote! {
        let __sdave_arity = source.item_count();
        let __sdave_variant = source.next_envelope()?;
        #chain
    }
}

fn variant_deserialize(variant: &syn::Variant, krate: &Path, de: &Lifetime) -> TokenStream2 {
    let vident = &variant.ident;
    let vname = syn::LitStr::new(&field_name(vident), vident.span());

    match &variant.fields {
        Fields::Unit => quote! {{
            if __sdave_arity != 1 {
                return ::core::result::Result::Err(
                    #krate::Error::new(#krate::ErrorKind::WrongArity {
                        expected: 1,
                        actual: __sdave_arity,
                    })
                    .in_variant(#vname),
                );
            }
            ::core::result::Result::Ok(Self::#vident)
        }},
        Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
            let ty = &fields.unnamed[0].ty;
            quote! {{
                if __sdave_arity != 2 {
                    return ::core::result::Result::Err(
                        #krate::Error::new(#krate::ErrorKind::WrongArity {
                            expected: 2,
                            actual: __sdave_arity,
                        })
                        .in_variant(#vname),
                    );
                }
                let mut __sdave_body = source
                    .next_envelope()
                    .map_err(|__sdave_error| __sdave_error.in_variant(#vname))?;
                let __sdave_value = <#ty as #krate::Deserialize<#de>>::deserialize(&mut __sdave_body)
                    .map_err(|__sdave_error| __sdave_error.in_variant(#vname))?;
                ::core::result::Result::Ok(Self::#vident(__sdave_value))
            }}
        }
        Fields::Unnamed(fields) => {
            let count = fields.unnamed.len();
            let locals: Vec<Ident> = (0..count)
                .map(|index| Ident::new(&format!("__sdave_value_{index}"), Span::call_site()))
                .collect();
            let types: Vec<&Type> = fields.unnamed.iter().map(|field| &field.ty).collect();
            let indices: Vec<syn::Index> = (0..count).map(syn::Index::from).collect();
            quote! {{
                if __sdave_arity != 2 {
                    return ::core::result::Result::Err(
                        #krate::Error::new(#krate::ErrorKind::WrongArity {
                            expected: 2,
                            actual: __sdave_arity,
                        })
                        .in_variant(#vname),
                    );
                }
                let mut __sdave_body = source
                    .next_envelope()
                    .map_err(|__sdave_error| __sdave_error.in_variant(#vname))?;
                let __sdave_inner = __sdave_body.item_count();
                if __sdave_inner != #count {
                    return ::core::result::Result::Err(
                        #krate::Error::new(#krate::ErrorKind::WrongArity {
                            expected: #count,
                            actual: __sdave_inner,
                        })
                        .in_variant(#vname),
                    );
                }
                #(
                    let #locals = __sdave_body
                        .next_value::<#types>()
                        .map_err(|__sdave_error| __sdave_error.at_index(#indices))?;
                )*
                ::core::result::Result::Ok(Self::#vident(#(#locals),*))
            }}
        }
        Fields::Named(fields) => {
            let (decls, values, loop_body) = named_fields(fields, krate, de);
            let idents: Vec<&Ident> = fields
                .named
                .iter()
                .map(|field| field.ident.as_ref().expect("a named field"))
                .collect();
            quote! {{
                if __sdave_arity != 2 {
                    return ::core::result::Result::Err(
                        #krate::Error::new(#krate::ErrorKind::WrongArity {
                            expected: 2,
                            actual: __sdave_arity,
                        })
                        .in_variant(#vname),
                    );
                }
                let mut __sdave_body = source
                    .next_envelope()
                    .map_err(|__sdave_error| __sdave_error.in_variant(#vname))?;
                let source = &mut __sdave_body;
                #decls
                #loop_body
                ::core::result::Result::Ok(Self::#vident { #( #idents: #values ),* })
            }}
        }
    }
}

// ---------------------------------------------------------------------------
// the `dyn` pair
// ---------------------------------------------------------------------------

fn expand_dyn_payload(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let mut krate: Option<Path> = None;
    if !attr.is_empty() {
        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse::<Path>()?);
                Ok(())
            } else {
                Err(meta.error("unknown `dyn_payload` argument"))
            }
        });
        syn::parse::Parser::parse2(parser, attr)?;
    }
    let krate = krate.unwrap_or_else(|| parse_quote!(::sdave_codec));

    let item_trait: syn::ItemTrait = syn::parse2(item.clone())?;
    let trait_ident = item_trait.ident.clone();

    let has_dyn_serialize = item_trait.supertraits.iter().any(|bound| match bound {
        syn::TypeParamBound::Trait(bound) => bound
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "DynSerialize"),
        _ => false,
    });
    if !has_dyn_serialize {
        return Err(syn::Error::new_spanned(
            &trait_ident,
            "a `#[dyn_payload]` trait must have `DynSerialize` as a supertrait",
        ));
    }

    Ok(quote! {
        #item

        #[allow(clippy::result_large_err)]
        impl #krate::Serialize for dyn #trait_ident {
            const SHAPE: #krate::Shape = #krate::Shape::Dynamic;
            const DEPTH: u16 = #krate::DYN_PAYLOAD_DEPTH;

            fn append(&self, sink: &mut #krate::Sink<'_>) -> ::core::result::Result<(), #krate::Error> {
                #krate::DynSerialize::append_dyn(self, sink)
            }
        }
    })
}

fn expand_dyn_payload_impl(input: DeriveInput) -> syn::Result<TokenStream2> {
    let mut cfg = container_cfg(&input.attrs)?;
    let mut dyn_trait: Option<Path> = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("dyn_payload_impl") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("dyn_trait") {
                dyn_trait = Some(meta.value()?.parse::<Path>()?);
                Ok(())
            } else if meta.path.is_ident("crate") {
                cfg.krate = Some(meta.value()?.parse::<Path>()?);
                Ok(())
            } else {
                Err(meta.error("unknown `dyn_payload_impl` argument"))
            }
        })?;
    }
    let dyn_trait = dyn_trait.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "`#[derive(DynPayload)]` needs `#[dyn_payload_impl(dyn_trait = …)]`",
        )
    })?;
    let krate = crate_path(&cfg);
    let ident = input.ident;
    let (ig, tg, wc) = input.generics.split_for_impl();

    Ok(quote! {
        #[allow(clippy::result_large_err)]
        impl #ig #krate::DynSerialize for #ident #tg #wc {
            fn append_dyn(&self, sink: &mut #krate::Sink<'_>) -> ::core::result::Result<(), #krate::Error> {
                sink.marked_document(::core::any::type_name::<Self>(), self)
            }
        }

        #[allow(clippy::result_large_err)]
        impl #ig #dyn_trait for #ident #tg #wc {
            fn as_any(&self) -> &dyn ::core::any::Any {
                self
            }

            fn eq_dyn(&self, other: &dyn #dyn_trait) -> bool {
                match other.as_any().downcast_ref::<Self>() {
                    ::core::option::Option::Some(that) => self == that,
                    ::core::option::Option::None => false,
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// the rejections, and the expansions they guard
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! The rejections the README requires, asserted against the expansion functions themselves.
    //!
    //! A `compile_fail` doctest only proves that *something* failed to compile, so a typo in the
    //! snippet — or an unrelated error — would pass for a rejection. These call the very functions
    //! the derive macros call and assert the targeted message, and they also pin the shape of a
    //! successful expansion.

    use super::*;
    use quote::quote;

    fn item(source: &str) -> DeriveInput {
        syn::parse_str::<DeriveInput>(source).expect("the test's own item parses")
    }

    fn serialize_rejection(source: &str) -> String {
        expand_serialize(item(source))
            .expect_err("the expansion must be rejected")
            .to_string()
    }

    fn deserialize_rejection(source: &str) -> String {
        expand_deserialize(item(source))
            .expect_err("the expansion must be rejected")
            .to_string()
    }

    fn assert_contains(haystack: &str, needle: &str) {
        assert!(
            haystack.contains(needle),
            "expected `{needle}` in:\n{haystack}"
        );
    }

    // -----------------------------------------------------------------------
    // container attributes
    // -----------------------------------------------------------------------

    #[test]
    fn an_unknown_container_attribute_is_rejected() {
        assert_contains(
            &serialize_rejection("#[sdave(nonsense)] struct S { a: u8 }"),
            "unknown `sdave` attribute",
        );
    }

    #[test]
    fn rename_is_rejected_on_a_container() {
        assert_contains(
            &serialize_rejection(r#"#[sdave(rename = "other")] struct S { a: u8 }"#),
            "rename",
        );
    }

    #[test]
    fn skip_is_rejected_on_a_container() {
        assert_contains(
            &serialize_rejection("#[sdave(skip)] struct S { a: u8 }"),
            "skip",
        );
    }

    #[test]
    fn basic_with_a_value_is_rejected() {
        assert_contains(
            &serialize_rejection("#[sdave(basic = something)] struct S { a: u8 }"),
            "does not take a value",
        );
    }

    #[test]
    fn a_container_level_default_is_rejected() {
        assert_contains(
            &serialize_rejection("#[sdave(default)] struct S { a: u8 }"),
            "not to a container",
        );
    }

    #[test]
    fn a_union_is_rejected() {
        assert_contains(&serialize_rejection("union U { a: u8, b: u8 }"), "union");
    }

    // -----------------------------------------------------------------------
    // field attributes
    // -----------------------------------------------------------------------

    #[test]
    fn an_unknown_field_attribute_is_rejected() {
        assert_contains(
            &serialize_rejection("struct S { #[sdave(nonsense)] a: u8 }"),
            "unknown `sdave` attribute",
        );
    }

    #[test]
    fn rename_is_rejected_on_a_field() {
        assert_contains(
            &serialize_rejection(r#"struct S { #[sdave(rename = "other")] a: u8 }"#),
            "rename",
        );
    }

    #[test]
    fn skip_is_rejected_on_a_field() {
        assert_contains(
            &serialize_rejection("struct S { #[sdave(skip)] a: u8 }"),
            "skip",
        );
    }

    #[test]
    fn a_field_level_basic_is_rejected() {
        assert_contains(
            &serialize_rejection("struct S { #[sdave(basic)] a: u8 }"),
            "applies to a type",
        );
    }

    #[test]
    fn a_field_level_crate_is_rejected() {
        assert_contains(
            &serialize_rejection("struct S { #[sdave(crate = my_codec)] a: u8 }"),
            "applies to a container",
        );
    }

    #[test]
    fn a_default_on_a_positional_struct_field_is_rejected() {
        assert_contains(
            &serialize_rejection("struct S(#[sdave(default)] u8);"),
            "named field only",
        );
    }

    #[test]
    fn a_default_on_a_positional_variant_field_is_rejected() {
        assert_contains(
            &deserialize_rejection("enum E { V(#[sdave(default)] u8) }"),
            "named field only",
        );
    }

    // -----------------------------------------------------------------------
    // the `dyn` pair
    // -----------------------------------------------------------------------

    #[test]
    fn a_dyn_trait_without_the_supertrait_is_rejected() {
        let trait_item = quote! {
            trait T: core::fmt::Debug + 'static {
                fn as_any(&self) -> &dyn core::any::Any;
            }
        };
        let message = expand_dyn_payload(TokenStream2::new(), trait_item)
            .expect_err("the trait must be rejected")
            .to_string();
        assert_contains(&message, "DynSerialize");
    }

    #[test]
    fn an_unknown_dyn_payload_argument_is_rejected() {
        let trait_item = quote! {
            trait T: sdave_codec::DynSerialize + 'static {
                fn as_any(&self) -> &dyn core::any::Any;
            }
        };
        let message = expand_dyn_payload(quote!(nonsense = 1), trait_item)
            .expect_err("the argument must be rejected")
            .to_string();
        assert_contains(&message, "unknown `dyn_payload` argument");
    }

    #[test]
    fn a_dyn_implementor_without_the_pairing_attribute_is_rejected() {
        let message = expand_dyn_payload_impl(item("struct T { a: u8 }"))
            .expect_err("the implementor must be rejected")
            .to_string();
        assert_contains(&message, "dyn_payload_impl");
    }

    // -----------------------------------------------------------------------
    // the expansions themselves
    // -----------------------------------------------------------------------

    #[test]
    fn a_named_struct_expands_to_a_record() {
        let tokens = expand_serialize(item("struct S { a: u8 }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "Shape :: Record");
        assert_contains(&tokens, "depth_of");
        assert_contains(&tokens, "FieldSlot");
    }

    #[test]
    fn a_tuple_struct_expands_to_a_tuple() {
        let tokens = expand_serialize(item("struct S(u8);"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "Shape :: Tuple");
        assert_contains(&tokens, "item_run");
    }

    #[test]
    fn an_enum_expands_to_an_enum_shape() {
        let tokens = expand_serialize(item("enum E { A, B(u8) }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "Shape :: Enum");
        assert_contains(&tokens, "variant");
    }

    #[test]
    fn a_basic_type_delegates_to_its_basic_type_impl() {
        let tokens = expand_serialize(item("#[sdave(basic)] struct S;"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "BasicType :: append_basic");
    }

    #[test]
    fn the_deserialize_impl_adds_the_lifetime_only_to_the_impl() {
        let tokens = expand_deserialize(item("struct S { a: u8 }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "Deserialize < '__sdave_de >");
        assert_contains(&tokens, "for S");
        assert!(
            !tokens.contains("S < '__sdave_de >"),
            "the deriving type must not gain the fresh lifetime:\n{tokens}"
        );
    }

    #[test]
    fn a_type_with_its_own_lifetime_keeps_it() {
        let tokens = expand_deserialize(item("struct Text<'a> { s: &'a str }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "for Text < 'a >");
        assert_contains(&tokens, "'__sdave_de : 'a");
    }

    #[test]
    fn a_generic_field_gets_a_codec_bound() {
        let tokens = expand_serialize(item("struct W<T> { inner: T }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "T : :: sdave_codec :: Serialize");
    }

    #[test]
    fn a_recursive_field_saturates_the_depth() {
        let tokens = expand_serialize(item("struct N { next: Option<Box<N>> }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "u16 :: MAX");
    }

    #[test]
    fn the_crate_path_can_be_redirected() {
        let tokens = expand_serialize(item("#[sdave(crate = my_codec)] struct S { a: u8 }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "my_codec :: Serialize");
    }

    #[test]
    fn a_default_is_expanded_to_the_provider_hook() {
        let tokens = expand_deserialize(item("struct S { #[sdave(default)] a: u8 }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "option_or_default");
    }

    #[test]
    fn the_field_spelling_is_a_string_literal() {
        let tokens = expand_serialize(item("struct S { a: u8 }"))
            .expect("expands")
            .to_string();
        assert_contains(&tokens, "\"a\"");
    }
}
