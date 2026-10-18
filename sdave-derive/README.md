# sdave-derive

The **derive** layer of SDAVE: `#[derive(Serialize)]`, `#[derive(Deserialize)]` and the
`#[sdave_dyn(ConcreteA, ConcreteB, ...)]` attribute.

It generates the `sdave_codec` impls whose wire shapes are described in `sdave-codec/README.md`; this file
covers what the macros accept, what they emit, and the one requirement their output puts on a
`Cargo.toml`.

This crate is a thin `proc-macro` wrapper. A `proc-macro` crate may only export proc macros, so no
generator can live here: every token stream is produced by `sdave-derive-impl`, a normal library crate
that can be public, documented and tested.

## Wiring it up

```toml
[dependencies]
sdave-codec = "0.5"
sdave-derive = "0.5"
```

```rust
use sdave_codec::*;
use sdave_derive::{Deserialize, Serialize, sdave_dyn};
```

The traits and the derives share their names but not their namespace, so the two imports coexist: the
trait comes from `sdave-codec`, the derive macro from `sdave-derive`.

The generated code refers to the codec through its absolute path (`::sdave_codec::…`), which is what keeps
a user module from shadowing it. That is a requirement on you, not a detail: the codec dependency MUST be
called `sdave_codec` in the depending crate's `Cargo.toml`. Renaming it
(`sdave = { package = "sdave-codec" }`) makes the generated code fail to resolve. The rest of the
conventions the generated code follows are listed in `sdave-derive-impl/README.md`.

## `#[derive(Serialize)]` / `#[derive(Deserialize)]`

| annotated item | `Serialize` | `Deserialize` | notes |
| --- | --- | --- | --- |
| struct | yes | yes | |
| enum | yes | yes | discriminants are never on the wire |
| generic type (lifetime/type/const parameters) | yes | yes | flattened path, internals in `sdave-derive-impl/README.md` |
| union | no | no | rejected with a spanned error; why, in the codec's capability boundary |

### How the derive maps an item onto the wire shape

The shapes themselves — the per-field `NonEnvelop(Signature) ++ Envelop(value)` pair, the outer Envelop,
the typed wrapper — are stated once in `sdave-codec/README.md`. What this derive decides is the mapping:

- a named field's `Signature` is `name ++ ": " ++ Type`, a positional field's is **nothing**: a
  positional composite's payload is just the sequential field Envelops, and the composite names itself
  through its `TypeName` (`MyType(f32, f32)`; the parens are what make that text impossible for a field
  name, so the two shapes stay distinguishable). A bare tuple has no impl — put it in a tuple struct or
  tuple variant and derive, and the wire shape is the same;
- named fields are matched **by signature**, so the order they arrive in is irrelevant and a pair this
  type does not know is ignored;
- positional fields are matched **by position**: the `n`-th value Envelop of the payload is the `n`-th
  field, so a short document cannot say *which* position is missing — which is why `#[default]` is
  rejected there (see [Field attributes](#field-attributes)), while `#[skip]` is fine;
- an enum variant's tag is its ident, and a variant with **no fields at all** — a unit variant, or an
  empty braced/tuple one — is written as the name alone, which is `()`'s wire shape.

### How the generated reader allocates

`Deserialize::deserialize` takes a `&mut FlatDeserializer` rather than a byte slice (the trait, and the
contract an impl has to honour, are in `sdave-codec/README.md`). The derive leans on that: **a generated
`deserialize` never allocates for the traversal itself** — only the values it returns, plus whatever an
iterable field's own element walk needs (one range list, documented in the codec).

- it deframes its own Envelop with `envelop_payload`, then collects the channel items into a fixed-size
  `[Option<Item>; 2 × encoded fields]` array with `collect_items` — one slot per *value* plus one per
  `Signature`, and for a positional shape exactly one slot per value, because it has no per-field
  signatures. The count is exact for the type, so no `Vec<Item>` is built — and the array lives on the
  stack of the generated function.
- every field value is decoded through `with_deserializer`, which lends the enclosing deserializer's three
  `Vec`s to the nested value and takes them back on return. Nesting therefore does not multiply the
  allocator traffic; only a value that itself owns heap storage allocates.
- a positional composite's identity is its `TypeName`, so `#[derive(Serialize)]` renders a tuple struct as
  `Ident(field types)` (`Plain()` for an empty one); named and unit structs keep their plain name.
- an enum does the same twice (outer tag channel, then the variant's inner channel), one `with_deserializer`
  further down. A variant with no fields never looks for an inner Envelop.
- `#[sdave_dyn]`'s reader collects the single `NonEnvelop(concrete type name)` item it inspects and then
  descends into the value bytes the same way.

A channel with more items than the type expects is still walked to the end (so a malformed tail is seen as
such) and the surplus is simply ignored, which is the same leniency the type had before the array replaced
the `Vec`.

The generated `deserialize` has exactly the signature of the trait method, so a `#[derive(Deserialize)]`
type and a hand-written `Deserialize` impl are interchangeable at any position — including as the concrete
type of a `#[sdave_dyn]` field.

### Bounds the generated impls carry

For every field that reaches the wire (i.e. not `#[skip]`ped):

| impl | bounds added |
| --- | --- |
| `Serialize` | `FieldTy: Serialize<u8, A>`, `FieldTy: TypeName` |
| `Deserialize` | `FieldTy: Deserialize<u8, A, Type = FieldTy>`, `FieldTy: TypeName` |

plus `A: Allocator + Clone`, the annotated type's own `where`-clauses, and — for the generated `TypeName` —
`TypeParam: TypeName` for each of the type's type parameters, which its `render` composes. A `#[skip]`ped
field's type needs no codec impl at all.

A field type that does not have them is a compile error, which is how a codec feature you left off
surfaces. Which shapes the codec provides, and which feature gates each one, is `sdave-codec/README.md`'s
capability boundary.

The `TypeName` impl rides along with `#[derive(Serialize)]` only: a type that is only ever deserialized has
no wire name of its own, which is exactly the half it never needs. Its `FULL` is the type's module path
plus its ident, so a wire name follows the module layout unless a `NamePolicy` shortens it.

### Field attributes

Both take the expression that builds the field without its bytes. This section is the single source for
what they mean: the rustdoc of the two macros, `sdave-derive-impl`'s `FieldInfo` and
`sdave-codec/README.md`'s wire section all point here.

- `#[default(<expr>)]` — the field is written like any other, but a document that omits it still
  deserializes, with `<expr>` as the value. The wire shape is unchanged, so a peer that always writes
  the field still interoperates. **Named fields only**: on a positional field it is a compile error
  (see below).
- `#[skip(<expr>)]` — the field is local to this type: it is never written and never read, so its type
  needs no codec impl and `<expr>` always builds it. A peer that writes the field has that value ignored,
  exactly as if this type did not know the field. Legal on a named field and, any number of times, on a
  positional one.

```rust
#[derive(Serialize, Deserialize)]
struct Config {
    id: u32,
    #[default(8080)]
    port: u16,
    #[skip(String::new())]
    cache: String,
}
```

The two attributes describe different sides of the wire and may be combined on one field: `#[skip]` takes
it off the codec, `#[default]` supplies the value used in its place. When both are given, `#[default]`'s
expression is the value and `#[skip]`'s is not consulted.

A *positional* field has one attribute only: `#[skip]`. `#[default]` is rejected with a spanned compile
error, because it would be ambiguous rather than merely narrow — positional fields are matched by
position, so a document that ends early cannot say *which* position is missing. With `(u32, u32, u32, u32)`
and two optional positions, a four-element payload could have dropped either of them, and the reader would
have no way to choose. `#[skip]` has no such problem: a skipped position is removed for both peers, so the
remaining fields keep their order (and a skipped field adds no slot in either shape — nor any entry in the
reader's fixed-size item array).

## `#[sdave_dyn(ConcreteA, ConcreteB, ...)]`

Applied to a trait, this generates:

- `TypeName` for `dyn Trait + 'static`, named `<module>::dyn Trait`;
- `Serialize` for `dyn Trait + 'static`, which downcasts to the first listed type that matches and
  self-claims it through that type's `serialize_typed`, flattened into one Envelop;
- `Deserialize` for `Box<dyn Trait + 'static, A>`, which renders each listed type's name and takes the
  first one that matches the document.

`&dyn Trait` and `Box<dyn Trait>` serialize through the codec's `&T`/`Box<T, A>` forwarding, which is why
the macro only emits an impl for `dyn Trait` itself. The wire consequence — one Envelop, so a `dyn` field
takes exactly one limiter/value/delimiter triple — is in `sdave-codec/README.md`. The reader matches the
name, then descends into the concrete value's bytes with the same lent deserializer a hand-written impl
would use, and boxes the result in the caller's allocator.

`Any` is added to the trait's supertraits (unless already present) so the concrete value can be recovered
from the trait object, and a sealing supertrait `__SdaveDynClaim_<Trait>` is added and implemented only
for the listed types. That is why `impl Trait for Other` becomes a compile error rather than a value that
can be built but never (de)serialized.

Requirements — the first two are rejected by the macro itself, the other two by the bounds it generates:

- the attribute MUST list at least one type;
- the trait MUST NOT be generic (a `dyn Trait` needs object safety regardless);
- every implementor MUST be listed;
- every listed type MUST be `'static`, because it travels through `Any`.

## Limits

The capability boundary — unions, recursive types, an empty enum, the `#[sdave_dyn]` limits, and why each
is a gap — is kept in one list in `sdave-codec/README.md`, so it is not repeated here. Two things are the
macros' own: a rejected item or attribute produces a spanned compile error rather than silently emitting
nothing, and `#[default]` on a positional field is rejected (see [Field attributes](#field-attributes)).

## Design notes

Codegen details — how `Slices` is sized for a monomorphic type, why a generic type flattens instead, the
reader's fixed-size item array, and the identifier conventions the generated code follows — are
`sdave-derive-impl`'s subject; see its `README.md`. From the outside, three properties are worth knowing:

- every generated impl is generic over the allocator and over the `NamePolicy` **per call**, so one binary
  can carry several naming policies and still round-trip;
- the serializer is the allocation-free half: an iterable's per-element slice list lives on native stack
  frames (the core's `Stack`) and is flattened in place, so a compile-time-unsized number of slices never
  becomes a heap list;
- the deserializer matches the `sdave-codec` trait exactly and only allocates what the value it returns
  owns. The wire format is therefore identical for a derived type, a hand-written impl and a `dyn Trait`
  claim — and so is the allocation profile.

## Toolchain

This crate targets the same toolchain as `sdave-codec`; see the MSRV section of `sdave-codec/README.md`.
The macros themselves need no `#![feature(…)]` (the one unusual attribute they emit,
`#[diagnostic::on_unimplemented]`, is stable), but the codec they target is what actually decides the
floor.

## Where to look

- `sdave-derive-impl`'s `README.md` — the codegen internals, module by module.
- `sdave-codec/README.md` — the serialize/deserialize contract, the allocation model and the capability
  tables.
- `sdave-codec/tests/derive.rs` — every runtime claim above, pinned (including the deserializer-reuse
  path).
- `sdave-derive-impl/tests/codegen.rs` — the token streams themselves, pinned.
