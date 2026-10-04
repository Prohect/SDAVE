//! Native SDAVE derives.
//!
//! Named (and unit) structs use records; tuple structs always use positional
//! tuple bodies, including newtypes and zero-field tuple structs. Enum variants
//! retain their source shape, so `V`, `V()`, and `V {}` have different framing.
//! Only named fields may specify `#[sdave(default)]` or
//! `#[sdave(default = function_path)]`. `#[sdave(basic)]` delegates an entire
//! type to its `sdave::BasicType` implementation without inspecting its storage.
//!
//! Generated code refers to `::sdave`. A renamed dependency must also be made
//! available under that name (for example, with `extern crate codec as sdave`).

use std::collections::BTreeSet;

use proc_macro::TokenStream;
use proc_macro2::{Ident, Span, TokenStream as TokenStream2, TokenTree};
use quote::{ToTokens, quote};
use syn::ext::IdentExt;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Data, DeriveInput, ExprPath, Fields, GenericParam, Generics, Lifetime,
    LifetimeParam, LitStr, Member, Token, Type, WherePredicate, parse_macro_input, parse_quote,
};

/// Derive native SDAVE serialization using source-derived field/variant names.
#[proc_macro_derive(Serialize, attributes(sdave))]
pub fn derive_serialize(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input, Derive::Serialize)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive native SDAVE deserialization, with optional named-field defaults.
#[proc_macro_derive(Deserialize, attributes(sdave))]
pub fn derive_deserialize(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input, Derive::Deserialize)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Clone, Copy)]
enum Derive {
    Serialize,
    Deserialize,
}

#[derive(Clone, Copy)]
enum AttributeSite {
    Container,
    Variant,
    NamedField,
    PositionalField,
}

#[derive(Default)]
struct Attributes {
    basic: bool,
    default: Option<FieldDefault>,
}

struct FieldDefault {
    kind: DefaultKind,
    span: Span,
}

enum DefaultKind {
    Trait,
    Provider(ExprPath),
}

struct Field {
    ident: Option<Ident>,
    ty: Type,
    default: Option<FieldDefault>,
}

enum FieldSet {
    Named(Vec<Field>),
    Unnamed(Vec<Field>),
    Unit,
}

impl FieldSet {
    fn fields(&self) -> &[Field] {
        match self {
            Self::Named(fields) | Self::Unnamed(fields) => fields,
            Self::Unit => &[],
        }
    }
}

struct Variant {
    ident: Ident,
    fields: FieldSet,
}

enum ModelData {
    Struct(FieldSet),
    Enum(Vec<Variant>),
}

struct Model {
    basic: bool,
    data: ModelData,
}

impl Model {
    fn parse(input: &DeriveInput) -> syn::Result<Self> {
        if matches!(&input.data, Data::Union(_)) {
            return Err(syn::Error::new_spanned(
                input,
                "SDAVE derives do not support unions",
            ));
        }
        let attributes = parse_attributes(&input.attrs, AttributeSite::Container)?;
        let data = match &input.data {
            Data::Struct(data) => ModelData::Struct(parse_fields(&data.fields)?),
            Data::Enum(data) => {
                let mut variants = Vec::with_capacity(data.variants.len());
                for variant in &data.variants {
                    parse_attributes(&variant.attrs, AttributeSite::Variant)?;
                    variants.push(Variant {
                        ident: variant.ident.clone(),
                        fields: parse_fields(&variant.fields)?,
                    });
                }
                ModelData::Enum(variants)
            }
            Data::Union(_) => unreachable!("unions were rejected above"),
        };
        let model = Self {
            basic: attributes.basic,
            data,
        };
        if model.basic {
            for field in model.fields() {
                if let Some(default) = &field.default {
                    return Err(syn::Error::new(
                        default.span,
                        "`sdave(basic)` cannot be combined with field defaults",
                    ));
                }
            }
        }
        Ok(model)
    }

    fn fields(&self) -> Vec<&Field> {
        match &self.data {
            ModelData::Struct(fields) => fields.fields().iter().collect(),
            ModelData::Enum(variants) => variants
                .iter()
                .flat_map(|variant| variant.fields.fields())
                .collect(),
        }
    }

    fn shape(&self) -> TokenStream2 {
        if self.basic {
            return quote!(::sdave::Shape::Basic);
        }
        match &self.data {
            ModelData::Struct(FieldSet::Unnamed(_)) => quote!(::sdave::Shape::Tuple),
            ModelData::Struct(_) => quote!(::sdave::Shape::Record),
            ModelData::Enum(_) => quote!(::sdave::Shape::Enum),
        }
    }
}

fn parse_attributes(attributes: &[Attribute], site: AttributeSite) -> syn::Result<Attributes> {
    let mut parsed = Attributes::default();
    for attribute in attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("sdave"))
    {
        let mut entries = 0;
        attribute.parse_nested_meta(|meta| {
            entries += 1;
            if meta.path.is_ident("basic") {
                if !matches!(site, AttributeSite::Container) {
                    return Err(meta.error("`sdave(basic)` is only supported on a type"));
                }
                if parsed.basic {
                    return Err(meta.error("duplicate `sdave(basic)` attribute"));
                }
                if !meta.input.is_empty() && !meta.input.peek(Token![,]) {
                    return Err(meta.error("`sdave(basic)` does not accept a value or arguments"));
                }
                parsed.basic = true;
            } else if meta.path.is_ident("default") {
                match site {
                    AttributeSite::Container | AttributeSite::Variant => {
                        return Err(
                            meta.error("`sdave(default)` is only supported on named fields")
                        );
                    }
                    AttributeSite::PositionalField => {
                        return Err(meta.error("defaults are not supported on positional fields"));
                    }
                    AttributeSite::NamedField => {}
                }
                if parsed.default.is_some() {
                    return Err(meta.error("duplicate `sdave(default)` attribute"));
                }
                let kind = if meta.input.peek(Token![=]) {
                    let value = meta.value()?;
                    if value.peek(LitStr) {
                        return Err(value.error("expected a Rust function path, not a string"));
                    }
                    DefaultKind::Provider(value.parse::<ExprPath>()?)
                } else {
                    if !meta.input.is_empty() && !meta.input.peek(Token![,]) {
                        return Err(meta.error("expected `default` or `default = function_path`"));
                    }
                    DefaultKind::Trait
                };
                parsed.default = Some(FieldDefault {
                    kind,
                    span: meta.path.segments[0].ident.span(),
                });
            } else if meta.path.is_ident("rename") || meta.path.is_ident("skip") {
                return Err(meta.error(
                    "SDAVE does not support rename or skip; names and fields come from Rust source",
                ));
            } else {
                return Err(meta.error(
                    "unknown `sdave` attribute; supported attributes are `basic` and `default`",
                ));
            }
            Ok(())
        })?;
        if entries == 0 {
            return Err(syn::Error::new_spanned(
                attribute,
                "empty `sdave` attribute",
            ));
        }
    }
    Ok(parsed)
}

fn parse_fields(fields: &Fields) -> syn::Result<FieldSet> {
    let (source, site) = match fields {
        Fields::Named(fields) => (&fields.named, AttributeSite::NamedField),
        Fields::Unnamed(fields) => (&fields.unnamed, AttributeSite::PositionalField),
        Fields::Unit => return Ok(FieldSet::Unit),
    };
    let mut parsed = Vec::with_capacity(source.len());
    for field in source {
        let attributes = parse_attributes(&field.attrs, site)?;
        parsed.push(Field {
            ident: field.ident.clone(),
            ty: field.ty.clone(),
            default: attributes.default,
        });
    }
    Ok(match site {
        AttributeSite::NamedField => FieldSet::Named(parsed),
        _ => FieldSet::Unnamed(parsed),
    })
}

/// Use fresh names even in the presence of const generics, higher-ranked
/// lifetimes, or unqualified default-provider paths with similar names.
struct Names {
    used: BTreeSet<String>,
}

impl Names {
    fn new(input: &DeriveInput) -> Self {
        fn collect(tokens: TokenStream2, used: &mut BTreeSet<String>) {
            for token in tokens {
                match token {
                    TokenTree::Ident(ident) => {
                        used.insert(ident.unraw().to_string());
                    }
                    TokenTree::Group(group) => collect(group.stream(), used),
                    _ => {}
                }
            }
        }
        let mut used = BTreeSet::new();
        collect(input.to_token_stream(), &mut used);
        Self { used }
    }

    fn ident(&mut self, base: &str) -> Ident {
        let mut name = base.to_owned();
        while !self.used.insert(name.clone()) {
            name.push('_');
        }
        Ident::new(&name, Span::mixed_site())
    }

    fn lifetime(&mut self, base: &str) -> Lifetime {
        let ident = self.ident(base);
        Lifetime::new(&format!("'{ident}"), ident.span())
    }
}

fn source_name(ident: &Ident) -> LitStr {
    LitStr::new(&ident.unraw().to_string(), ident.span())
}

/// Whole-field codec predicates can cycle through another derived type or a
/// recursive alias, even when the current type is not spelled in the field.
/// Concrete types need no predicate: generated calls check their codecs.
/// Infer bounds on ordinary type arguments instead, retaining opaque generic
/// projections without requiring their owners to implement a codec.
struct CodecBounds<'a> {
    current: &'a Ident,
    generics: &'a Generics,
    types: Vec<Type>,
}

impl CodecBounds<'_> {
    fn parameter_path(&self, path: &syn::Path) -> bool {
        path.leading_colon.is_none()
            && path.segments.first().is_some_and(|segment| {
                self.generics
                    .type_params()
                    .any(|parameter| parameter.ident.unraw() == segment.ident.unraw())
            })
    }

    fn reference_target(&self, ty: &Type) -> bool {
        match ty {
            Type::Path(path) => {
                (path.qself.is_some() || self.parameter_path(&path.path))
                    && depends_on_generics(ty, self.generics)
            }
            Type::Reference(reference) => self.reference_target(&reference.elem),
            Type::Paren(paren) => self.reference_target(&paren.elem),
            Type::Group(group) => self.reference_target(&group.elem),
            _ => false,
        }
    }
}

fn depends_on_generics(ty: &Type, generics: &Generics) -> bool {
    struct Finder<'a> {
        generics: &'a Generics,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Finder<'_> {
        fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
            if path.path.leading_colon.is_none()
                && path.path.segments.first().is_some_and(|segment| {
                    self.generics
                        .type_params()
                        .any(|parameter| parameter.ident.unraw() == segment.ident.unraw())
                })
            {
                self.found = true;
            }
            visit::visit_type_path(self, path);
        }

        fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
            if path.path.leading_colon.is_none()
                && path.path.segments.first().is_some_and(|segment| {
                    self.generics
                        .params
                        .iter()
                        .any(|parameter| match parameter {
                            GenericParam::Type(parameter) => {
                                parameter.ident.unraw() == segment.ident.unraw()
                            }
                            GenericParam::Const(parameter) => {
                                parameter.ident.unraw() == segment.ident.unraw()
                            }
                            GenericParam::Lifetime(_) => false,
                        })
                })
            {
                self.found = true;
            }
            visit::visit_expr_path(self, path);
        }

        fn visit_lifetime(&mut self, lifetime: &'ast Lifetime) {
            if self
                .generics
                .lifetimes()
                .any(|parameter| parameter.lifetime.ident.unraw() == lifetime.ident.unraw())
            {
                self.found = true;
            }
        }
    }
    let mut finder = Finder {
        generics,
        found: false,
    };
    finder.visit_type(ty);
    finder.found
}

fn phantom_data(path: &syn::Path) -> bool {
    let name = path
        .segments
        .iter()
        .map(|segment| segment.ident.unraw().to_string())
        .collect::<Vec<_>>()
        .join("::");
    matches!(
        name.as_str(),
        "PhantomData" | "core::marker::PhantomData" | "std::marker::PhantomData"
    )
}

fn path_mentions_current(path: &syn::Path, current: &Ident) -> bool {
    path.segments.iter().any(|segment| {
        let name = segment.ident.unraw();
        name == "Self" || name == current.unraw()
    })
}

fn mentions_current(ty: &Type, current: &Ident) -> bool {
    struct Finder<'a> {
        current: &'a Ident,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Finder<'_> {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if path_mentions_current(path, self.current) {
                self.found = true;
            }
            visit::visit_path(self, path);
        }
    }
    let mut finder = Finder {
        current,
        found: false,
    };
    finder.visit_type(ty);
    finder.found
}

impl<'ast> Visit<'ast> for CodecBounds<'_> {
    fn visit_type(&mut self, ty: &'ast Type) {
        if matches!(ty, Type::Path(path) if path_mentions_current(&path.path, self.current)) {
            return;
        }
        match ty {
            Type::Path(path) if phantom_data(&path.path) => return,
            Type::Path(path)
                if path.qself.is_some()
                    || (self.parameter_path(&path.path) && path.path.segments.len() > 1) =>
            {
                if !mentions_current(ty, self.current) && depends_on_generics(ty, self.generics) {
                    self.types.push(ty.clone());
                }
                // A projection's arguments describe its owner/trait, not its
                // encoded value; do not add codec bounds on those arguments.
                return;
            }
            Type::Path(path) if self.parameter_path(&path.path) => {
                self.types.push(ty.clone());
                return;
            }
            Type::Reference(reference)
                if self.reference_target(&reference.elem)
                    && !mentions_current(ty, self.current) =>
            {
                // Borrowing a generic T need not deserialize T by value (T
                // may even be unsized). Retain the reference's own predicate.
                self.types.push(ty.clone());
                return;
            }
            _ => {}
        }
        visit::visit_type(self, ty);
    }

    fn visit_expr(&mut self, _: &'ast syn::Expr) {
        // A parameter used only in an array length is not an encoded value.
    }
}

fn add_bounds(
    generics: &mut Generics,
    input: &DeriveInput,
    model: &Model,
    derive: Derive,
    de: &Lifetime,
) {
    if matches!(derive, Derive::Deserialize) {
        let parameter: LifetimeParam = parse_quote!(#de);
        generics.params.insert(0, GenericParam::Lifetime(parameter));
    }
    if model.basic {
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(Self: ::sdave::BasicType));
        return;
    }

    let mut predicates = Vec::<WherePredicate>::new();
    if matches!(derive, Derive::Deserialize) {
        for lifetime in input.generics.lifetimes() {
            let lifetime = &lifetime.lifetime;
            predicates.push(parse_quote!(#de: #lifetime));
        }
    }
    let mut bounds = CodecBounds {
        current: &input.ident,
        generics: &input.generics,
        types: Vec::new(),
    };
    for field in model.fields() {
        bounds.visit_type(&field.ty);
        if let (
            Derive::Deserialize,
            Some(FieldDefault {
                kind: DefaultKind::Trait,
                ..
            }),
        ) = (derive, &field.default)
        {
            let ty = &field.ty;
            predicates.push(parse_quote!(#ty: ::core::default::Default));
        }
    }
    for ty in bounds.types {
        predicates.push(match derive {
            Derive::Serialize => parse_quote!(#ty: ::sdave::Serialize),
            Derive::Deserialize => parse_quote!(#ty: ::sdave::Deserialize<#de>),
        });
    }
    let mut seen = BTreeSet::new();
    for predicate in predicates {
        if seen.insert(predicate.to_token_stream().to_string()) {
            generics.make_where_clause().predicates.push(predicate);
        }
    }
}

fn expand(input: &DeriveInput, derive: Derive) -> syn::Result<TokenStream2> {
    let model = Model::parse(input)?;
    let mut names = Names::new(input);
    let de = names.lifetime("__sdave_de");
    let policy = names.ident("__SdavePolicy");
    let serializer = names.ident("__sdave_serializer");
    let decoder = names.ident("__sdave_decoder");
    let run = names.ident("__sdave_run");
    let payload = names.ident("__sdave_payload");
    let error = names.ident("__sdave_error");
    let mut generator = Generator {
        names,
        de,
        policy,
        serializer,
        decoder,
        run,
        payload,
        error,
    };
    let mut generics = input.generics.clone();
    add_bounds(&mut generics, input, &model, derive, &generator.de);

    let (impl_generics, _, where_clause) = generics.split_for_impl();
    let (_, type_generics, _) = input.generics.split_for_impl();
    let ident = &input.ident;
    let shape = model.shape();
    let methods = match derive {
        Derive::Serialize => generator.serialize(&model),
        Derive::Deserialize => generator.deserialize(&model),
    };
    let trait_path = match derive {
        Derive::Serialize => quote!(::sdave::Serialize),
        Derive::Deserialize => {
            let de = &generator.de;
            quote!(::sdave::Deserialize<#de>)
        }
    };
    Ok(quote! {
        #[automatically_derived]
        #[allow(non_upper_case_globals)]
        impl #impl_generics #trait_path for #ident #type_generics #where_clause {
            const SHAPE: ::sdave::Shape = #shape;
            #methods
        }
    })
}

struct Generator {
    names: Names,
    de: Lifetime,
    policy: Ident,
    serializer: Ident,
    decoder: Ident,
    run: Ident,
    payload: Ident,
    error: Ident,
}

impl Generator {
    fn serialize_method(&self, name: &str, body: TokenStream2) -> TokenStream2 {
        let name = Ident::new(name, Span::call_site());
        let policy = &self.policy;
        let serializer = &self.serializer;
        quote! {
            fn #name<#policy: ::sdave::NamePolicy>(
                &self,
                #serializer: &mut ::sdave::Serializer<#policy>,
            ) -> ::sdave::Result<::std::vec::Vec<::core::primitive::u8>> {
                #body
            }
        }
    }

    fn deserialize_method(&self, payload_method: bool, body: TokenStream2) -> TokenStream2 {
        let policy = &self.policy;
        let decoder = &self.decoder;
        let de = &self.de;
        let (name, argument) = if payload_method {
            let payload = &self.payload;
            (
                quote!(deserialize_payload),
                quote!(#payload: ::sdave::Payload<#de>),
            )
        } else {
            let run = &self.run;
            (
                quote!(deserialize_value),
                quote!(#run: ::sdave::ValueRun<#de>),
            )
        };
        quote! {
            fn #name<#policy: ::sdave::NamePolicy>(
                #argument,
                #decoder: &mut ::sdave::Deserializer<#policy>,
            ) -> ::sdave::Result<Self> {
                #body
            }
        }
    }

    fn encode_fields(
        &mut self,
        fields: &[Field],
        values: &[TokenStream2],
    ) -> (Ident, TokenStream2) {
        let body = self.names.ident("__sdave_body");
        let serializer = &self.serializer;
        let mutable = if fields.is_empty() {
            quote!()
        } else {
            quote!(mut)
        };
        let writes = fields.iter().zip(values).map(|(field, value)| {
            let name = source_name(field.ident.as_ref().expect("named field"));
            quote!(#serializer.field(#name, #value, &mut #body)?;)
        });
        let code = quote! {
            let #mutable #body = ::std::vec::Vec::<::core::primitive::u8>::new();
            #(#writes)*
        };
        (body, code)
    }

    fn encode_items(&mut self, values: &[TokenStream2]) -> (Ident, TokenStream2) {
        let body = self.names.ident("__sdave_body");
        let serializer = &self.serializer;
        let error = &self.error;
        let mutable = if values.is_empty() {
            quote!()
        } else {
            quote!(mut)
        };
        let writes = values.iter().enumerate().map(|(index, value)| {
            quote! {
                #serializer.item(#value, &mut #body)
                    .map_err(|#error| #error.at_index(#index))?;
            }
        });
        let code = quote! {
            let #mutable #body = ::std::vec::Vec::<::core::primitive::u8>::new();
            #(#writes)*
        };
        (body, code)
    }

    fn serialize(&mut self, model: &Model) -> TokenStream2 {
        let serializer = self.serializer.clone();
        if model.basic {
            let value = self.serialize_method("serialize_value", quote!(#serializer.basic(self)));
            let payload = self.serialize_method(
                "serialize_payload",
                quote! {
                    <Self as ::sdave::BasicType>::encode_basic(self, #serializer)
                },
            );
            return quote!(#value #payload);
        }
        match &model.data {
            ModelData::Struct(FieldSet::Named(fields)) => {
                let values = fields
                    .iter()
                    .map(|field| {
                        let ident = field.ident.as_ref().expect("named field");
                        quote!(&self.#ident)
                    })
                    .collect::<Vec<_>>();
                let (body, writes) = self.encode_fields(fields, &values);
                self.serialize_method(
                    "serialize_value",
                    quote! {
                        #writes
                        #serializer.frame(&#body)
                    },
                )
            }
            ModelData::Struct(FieldSet::Unit) => {
                self.serialize_method("serialize_value", quote!(#serializer.frame(&[])))
            }
            ModelData::Struct(FieldSet::Unnamed(fields)) => {
                let values = fields
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        let member = Member::Unnamed(syn::Index::from(index));
                        quote!(&self.#member)
                    })
                    .collect::<Vec<_>>();
                let (body, writes) = self.encode_items(&values);
                let value = self.serialize_method("serialize_value", quote! {
                    let #body = <Self as ::sdave::Serialize>::serialize_payload(self, #serializer)?;
                    #serializer.frame(&#body)
                });
                let payload = self.serialize_method(
                    "serialize_payload",
                    quote! {
                        #writes
                        ::core::result::Result::Ok(#body)
                    },
                );
                quote!(#value #payload)
            }
            ModelData::Enum(variants) => {
                if variants.is_empty() {
                    return self.serialize_method("serialize_value", quote!(match *self {}));
                }
                let mut arms = Vec::new();
                for variant in variants {
                    let ident = &variant.ident;
                    let name = source_name(ident);
                    let bindings = variant
                        .fields
                        .fields()
                        .iter()
                        .enumerate()
                        .map(|(index, _)| self.names.ident(&format!("__sdave_field_{index}")))
                        .collect::<Vec<_>>();
                    let values = bindings
                        .iter()
                        .map(|binding| quote!(#binding))
                        .collect::<Vec<_>>();
                    let (pattern, body) = match &variant.fields {
                        FieldSet::Unit => (
                            quote!(Self::#ident),
                            quote! {
                                #serializer.variant(#name, ::core::option::Option::None)
                            },
                        ),
                        FieldSet::Unnamed(fields) if fields.len() == 1 => {
                            let binding = &bindings[0];
                            let associated = self.names.ident("__sdave_associated");
                            (
                                quote!(Self::#ident(#binding)),
                                quote! {
                                    let #associated = #serializer.payload(#binding)?;
                                    #serializer.variant(#name, ::core::option::Option::Some(#associated.as_slice()))
                                },
                            )
                        }
                        FieldSet::Unnamed(_) => {
                            let (body, writes) = self.encode_items(&values);
                            (
                                quote!(Self::#ident(#(#bindings),*)),
                                quote! {
                                    #writes
                                    #serializer.variant(#name, ::core::option::Option::Some(#body.as_slice()))
                                },
                            )
                        }
                        FieldSet::Named(fields) => {
                            let members = fields.iter().zip(&bindings).map(|(field, binding)| {
                                let field = field.ident.as_ref().expect("named field");
                                quote!(#field: #binding)
                            });
                            let (body, writes) = self.encode_fields(fields, &values);
                            let record = self.names.ident("__sdave_record");
                            (
                                quote!(Self::#ident { #(#members),* }),
                                quote! {
                                    #writes
                                    let #record = #serializer.frame(&#body)?;
                                    #serializer.variant(#name, ::core::option::Option::Some(#record.as_slice()))
                                },
                            )
                        }
                    };
                    let branch = self.variant_branch(
                        body,
                        &name,
                        quote!(::std::vec::Vec<::core::primitive::u8>),
                    );
                    arms.push(quote!(#pattern => #branch));
                }
                self.serialize_method("serialize_value", quote!(match self { #(#arms),* }))
            }
        }
    }

    /// Collect and validate all fields before evaluating any provider. Presence
    /// is an outer Option, so a supplied `None` remains a present value.
    fn decode_fields(&mut self, fields: &[Field], run: TokenStream2) -> (Vec<Ident>, TokenStream2) {
        let record = self.names.ident("__sdave_record");
        let present = self.names.ident("__sdave_present");
        let decoder = &self.decoder;
        let policy = &self.policy;
        let mutable = if fields.is_empty() {
            quote!()
        } else {
            quote!(mut)
        };
        let mut reads = Vec::new();
        let mut required = Vec::new();
        let mut resolve = Vec::new();
        let mut values = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            let option = self.names.ident(&format!("__sdave_field_{index}"));
            let value = self.names.ident(&format!("__sdave_value_{index}"));
            let name = source_name(field.ident.as_ref().expect("named field"));
            let ty = &field.ty;
            reads.push(quote! {
                let #option = #record.take::<#ty, #policy>(#name, #decoder)?;
            });
            let fallback = match &field.default {
                Some(FieldDefault {
                    kind: DefaultKind::Trait,
                    ..
                }) => {
                    quote!(<#ty as ::core::default::Default>::default())
                }
                Some(FieldDefault {
                    kind: DefaultKind::Provider(path),
                    ..
                }) => quote!(#path()),
                None => {
                    required.push(quote!(#record.require_present(#name, #option.is_some())?;));
                    quote!(return ::core::result::Result::Err(#record.missing_field(#name)))
                }
            };
            resolve.push(quote! {
                let #value = match #option {
                    ::core::option::Option::Some(#present) => #present,
                    ::core::option::Option::None => #fallback,
                };
            });
            values.push(value);
        }
        let code = quote! {
            let #mutable #record = #decoder.record(#run)?;
            #(#reads)*
            #record.finish()?;
            #(#required)*
            #(#resolve)*
        };
        (values, code)
    }

    fn decode_items(
        &mut self,
        fields: &[Field],
        payload: TokenStream2,
    ) -> (Vec<Ident>, TokenStream2) {
        let count = fields.len();
        let decoder = &self.decoder;
        if fields.is_empty() {
            return (Vec::new(), quote!(#decoder.run(#payload)?.exact(#count)?;));
        }
        let items = self.names.ident("__sdave_items");
        let present = self.names.ident("__sdave_present");
        let error = &self.error;
        let mut reads = Vec::new();
        let mut values = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            let item = self.names.ident(&format!("__sdave_item_{index}"));
            let value = self.names.ident(&format!("__sdave_value_{index}"));
            let ty = &field.ty;
            reads.push(quote! {
                let #item = match #items.next() {
                    ::core::option::Option::Some(#present) => #present,
                    ::core::option::Option::None => return ::core::result::Result::Err(
                        ::sdave::Error::with_source(::sdave::ErrorKind::EnvelopeCount {
                            expected: #count,
                            actual: #index,
                        }, file!(), line!()),
                    ),
                };
                let #value = #decoder.payload::<#ty>(#item)
                    .map_err(|#error| #error.at_index(#index))?;
            });
            values.push(value);
        }
        let code = quote! {
            let mut #items = #decoder.run(#payload)?.exact(#count)?.into_iter();
            #(#reads)*
        };
        (values, code)
    }

    fn variant_branch(
        &self,
        body: TokenStream2,
        name: &LitStr,
        output: TokenStream2,
    ) -> TokenStream2 {
        let error = &self.error;
        quote! {
            (|| -> ::sdave::Result<#output> { #body })()
                .map_err(|#error| #error.in_variant(#name))
        }
    }

    fn deserialize(&mut self, model: &Model) -> TokenStream2 {
        let run = self.run.clone();
        let payload_argument = self.payload.clone();
        let decoder = self.decoder.clone();
        let de = self.de.clone();
        if model.basic {
            let value = self.deserialize_method(false, quote!(#decoder.basic::<Self>(#run)));
            let payload = self.deserialize_method(
                true,
                quote! {
                    <Self as ::sdave::BasicType>::decode_basic(#payload_argument, #decoder)
                },
            );
            return quote!(#value #payload);
        }
        match &model.data {
            ModelData::Struct(FieldSet::Named(fields)) => {
                let (values, reads) = self.decode_fields(fields, quote!(#run));
                let members = fields.iter().zip(&values).map(|(field, value)| {
                    let field = field.ident.as_ref().expect("named field");
                    quote!(#field: #value)
                });
                self.deserialize_method(
                    false,
                    quote! {
                        #reads
                        ::core::result::Result::Ok(Self { #(#members),* })
                    },
                )
            }
            ModelData::Struct(FieldSet::Unit) => {
                let (_, reads) = self.decode_fields(&[], quote!(#run));
                self.deserialize_method(
                    false,
                    quote! {
                        #reads
                        ::core::result::Result::Ok(Self)
                    },
                )
            }
            ModelData::Struct(FieldSet::Unnamed(fields)) => {
                let value = self.deserialize_method(false, quote! {
                    let #payload_argument = #run.single()?;
                    <Self as ::sdave::Deserialize<#de>>::deserialize_payload(#payload_argument, #decoder)
                });
                let (values, reads) = self.decode_items(fields, quote!(#payload_argument));
                let payload = self.deserialize_method(
                    true,
                    quote! {
                        #reads
                        ::core::result::Result::Ok(Self(#(#values),*))
                    },
                );
                quote!(#value #payload)
            }
            ModelData::Enum(variants) => {
                let variant_run = self.names.ident("__sdave_variant");
                let mut arms = Vec::new();
                for variant in variants {
                    let ident = &variant.ident;
                    let name = source_name(ident);
                    let body = match &variant.fields {
                        FieldSet::Unit => quote! {
                            #variant_run.unit()?;
                            ::core::result::Result::Ok(Self::#ident)
                        },
                        FieldSet::Unnamed(fields) if fields.len() == 1 => {
                            let associated = self.names.ident("__sdave_associated");
                            let value = self.names.ident("__sdave_value");
                            let ty = &fields[0].ty;
                            quote! {
                                let #associated = #variant_run.associated()?;
                                let #value = #decoder.payload::<#ty>(#associated)?;
                                ::core::result::Result::Ok(Self::#ident(#value))
                            }
                        }
                        FieldSet::Unnamed(fields) => {
                            let associated = self.names.ident("__sdave_associated");
                            let (values, reads) = self.decode_items(fields, quote!(#associated));
                            quote! {
                                let #associated = #variant_run.associated()?;
                                #reads
                                ::core::result::Result::Ok(Self::#ident(#(#values),*))
                            }
                        }
                        FieldSet::Named(fields) => {
                            let associated = self.names.ident("__sdave_associated");
                            let record_run = self.names.ident("__sdave_record_run");
                            let (values, reads) = self.decode_fields(fields, quote!(#record_run));
                            let members = fields.iter().zip(&values).map(|(field, value)| {
                                let field = field.ident.as_ref().expect("named field");
                                quote!(#field: #value)
                            });
                            quote! {
                                let #associated = #variant_run.associated()?;
                                let #record_run = #decoder.run(#associated)?;
                                #reads
                                ::core::result::Result::Ok(Self::#ident { #(#members),* })
                            }
                        }
                    };
                    let branch = self.variant_branch(body, &name, quote!(Self));
                    arms.push(quote!(#name => #branch));
                }
                self.deserialize_method(
                    false,
                    quote! {
                        let #variant_run = #run.variant()?;
                        match #variant_run.name() {
                            #(#arms,)*
                            _ => ::core::result::Result::Err(#variant_run.unknown()),
                        }
                    },
                )
            }
        }
    }
}

#[cfg(test)]
mod tests;
