#![cfg(feature = "derive")]

use sdave::{Variant::V1, *};
use std::{
    cell::Cell,
    fmt::Debug,
    num::{NonZeroU32, NonZeroUsize},
};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct InnerStruct {
    x: u32,
    y: u32,
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct OuterStruct {
    id: u32,
    inner: InnerStruct,
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Enum1<T> {
    E1,
    E2,
    E3(T),
    E4(T),
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum E {
    E1,
    New(String),
    Pair(u32, u64),
    E2 { x: u32, y: u64 },
    EmptyTuple(),
    EmptyRecord {},
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct S {
    foo: E,
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Positional(u32, String);
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Newtype(String);
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct UnitStruct;
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Node {
    value: u32,
    children: Vec<Node>,
    next: Option<Box<Node>>,
}

thread_local! { static DEFAULT_CALLS: Cell<usize> = const { Cell::new(0) }; }
fn default_timeout_ms() -> Option<u64> {
    DEFAULT_CALLS.with(|calls| calls.set(calls.get() + 1));
    Some(30_000)
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ToolCall {
    name: String,
    #[sdave(default)]
    arguments: Vec<String>,
    #[sdave(default = default_timeout_ms)]
    timeout_ms: Option<u64>,
    required_last: u32,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[sdave(basic)]
struct NonMaxU32 {
    inner: NonZeroU32,
}
impl BasicType for NonMaxU32 {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        serializer.payload(&(self.inner.get() - 1))
    }
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        let semantic = decoder.payload::<u32>(payload)?;
        let inner = semantic
            .checked_add(1)
            .and_then(NonZeroU32::new)
            .ok_or_else(|| error_payload!("NonMaxU32 excludes u32::MAX"))?;
        Ok(Self { inner })
    }
}
#[derive(Debug, PartialEq)]
struct NoCodecStorage;
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[sdave(basic)]
struct Opaque {
    semantic: u32,
    storage: NoCodecStorage,
}
impl BasicType for Opaque {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        serializer.payload(&self.semantic)
    }
    fn decode_basic<P: NamePolicy>(
        payload: Payload<'_>,
        decoder: &mut Deserializer<P>,
    ) -> Result<Self> {
        Ok(Self {
            semantic: decoder.payload(payload)?,
            storage: NoCodecStorage,
        })
    }
}
fn default_opaque() -> Opaque {
    Opaque {
        semantic: 7,
        storage: NoCodecStorage,
    }
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct HasOpaque {
    #[sdave(default = default_opaque)]
    opaque: Opaque,
}

struct AppNames;
impl NamePolicy for AppNames {
    const SHORT_TYPES: &'static [ShortType] = &[
        ShortType::of::<String>(),
        ShortType::of::<Vec<()>>(),
        ShortType::of::<Option<()>>(),
        ShortType::of::<Box<()>>(),
        ShortType::of::<InnerStruct>(),
        ShortType::of::<OuterStruct>(),
        ShortType::of::<Enum1<()>>(),
        ShortType::of::<E>(),
        ShortType::of::<S>(),
        ShortType::of::<ToolCall>(),
        ShortType::of::<NonMaxU32>(),
        ShortType::of::<Opaque>(),
        ShortType::of::<HasOpaque>(),
    ];
}
fn pair(limiter: &[u8], delimiter: &[u8], repeat: usize) -> LimiterPair<u8> {
    LimiterPair {
        least_repeat: NonZeroUsize::new(repeat),
        limiter: limiter.to_vec(),
        delimiter: delimiter.to_vec(),
    }
}
fn profile(variant: Variant) -> Config {
    Config::new(
        variant,
        vec![
            pair(b"~", b"^", 2),
            pair(b"?", b"!", 2),
            pair(b"{", b"}", 2),
            pair(b"#", b"@", 2),
        ],
    )
    .unwrap()
}
fn decode<'de, T: Deserialize<'de>>(input: &'de [u8]) -> Result<T> {
    Deserializer::<AppNames>::new(profile(Variant::V1))?.from_slice(input)
}
fn document<T: Serialize>(payload: &[u8]) -> Vec<u8> {
    let mut serializer = Serializer::<AppNames>::new(profile(Variant::V1)).unwrap();
    let mut bytes = serializer.type_marker::<T>().unwrap().into_bytes();
    bytes.extend_from_slice(&serializer.frame(payload).unwrap());
    bytes
}
fn frames<'a>(input: &'a [u8], config: &Config) -> Vec<&'a [u8]> {
    let mut parser = FlatParser::new(input, config.variant(), config.limiter_pairs().to_vec());
    parser
        .iter()
        .filter_map(|state| match state {
            State::E { envelop } if !envelop.is_phantom() => {
                if config.variant() == V1 {
                    assert!(envelop.tail_offset().is_some(), "pending test frame");
                } else {
                    if envelop.tail_offset().is_none() {
                        assert_eq!(
                            envelop.payload_head_offset().get() - envelop.head_offset().get(),
                            input.len() - envelop.payload_tail_offset().get()
                        );
                    }
                }
                Some(
                    &input
                        [envelop.payload_head_offset().get()..envelop.payload_tail_offset().get()],
                )
            }
            _ => None,
        })
        .collect()
}
fn roundtrip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + Debug>(value: &T) {
    for variant in [Variant::V1, Variant::V2] {
        let config = profile(variant);
        let bytes = Serializer::<AppNames>::new(config.clone())
            .unwrap()
            .to_vec(value)
            .unwrap();
        let result = Deserializer::<AppNames>::new(config)
            .unwrap()
            .from_slice::<T>(&bytes)
            .unwrap();
        assert_eq!(&result, value);
    }
    let bytes = to_vec(value).unwrap();
    assert_eq!(&from_slice::<T>(&bytes).unwrap(), value);
}

#[test]
fn literal_scalar_and_vector_examples_and_real_headers() {
    assert_eq!(decode::<String>(b"String~~hello^^").unwrap(), "hello");
    assert_eq!(decode::<String>(b"String~~^^").unwrap(), "");
    for (wire, expected) in [
        ("Vec<String>~~^^", vec![]),
        ("Vec<String>~~^^~~hello^^", vec!["hello"]),
        ("Vec<String>~~^^~~^^", vec![""]),
        ("Vec<String>~~^^~~hello^^~~world^^", vec!["hello", "world"]),
    ] {
        assert_eq!(decode::<Vec<String>>(wire.as_bytes()).unwrap(), expected);
    }
    assert!(matches!(
        decode::<String>(b"String~~hello^^~~world^^")
            .unwrap_err()
            .kind,
        ErrorKind::EnvelopeCount {
            expected: 1,
            actual: 2
        }
    ));
    assert!(matches!(
        decode::<Vec<String>>(b"Vec<String>").unwrap_err().kind,
        ErrorKind::MissingListHeader
    ));
    assert!(matches!(
        decode::<Vec<String>>(b"Vec<String>~~hello^^")
            .unwrap_err()
            .kind,
        ErrorKind::NonemptyListHeader
    ));
    assert!(decode::<Vec<String>>(b"Vec<String>~~ \n^^").is_err());
    let mut parser = FlatParser::new(
        b"Vec<String>~~^^",
        Variant::V1,
        profile(Variant::V1).limiter_pairs().to_vec(),
    );
    parser.parse_incremental();
    assert!(parser.archived_boundaries()[0].is_phantom());
    assert!(!parser.archived_boundaries()[1].is_phantom());
}

#[test]
fn literal_nested_struct_omits_only_redundant_markers() {
    let expected = OuterStruct {
        id: 42,
        inner: InnerStruct { x: 6, y: 9 },
    };
    assert_eq!(
        decode::<OuterStruct>(
            b"OuterStruct~~~id: u32??42!!inner: InnerStruct??x: u32~~6^^y: u32~~9^^!!^^^"
        )
        .unwrap(),
        expected
    );
    let bytes = Serializer::<AppNames>::new(profile(Variant::V1))
        .unwrap()
        .to_vec(&expected)
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&bytes)
            .matches("InnerStruct")
            .count(),
        1
    );
    let root = frames(&bytes, &profile(Variant::V1));
    let fields = frames(root[0], &profile(Variant::V1));
    assert_eq!(
        frames(fields[1], &profile(Variant::V1)),
        [b"6".as_slice(), b"9"]
    );
    roundtrip(&expected);
}

#[test]
fn literal_options_tuples_and_variant_record_boundaries() {
    assert_eq!(
        decode::<Option<u64>>(b"Option<u64>~~Some^^~~500^^").unwrap(),
        Some(500)
    );
    assert_eq!(
        decode::<Option<(u32, u64)>>(b"Option<(u32, u64)>~~Some^^~~??500!!??600!!^^").unwrap(),
        Some((500, 600))
    );
    assert_eq!(
        decode::<Option<(u32, u64)>>(b"Option<(u32,u64)>~~None^^").unwrap(),
        None
    );
    assert_eq!(
        decode::<Option<()>>(b"Option<()>~~Some^^~~^^").unwrap(),
        Some(())
    );
    assert_eq!(
        decode::<Enum1<u32>>(b"Enum1<u32>~~E1^^").unwrap(),
        Enum1::E1
    );
    assert_eq!(
        decode::<Enum1<u32>>(b"Enum1<u32>~~E3^^~~42^^").unwrap(),
        Enum1::E3(42)
    );
    let wire = document::<S>(b"foo: E~~E2^^~~~??x: u32~~42^^y: u64~~255^^!!^^^");
    assert_eq!(
        decode::<S>(&wire).unwrap(),
        S {
            foo: E::E2 { x: 42, y: 255 }
        }
    );
    let value = E::E2 { x: 42, y: 255 };
    let config = profile(Variant::V1);
    let bytes = Serializer::<AppNames>::new(config.clone())
        .unwrap()
        .to_vec(&value)
        .unwrap();
    let enum_run = frames(&bytes, &config);
    assert_eq!(enum_run.len(), 2);
    assert_eq!(enum_run[0], b"E2");
    let record = frames(enum_run[1], &config);
    assert_eq!(record.len(), 1);
    assert!(!String::from_utf8_lossy(record[0]).contains("E2"));
    assert_eq!(frames(record[0], &config), [b"42".as_slice(), b"255"]);
}

#[test]
fn all_variant_and_positional_forms_have_declared_arity() {
    for value in [
        E::E1,
        E::New("".into()),
        E::Pair(1, 2),
        E::E2 { x: 3, y: 4 },
        E::EmptyTuple(),
        E::EmptyRecord {},
    ] {
        roundtrip(&value);
    }
    roundtrip(&Positional(7, "nested".into()));
    roundtrip(&Newtype("newtype".into()));
    roundtrip(&UnitStruct);
    roundtrip(&());
    roundtrip(&(42u32,));
    for bad in [
        b"E~~E1^^~~^^".as_slice(),
        b"E~~New^^",
        b"E~~EmptyTuple^^",
        b"E~~EmptyRecord^^",
        b"E~~ E1 ^^",
        b"E~~Unknown^^",
    ] {
        assert!(decode::<E>(bad).is_err(), "{bad:?}");
    }
    for body in [b"??1!!".as_slice(), b"??1!!??2!!??3!!", b"u32??1!!??2!!"] {
        assert!(decode::<(u32, u64)>(&document::<(u32, u64)>(body)).is_err());
    }
    assert_eq!(
        decode::<(u32, u64)>(b"(u32 ,\t\n u64)~~??1!!??2!!^^").unwrap(),
        (1, 2)
    );
}

#[test]
fn nested_payload_table_preserves_item_boundaries() {
    roundtrip(&vec![vec!["hello".to_owned(), "".into()], vec![]]);
    roundtrip(&vec![Some(500u64), None]);
    roundtrip(&(Some(500u64), vec!["one".to_owned(), "two".into()]));
    roundtrip(&Some(vec![(1u32, 2u64), (3, 4)]));
    roundtrip(&vec![
        InnerStruct { x: 1, y: 2 },
        InnerStruct { x: 3, y: 4 },
    ]);
    roundtrip(&Some(InnerStruct { x: 5, y: 6 }));
    roundtrip(&std::result::Result::<InnerStruct, String>::Ok(
        InnerStruct { x: 1, y: 2 },
    ));
    let config = profile(Variant::V1);
    let bytes = Serializer::<AppNames>::new(config.clone())
        .unwrap()
        .to_vec(&vec![Some(500u64)])
        .unwrap();
    let list = frames(&bytes, &config);
    assert_eq!(list.len(), 2);
    assert!(list[0].is_empty());
    assert_eq!(frames(list[1], &config), [b"Some".as_slice(), b"500"]);
    let records = Serializer::<AppNames>::new(config.clone())
        .unwrap()
        .to_vec(&vec![InnerStruct { x: 1, y: 2 }])
        .unwrap();
    assert_eq!(frames(frames(&records, &config)[1], &config).len(), 1);
}

#[test]
fn named_fields_are_reordered_but_never_ignored_or_defaulted_on_error() {
    let wire = document::<InnerStruct>(b"y: u32~~9^^x: u32~~6^^");
    assert_eq!(
        decode::<InnerStruct>(&wire).unwrap(),
        InnerStruct { x: 6, y: 9 }
    );
    for (body, check) in [
        ("x: u32~~1^^", "missing"),
        ("x: u32~~1^^x: u32~~2^^y: u32~~3^^", "duplicate"),
        ("x: u32~~1^^y: u32~~2^^z: u32~~3^^", "unknown"),
        ("x: u64~~1^^y: u32~~2^^", "type"),
        ("x:u32~~1^^y: u32~~2^^", "separator"),
        ("x:\tu32~~1^^y: u32~~2^^", "separator"),
    ] {
        let error = decode::<InnerStruct>(&document::<InnerStruct>(body.as_bytes())).unwrap_err();
        assert!(error.offset.is_some(), "{check}: {error}");
        match check {
            "missing" => assert!(matches!(error.kind, ErrorKind::MissingField(_))),
            "duplicate" => assert!(matches!(error.kind, ErrorKind::DuplicateField(_))),
            "unknown" => assert!(matches!(error.kind, ErrorKind::UnknownField(_))),
            "type" => assert!(matches!(error.kind, ErrorKind::TypeMismatch { .. })),
            _ => assert!(matches!(error.kind, ErrorKind::InvalidFieldHeader(_))),
        }
    }
}

#[test]
fn defaults_wait_for_completion_validation_and_all_required_fields() {
    DEFAULT_CALLS.with(|calls| calls.set(0));
    let wire = document::<ToolCall>(b"required_last: u32~~7^^name: String~~tool^^");
    let call = decode::<ToolCall>(&wire).unwrap();
    assert_eq!(call.timeout_ms, Some(30_000));
    assert!(call.arguments.is_empty());
    DEFAULT_CALLS.with(|calls| assert_eq!(calls.get(), 1));
    let present = document::<ToolCall>(b"timeout_ms: Option<u64>~~None^^arguments: Vec<String>~~^^name: String~~^^required_last: u32~~0^^");
    let call = decode::<ToolCall>(&present).unwrap();
    assert_eq!(call.timeout_ms, None);
    assert_eq!(call.name, "");
    DEFAULT_CALLS.with(|calls| assert_eq!(calls.get(), 1));
    for body in [
        b"name: String~~tool^^".as_slice(),
        b"name: String~~tool^^required_last: u32~~bad^^",
        b"name: String~~tool^^required_last: u32~~0^^unknown: u32~~7^^",
        b"name: String~~tool^^required_last: u32~~0^^arguments: Vec<String>~~not-a-header^^",
        b"name: String~~tool^^required_last: u32~~0^^timeout_ms: Option<u64>~~Some^^",
        b"name: String~~tool^^required_last: u32~~0^^timeout_ms: Option<u32>~~None^^",
        b"name: String~~tool^^required_last: u32~~0^^timeout_ms: Option<u64>~~None",
    ] {
        assert!(decode::<ToolCall>(&document::<ToolCall>(body)).is_err());
        DEFAULT_CALLS.with(|calls| assert_eq!(calls.get(), 1));
    }
    let encoded = Serializer::<AppNames>::new(profile(Variant::V1))
        .unwrap()
        .to_string(&call)
        .unwrap();
    assert!(encoded.contains("arguments: Vec<String>"));
    assert!(encoded.contains("timeout_ms: Option<u64>"));
    let defaulted = decode::<HasOpaque>(&document::<HasOpaque>(b"")).unwrap();
    assert_eq!(defaulted.opaque.semantic, 7); // Provider returns the complete non-Default type.
}

#[test]
fn basic_helpers_use_semantic_values_and_do_not_traverse_storage() {
    let zero = NonMaxU32 {
        inner: NonZeroU32::new(1).unwrap(),
    };
    let wire = Serializer::<AppNames>::new(profile(Variant::V1))
        .unwrap()
        .to_vec(&zero)
        .unwrap();
    assert_eq!(frames(&wire, &profile(Variant::V1)), [b"0".as_slice()]);
    assert_eq!(decode::<NonMaxU32>(&wire).unwrap(), zero);
    assert!(decode::<NonMaxU32>(&document::<NonMaxU32>(u32::MAX.to_string().as_bytes())).is_err());
    assert!(decode::<HasOpaque>(&document::<HasOpaque>(b"opaque: Opaque~~bad^^")).is_err());
    roundtrip(&Opaque {
        semantic: 42,
        storage: NoCodecStorage,
    });
    roundtrip(&NonZeroU32::new(u32::MAX).unwrap());
    assert!(decode::<NonZeroU32>(&document::<NonZeroU32>(b"0")).is_err());
    let invalid = document::<NonMaxUsize>(usize::MAX.to_string().as_bytes());
    assert!(decode::<NonMaxUsize>(&invalid).is_err());
}

#[test]
fn metadata_trimming_is_ascii_only_and_payloads_are_verbatim() {
    let formatted = b" \r\n Vec<String> \t ~~^^\t\n~~hello^^\n~~^^ \r ";
    assert_eq!(decode::<Vec<String>>(formatted).unwrap(), ["hello", ""]);
    assert_eq!(
        decode::<String>(b"String~~ \t\r\n42 ~~^^").unwrap(),
        " \t\r\n42 ~~"
    );
    assert!(decode::<u32>(b"u32~~ 42 ^^ ").is_err());
    assert!(decode::<String>("\u{a0}String~~hello^^".as_bytes()).is_err());
    assert!(decode::<String>(b"prose String~~hello^^").is_err());
    assert!(decode::<String>(b"String~~hello^^String~~world^^").is_err());
    assert!(decode::<Option<u32>>(b"Option<u32>~~ Some ^^~~42^^").is_err());
}

#[test]
fn v2_confirms_each_child_within_the_parent_and_rejects_every_truncation() {
    let value = ToolCall {
        name: "tool".into(),
        arguments: vec![],
        timeout_ms: None,
        required_last: 1,
    };
    let config = profile(Variant::V2);
    let bytes = Serializer::<AppNames>::new(config.clone())
        .unwrap()
        .to_vec(&value)
        .unwrap();
    let root = frames(&bytes, &config);
    assert_eq!(frames(root[0], &config).len(), 4);
    DEFAULT_CALLS.with(|calls| calls.set(0));
    for end in 0..bytes.len() {
        assert!(
            Deserializer::<AppNames>::new(config.clone())
                .unwrap()
                .from_slice::<ToolCall>(&bytes[..end])
                .is_err(),
            "accepted prefix at {end}"
        );
        DEFAULT_CALLS.with(|calls| assert_eq!(calls.get(), 0));
    }
    assert_eq!(
        Deserializer::<AppNames>::new(config)
            .unwrap()
            .from_slice::<ToolCall>(&bytes)
            .unwrap(),
        value
    );
    assert!(decode::<String>(b"String~~hello^^~").is_err());
}

#[test]
fn borrowed_leaves_references_and_recursive_types_work() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Borrowed<'a> {
        text: &'a str,
        bytes: Bytes<'a>,
    }
    let value = Borrowed {
        text: " \nhello♣",
        bytes: Bytes(&[0, 255, 128]),
    };
    let input = to_vec(&value)
        .unwrap_or_else(|error| panic!("{}: {error}", std::any::type_name::<Borrowed<'_>>()));
    let decoded: Borrowed<'_> = from_slice(&input).unwrap();
    assert_eq!(decoded, value);
    let text_start = decoded.text.as_ptr() as usize;
    assert!((input.as_ptr() as usize..input.as_ptr() as usize + input.len()).contains(&text_start));
    roundtrip(&Node {
        value: 1,
        children: vec![Node {
            value: 2,
            children: vec![],
            next: None,
        }],
        next: Some(Box::new(Node {
            value: 3,
            children: vec![],
            next: None,
        })),
    });
}

#[test]
fn collision_adaptation_preserves_utf8_and_arbitrary_binary_bytes() {
    let pieces = [
        "", "♦", "♣", "♥", "♠", "★", "☆", "♣♣x", "♣♣", "\n\t", "~~^^??!!", "🌍",
    ];
    for variant in [Variant::V1, Variant::V2] {
        let config = Config::default().with_variant(variant);
        let mut serializer = Serializer::<DefaultNames>::new(config.clone()).unwrap();
        let mut decoder = Deserializer::<DefaultNames>::new(config).unwrap();
        for head in pieces {
            for tail in pieces {
                let value = format!("{head}verbatim{tail}");
                let wire = serializer.to_vec(&value).unwrap();
                assert_eq!(decoder.from_slice::<String>(&wire).unwrap(), value);
            }
        }
        let value = ByteBuf((0..=255).collect());
        let wire = serializer.to_vec(&value).unwrap();
        assert_eq!(decoder.from_slice::<ByteBuf>(&wire).unwrap(), value);
        assert!(serializer.to_string(&value).is_err());
    }
    assert_eq!(to_string(&"♣".to_owned()).unwrap(), "String♦♣♣");
    let repeated = "♣".repeat(100);
    assert_eq!(
        from_slice::<String>(&to_vec(&repeated).unwrap()).unwrap(),
        repeated
    );
}

#[test]
fn primitive_validation_and_nonzero_domain_errors_are_not_panics() {
    for invalid in ["", "+1", " 1", "1 ", "-1", "4294967296", "1_000", "0x10"] {
        assert!(decode::<u32>(&document::<u32>(invalid.as_bytes())).is_err());
    }
    assert_eq!(decode::<i32>(&document::<i32>(b"-001")).unwrap(), -1);
    assert!(decode::<i8>(&document::<i8>(b"128")).is_err());
    assert!(decode::<bool>(&document::<bool>(b"True")).is_err());
    assert!(decode::<char>(&document::<char>(b"ab")).is_err());
    assert!(decode::<String>(&document::<String>(&[255])).is_err());
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(to_vec(&value).is_err());
    }
    for value in ["NaN", "inf", "-inf", "1e999", " 1.0"] {
        assert!(decode::<f64>(&document::<f64>(value.as_bytes())).is_err());
    }
    let negative_zero = from_slice::<f64>(&to_vec(&-0.0f64).unwrap()).unwrap();
    assert_eq!(negative_zero.to_bits(), (-0.0f64).to_bits());
    roundtrip(&'🌍');
    roundtrip(&u128::MAX);
    roundtrip(&i128::MIN);
}

#[test]
fn profiles_reject_ambiguity_and_marker_collisions_without_changing_flat_parser() {
    for pairs in [
        vec![],
        vec![pair(b"%", b"%", 2)],
        vec![pair(b"~", b"^", 2), pair(b"~", b"!", 2)],
        vec![pair(b"~", b"^", 2), pair(b"^", b"!", 2)],
        vec![pair(b"~~", b"^", 2)],
        vec![pair(b"~", b" ", 2)],
        vec![LimiterPair {
            least_repeat: None,
            limiter: b"~".to_vec(),
            delimiter: b"^".to_vec(),
        }],
    ] {
        assert!(matches!(
            Config::new(Variant::V1, pairs).unwrap_err().kind,
            ErrorKind::InvalidProfile(_)
        ));
    }
    let single = Config::new(Variant::V1, vec![pair(b"~", b"^", 2)]).unwrap();
    assert!(matches!(
        Serializer::<DefaultNames>::new(single)
            .unwrap()
            .to_vec(&"~head".to_owned())
            .unwrap_err()
            .kind,
        ErrorKind::NoDelimiter
    ));
    let colliding = Config::new(Variant::V1, vec![pair(b":", b"!", 2)]).unwrap();
    assert!(matches!(
        Serializer::<DefaultNames>::new(colliding)
            .unwrap()
            .to_vec(&InnerStruct { x: 1, y: 2 })
            .unwrap_err()
            .kind,
        ErrorKind::InvalidProfile(_)
    ));
}

#[test]
fn limits_and_output_failures_propagate_and_objects_reset_per_document() {
    for limits in [
        Limits {
            max_output_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_elements: 0,
            ..Limits::default()
        },
        Limits {
            max_delimiter_work: 0,
            ..Limits::default()
        },
        Limits {
            max_depth: 0,
            ..Limits::default()
        },
    ] {
        let config = Config::default().with_limits(limits).unwrap();
        assert!(matches!(
            Serializer::<DefaultNames>::new(config)
                .unwrap()
                .to_vec(&1u32)
                .unwrap_err()
                .kind,
            ErrorKind::LimitExceeded { .. }
        ));
    }
    let config = Config::default()
        .with_limits(Limits {
            max_input_bytes: 1,
            ..Limits::default()
        })
        .unwrap();
    let bytes = Serializer::<DefaultNames>::new(config.clone())
        .unwrap()
        .to_vec(&42u32)
        .unwrap();
    assert!(matches!(
        Deserializer::<DefaultNames>::new(config)
            .unwrap()
            .from_slice::<u32>(&bytes)
            .unwrap_err()
            .kind,
        ErrorKind::LimitExceeded {
            resource: "input bytes",
            ..
        }
    ));
    let config = Config::default()
        .with_limits(Limits {
            max_elements: 1,
            ..Limits::default()
        })
        .unwrap();
    let mut serializer = Serializer::<DefaultNames>::new(config.clone()).unwrap();
    let empty = serializer.to_vec(&Vec::<String>::new()).unwrap();
    assert!(serializer.to_vec(&vec!["item".to_owned()]).is_err());
    assert_eq!(serializer.to_vec(&Vec::<String>::new()).unwrap(), empty);
    assert_eq!(
        Deserializer::<DefaultNames>::new(config)
            .unwrap()
            .from_slice::<Vec<String>>(&empty)
            .unwrap(),
        Vec::<String>::new()
    );
    let config = Config::default()
        .with_limits(Limits {
            max_depth: 1,
            ..Limits::default()
        })
        .unwrap();
    let nested = to_vec(&(1u32, 2u32)).unwrap();
    assert!(
        Deserializer::<DefaultNames>::new(config.clone())
            .unwrap()
            .from_slice::<(u32, u32)>(&nested)
            .is_err()
    );
    assert!(
        Serializer::<DefaultNames>::new(config)
            .unwrap()
            .to_vec(&(1u32, 2u32))
            .is_err()
    );
    struct BrokenWriter;
    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("broken output"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(matches!(
        to_writer(BrokenWriter, &42u32).unwrap_err().kind,
        ErrorKind::Io(_)
    ));
}
