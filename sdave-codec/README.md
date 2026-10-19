# sdave-codec

The **codec** of SDAVE: it turns a Rust value into SDAVE documents and back, on top of the flat framing
implemented in `sdave` core, which is `no_std` and does **no** heap allocation at all.

If sdave-codec MUST allocate on the heap, it MUST explicitly require the allocator from the caller and use
it — and it allocates as little as the value it hands back allows. The three `Vec`s a `FlatDeserializer`
owns are **lent down the recursion and taken back** ([Allocation model](#allocation-model)), so a nested
document is read with one set of them instead of one set per nesting level.

sdave-codec MUST NOT support dynamically expanding its capability (specifically, what types of value it
can handle) at run-time: serialization and deserialization MUST be determined at compile time, and errors
sdave-codec users (AKA applications) are oriented to MUST be sound — as a compile error.

This file is the canonical description of the **wire format**, the **allocation model** and the
**capability boundary**. The macro surface (what the derives accept) is described once in
`sdave-derive/README.md`, the codegen internals once in `sdave-derive-impl/README.md`, and the framing
protocol itself (including the `Stack` collector) in the root `README.md`.

## Allocation model

The codec is allocator-parameterised (`A: Allocator + Clone`) and treats the heap as the caller's
resource: it never reaches for `Global` on its own. What matters for a heavily multi-threaded application
is not only _how much_ is allocated, but _how many_ small allocations are created and dropped per value —
each one is a contended allocator hit and a later free. The codec is written so that the count does not
grow with document size or nesting depth.

### What allocates

| allocation                                          | when                                                                                                                                                                                                    | size                                                                                             |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| the value the caller asked for                      | always, when that value owns heap storage (`Vec<T, A>`, `Box`, `String`, `Cow`'s owned form, `Rc`/`Arc`, `BTreeSet` nodes, …)                                                                           | proportional to the data                                                                         |
| a `FlatDeserializer`'s three `Vec`s                 | once per **top-level** deserialize — the caller builds the one instance (`FlatDeserializer::new`); nesting never builds another                                                                          | `limiter_pairs` + a `repeat_map` entry per pair, plus the archived boundaries **it** accumulates |
| `[T; N]`'s element list                             | once per array deserialize (`deserialize_iterable_elements`); the heap containers push straight into their own container and skip it                                                                     | one `T` per element                                                                              |
| the ranges and slices of an iterable (`Stack`)      | never — both directions keep one frame per element on native stack frames                                                                                                                               | —                                                                                                |
| the flatten scratch of an iterable `serialize`      | never — the bytes are appended to the caller's buffer and flattened in place                                                                                                                            | —                                                                                                |

### What does not

- **Nesting is free.** `Deserialize::deserialize` takes the deserializer by `&mut`, and every level that
  needs a deserializer over a sub-frame of the same buffer borrows its parent's three `Vec`s with
  `with_deserializer`, which hands them back on return (see [Deserializer plumbing](#deserializer-plumbing)).
  A document nested ten levels deep therefore costs the same three `Vec`s as a flat one. On ascent the scan
  cache is cleared (`update_internal_cache(.., None)`) and a partially scanned item is rewound to its last
  settled boundary — the rescan is the price of reuse, and it is bounded by the value being read.
- **Field/item lists are not collected.** A derived type knows that its channel holds exactly one
  `NonEnvelop` signature plus one `Envelop` value per _encoded_ field — and, for a positional shape, one
  `Envelop` value alone — so the generated reader collects the items into a fixed-size
  `[Option<Item>; n]` array (`collect_items`) instead of a `Vec<Item>`. The same is true of
  `Option`/`Result`'s tag/value split (a 2-slot array).
- **An iterable's elements are stacked, not listed — in both directions.** Serializing `Vec<T, A>`, `[T]` or
  `[T; N]` walks the elements onto native stack frames with the core's `Stack`, then flattens the region in
  place with one move, so a compile-time-unsized number of `Slices` never becomes a heap list; deserializing
  one stacks the element ranges the same way, one frame per channel item, and decodes them off that stack. A
  placeholder frame (`Some(None)`) stands for a channel item that is not a complete range — the marker
  Envelop, the `Signature`, and the first element, whose tail is the next item's head — which keeps `None`
  free as `Stack`'s stop signal. `str` is not an iterable: its payload already is the bytes.

Inside a `deserialize` call there is no allocation for the traversal at all: a nested value reuses its
parent's three `Vec`s, and an iterable's ranges live on native frames. The one intermediate element list left
is `[T; N]`'s, which is built after the count is known and handed back inside the array.

## The serialize/deserialize contract

sdave-codec supports primitives, tuples, structs, enums and `dyn Trait` fields. Unions are out of scope:
the application MUST own its own design for union serialization/deserialization and impl `serialize` &
`deserialize` manually, while sdave-codec only provides a scaffold.

sdave-codec and sdave-derive MUST NOT provide a derive macro for a union, or for any data structure
containing a union, without a manual `serialize` & `deserialize` impl.

sdave-codec impls `Serialize` and `Deserialize` for primitives, behind the `primitive` feature (see
[Readable leaf vs raw bytes](#readable-leaf-vs-raw-bytes)).

A `Deserialize::deserialize` call takes the value's `&mut FlatDeserializer` (positioned on exactly one
value's bytes), a `final_buffer: Option<Offset>` flag and the allocator. The flag is documented on the
trait method itself: `Some(offset)` is the caller's promise that the buffer is complete (asserting
`offset == deserializer.buffer().len()`), which is the only way to accept a `Variant::V2` Envelop whose
delimiter slice has no following non-delimiter byte; `None` rejects such an Envelop with
`Error::UnconfirmedDelimiter`. Internal recursive deserialization always hands a child the raw prefix
`buffer[..tail]` of its own buffer plus the frame it is to read, so the child's `buffer().len()` *is* the
value's tail and it always receives `Some(tail)` — nested `Variant::V2` Envelops are handled gracefully,
and every offset any level reports is absolute in the origin buffer.

Every `Signature` type text is rendered through a `NamePolicy` method parameter: each type provides its
full qualified name (ignoring generic arguments) via `TypeName`, and the policy decides which names it
shortens to their last `::` segment. Rendering is per node, so shortening one type (e.g. an outer generic)
never affects another (e.g. its arguments). The default `NoShortening` policy never shortens. A type used
in a signature MUST implement `TypeName` (impls are provided for the primitives and the standard
containers; `#[derive(Serialize)]` provides one for a user type).

A `Signature` names a value inside its parent's payload. There are exactly two ways a value is written,
and which one applies is decided by the *shape* of the field, not by the field's type:

| field kind | on the wire | matched by |
| --- | --- | --- |
| named (`z`, `value1`), or a typed value | `NonEnvelop(name ++ separator ++ type) ++ Envelop(value)`, e.g. `z: MyType` | the signature text, so the arrival order is irrelevant |
| positional (a field of a tuple struct / tuple variant, `z.0`) | just `Envelop(value)`, one after another | its position: the `n`-th `Envelop` is the `n`-th field |

A positional composite therefore has **no per-field signature at all**: its payload is the sequential
Envelops of its fields. What identifies it is its *own* type text — `MyType(f32, f32)` for a tuple struct —
which is what a parent writes in the field's signature (`z: MyType(f32, f32)` here). The parentheses are
the marker: `(` and `)` are reserved by Rust and cannot be part of a valid ident, so a
`name ++ separator ++ type` signature whose type contains them can only name a positional composite, never
a field name.

The separator is what keeps `name` and `type` apart (e.g. `: ` for a `u8` buffer, as `::` can appear inside
`type`). A `Signature` is a NonEnvelop.

NonEnvelop(`Signature`) + Envelop(`value`) MUST be sequential: NonEnvelop(`Signature`) followed by its
<!-- plural for Iterables --> Envelop(s) — that is the rule for a **named** field. A positional composite
writes the Envelops alone:

```conceptual
Envelop(Envelop(positional field 0) ++ Envelop(positional field 1) ++ ...)
```

Because SDAVE core needs the payload determined so that it can find the longest run (assume repeats as
`r`) in the payload matching the delimiter, then frame the payload with the delimiter slice repeated
(`r` + NonZero) times to generate a valid Envelop, `serialize` MUST call `serialize` recursively
internally BEFORE framing its own layer.

The sdave-codec deserializer MUST support orderless field serialization (we have
NonEnvelop(`Signature`) ahead of Envelop(`value`) for each **named** field). Positional fields are matched
**by position** instead, which is why they need no signature of their own — the `n`-th `Envelop` of the
composite's payload is the `n`-th field.

A fieldless shape — the unit tuple `()`, and the unit-tuple-alike structs `Plain()` / `Plain{}` — writes an
**empty** Envelop, so the tuple and the struct abstraction share one solution. A bare unit struct
(`struct PlainUnit;`) keeps its serialization unchanged.

```rust
#[derive(Clone, Debug, PartialEq)]
struct MyType(f32, f32);
#[derive(Clone, Debug, PartialEq)]
struct ToSerialize {
    x: u32,
    y: usize,
    z: MyType,
}
```

Assume we shorten `Value` (serialized) as `V`, `Signature` as `S`, `LimiterSlice` as `L`, `DelimiterSlice`
as `D`:

A tuple struct and a tuple variant share one wire shape: **one Envelop whose payload is the sequential
field Envelops**, with no per-field signature. Their own type text — `MyType(f32, f32)` for this tuple
struct — is what a parent writes when the value is a field (`z: MyType(f32, f32)` here), and what
`serialize_typed` writes around the value itself. There is no bare-tuple impl for exactly this reason: a
`(f32, f32)` has no name to identify it, so a positional composite is always a struct or a struct variant.

```conceptual
let mut buffer: Vec<u8> = Vec::<u8>::new();
assume_eq!(
    z.serialize(&mut buffer),
    Envelop(    // L0
        Envelop("114.514"), // L1 V2 D3
        Envelop("350234.0"), // L4 V5 D6
    ),  // D7
);
buffer.clear();
assume_eq!(
    z.serialize_typed(&mut buffer, Name { head_offset: 0, tail_offset: 0 }),
    Envelop(
        NonEnvelop("MyType(f32, f32)"),
        Envelop(
            Envelop("114.514"),
            Envelop("350234.0"),
        ),
    )
);
buffer.clear();
assume_eq!(
    to_serialize.serialize(&mut buffer),
    Envelop(    // L0
        NonEnvelop("x: u32"),   // S1
        Envelop("42"),  // L2 V3 D4
        NonEnvelop("y: usize"), // S5
        Envelop("69"),  // L6 V7 D8
        NonEnvelop("z: MyType(f32, f32)"),    // S9
        Envelop(    // L10
            Envelop("114.514"), // L11 V12 D13
            Envelop("350234.0"), // L14 V15 D16
        ),  // D17
    ),  // D18
);
buffer.clear();
buffer.extend_from_slice(b"to_serialize");
assume_eq!(
    to_serialize.serialize_typed(&mut buffer, Name { head_offset: 0, tail_offset: 12 }),
    Envelop(
        NonEnvelop("to_serialize: ToSerialize"),
        Envelop(
            NonEnvelop("x: u32"),
            Envelop("42"),
            NonEnvelop("y: usize"),
            Envelop("69"),
            NonEnvelop("z: MyType(f32, f32)"),
            Envelop(
                Envelop("114.514"),
                Envelop("350234.0"),
            ),
        ),
    )
);
```

`Envelop(Envelop(b<114.514>)Envelop(b<350234.0>))` comes from `serialize` of `MyType`: the payload is
just the two field Envelops, in order. It MUST be determined and serialized by a `MyType` value holder —
the `to_serialize`'s `z` here.

An empty `Name` (`head_offset == tail_offset`) writes no `name` and no separator, so the signature is the
bare type describer, e.g. `MyType`.

We have the `serialize` shaped to return an `[Option<(Offset, Offset)>; n]` while mutating the `Vec<T>`.

```rust
enum MyEnum {
    UnitLike,
    TupleLike(i32, u32),
    StructLike { value1: i32, value2: i32 },
}
```

```conceptual
let mut buffer: Vec<u8> = Vec::<u8>::new();
assume_eq!(
    MyEnum::StructLike{ value1: -6, value2: 9 }.serialize(&mut buffer),
    Envelop(    // L0
        NonEnvelop("StructLike"),    // S1
        Envelop(  // L2
            NonEnvelop("value1: i32"),  // S3
            Envelop("-6"),  // L4 V5 D6
            NonEnvelop("value2: i32"),  // S7
            Envelop("9"), // L8 V9 D10
        ), // D11
    )   // D12
);
buffer.clear();
assume_eq!(
    MyEnum::TupleLike(1, 2).serialize(&mut buffer),
    Envelop(    // L0
        NonEnvelop("TupleLike"),    // S1
        Envelop(  // L2
            Envelop("1"),  // L3 V4 D5
            Envelop("2"),  // L6 V7 D8
        ), // D9
    )   // D10
);
buffer.clear();
assume_eq!(
    MyEnum::UnitLike.serialize(&mut buffer),
    Envelop("UnitLike") // L0 V1 D2
);
```

Rust solves the size of such type by padding and then all variants can be Sized on stack, i.e. for the
example above, it would be size 12 align 4. Similarly, `serialize` MUST return
`[Option<(Offset, Offset)>; n]` where `n` is the greatest possible `n` over ALL variants, using a phantom
offset (`None`) as the placeholder for padding. i.e. for the `MyEnum` here, it needs
`[Option<(Offset, Offset)>; 13]` to store its serialization slices — 13 from the *named* `StructLike`
variant, since a positional variant such as `TupleLike` always costs exactly 6 (its fields share one
flattened inner payload) — while for UnitLike, it has 3 Somes and 10 Nones in the returned result of
`serialize`.

Specially, for a NOT Sized type (serialization not sized, not Rust Layout, semantical instead of
transparent inner raw value), e.g. Iterable (`Vec`, `Map`, `LinkedList` etc.), or `dyn Trait`
pointer (`&dyn Trait`, `&mut dyn Trait`, `Box<dyn Trait>` etc.), sdave-codec or sdave-derive cannot
provide a light, safe, idiomatic way to handle them without (a) heap memory copying/sorting; OR (b)
relying on an unsafe procedural that can always produce it.

### `dyn Trait`

A `dyn` field is enabled by `#[sdave_dyn(..)]` on the trait; what the attribute does is described once in
`sdave-derive/README.md`. Its wire consequence: serializing a `Box<dyn Trait>` / `&dyn Trait` writes one
Envelop whose payload is the concrete type's `Signature` NonEnvelop followed by the concrete value's
Envelop, flattened in place so the `dyn` field still takes exactly one limiter/value/delimiter triple,
like any other Sized value.

sdave-codec won't duplicate the type marker for each element. It instead designs:
(NonEnvelop(`Signature`) ++ ) Envelop(empty payload) ++ Envelop(element0) ++ Envelop(element1) ++ .....
<==> Iterable. This differs real empty v.s. semantic empty, e.g. `[]` and `[""]`.
`[Empty Envelop, [Envelop ......]]`. These Envelops are expected to be serialized sequentially in buffer
when returned to its caller (application or some recursive from other `serialize` that depends on it),
they would together take 1 LimiterSlice slot and 1 ValueSlice slot and 1 DelimiterSlice slot to locate
them in buffer — thus it won't break the deserialization — but MUST require being flattened in-place
(within implementation of such `serialize`). The per-element slice lists are stacked on native frames by
the core's `Stack` and flattened once, then dropped with the frames: no heap list of slices is built and
nothing is sorted.

Specially, `&str` MUST NOT be considered Iterable: it is the raw-text leaf shape (payload = the UTF-8
bytes), not a sequence.

### Field attributes

`#[derive(Serialize, Deserialize)]` reads two field attributes, `#[default(<expr>)]` and
`#[skip(<expr>)]`. Their semantics are documented once, in `sdave-derive/README.md`; what belongs here is
the wire consequence:

- `#[skip(<expr>)]` — the type has one field fewer on the wire: no `Signature`, no slot.
- `#[default(<expr>)]` — nothing changes on the wire; only the reader may go without the field. Named
  fields only: a positional field cannot be optional (the reader could not tell which position is
  missing), and the derive rejects it.

## Deserializer plumbing

`Deserialize` is written against a **lent** deserializer, not against a byte slice, because that is what
makes [storage reuse](#what-does-not) possible:

```rust
fn deserialize<N: NamePolicy>(
    deserializer: &mut FlatDeserializer<'_, '_, T, A>,
    final_buffer: Option<Offset>,
    allocator: A,
) -> Result<Self::Type, Error>;
```

The allocator stays a parameter because a value that _is_ heap storage has to be built in it and handed
back. An impl MUST treat the deserializer as lent: descend into sub-frames with `with_deserializer`, never
replace its buffer with anything outside the bytes it was given, and leave it positioned after the value
it consumed. Its item cache may be cleared on return, so a walk over that deserializer MUST NOT be resumed
after a nested call.

The helpers an impl is built from, all `pub`:

| helper                                                                                     | what it does                                                                                                                                                                                                                                         |
| ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `envelop_payload(&mut FlatDeserializer, final_buffer)`                                     | drives the one Envelop the deserializer is positioned on and returns a `Deframed`: the raw buffer it read, plus the payload's absolute `(head, tail)` frame                                                                                         |
| `with_deserializer(&mut FlatDeserializer, frame, consumer)`                                | lends the three `Vec`s to a deserializer over the raw prefix `buffer[..frame.1]` that starts its channel at `frame.0`; on return it restores the parent's position with them and leaves the parent's scan cache cleared (a partial scan is rewound to its last settled boundary). The child owns what it was lent, so it may itself be used as a parent once its own channel walk has finished                                                                                                                                                    |
| `collect_items(&mut FlatDeserializer, &mut [Option<Item>])`                                | walks a channel into a caller-sized fixed array, returning the item count (item `0` is the frame marker, not an item)                                                                                                                                |
| `for_each_element_range(&mut FlatDeserializer, payload, visit)`                            | the iterable element walk — the one place a payload is split into element ranges; `visit_iterable_elements` builds on it, and a caller that wants the ranges without decoding collects them itself. The ranges live on native stack frames (`Stack`), one frame per channel item, with a placeholder frame for an item that is not a complete range                                                                     |
| `visit_iterable_elements(..)` / `deserialize_iterable_elements(..)`                        | the iterable element walk (ranges stacked, then one decode per element through the recycled storage)                                                                                                                                                 |

A hand-written `Serialize`/`Deserialize` pair therefore looks like:
`frame_slice` out, `envelop_payload` + `collect_items` in, and `with_deserializer` for every nested value.
`sdave-codec/tests/composite.rs` is a worked example (`Point`, `Word`, `ByteOffset`); the derive emits the
same shape, with the item array sized exactly to its own field count.

Note what the list does *not* contain: nothing builds or drives a deserializer for you. Building the one
`FlatDeserializer` over a finished buffer is the caller's step (`FlatDeserializer::new`, or a builder of
your own around it), and everything after that is lent. The one-shot drivers a buffer-owning caller might
want — `flat_deserializer`, `deserialize_one`, `items_of`, `single_item_buffer_as_envelop_payload`,
`iterable_element_ranges` — each build a deserializer of their own, so they live in
`sdave-codec/tests/common/mod.rs` rather than in the crate.

## Capability boundary

What the codec and the derive support today. Every _runtime_ claim below is pinned by a test file of the
codec: `sdave-codec/tests/primitive.rs` (the primitives), `iterable.rs`, `composite.rs` (hand-written
impls), `derive.rs` (the derive), `features.rs` (the feature-gated shapes) and `boundary.rs` (the error
paths). The compile-time gaps are pinned differently: they are the absence of an impl (or a macro
rejection), which no test can observe at run time.

### Supported (always available)

No feature can take these away. The structural ones say how values are arranged rather than how a leaf is
spelt, and `str` is the raw-text leaf: its payload already _is_ the bytes, so there is nothing about it to
make optional.

| shape                                             | `Serialize` | `Deserialize` | notes                                                                                                                                                                    |
| ------------------------------------------------- | ----------- | ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `()`                                              | yes         | yes           | an empty-payload Envelop                                                                                                                                                 |
| `str`, `&str`                                     | yes         | no            | the payload IS the string bytes, with no formatting or parsing in either direction; a `str` is deliberately NOT an iterable (`String` is the deserializable counterpart) |
| `&T`, `Box<T, A>`, `&[T]`, `Box<[T]>`             | yes         | no            | forwards to the referent's `Serialize`                                                                                                                                   |
| `[T]`                                             | yes         | no            | the iterable wire shape; a slice is unsized, so `Vec<T, A>` is its deserializable counterpart                                                                            |
| `[T; N]`                                          | yes         | yes           | the same iterable wire shape with a statically known element count                                                                                                       |
| a `#[derive(Serialize, Deserialize)]` struct/enum | yes         | yes           | named fields are matched by `Signature`, so field order is irrelevant; a tuple struct or tuple variant is matched by position                                             |
| `dyn Trait`, `Box<dyn Trait>`                     | yes         | yes (`Box`)   | needs `#[sdave_dyn(..)]`, whose requirements are in `sdave-derive/README.md`                                                                                             |

### Supported (one feature each)

Each shape is behind its **own** Cargo feature, so an application pulls in only the wire shapes it means
to accept. `primitive` and `vec` are on by default.

| feature                 | shape                                                                                               |
| ----------------------- | --------------------------------------------------------------------------------------------------- |
| `primitive` _(default)_ | the readable primitives: `u8`..`u128`, `usize`, `i8`..`i128`, `isize`, `f32`, `f64`, `bool`, `char` |
| `vec` _(default)_       | `Vec<T, A>`                                                                                         |
| `vec_deque`             | `VecDeque<T>`                                                                                       |
| `linked_list`           | `LinkedList<T>`                                                                                     |
| `btree_set`             | `BTreeSet<T>`                                                                                       |
| `option`                | `Option<T>`                                                                                         |
| `result`                | `Result<T, E>`                                                                                      |
| `boxed`                 | `Box<T, A>` **deserializer** (the `Serialize` side is always on)                                    |
| `rc`                    | `Rc<T>`                                                                                             |
| `arc`                   | `Arc<T>`                                                                                            |
| `cow`                   | `Cow<'_, B>` (serializes as `B`; deserializes to the owned form)                                    |
| `cell`                  | `Cell<T>`                                                                                           |
| `unsafe_cell`           | `UnsafeCell<T>`                                                                                     |
| `string`                | `String` (the owned, deserializable counterpart of `str`)                                           |

#### Readable leaf vs raw bytes

`primitive` is the one gate that is _on_ rather than off by habit: a number, `bool` or `char` is spelt as
its `Display` text and read back with `FromStr`, which is what makes a document human-readable — and what
makes a `u32` payload its decimal digits instead of its four raw bytes.

An application that does not need that readability turns the feature off and brings a leaf of its own. The
structural vocabulary does not change: tuples, `()`, arrays and slices are always on, and each of them
carries _whatever_ leaf it is given — `[T; N]` is first-class structural support for any `T` with the
codec impls, exactly like a tuple of `T`s. What changes is which leaves exist: `[u8; N]` stops compiling
when `primitive` does, because `u8` is one of the gated leaves, while `[RawByte; N]` (or an array of them)
works as soon as `RawByte` does.

Writing that leaf is the one thing the application owns: `sdave-codec/tests/composite.rs` is a worked
example of a hand-written `Serialize`/`Deserialize` pair (`frame_slice` out, `envelop_payload` in). And the
gate keeps the choice explicit — with the feature off there is no `Serialize`/`TypeName` impl for `u32`, so
a field that still names `u32` fails to compile instead of quietly shipping text:

```
error[E0277]: the trait bound `u32: sdave_codec::TypeName` is not satisfied
```

Nothing structural changes: `()`, arrays and slices keep their impls and their wire shapes, and `str`
stays raw. They compose with whichever leaves exist, so the gate only decides which leaf a container can
carry.

```toml
# readable documents: the default
sdave-codec = "0.5"

# raw bytes only: no `Display`/`FromStr` leaf. Re-list any other shape you still want,
# because `default-features = false` drops `vec` along with `primitive`.
sdave-codec = { version = "0.5", default-features = false, features = ["vec"] }
```

### NOT supported

- **Compile-time gaps** (the trait is simply not implemented, never a runtime capability): a shape whose
  feature is off (e.g. `u32` without `primitive`); `HashMap`/`HashSet` (they need `std`, and the codec is
  `no_std`); unions; references-as-output (`&T` has no deserializer, and neither does `&str`).
- **Bare tuples** (`(A, B, …)`): there is no impl for them, and that is deliberate. A tuple has no type
  name to identify it in a parent's `Signature`, and its slice count is not statically known, so it could
  only be handled through an in-place flatten and an unnameable value. Store a tuple-like value in a tuple
  struct (or a tuple enum variant) and derive `Serialize`/`Deserialize`: the wire shape is the same, the
  type names itself (`Pair(f32, f32)`), and the slice count is known at compile time. The unit tuple `()`
  *is* implemented, as the empty-payload Envelop.
- **Recursive / self-referential owned types** (a hand-built linked list or tree) do NOT derive, whatever
  the features are. A field's compile-time `SLICES` is needed to size the type's own `Slices` array, so a
  type that recurses through a container (`children: Vec<Self>`) trips the type-level constant cycle
  (`E0391`); a _generic_ recursive type compiles its impl but overflows (`E0275`) on use. Model recursive
  data as a `Vec` of a non-recursive element type, or hand-write its `Serialize`/`Deserialize` (as for
  unions).
- **An empty enum** (`enum Never {}`) derives `SLICES = 0` and a `match self {}`, which stable Rust rejects
  for a `&Self` scrutinee, so that shape is not supported yet.
- **A blanket `IntoIterator` impl is impossible**: `impl<T: IntoIterator> Serialize for T` overlaps every
  impl on a foreign type (`u32`, `Vec`, `[T; N]`, …) and is rejected by coherence (`E0119`). The
  containers above are enumerated instead.
- **`#[sdave_dyn]`**: a `dyn Trait` is bounded by what the attribute can claim, and the attribute's own
  requirements are `sdave-derive/README.md`'s.

### Malformed buffers

A malformed buffer never panics; it maps to a distinct `Error`, and every variant that describes
malformed data carries the absolute `(head, tail)` offsets of what it caught — a nested reader sees a raw
prefix of the very buffer its parent sees, so those offsets never have to be re-based by the caller.
An *empty* `limiter_pairs` set is not a document fault and so has no variant: building a
`FlatDeserializer` with one panics (core's `FlatDeserializer::new` contract).

- `NoSerializableLimiterPair { len }` — none of the `len` pairs provided could frame the payload under
  the strategy.
- `UnexpectedSignature { valid_signatures, caught }` — a signature the channel did not deliver or
  cannot accept: the required one is not there at all (`caught` locates the range that was searched),
  or the one that is there identifies nothing this type takes (a signature where none belongs — a
  positional slot, an iterable element — or a discriminator matching none of `valid_signatures`).
- `MissingEnvelop { required_by, caught }` — an Envelop that must be there is missing: `required_by` is
  `None` when `deserialize` was not fed a valid Envelop at all (and `caught` is the whole item that stood
  there), `Some(signature)` when that signature requires Envelop(s) that are absent (and `caught` is the
  range the reader searched: from the signature's tail to the end of its channel).
- `UnconfirmedDelimiter { limiter_slice, unconfirmed }` — a `Variant::V2` Envelop's delimiter is
  unconfirmed and the buffer was not declared final; `limiter_slice` is the frame it was read under and
  `unconfirmed` is the delimiter slice nothing terminated (the payload it settled on sits between the
  two).
- `InvalidPayload { type, caught }` — the payload at `caught` is not valid for `type`.
- `ArithmeticOverflow` — a well-formed decimal overflows the target integer (only reachable with the
  `primitive` feature).

`fmt_error(error, buffer)` (behind the `primitive` feature) renders any of them against the buffer it
was raised on: every range is printed with the bytes it covers, `\xNN`-escaping whatever is not valid
UTF-8, and saying `<outside the buffer>` when the buffer no longer holds it. `Display` cannot do that on
its own — an `Error` carries offsets, not the bytes they index, and nothing guarantees they are text.

## MSRV

**Rust 1.100+** — the allocator-parameterised core requires `allocator_api`. Until that stable release
lands, nightly is acceptable (verified: it compiles ungated on `1.101.0-nightly`); do not use
`#![feature(…)]` in library code, so the same source keeps working on stable 1.100.
