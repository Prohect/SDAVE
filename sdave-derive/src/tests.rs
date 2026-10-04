use super::*;
use syn::{ImplItem, ImplItemFn, ItemImpl};

fn implementation(source: &str, derive: Derive) -> ItemImpl {
    let input: DeriveInput = syn::parse_str(source).expect("valid derive input");
    syn::parse2(expand(&input, derive).expect("valid SDAVE derive")).expect("valid generated impl")
}

fn compact(tokens: impl ToTokens) -> String {
    tokens
        .to_token_stream()
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn method<'a>(implementation: &'a ItemImpl, name: &str) -> &'a ImplItemFn {
    implementation
        .items
        .iter()
        .find_map(|item| match item {
            ImplItem::Fn(method) if method.sig.ident == name => Some(method),
            _ => None,
        })
        .expect("generated method")
}

fn bounds(implementation: &ItemImpl) -> String {
    compact(&implementation.generics.where_clause)
}

fn reject(source: &str, message: &str) {
    let input: DeriveInput = syn::parse_str(source).expect("valid Rust input");
    for derive in [Derive::Serialize, Derive::Deserialize] {
        let error = expand(&input, derive).expect_err("unsupported input must fail");
        assert!(
            error.to_string().contains(message),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn rejects_unsupported_attributes_at_every_level() {
    for source in [
        "#[sdave(rename = \"Other\")] struct S;",
        "struct S { #[sdave(rename = \"other\")] field: u32 }",
        "enum E { #[sdave(rename = \"Other\")] V }",
        "#[sdave(skip)] struct S;",
        "struct S { #[sdave(skip)] field: u32 }",
        "enum E { V(#[sdave(skip)] u32) }",
    ] {
        reject(source, "does not support rename or skip");
    }
    for source in [
        "#[sdave(unknown)] struct S;",
        "struct S { #[sdave(unknown)] field: u32 }",
        "enum E { #[sdave(unknown)] V }",
        "enum E { V(#[sdave(unknown)] u32) }",
    ] {
        reject(source, "unknown `sdave` attribute");
    }
    reject("#[sdave()] struct S;", "empty `sdave` attribute");
    reject("#[sdave(basic = true)] struct S;", "does not accept");
    reject("#[sdave(basic, basic)] struct S;", "duplicate");
    reject("#[sdave(basic)] #[sdave(basic)] struct S;", "duplicate");
    reject(
        "struct S { #[sdave(default, default)] x: u32 }",
        "duplicate",
    );
    reject(
        "struct S { #[sdave(default)] #[sdave(default = make)] x: u32 }",
        "duplicate",
    );
    reject(
        "struct S { #[sdave(default = \"make\")] x: u32 }",
        "not a string",
    );
    reject(
        "struct S { #[sdave(default(make))] x: u32 }",
        "expected `default`",
    );
    reject("union U { x: u32, y: f32 }", "do not support unions");
}

#[test]
fn restricts_defaults_and_basic_to_their_supported_sites() {
    for source in [
        "#[sdave(default)] struct S;",
        "#[sdave(default = make)] struct S {}",
        "#[sdave(default)] enum E { V }",
        "enum E { #[sdave(default)] V {} }",
    ] {
        reject(source, "only supported on named fields");
    }
    for source in [
        "struct S(#[sdave(default)] u32);",
        "struct S(#[sdave(default = make)] u32);",
        "enum E { V(#[sdave(default)] u32) }",
        "enum E { V(u32, #[sdave(default = make)] u32) }",
    ] {
        reject(source, "positional fields");
    }
    for source in [
        "struct S { #[sdave(basic)] x: u32 }",
        "struct S(#[sdave(basic)] u32);",
        "enum E { #[sdave(basic)] V }",
        "enum E { V { #[sdave(basic)] x: u32 } }",
    ] {
        reject(source, "only supported on a type");
    }
    for source in [
        "#[sdave(basic)] struct S { #[sdave(default)] x: u32 }",
        "#[sdave(basic)] struct S { #[sdave(default = make)] x: u32 }",
        "#[sdave(basic)] enum E { V { #[sdave(default)] x: u32 } }",
    ] {
        reject(source, "cannot be combined");
    }
}

#[test]
fn named_defaults_wait_for_all_reads_finish_and_required_presence() {
    let implementation = implementation(
        "struct S<T> { #[sdave(default = make)] optional: Option<T>, first: u32, \
         #[sdave(default)] values: Vec<T>, last: String }",
        Derive::Deserialize,
    );
    let body = compact(&method(&implementation, "deserialize_value").block);
    let finish = body.find(".finish()?").unwrap();
    let provider = body.find("make()").unwrap();
    let default = body.find("::default()").unwrap();
    assert_eq!(body.match_indices(".take::<").count(), 4);
    assert!(
        body.match_indices(".take::<")
            .all(|(index, _)| index < finish)
    );
    assert_eq!(body.match_indices(".require_present(").count(), 2);
    assert!(
        body.match_indices(".require_present(")
            .all(|(index, _)| { finish < index && index < provider && index < default })
    );
    assert_eq!(body.match_indices("make()").count(), 1);
    assert!(!body.contains(".unwrap(") && !body.contains(".expect("));
    let bounds = bounds(&implementation);
    assert!(bounds.contains("Vec<T>:::core::default::Default"));
    assert!(!bounds.contains("T:::core::default::Default"));
    assert!(!bounds.contains("Option<T>:::core::default::Default"));
}

#[test]
fn serialize_never_requires_field_defaults() {
    let implementation = implementation(
        "struct S<T> { #[sdave(default)] x: T, #[sdave(default = make)] y: Vec<T> }",
        Derive::Serialize,
    );
    assert!(!bounds(&implementation).contains("Default"));
    assert!(!compact(implementation).contains("make"));
}

#[test]
fn accepts_direct_and_qualified_function_paths() {
    for source in [
        "struct S { #[sdave(default = crate::defaults::make)] x: u32 }",
        "struct S<T> { #[sdave(default = T::make)] x: T }",
        "struct S<T> { #[sdave(default = <T as Provider>::make)] x: T }",
    ] {
        let implementation = implementation(source, Derive::Deserialize);
        assert!(!bounds(&implementation).contains("Default"));
        assert!(compact(&method(&implementation, "deserialize_value").block).contains("::make()"));
    }
}

#[test]
fn basic_is_opaque_and_requires_only_the_semantic_helper() {
    for derive in [Derive::Serialize, Derive::Deserialize] {
        let implementation = implementation(
            "#[sdave(basic)] struct Opaque<'a, T> { storage: T, borrowed: &'a NoCodec }",
            derive,
        );
        assert_eq!(bounds(&implementation), "whereSelf:::sdave::BasicType");
        let code = compact(&implementation);
        assert!(
            !code.contains("NoCodec") && !code.contains(".storage") && !code.contains(".borrowed")
        );
        assert!(code.contains("::sdave::Shape::Basic"));
        match derive {
            Derive::Serialize => {
                assert!(code.contains(".basic(self)"));
                assert!(code.contains("<Selfas::sdave::BasicType>::encode_basic"));
            }
            Derive::Deserialize => {
                assert!(code.contains(".basic::<Self>"));
                assert!(code.contains("<Selfas::sdave::BasicType>::decode_basic"));
            }
        }
    }
}

#[test]
fn recursive_bounds_skip_self_but_keep_other_components_and_associated_types() {
    for derive in [Derive::Serialize, Derive::Deserialize] {
        let implementation = implementation(
            "struct Node<T: HasItem> { value: T::Item, next: Option<Box<Node<T>>>, \
             children: Vec<Self>, mixed: (T, Option<Box<Self>>) }",
            derive,
        );
        let predicates = bounds(&implementation);
        assert!(predicates.contains("T::Item:::sdave::"));
        assert!(predicates.contains("T:::sdave::"));
        assert!(!predicates.contains("Node") && !predicates.contains("Self"));
        assert!(
            !predicates.contains("Box")
                && !predicates.contains("Option")
                && !predicates.contains("Vec")
        );
        assert!(compact(&implementation).contains("constSHAPE:::sdave::Shape="));
    }
}

#[test]
fn fresh_names_preserve_generics_and_borrowing() {
    let implementation = implementation(
        "struct S<'__sdave_de, __SdavePolicy, const __sdave_record: usize> \
         where __SdavePolicy: Clone { text: &'__sdave_de str, value: __SdavePolicy, \
         #[sdave(default = __sdave_field_0)] defaulted: u32 }",
        Derive::Deserialize,
    );
    let code = compact(&implementation);
    assert!(
        code.contains("impl<'__sdave_de_,'__sdave_de,__SdavePolicy,const__sdave_record:usize>")
    );
    assert!(
        code.contains("Deserialize<'__sdave_de_>forS<'__sdave_de,__SdavePolicy,__sdave_record>")
    );
    assert!(code.contains("'__sdave_de_:'__sdave_de"));
    assert!(code.contains("where__SdavePolicy:Clone"));
    let value = method(&implementation, "deserialize_value");
    assert_eq!(
        value.sig.generics.type_params().next().unwrap().ident,
        "__SdavePolicy_"
    );
    let body = compact(&value.block);
    assert!(body.contains("letmut__sdave_record_="));
    assert!(body.contains("let__sdave_field_0_="));
    assert!(body.contains("__sdave_field_0()"));
}

#[test]
fn source_order_and_unraw_identifiers_are_the_naming_authority() {
    let struct_impl = implementation(
        "struct S { z: u32, r#type: u32, a: u32 }",
        Derive::Serialize,
    );
    let body = compact(&method(&struct_impl, "serialize_value").block);
    let z = body.find(".field(\"z\"").unwrap();
    let raw = body.find(".field(\"type\"").unwrap();
    let a = body.find(".field(\"a\"").unwrap();
    assert!(z < raw && raw < a);
    assert!(body.contains("&self.r#type"));
    assert!(!body.contains("\"r#type\""));
    for derive in [Derive::Serialize, Derive::Deserialize] {
        let implementation = implementation("enum E { r#match { r#type: u32 } }", derive);
        let code = compact(&implementation);
        assert!(code.contains("\"match\"") && code.contains("\"type\""));
        assert!(!code.contains("\"r#match\"") && !code.contains("\"r#type\""));
    }
}

#[test]
fn tuple_structs_keep_one_item_envelope_per_field_even_for_newtypes() {
    for source in ["struct T();", "struct T(u32);", "struct T(u32, String);"] {
        let serialize = implementation(source, Derive::Serialize);
        let deserialize = implementation(source, Derive::Deserialize);
        assert!(compact(&serialize).contains("::sdave::Shape::Tuple"));
        let serialize_value = compact(&method(&serialize, "serialize_value").block);
        assert!(
            serialize_value.contains("::serialize_payload(self,")
                && serialize_value.contains(".frame(")
        );
        let deserialize_value = compact(&method(&deserialize, "deserialize_value").block);
        assert!(
            deserialize_value.contains(".single()?")
                && deserialize_value.contains("::deserialize_payload(")
        );
        let payload = compact(&method(&deserialize, "deserialize_payload").block);
        assert!(payload.contains(".run(") && payload.contains(".exact("));
        assert!(!payload.contains(".unwrap(") && !payload.contains(".expect("));
    }
    let serialize = implementation("struct T(u32, String);", Derive::Serialize);
    let payload = compact(&method(&serialize, "serialize_payload").block);
    assert_eq!(payload.match_indices(".item(").count(), 2);
    let deserialize = implementation("struct T(u32, String);", Derive::Deserialize);
    let payload = compact(&method(&deserialize, "deserialize_payload").block);
    assert_eq!(payload.match_indices(".at_index(").count(), 2);
}

#[test]
fn unit_and_named_structs_retain_their_record_payload_envelope() {
    for source in ["struct Unit;", "struct Empty {}", "struct Named { x: u32 }"] {
        for derive in [Derive::Serialize, Derive::Deserialize] {
            let implementation = implementation(source, derive);
            assert!(compact(&implementation).contains("::sdave::Shape::Record"));
            assert_eq!(
                implementation
                    .items
                    .iter()
                    .filter(|item| matches!(item, ImplItem::Fn(_)))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn enum_branches_validate_shape_before_decoding_and_add_variant_context() {
    let implementation = implementation(
        "enum E { Unit, Newtype(u32), Many(u32, String), EmptyTuple(), \
         EmptyRecord {}, Named { required: u32, #[sdave(default = make)] optional: u32 } }",
        Derive::Deserialize,
    );
    let body = compact(&method(&implementation, "deserialize_value").block);
    assert_eq!(body.match_indices(".in_variant(").count(), 6);
    assert_eq!(body.match_indices(".associated()?").count(), 5);
    assert_eq!(body.match_indices(".unit()?").count(), 1);
    assert_eq!(body.match_indices(".record(").count(), 2);
    assert!(body.contains(".unknown()"));
    let newtype = &body[body.find("\"Newtype\"=>").unwrap()..body.find("\"Many\"=>").unwrap()];
    assert!(newtype.find(".associated()?").unwrap() < newtype.find(".payload::<").unwrap());
    let many = &body[body.find("\"Many\"=>").unwrap()..body.find("\"EmptyTuple\"=>").unwrap()];
    assert!(many.find(".associated()?").unwrap() < many.find(".exact(").unwrap());
    assert!(many.find(".exact(").unwrap() < many.find(".payload::<").unwrap());
    let named = &body[body.find("\"Named\"=>").unwrap()..];
    assert!(named.find(".finish()?").unwrap() < named.find(".require_present(").unwrap());
    assert!(named.find(".require_present(").unwrap() < named.find("make()").unwrap());
}

#[test]
fn empty_enums_generate_valid_impl_syntax() {
    let serialize = implementation("enum Never {}", Derive::Serialize);
    assert_eq!(
        compact(&method(&serialize, "serialize_value").block),
        "{match*self{}}"
    );
    let deserialize = implementation("enum Never {}", Derive::Deserialize);
    assert!(compact(&method(&deserialize, "deserialize_value").block).contains(".unknown()"));
}
