# Rust Struct Serialization and Deserialization

## Status and scope

This document records the design for the native Rust serialization and
deserialization layer being planned for SDAVE 0.2.1. It is a design document,
not a claim that the APIs or derives below are implemented.

**Agreed rules** are stated with MUST, MUST NOT, and SHOULD. Sections explicitly
marked **Proposed** or **Open** identify details that still need to be frozen
before implementation. Public trait names and API sketches are provisional.

The goals are:

- Readable, LLM-oriented serialization of Rust values.
- One naming authority: Rust types and source identifiers, not separately
  maintained wire-name declarations.
- Metadata in `NonEnvelop`; values and nested serialization in `Envelop`.
- Verbatim framing, without escaping string or opaque payload bytes.
- Native, statically typed serialization/deserialization contracts and derives.
- Explicit application-controlled codecs for semantic/basic types.

The design does **not** use Serde as its serialization backend. The native
contracts need type/shape information before inspecting values, including empty
collections and unit enum variants.

This layer sits above the existing flat framing protocol. `FlatParser` remains
payload-agnostic and nesting-agnostic; recursive interpretation belongs to the
Rust codec. The fundamental protocol's support for mixed channels is unchanged.
A typed decoder must not silently treat arbitrary mixed-channel text as valid
struct metadata.

## 1. Notation and type information

The following notation describes logical structure, not literal delimiters:

```text
N(metadata)       NonEnvelop containing metadata
E(payload)        one real, completed Envelop
E(empty)          one real Envelop with an empty payload
P_T(value)        a value's payload when its type T is supplied by its context
S_T(value)        a standalone, type-marked serialization of a named struct
C_T(value)        a context-known named struct: E(named_field_runs), without N(T)
```

Nested `E(...)` means an envelope containing encoded child envelopes. Repeated
sibling envelopes are at the same parsing level. Insignificant formatting gaps
are omitted from these diagrams.

Concrete examples use `~~...^^` and `??...!!` as illustrative framing. Repeats
may increase to protect nested content. They do not prescribe the default
limiter set. Examples without confirmation suffixes are V1 examples; V2 also
requires the confirmation described in section 11.

Short names in examples assume an appropriate whitelist. Application types
otherwise use their fully qualified Rust names.

A type marker MUST be a nonempty slice after the permitted metadata trimming.
Markers are readable names/expressions, not one-character kind tags.

The expected Rust type and generated metadata determine the shape to decode.
The decoder MUST NOT infer types from payload text or infer scalar versus list
from the number of envelopes. A string containing `42` remains a string.
Type names are not a dynamic string-to-Rust-type construction mechanism.

## 2. Rust-derived names and immutable whitelists

### 2.1 Naming authority

Real type names come from Rust's compiler-reported type information, such as
`std::any::type_name::<T>()`. Field and variant identifiers come automatically
from derive input. Applications MUST NOT be required to repeat those names in
traits or attributes.

No independent marker-name registry or arbitrary wire-name override is part of
this design. The virtual structs for named-field enum variants are an explicit
source-derived case described in section 8.

Type aliases are resolved by Rust; a compiler-reported name need not preserve an
alias's source spelling. For example, a nonzero integer alias may be reported
as an instantiation of the underlying generic nonzero type.

Rust does not guarantee that diagnostic type-name spelling is stable across
compiler versions or globally unique. The codec is statically typed, and this
design does not promise an independent cross-toolchain naming standard.

### 2.2 Constructor-based abbreviation

Start from each type constructor's fully qualified name:

- If its qualified constructor path is whitelisted, use its short name.
- Otherwise retain that constructor's fully qualified name.
- Render every type argument independently using the same rule.

Whitelist matching MUST use the complete constructor path, not a suffix and
not an entire concrete generic instantiation. A `Vec` entry covers every
`Vec<T>`, not just one selected element type.

Qualification MUST NOT propagate from a child to its parent or vice versa.
For a policy shortening `Vec` and `String`, but not application types:

```text
Vec<String>
Vec<my_app::Message>
Vec<Vec<my_app::Message>>
my_app::Wrapper<Vec<String>>
```

If `Vec` is not whitelisted but `String` is, the result is:

```text
alloc::vec::Vec<String>
```

An unwhitelisted `Message` MUST NOT cause a whitelisted `Vec` to fall back to
`alloc::vec::Vec<my_app::Message>`.

The whitelist is an abbreviation filter, not a source of replacement names.
Basic-type status and abbreviation are independent decisions.

### 2.3 Policy lifetime

The library supplies a default whitelist for widely used standard types.
Applications may own multiple alternative whitelists.

Each whitelist MUST be a compile-time constant. The selected whitelist MUST
remain immutable for the entire lifetime of a serializer/deserializer object,
including its nested operations. No mutable global registration or per-value
policy switching is allowed.

**Proposed API shape:** a policy type with an associated constant:

```rust
pub trait NamePolicy {
    const SHORT_TYPES: &'static [ShortType];
}
```

An illustrative application policy is:

```rust
struct AppNames;

impl NamePolicy for AppNames {
    const SHORT_TYPES: &'static [ShortType] = &[
        ShortType::of::<String>(),
        ShortType::of::<Vec<()>>(),
        ShortType::of::<Option<()>>(),
    ];
}
```

Here `Vec<()>` selects the `Vec` constructor; `()` is only an exemplar argument.
Type-based entries avoid handwritten copies of qualified paths. A const entry
can store a monomorphized `type_name::<T>` function pointer: the table is const
without requiring name evaluation or rendering itself to run in const context.

A bare `&'static` slice is not by itself proof of compile-time construction.
The public policy interface must enforce the const requirement.

Ambiguous abbreviation rules for distinct constructor paths MUST be rejected,
not resolved by order. Whitelisting both `a::Thing` and `b::Thing` must not
silently collapse them to the same short constructor name.

Sender and receiver must agree on the effective spelling of exchanged types.
A const policy freezes configuration, not Rust's spelling across toolchains.

### 2.4 Trait-object (`dyn`) spellings (prototype)

A type marker may be a trait object. Rust renders one as `dyn path::Trait`, optionally
behind `Box`, `&`, `Vec`, etc. SDAVE's type-expression parser accepts these spellings
(bare, `alloc::boxed::Box<dyn path::Trait>`, `&dyn path::Trait`, and nested) instead of
rejecting them.

- The erased marker is **`dyn ` followed by the same policy-rendered trait path as any
  constructor**. SDAVE owns this canonical form: the vtable of a `dyn` value cannot be
  generic over an application `NamePolicy`, so the `dyn` spelling is fixed by the parser
  and a whitelist only abbreviates the trait path like any other path.
- It is deterministic and is **one marker per trait regardless of the concrete
  implementor** -- that erasure is the point.
- It cannot collide with a concrete marker: a concrete type is never spelled with the
  `dyn` keyword. Two traits collide only if their paths abbreviate to one short name,
  which the existing ambiguous-abbreviation rule already rejects.
- Non-`dyn` spellings are unchanged byte for byte.

The value bytes behind a `dyn` field are produced by the application supplying
`impl Serialize for dyn Trait`, delegating through SDAVE's object-safe `DynSerialize`
hook. Because the vtable is not policy-generic, that inner document is rendered with the
fixed `DynNames` policy, while the outer framing, the `dyn` marker and the surrounding
field headers use the caller's policy. Trait-object bounds joined with `+`
(`dyn Trait + Send`) are not accepted by this prototype.

## 3. Metadata grammar and formatting

### 3.1 Trimming

Formatting characters are:

```rust
b' ', b'\t', b'\r', b'\n'
```

Deserialization strips runs consisting only of these characters from the head
and tail of `NonEnvelop` metadata. This is not general Unicode whitespace
normalization and does not remove arbitrary internal whitespace.

A `NonEnvelop` region that becomes empty after trimming is a formatting gap,
not a type marker. It MUST NOT start a new value or terminate a list. The
nonempty-marker requirement applies to meaningful metadata, not such gaps.

Generic decoding MUST NOT trim envelope payloads. This includes strings,
variant selectors, and the bytes supplied to basic-type helpers. Core payload
slices remain byte-identical; typed interpretation does not rewrite them.

### 3.2 Named-field separator

Named-field metadata has the form:

```text
field_name: TypeMarker
```

The separator is the exact two-byte slice `": "`. Split at that slice, not at
`":"`. Namespace separators `"::"` remain part of the type expression.

After outer metadata trimming, the field name must match its Rust source
identifier and the type must match the expected, policy-rendered type.
Separator alternatives MUST NOT be silently rewritten. For example,
`field:Type` is not the agreed separator syntax.

Type-only metadata, such as a standalone struct's root marker, is interpreted
in its type-only context rather than passed through named-field splitting.
Nested struct markers are omitted when the surrounding context supplies the
type, as specified in section 7.

### 3.3 Tuple type expressions

Types in a tuple are separated by `,`. Formatting characters adjacent to a
tuple-member comma are ignored. Thus comma padding alone does not distinguish:

```text
Option<(u32,u64)>
Option<(u32, u64)>
```

Parsing MUST respect nesting. A comma in an inner tuple or generic argument
list is not a separator for an enclosing tuple. Plain `split(',')` is not
sufficient for expressions such as `(u32, Option<(u32, u64)>)`.

Additional internal spacing rules, including generic-argument comma padding,
are not implicitly settled by this tuple rule; see section 14.

## 4. Single-value/basic shape

A single-value type such as `String` uses:

```text
N(type marker) E(payload)
```

A named field replaces the type-only header with its field header:

```text
N("name: String") E("hello")
```

`String` MUST be followed by exactly one value envelope at that level, not an
implicit list. Another sibling value envelope without another meaningful
marker is an error.

An empty payload is a present value:

```text
N("String") E(empty)       the empty string
```

String payloads are raw UTF-8, not quoted or escaped. An SDAVE-looking sequence
inside a string is not recursively interpreted by the Rust codec.

## 5. Homogeneous lists: `Vec<T>`

A vector uses:

```text
N("Vec<T>") E(empty) E(P_T(item_0)) E(P_T(item_1)) ...
            ^ header
```

For a field, its marker is `field_name: Vec<T>`.

The first empty envelope is mandatory. It is semantically part of the type
header and contributes no list element. Zero actual elements are permitted.
The constructor and element type are known before inspecting any element,
including for an empty vector.

Only that first envelope is the header. Later empty envelopes are real items.
For illustrative V1 framing:

| Representation | Meaning |
| --- | --- |
| `String~~hello^^` | `"hello"` |
| `String~~^^` | `""` |
| `Vec<String>~~^^` | `[]` |
| `Vec<String>~~^^~~hello^^` | `["hello"]` |
| `Vec<String>~~^^~~^^` | `[""]` |
| `Vec<String>~~^^~~hello^^~~world^^` | `["hello", "world"]` |

A missing header or a nonempty first envelope is an error. The codec MUST NOT
reinterpret the first nonempty item as a scalar.

A list runs until the next meaningful metadata region at that level or the
end of the enclosing payload/complete document. Formatting-only gaps do not
end it. On a live stream, receiving just the header does not prove that the
list is finished.

### Real header versus parser phantom

The list header is a real, framed empty envelope. It is distinct from the
synthetic zero-width `Envelop::is_phantom()` marker used by `FlatParser`.
The Rust codec recognizes a list header by position and empty payload, not by
`is_phantom()`.

Parser-synthetic markers and zero-width gaps are bookkeeping; real empty
envelopes MUST NOT be discarded globally.

## 6. Tuples and unit

A tuple uses one outer value envelope containing positional item envelopes:

```text
N("(T0, T1)") E(E(P_T0(item_0)) E(P_T1(item_1)))
```

For a named field, use `field_name: (T0, T1)` as its marker.

At the tuple-body level there are no additional type markers for its items.
Their types are already declared by the tuple type/context. Insignificant
formatting gaps are permitted; meaningful `NonEnvelop` metadata between tuple
items is not.

- Tuple arity MUST match exactly.
- Tuple positions remain ordered, unlike named struct fields.
- Each item occupies one item envelope, even if its body contains nested
  frames or a context-known struct record.
- No synthetic struct name is invented for a tuple.

`()` is the zero-item tuple. Its payload is empty, so an explicitly present
unit value still has an enclosing empty value envelope. A one-item tuple
`(T,)` remains different from `T`.

The maximum supported tuple arity is an implementation-scope decision still
listed in section 14.

## 7. Named structs and fields

A standalone named struct carries its own type marker because no surrounding
wire context supplies its type:

```text
S_S(value) = N(type_of_S) E(named_field_runs)
```

When the surrounding context identifies the struct type, omit that redundant
marker and serialize the record envelope directly:

```text
C_S(value) = E(named_field_runs)
```

For a named field, the header already declares the child's type:

```text
N("child: my_app::Child")
E(named_child_field_runs)
```

The field's value envelope is the child's record envelope. Do not add another
`N("my_app::Child")` or wrap another record envelope inside it merely to repeat
the child's standalone serialization.

For example:

```rust
struct InnerStruct {
    x: u32,
    y: u32,
}

struct OuterStruct {
    id: u32,
    inner: InnerStruct,
}
```

A concrete V1 serialization is:

```sdave
OuterStruct~~~id: u32??42!!inner: InnerStruct??x: u32~~6^^y: u32~~9^^!!^^^
```

`OuterStruct` supplies the standalone root type. The `inner: InnerStruct`
header supplies the nested type, so its envelope contains the `x` and `y`
field runs directly, without another `InnerStruct` marker.

Context means surrounding wire metadata or a selected enum variant, not just
the decoder's generic Rust type parameter. A standalone root MUST still emit
its marker; a context-known struct MUST NOT repeat it. Field names and field
type markers within the record are retained.

Marker elision does not generally flatten value boundaries. If a context-known
record is the payload of a separate tuple/vector item or enum-associated-value
envelope, its record envelope remains inside that enclosing envelope, as in
section 8.3. A struct marked basic instead uses section 10 and exposes no
structural field runs.

### Name-based decoding

Named fields use their source-derived names and independently formatted types.
Deserialization matches by name, not declaration position, and accepts any
field order. Fields of the same type remain distinguishable by their names.

The decoder validates the expected type marker before decoding the field's
value run. It tracks presence separately from the value, including when that
value is `None` or empty.

Duplicate fields, unknown fields, wrong type markers, and malformed value
runs are errors. They are not silently ignored or converted into defaults.

**Proposed canonical output:** emit all fields in source declaration order.
Default annotations affect missing-field deserialization, not automatic
omission of fields during serialization. No skip-by-default-value feature is
part of the agreed design.

## 8. Rust enums and `Option<T>`

Enums have two forms, selected by the declared variant:

```text
unit variant:
N(enum type) E(variant identifier)

data-bearing variant:
N(enum type) E(variant identifier) E(associated payload)
```

For a field, its header is `field_name: enum_type`.

The variant identifier comes directly from Rust source and is scoped to its
enum. It is an envelope payload, not a `NonEnvelop` type marker. The decoder
MUST NOT guess variant shape from how many frames happen to arrive.

For example:

```rust
enum Enum1<T> {
    E1,
    E2,
    E3(T),
    E4(T),
}
```

`E1` and `E2` have only the variant envelope. `E3` and `E4` require exactly one
additional value envelope containing `P_T(value)`.

Unknown variants, extra value envelopes for unit variants, and missing value
envelopes for data-bearing variants are errors. A data-bearing variant needs
its value envelope even when its associated payload is empty.

### 8.1 `Option<T>` is an ordinary enum

`None` is a unit variant. `Some(T)` carries one value.

```sdave
timeout_ms: Option<u64>~~Some^^~~500^^
```

For a tuple value:

```sdave
timeout_ms: Option<(u32, u64)>~~Some^^~~??500!!??600!!^^
```

For `None`:

```sdave
timeout_ms: Option<(u32, u64)>~~None^^
```

For an explicitly supplied unit value:

```sdave
demo: Option<()>~~Some^^~~^^
```

`Some(())` is not `None`: it has an additional real empty value envelope.
Omitting a field is different from both, as section 9 specifies.

### 8.2 Unnamed tuple variants

One unnamed field, such as `Some(T)` or `E3(T)`, uses that field's payload
within the one associated-value envelope.

Multiple unnamed fields, such as `E3(u32, u64)`, use a positional tuple body
inside the single associated-value envelope. They do not become a
variant-named struct, and do not emit separate sibling associated-value
envelopes.

The zero-field tuple-variant edge case, such as `E3()`, still needs an explicit
final convention; see section 14.

### 8.3 Named-field variants are variant-named virtual structs

For:

```rust
enum E {
    E1,
    E2 {
        x: u32,
        y: u64,
    },
}

struct S {
    foo: E,
}
```

Treat the inline record as a virtual struct named exactly `E2`. The enum's
variant envelope already identifies that record, so its associated-value
payload contains the context-known record envelope without another `E2` type
marker:

```text
N("foo: E")
E("E2")
E(
    E(
        N("x: u32") E("42")
        N("y: u64") E("255")
    )
)
```

A concrete V1 example is:

```sdave
foo: E~~E2^^~~~??x: u32~~42^^y: u64~~255^^!!^^^
```

The one `E2` selects the enum variant and supplies the virtual record's type
from context. There is no repeated `N("E2")`. The associated-value envelope
and the inner record envelope are both retained: marker elision does not
flatten the record's fields into the associated-value envelope.

The virtual schema name is generated from the variant's source identifier and
scoped by the selected enum. It is not emitted as a second wire marker in this
context. There is no actual Rust type for `type_name::<E2>()`, no user-declared
naming trait, and no need to introduce a public Rust struct named `E2`.

Ordinary named-struct rules apply inside it: fields may be reordered, named
fields may have defaults, and duplicates/unknown fields are errors. Decode the
record using the selected variant's field schema, not a second type selector.

An empty named-field variant retains the virtual-record form; it does not
silently become a unit variant. Field defaults cannot replace the entire
missing associated-value envelope.

## 9. Field-level defaults

Default behavior belongs to the deserialization derive and is field-level
only. The intended derive-helper spelling is:

```rust
#[sdave(default)]
#[sdave(default = function_path)]
```

An illustrative declaration is:

```rust
fn default_timeout_ms() -> Option<u64> {
    Some(30_000)
}

#[derive(sdave::Serialize, sdave::Deserialize)]
struct ToolCall {
    name: String,

    #[sdave(default)]
    arguments: Vec<String>,

    #[sdave(default = default_timeout_ms)]
    timeout_ms: Option<u64>,
}
```

| Annotation | Missing-field behavior |
| --- | --- |
| None | Error |
| `#[sdave(default)]` | Call the field type's `Default::default()` |
| `#[sdave(default = path)]` | Call `path()` |

A provider is a zero-argument Rust function returning the complete field type,
not an underlying representation. Use a Rust path directly, not a string
containing its name. Rust checks its signature. A custom provider does not
require the field type to implement `Default`.

Defaults apply only to absent fields. In particular:

- Explicit `None` is present and does not invoke an `Option<T>` field's
  default provider.
- An explicitly empty string or vector is present.
- An empty vector is represented by its mandatory empty header envelope,
  not by a missing field.
- Invalid values, incorrect markers, malformed list headers, and truncated
  envelopes are errors, never requests for a default.

Providers are resolved only after the enclosing struct is confirmed complete
and its supplied fields and required-field presence have been validated.
A streaming parser must not default fields that may still arrive. Each absent
field's provider is called once during successful construction, not eagerly
while collecting fields.

Struct-level default annotations are rejected. The same field rules apply to
named-field enum variants. Defaults on unnamed positional fields are not yet
specified; the proposed initial restriction is to reject them.

Providers are ordinary runtime callbacks and need not be const. Only naming
whitelists have the compile-time-const requirement. A separate fallible-default
provider API is not part of the agreed design.

## 10. Basic/opaque semantic types

### 10.1 Purpose and selection

A basic type stops the Rust struct codec from automatically traversing its
physical fields. It supplies a semantic payload codec instead.

**Proposed names and marker spelling:**

```rust
#[derive(sdave::Serialize, sdave::Deserialize)]
#[sdave(basic)]
struct NonMaxU32 {
    inner: std::num::NonZeroU32,
}
```

The application implements a trait provisionally called `BasicType` with
serialization and deserialization helpers. The type-level derive marker
selects that implementation. The exact public name and method signatures
remain an API decision, but both fallible helpers are required.

The basic derive branch MUST:

- Require the semantic helper implementation.
- Not traverse or serialize physical fields.
- Not require codec implementations for those fields just because they exist.
- Use one value envelope containing the helper's payload.
- Return helper errors without structural fallback or missing-field defaulting.

A marked named struct is opaque, not nested using the ordinary struct rule.
Its marker still names the original Rust type, filtered by the object's
whitelist; it is not replaced by its internal carrier or semantic primitive.
Generic type-name rendering is independent of physical-field traversal.

### 10.2 Semantic value, not storage representation

The helper must reflect the type's public meaning. For a `NonMaxU32` whose
semantic `0` is stored internally as `NonZeroU32(1)`, encode semantic `0`, not
the carrier's `1`:

```text
N("limit: app::NonMaxU32") E("0")
```

Decoding parses the semantic value and validates the inverse construction.
If the domain excludes `u32::MAX`, that input must return `Error`, not panic
inside a constructor or be interpreted as a private field.

Helpers produce/consume the enclosing envelope's payload, not another outer
type marker or outer envelope. Primitive payload helpers should be available
so applications do not accidentally call a complete type-marked serializer
when they only need a number's payload.

### 10.3 Fallibility and panic contract

Both helpers MUST return errors for invalid input, failed semantic conversion,
and ordinary output failures. They MUST NOT panic. Use checked/fallible
constructors and propagate errors instead of `unwrap`, `expect`, or unchecked
invariant assumptions.

Returning `Result` does not statically prove that arbitrary application code
cannot panic. Non-panicking behavior is a required implementation contract.
Library helper implementations need malformed-input and domain-boundary tests.
Panics are not a normal validation mechanism; catching unwinds is not a
substitute for this contract, especially with `panic = "abort"`.

### 10.4 Standard wrappers and application types

The library provides semantic/basic implementations for supported, widely
used standard wrappers, including nonzero numeric types. Applications mark
and implement their own semantic types, such as `NonMaxU32`; the library does
not inspect arbitrary application storage to guess those semantics.

Implementations follow Rust's orphan rules. Foreign types require an
implementation from a trait/type owner, library integration, or an
application-owned newtype.

A broad blanket public codec implementation must not conflict with derived
structural implementations. Explicit basic selection and reusable helper
code are preferred over relying on specialization. Exact impl strategy and
standard-wrapper coverage remain implementation/API work.

### 10.5 Map-like structs are collections, not conversion traits

`Map` in the design discussion is a placeholder for a struct semantically
behaving like `Map<K, V>`. It does **not** name a proposed conversion/mapping
trait or an existing standard Rust trait usable as `T: Map`.

A semantic helper for an actual map-like type works with logical keys/values,
not its private buckets, hash state, allocation metadata, or other physical
Rust fields. A generic Rust impl must target the actual container type with
appropriate bounds.

Basic means opaque to automatic physical-field traversal; it does not require
the semantic payload to be a single numeric token. A map helper may deliberately
encode structured entry data. If it invokes nested SDAVE encoding, it must
inherit the current object's immutable naming policy and framing context.

No universal map entry wire format has been agreed yet. Entry layout, key
restrictions, duplicate-key behavior, ordering, and exact supported standard
map types remain open. They MUST NOT be inferred from Rust's internal map
implementation or silently invented by the basic derive.

## 11. Framing, V2 confirmation, and parser integration

All real envelopes obey the existing limiter/delimiter protocol. The typed
codec must validate its profile and marker compatibility rather than changing
fundamental parser semantics.

- Metadata must really remain `NonEnvelop` under the configured limiter set.
- Empty envelopes must be representable. Equal/repetition-related limiter and
  delimiter units can make them impossible; an unusable profile is an error.
- Increasing repeat counts is not a remedy for every boundary collision.
- Individual `can_serialize` results do not establish that a whole ambiguous
  pair set parses unambiguously.
- Existing unsafe serialization helpers must be called only with their
  clean-boundary invariants established internally. The typed public API
  should not require applications to uphold those unsafe invariants.
- If no pair can frame a value, return an error; do not escape or alter it.

### V2 and formatting suffixes

V2 needs an actual non-delimiter continuation to confirm a closing delimiter.
EOF alone is not confirmation. Metadata trimming permits formatting characters
to serve as confirmation suffixes without becoming new type markers.

The writer must physically emit a suitable formatting character after the
closing delimiter, outside that envelope's payload. The chosen suffix must
not continue/prefix the selected delimiter or create an opening-limiter
boundary conflict under the configured profile.

For nested data, the final inner envelope must have its confirmation available
within the bounded parent payload. A parent closing delimiter outside that
slice must not be assumed available to a decoder of the slice. Emitting a
formatting suffix after each child envelope provides that confirmation inside
the parent while preserving the child's payload.

Whether an API uses V1 or V2, the exact default profile and canonical suffix
strategy still need to be selected. The literal examples above do not imply
V2 can accept EOF without a suffix.

### Completion and buffering

A pending envelope is not a completed value. EOF/truncation must not become a
successful struct with defaults. Formatting gaps do not hide unfinished
limiter/delimiter slices or malformed frames.

The existing serializer helpers require complete payloads. Nested encoding
therefore naturally buffers payloads before selecting outer delimiters.
A future writer API may still buffer; it must not imply arbitrary unknown
payloads can be serialized incrementally without collision planning.

Incremental framing and resumable construction of a Rust value are separate
features. The initial owned/borrowed, whole-input/prefix/streaming API scope is
not yet frozen.

## 12. Composition across payload contexts

These agreed invariants must be preserved by the implementation:

- A tuple/vector item occupies one enclosing item envelope.
- Nested frames belong inside it, never flattened into adjacent items.
- A tuple body has no immediate item type markers; its context supplies them.
- Standalone named structs carry a type marker. Context-known named structs
  and virtual variant structs omit that marker but retain their record
  envelope and named field metadata.
- A named struct field's header is followed directly by its record envelope.
  An enum-associated-value envelope still wraps the context-known record
  envelope, as shown in section 8.3.
- Basic payloads are handed to their helpers without structural interpretation.
- Empty headers, empty values, and absent fields are different concepts.

**Proposed completion of the payload table:**

| T | `P_T(value)` inside an item/associated-value envelope |
| --- | --- |
| Primitive/string/basic type | Raw semantic/helper payload |
| `()` | Empty |
| Tuple | Positional item envelopes |
| Ordinary named struct | `C_T(value)`: its record envelope with field runs, without its own type marker |
| Virtual named-field variant record | Context-known record envelope with field runs; the enum variant supplies its type |
| `Vec<U>` | Empty list-header envelope followed by item envelopes; no repeated vector marker |
| Enum | Variant envelope and optional value envelope; no repeated enum type marker |

The tuple, struct, basic, and variant-record cases follow the agreed rules.
The table explicitly proposes the remaining marker-elision rule for nested
vectors/enums when their type is already supplied by the surrounding context.
It needs final confirmation rather than ad hoc choices at individual call sites.

Examples needed to freeze and test it include:

```rust
Vec<Vec<String>>
Vec<Option<u64>>
(Option<u64>, Vec<String>)
Option<Vec<(u32, u64)>>
Vec<SomeNamedStruct>
```

For example, the proposed payload of one `Vec<Option<u64>>` item containing
`Some(500)` is `E("Some") E("500")` inside its one item envelope, not extra
vector-level siblings and not a repeated `Option<u64>` marker.

## 13. Implementation architecture and verification

**Proposed organization:** retain the current framing library and add a native
typed codec plus an optional derive proc-macro crate. No mandatory Serde
dependency is introduced. Trait, crate, feature, and method names need not be
frozen by this document's illustrative Rust code.

The implementation needs:

- Source/generated shape metadata available before inspecting a value.
- Static element-type information for empty generic collections.
- Separate standalone type-marked serialization, context-known record
  serialization, and enclosing item/associated-value payload operations.
  Eliding a type marker must not accidentally add a redundant field wrapper
  or remove a required item/associated-value envelope.
- A framing owner that handles headers, real empty frames, completion, and
  delimiter selection consistently across all type implementations.
- Lazy child metadata/hooks so recursive Rust types do not eagerly expand
  their schemas forever.
- Name-policy propagation to all nested codec/helper operations.
- Structured errors with useful position and field/variant context.
- Resource limits for depth, input/output size, elements, and delimiter work.

Wire construction details such as caching, buffer layout, and exact error
wording are implementation choices. Public trait signatures, borrowing
lifetimes, and helper contexts must be chosen before exposing the API.

### Acceptance tests

At minimum, test:

- The literal examples in this document, plus adaptation around collisions.
- Strings/basic payloads preserved without escaping or generic trimming.
- Scalars rejecting extra sibling envelopes.
- Empty vectors, one empty item, missing/nonempty list headers, and real empty
  envelopes versus parser-synthetic phantoms.
- Tuples with correct, missing, and extra items; nested tuple type commas.
- Unit, newtype, tuple, and named-field enum variants; `None` versus `Some(())`.
- Standalone struct markers versus context-known struct marker elision,
  including the direct nested-field example in section 7.
- Named enum variants with no repeated variant type marker, retaining both
  associated-value and record envelopes and validating the selected schema.
- Field permutations, required missing fields, duplicates, unknown fields,
  explicit `None`/empty values, and provider call counts.
- Custom default providers returning the full field type, not its carrier.
- Standard and application basic codecs, invalid domain values, and lack of
  physical-field codec bounds for marked basic structs.
- Per-constructor shortening, unknown inner types not qualifying parents,
  alternative const policies, and ambiguous abbreviation rules.
- Metadata head/tail formatting, whitespace-only gaps, exact `": "`, and
  namespace `"::"` preservation.
- V2 confirmation at every nested boundary and truncation at chunk boundaries.
- Unsupported attributes/type forms as compile-time failures, and limit
  violations/invalid payloads as errors rather than panics.

Typed decode followed by encode need not reproduce input field order,
formatting, or omission of defaulted fields. Verbatim framing and leaf bytes
must not be confused with a byte-identical logical struct round trip.

## 14. Remaining decisions before implementation

The following are deliberately not silently resolved here:

1. Final native trait/helper names and signatures, including basic helper
   context and ownership/borrowing lifetimes. How semantic map helpers invoke
   child codecs while preserving policy and resource limits.
2. Confirmation of the nested vector/enum payload table in section 12.
3. Primitive syntax: integer spellings, booleans, character validation,
   floating-point formatting, nonfinite values, signed zero, and binary data.
4. Exact initial type coverage: numeric families, strings/borrowing, supported
   tuple arity, tuple/newtype structs, arrays, slices, references, and standard
   wrappers/containers beyond the examples.
5. Map entry encoding, key/value bounds, ordering, duplicate-key behavior, and
   which actual map-like types have library helper implementations.
6. Raw-identifier spelling, any additional type-expression whitespace rules,
   and generic/const-argument forms supported by the name renderer. Do not
   broaden the agreed trimming or separator rules implicitly.
7. The zero-field tuple enum variant convention and whether unnamed
   positional fields must reject default attributes in the initial release.
8. Default limiter profile, V1/V2 API choice, custom-profile validation,
   confirmation-character strategy, and deterministic delimiter selection.
9. Whole-input versus prefix decoding, owned versus borrowed values, and
   whether resumable typed streaming belongs in the initial release.
10. Exact public error categories, resource-limit defaults, and compatibility
    guarantees for compiler-derived names and naming/framing profiles.

Source renames, changing structural/basic status, and changing abbreviation
policy can change the wire contract. Defaults enable selected missing-field
compatibility; they do not make unknown fields or unknown variants acceptable.

## 15. Public application-facing primitives (0.2.3)

An application that re-scans streaming payloads — for example, to surface a
partially received record before its enclosing envelope is confirmed — must apply
the same metadata grammar the codec uses. Publishing these primitives keeps one
source of truth, so an application's view cannot drift from what the codec
serializes and validates:

- `is_formatting`, `trim_metadata` / `trim_metadata_range` — the section 3.1
  ASCII-formatting trimming of a `NonEnvelop` metadata region. An empty trimmed
  region is a formatting gap, not a type or field marker.
- `parse_field_header` (with `FieldHeader`) — the section 3.2 named-field split
  at the exact `": "`. Namespace separators `"::"`, tuple and nested generic
  syntax, and virtual variant records stay inside the type expression;
  `field:Type` is rejected rather than rewritten.
- `Deserializer::check_marker` — compares an incoming marker with the
  policy-rendered marker for `T`, with the same tuple-padding equivalence used
  during decoding.

A framing profile also gains a stable identity:

- `Config`, `Limits` and `LimiterPair` derive `PartialEq`/`Eq`/`Hash` for
  in-memory comparison, and `Config::fingerprint()` returns a
  `ConfigFingerprint` reproducible across processes and releases (the standard
  library's `DefaultHasher` is not). The digest covers `variant`, the ordered
  limiter pairs, `limits` and the serializer's `PairSelect` policy; pair order is
  significant because SDAVE selects the first matching pair. The algorithm is
  part of the public contract and must remain stable across releases. The 0.3.0
  policy byte is appended after the 0.2.3 fields, so the existing layout is
  prefix-preserving, but appending it changes the digest of every profile
  (including profiles that keep the default policy).

### Serializer pair-selection policy (0.3.0)

Since 0.3.0 the serializer profile carries a `PairSelect` policy that chooses
which framing pair the serializer uses; the parser is unaffected and still takes
the first matching valid pair, so pair order remains significant and every
policy round-trips under the same profile. `FirstMatch` is the default and is the
pre-0.3.0 behaviour; `LeastRepeat` and `SmallestFrame` reduce delimiter repeats
and total envelope size respectively. The policy is consulted per framed
envelope and works over any ordered pair list a `Config` carries, including
profiles the library did not author.
