# sdave-derive

Native `Serialize` / `Deserialize` derives for [SDAVE](https://crates.io/crates/sdave).

This crate is a proc-macro companion to `sdave`. It turns ordinary Rust structs
and enums into SDAVE documents without any intermediate data model and without
Serde. The wire names are the Rust source names — there is no separate
name registry to keep in sync.

You normally do **not** depend on `sdave-derive` directly: the `sdave` crate
re-exports both derives behind its default `derive` feature.

```toml
[dependencies]
sdave = "0.2"
```

```rust
use sdave::{Deserialize, Serialize};
```

The generated code refers to `::sdave`, so the path `sdave` must resolve while
the derive expands. If you rename the dependency, make the original name
available as well:

```rust
extern crate my_codec as sdave;
```

## Quick start

```rust
use sdave::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ToolCall {
    name: String,
    #[sdave(default)]
    arguments: Vec<String>,
    #[sdave(default = default_timeout_ms)]
    timeout_ms: Option<u64>,
}

fn default_timeout_ms() -> Option<u64> {
    Some(30_000)
}

let call = ToolCall {
    name: "search".into(),
    arguments: vec!["rust".into()],
    timeout_ms: None,
};

let wire = sdave::to_string(&call).unwrap();
let back = sdave::from_str::<ToolCall>(&wire).unwrap();
assert_eq!(back, call);
```

`to_string` / `from_str` / `to_vec` / `from_slice` use the default naming policy
and the default V2 framing profile. For a custom, compile-time `NamePolicy` or a
different `Config` (limiter pairs, variant, limits), construct a
`sdave::Serializer` / `sdave::Deserializer` directly; child codecs inherit that
policy and profile throughout the document.

## Attributes

| Attribute | Site | Meaning |
| --- | --- | --- |
| `#[sdave(basic)]` | type | Treat the whole type as opaque and delegate to its `sdave::BasicType` implementation. |
| `#[sdave(default)]` | named field | Use `<FieldType as Default>::default()` for an absent field. |
| `#[sdave(default = path)]` | named field | Call `path()` for an absent field. |

- `path` is a Rust path, not a string, and must be a zero-argument function
  returning the field's complete type. A custom provider does not require the
  field type to implement `Default`; `#[sdave(default)]` does.
- Defaults are **deserialization-only**. Serialization always writes every field
  in declaration order; there is no skip-on-default behaviour.
- Defaults apply only to a genuinely absent field. A supplied `None`, an empty
  string, or an empty `Vec` is a present value and never triggers the provider.
- Defaults on positional (tuple-struct / tuple-variant) fields, and
  `#[sdave(default)]` at struct or variant level, are compile-time errors.

Anything else is rejected at compile time rather than guessed at: unknown
`sdave` attributes, `rename`, `skip`, `#[sdave(basic = ...)]`, `union`s, and
malformed attribute forms all fail the build with a targeted message.

## Shape mapping

The declared Rust shape selects the wire shape; `sdave::Shape` is generated as
an associated constant so the type and framing are known before any value is
inspected — which is what makes empty vectors and unit variants decodable.

| Declaration | Wire shape | Framing |
| --- | --- | --- |
| Named struct | `Record` | one record envelope of `Name: Type` field runs |
| Unit struct | `Record` | one empty record envelope |
| Tuple struct / newtype / zero-field tuple struct | `Tuple` | one value envelope holding positional item envelopes, one per field |
| Unit variant `V` | `Enum` | variant envelope only |
| Single-field variant `V(T)` | `Enum` | variant envelope + one associated-value envelope holding `T`'s payload |
| Multi-field variant `V(A, B)` | `Enum` | variant envelope + one associated-value envelope holding a positional tuple body |
| Named-field variant `V { x, y }` | `Enum` | variant envelope + one associated-value envelope holding a context-known record envelope |

`V`, `V()`, and `V {}` keep distinct framing: a zero-field tuple variant is not
the same as a unit variant.

Nested aggregates elide the redundant type marker only when the surrounding
context already supplies the type. A field header such as `inner: InnerStruct`
declares the child's type, so the child's envelope holds its record fields
directly instead of repeating the `InnerStruct` marker. A standalone document
root always emits its own marker.

Containers and leaves are handled by the `sdave` library rather than this
derive: `String`, `&str`, `sdave::Bytes` / `sdave::ByteBuf`, the numeric and
scalar primitives, `NonMaxUsize` and the standard nonzero types, `Vec<T>`,
`Option<T>`, `Box<T>`, `Result<T, E>`, `()`, and tuples. `Vec<T>` frames a
mandatory empty header envelope followed by one item envelope per element.

## Naming policy: abbreviating constructor paths

A type marker is taken from `std::any::type_name::<T>()`, so by default an
application type is written fully qualified, e.g. `my_app::Message`. That is
self-describing but verbose. An application can shorten it with an immutable,
compile-time [`NamePolicy`](https://docs.rs/sdave/latest/sdave/trait.NamePolicy.html):

```rust
use sdave::{Config, Deserializer, NamePolicy, Serializer, ShortType};

struct AppNames;

impl NamePolicy for AppNames {
    const SHORT_TYPES: &'static [ShortType] = &[
        ShortType::of::<String>(),
        ShortType::of::<Vec<()>>(),
        ShortType::of::<Option<()>>(),
        // Selected by an exemplar; only the short name `Message` is emitted.
        ShortType::of::<my_app::Message>(),
    ];
}

let mut serializer = Serializer::<AppNames>::new(Config::default()).unwrap();
let wire = serializer.to_vec(&value).unwrap();

let mut deserializer = Deserializer::<AppNames>::new(Config::default()).unwrap();
let back: my_app::Message = deserializer.from_slice(&wire).unwrap();
```

Rules:

- Start from each type constructor's fully qualified name. If that constructor's
  path is whitelisted, emit its short (last-identifier) name; otherwise keep the
  fully qualified path. Every type argument is rendered independently by the
  same rule.
- Entries are **type exemplars**, not names: `ShortType::of::<Vec<()>>()` selects
  the `Vec` constructor and then covers every `Vec<T>`. Generic arguments do not
  select anything themselves, and matching uses the complete constructor path,
  not a suffix — whitelisting `my_app::Message` does not abbreviate
  `other_crate::Message`.
- Shortening does not propagate across nesting: whitelisting `Vec` and `String`
  but not an application type yields `Vec<my_app::Message>`, not a fully
  qualified `alloc::vec::Vec<...>`.
- The list is only an abbreviation filter. Entries cannot supply replacement
  names; the sole possible abbreviation is the last identifier of the
  compiler-reported path. Primitives are already short and need no entry.
- Two distinct constructors that would abbreviate to the same short name are
  rejected as an `InvalidNamePolicy` error rather than resolved by order.
- The list is a compile-time constant and the selected policy is immutable for
  the whole lifetime of a `Serializer` / `Deserializer`, including nested child
  and basic-helper operations. There is no mutable global name table.
- Type aliases are resolved by the compiler; an alias's source spelling is not
  preserved in the marker.

Sender and receiver must agree on the policy: an incoming marker is compared
against the policy-rendered expectation, so a qualified spelling cannot bypass a
policy that requires the short spelling, and vice versa. The `sdave::to_vec` /
`to_string` / `from_slice` / `from_str` helpers use `DefaultNames`, which
already abbreviates `String`, `Vec`, `Option`, `Result`, `Box`, and
`NonZeroU32`. Use `sdave::type_marker::<T, P>()` to inspect a rendered marker.

## Decoding rules

- Named fields are matched **by name**, in any order. Duplicate, unknown, and
  missing fields are errors, never silently dropped or defaulted.
- Every field header's `Name: Type` marker is validated before its value run is
  read.
- Providers for absent fields run only after the record is confirmed complete
  and all required-field presence has been validated, and at most once each.
- Unknown variants, missing or extra envelopes for a variant's declared arity,
  nonempty or missing list headers, and truncated frames are all errors.
- Decoded strings and basic payloads are borrowed verbatim from the input where
  possible; leaf bytes are never escaped or rewritten.

Generic derives add codec bounds only for field types that actually mention a
generic parameter, and skip `Self`, so recursive types such as
`struct Node { next: Option<Box<Node>> }` compile without unbounded or
self-referential requirements. `#[sdave(basic)]` types place no codec bounds on
their storage fields at all.

## Basic / opaque types

`#[sdave(basic)]` stops automatic field traversal and requires the type to
implement `sdave::BasicType`. Both helpers are fallible and must not panic;
they work with the enclosing envelope's payload rather than adding an outer
marker:

```rust
use sdave::{BasicType, Deserialize, Deserializer, NamePolicy, Payload, Result, Serialize, Serializer};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[sdave(basic)]
struct NonMaxU32 {
    inner: std::num::NonZeroU32,
}

impl BasicType for NonMaxU32 {
    fn encode_basic<P: NamePolicy>(&self, serializer: &mut Serializer<P>) -> Result<Vec<u8>> {
        // semantic 0 is stored as NonZeroU32(1); encode the meaning, not the carrier
        serializer.payload(&(self.inner.get() - 1))
    }

    fn decode_basic<P: NamePolicy>(payload: Payload<'_>, decoder: &mut Deserializer<P>) -> Result<Self> {
        let semantic = decoder.payload::<u32>(payload)?;
        let inner = semantic
            .checked_add(1)
            .and_then(std::num::NonZeroU32::new)
            .ok_or_else(|| sdave::error_payload!("NonMaxU32 excludes u32::MAX"))?;
        Ok(Self { inner })
    }
}
```

## Scope and relation to the flat protocol

This layer sits above SDAVE's flat framing: `FlatParser` stays payload- and
nesting-agnostic, and recursive interpretation lives in the typed codec. A
document is one type-marked value, so the typed decoder does not treat arbitrary
mixed-channel text as valid struct metadata. The design rationale and the full
wire grammar are described in the `sdave` documentation and the protocol
specification in the repository.

Errors carry an absolute input byte offset plus an outer-to-inner path
(`Field` / `Variant` / `Index`), so a failure points at the offending envelope.

## License

MIT, matching the SDAVE workspace.
