# sdave-macro

The **macro** crate of SDAVE: the derives and the attribute macro that turn ordinary Rust
declarations into SDAVE documents, with no intermediate data model and no Serde. Wire names are
the Rust source names; there is no separate name registry to keep in sync.

**Status: to be built — and it goes last.** Only this README exists. An expansion names the
`sdave-codec` runtime, so the codec's public surface — `Shape`, the `Serialize` / `Deserialize`
traits, the plan/write entries, the type-erased sink, `BasicType`, field-header emission and marker
validation — must exist, pass review and be verified **before** any macro work starts. The emitted
signatures follow from that surface, and the codec README carries the contract (its "Obligations
toward the derive" section). Do not guess the API here; implement against the codec as built.

## The rule that shapes everything: the macro emits allocation-free code

The generated code must be usable by a `no_std`, allocator-free consumer. Therefore every
expansion:

- writes through the codec's append API into the caller's buffer;
- emits **no** `Vec`, `String`, `Box`, `format!`, or any allocation of its own;
- never names an allocator type parameter, and never names `sdave-alloc`;
- names the runtime through **one** path (see *Emitted path*), so a single `extern crate` alias
  keeps it working.

This is achievable because the codec frames each level as it closes, in a single inner-to-outer
pass, and never needs a scratch region — so an expansion has nothing to heap-allocate of its own.

## Position in the workspace

| Crate | Owns | Depends on |
| --- | --- | --- |
| `sdave` (core) | flat framing, the deserializer state machine, `can_serialize` | nothing |
| `sdave-codec` | the typed runtime an expansion names: traits, `Shape`, plan/write, markers, errors, `dyn` | `sdave` |
| `sdave-alloc` | allocation-bound conveniences | `sdave`, `sdave-codec` |
| **`sdave-macro`** (this) | the derives and the attribute macro; emits calls into the codec | nothing of ours (host-only proc-macro) |

This crate is **host-only**: it depends on `syn` / `quote` / `proc-macro2` and on nothing of ours,
so it never enters a runtime build graph and never constrains the runtime's `no_std` story. The
workspace root is `sdave/Cargo.toml`, whose `[workspace]` table currently lists **no members** —
this crate must be added there.

## What this crate owns, and what it must not

**Owns:** the `#[derive(Serialize)]` / `#[derive(Deserialize)]` expansions; the attribute macro(s)
that pair a hand-written object trait with per-type delegation for `field: dyn Trait`; compile-time
validation of every `#[sdave(...)]` attribute; the mapping from a declaration to a `Shape`; the
codec-bound computation for generic parameters.

**Must not:** own a wire decision (markers, naming, framing, shape semantics — all the codec's);
read or write bytes; keep or reference a name registry; allocate; perform a `Default` lookup at
runtime (it emits a call to the codec's default-provider hook); or guess a `rename` / `skip` /
`basic` intent that was not declared.

## Emitted path (settled)

An expansion names the runtime through exactly **one** path: the codec crate, `::sdave_codec`,
which owns every trait, type and entry point a derive needs. A serde-style
`#[sdave(crate = ::some::path)]` attribute on a container redirects that path, so a renamed or
vendored dependency still works; the `dyn` macros take the same override. There is **no facade
crate** — the core cannot re-export the codec without inverting the layering, so the codec itself
is the crate an expansion names, and it must therefore re-export, at its root, the handful of core
items an expansion touches. (Mirrors the codec README.)

## Derives

```rust
use sdave_codec::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ToolCall {
    name: String,
    #[sdave(default)]
    arguments: Vec<String>,
    #[sdave(default = default_timeout_ms)]
    timeout_ms: Option<u64>,
}

fn default_timeout_ms() -> Option<u64> { Some(30_000) }
```

### Attributes

| Attribute                  | Site        | Meaning                                                                        |
| -------------------------- | ----------- | ------------------------------------------------------------------------------ |
| `#[sdave(crate = path)]`   | container   | Emit calls against `path` instead of the default `::sdave_codec`.              |
| `#[sdave(basic)]`          | type        | Treat the whole type as opaque and delegate to its `BasicType` implementation. |
| `#[sdave(default)]`        | named field | Use `<FieldType as Default>::default()` for an absent field.                   |
| `#[sdave(default = path)]` | named field | Call `path()` for an absent field.                                             |

- `path` is a Rust path, not a string, and must be a zero-argument function returning the field's
  complete type. A custom provider does not require `Default`; `#[sdave(default)]` does.
- Defaults are **deserialization-only**. Serialization always writes every field in declaration
  order; there is no skip-on-default behaviour.
- Defaults apply only to a genuinely **absent** field: a supplied `None`, empty string or empty
  `Vec` is a present value and never triggers the provider.
- Defaults on positional (tuple-struct / tuple-variant) fields, and any container-level default,
  are compile-time errors.

Anything else is rejected at compile time rather than guessed at: unknown `sdave` attributes,
`rename`, `skip`, `#[sdave(basic = …)]`, `union`s, and malformed attribute forms all fail the build
with a targeted message.

## The `dyn` pair (first-class)

The `field: dyn <trait>` shape is a supported construct, not an emergent behaviour, and it takes
**two** macros because one half is per-_trait_ while the other is per-_type_:

```rust
// On the object trait: emits the trait's companion impls, including the single
// `impl Serialize for dyn ToolInput` the codec requires.
#[dyn_payload]
trait ToolInput: DynSerialize + Debug + 'static {
    fn as_any(&self) -> &dyn Any;
    fn eq_dyn(&self, other: &dyn ToolInput) -> bool;
}

// On each implementor: emits `impl ToolInput for T { as_any; eq_dyn }` plus the concrete
// type's type-erased `DynSerialize` delegation, with the concrete type's marked document in
// the value bytes.
#[derive(DynPayload)]
#[dyn_payload_impl(dyn_trait = crate::ToolInput)]
struct ReadFileToolInput { path: String }
```

Preconditions, all enforced with clear errors:

- the trait must be **object-safe**, `'static`-compatible, **non-generic**, and have the codec's
  `DynSerialize` as a supertrait;
- each concrete implementor must implement the codec's `Serialize` and `core::cmp::PartialEq`
  (equality is downcast-to-concrete plus that type's own `PartialEq`, so it is never vacuous);
- the `DynSerialize` methods are **type-erased** — no `NamePolicy`, no allocator generic — which is
  what keeps the trait object-safe; the write path passes a **sink that carries the naming policy
  dynamically** (the codec's type-erased write handle), so no fixed inner policy exists;
- the trait definition itself stays hand-written, and **decoding stays the application's job** —
  the macro cannot generate a registry, because a trait object cannot be constructed by the codec.

Wire rule (frozen in `sdave-codec`): the field header keeps the **verbatim** spelling
(`Box<dyn …>` differs from `&dyn …`), and the value bytes carry the concrete type's **marked**
document — identity is delivered, not erased.

## Shape mapping

The declared Rust shape selects the wire shape; `sdave_codec::Shape` is generated as an associated
constant, so the type and framing are known before any value is inspected — which is what makes
empty vectors and unit variants decodable.

| Declaration                                      | Wire shape | Framing                                                                                  |
| ------------------------------------------------ | ---------- | ---------------------------------------------------------------------------------------- |
| Named struct                                     | `Record`   | one record envelope of `Name: Type` field runs                                           |
| Unit struct                                      | `Record`   | one empty record envelope                                                                |
| Tuple struct / newtype / zero-field tuple struct | `Tuple`    | one value envelope holding positional item envelopes, one per field                      |
| Unit variant `V`                                 | `Enum`     | variant envelope only                                                                    |
| Single-field variant `V(T)`                      | `Enum`     | variant envelope + one associated-value envelope holding `T`'s payload                   |
| Multi-field variant `V(A, B)`                    | `Enum`     | variant envelope + one associated-value envelope holding a positional tuple body         |
| Named-field variant `V { x, y }`                 | `Enum`     | variant envelope + one associated-value envelope holding a context-known record envelope |

`V`, `V()`, and `V {}` keep distinct framing: a zero-field tuple variant is not a unit variant.
Tuple arity is capped at 12.

Nested aggregates elide the redundant type marker only when the surrounding context already
supplies the type — a field header such as `inner: InnerStruct` declares the child's type, so the
child's envelope holds its record fields directly instead of repeating the marker. A standalone
document root always emits its own marker.

## What lives in the codec, not here

The derive only maps a _declaration_ to a shape. Containers and leaves are the codec's:
`String`, `&str`, byte payloads, the numeric and scalar primitives, `NonMaxUsize` and the standard
nonzero types, `()`, `Option<T>`, `Box<T>`, `Result<T, E>`, tuples (arity ≤ 12). `Vec<T>` frames a
mandatory empty header envelope followed by one item envelope per element, and — because it needs
an allocator on decode — its impls live in `sdave-alloc`.

Markers, abbreviation rules, tuple-comma equivalence, error vocabulary, and the
`NamePolicy` / `ShortType` contract are all specified in `sdave-codec`. Two rules worth repeating
here because they are the derive author's to respect:

- a marker is the **verbatim** compiler spelling filtered by the policy, so it is stable only for a
  given build; a type whose marker must survive a rebuild or a `std`↔`no_std` move must be declared
  in `SHORT_TYPES`;
- the macro must not invent names, reorder fields, or rewrite leaf bytes.

## Generic and recursive types

Generic derives add codec bounds only for field types that actually mention a generic parameter,
and skip `Self`, so recursive types such as `struct Node { next: Option<Box<Node>> }` compile
without unbounded or self-referential requirements. `#[sdave(basic)]` types place no codec bounds
on their storage fields at all.

## Decoding rules the generated code must satisfy

- Named fields are matched **by name, in any order**; duplicate, unknown and missing fields are
  errors, never silently dropped or defaulted.
- Every field header's marker is validated before its value run is read.
- Providers for absent fields run only after the record is confirmed complete and all
  required-field presence has been validated, and at most once each.
- Unknown variants, missing or extra envelopes for a variant's declared arity, nonempty or missing
  list headers, and truncated frames are all errors.
- Decoded leaves are borrowed verbatim from the input wherever possible; leaf bytes are never
  escaped or rewritten.

## What the macro needs from the codec (the derive-facing surface)

The macro is written against these, so the codec must expose them and keep them stable (the codec
README states the same list as "Obligations toward the derive"):

1. a per-type **shape constant** the derive can set (`Record` / `Tuple` / `Enum`);
2. **one callable append entry per type** that a generated body calls for each child; the codec owns
   the order in which a record's fields are appended;
3. a **type-erased write handle** (the sink carrying the naming policy dynamically) so a generated
   `dyn` delegation is object-safe;
4. **field-header emission and marker validation**, performed by the codec — the derive only maps a
   declaration to a shape;
5. a **`BasicType`** escape hatch for `#[sdave(basic)]`;
6. a **default-provider hook** for absent named fields;
7. a **stable emitted path** (settled: `::sdave_codec`, overridable);
8. the **container and leaf impls** for everything the derive never generates.

## Open decisions

1. **The exact emitted signatures** — they follow from the codec's serializer API; expect an append
   call per type, and mirror whatever the codec settles (see the codec README's *Still open*).
2. **Whether the `dyn` macros need their own crate-path override** in addition to the container
   attribute, and whether the trait-side macro should also generate the registry scaffolding it
   cannot complete.
3. **Whether `BasicType` helpers take the serializer primitives directly** (so `basic` types are
   also allocation-free) or keep a richer handle.

## MSRV

**Rust 1.100+**, inherited from the core: the allocator-parameterised runtime requires
`allocator_api`. Nightly is acceptable until then. This crate is host-only and is not itself
`no_std`-constrained, but it must not force a feature gate onto the runtime it emits calls into.

## License

MIT, matching the SDAVE workspace.
