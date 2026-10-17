# sdave-codec

The **codec** of SDAVE: it turns a Rust value into SDAVE documents and back, on top of the flat framing
implemented in `sdave` core which supports no_std and no_alloc(core has NO heap allocation AT ALL).

If sdave-codec MUST alloc on heap, it MUST explicitly require allocator from implementation and use it.

sdave-codec MUST NOT support dynamically expanding its capability(specifically, what types of value it
can handle) at run-time, serialization and deserialization MUST be determined compile-time and errors
sdave-codec user(AKA application) oriented MUST be sound -- as compile error.

This file is the canonical description of the **wire format** and of the **capability boundary**. The
macro surface (what the derives accept) is described once in `sdave-derive/README.md`, the codegen
internals once in `sdave-derive-impl/README.md`, and the framing protocol itself in the root `README.md`.

## The serialize/deserialize contract

sdave-codec supports primitives, tuples, structs, enums and dyn Trait fields. Unions are out of scope: the application MUST own its own design for union serialization/deserialization and impl `serialize` & `deserialize` manually, while sdave-codec only provides a scaffold.

sdave-codec and sdave-derive MUST NOT provide a derive macro for a union, or for any data structure containing a union, without a manual `serialize` & `deserialize` impl.

sdave-codec impls `Serialize` and `Deserialize` for primitives, behind the `primitive` feature (see [Readable leaf vs raw bytes](#readable-leaf-vs-raw-bytes)).

A `Deserialize::deserialize` call takes a `final_buffer: Option<Offset>` flag, documented on the trait method itself: `Some(offset)` is the caller's promise that the buffer is complete (asserting `offset == buffer.len()`), which is the only way to accept a `Variant::V2` Envelop whose delimiter slice has no following non-delimiter byte; `None` rejects such an Envelop with `Error::UnconfirmedDelimiter`. Internal recursive deserialization always passes the flag for the bounded sub-slice it hands down, so nested `Variant::V2` Envelops are handled gracefully.

Every `Signature` type text is rendered through a `NamePolicy` method parameter: each type provides its full qualified name (ignoring generic arguments) via `TypeName`, and the policy decides which names it shortens to their last `::` segment. Rendering is per node, so shortening one type (e.g. an outer generic) never affects another (e.g. its arguments). The default `NoShortening` policy never shortens. A type used in a signature MUST implement `TypeName` (impls are provided for the primitives and the standard containers; `#[derive(Serialize)]` provides one for a user type).

There are two parts of `z: MyType`: `name` + `type`. When both exist (an anonymous field, i.e. a field in a tuple, is compatible), a separator is used to separate them (e.g. `: ` for a `u8` buffer, as `::` can appear in `type`). Let's name it `Signature`. A `Signature` is a NonEnvelop.

NonEnvelop(`Signature`) + Envelop(`value`) MUST be sequential: NonEnvelop(`Signature`) followed by its <!-- plural for Iterables --> Envelop(s).

Because SDAVE core needs the payload determined so that it can find the longest run(assume repeats as `r`) in the payload matching the delimiter, then frame the payload with the delimiter slice repeated (`r` + NonZero) times to generate a valid Envelop, `serialize` MUST call `serialize` recursively internally BEFORE framing its own layer. The sdave-codec deserializer MUST support orderless field serialization(we have NonEnvelop(`Signature`) ahead of Envelop(`value`) for each named field).

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

Assume we shorten `Value`(serialized) as `V`, `Signature` as `S`, `LimiterSlice` as `L`, `DelimiterSlice` as `D`:

A tuple struct and a bare tuple share one wire shape: each *positional* field is one
`NonEnvelop("(T)")` signature followed by its value Envelop. They differ only in the outer signature
`serialize_typed` writes (`MyType` here, versus `(f32, f32)` for the bare tuple).

```conceptual
let mut buffer: Vec<u8> = Vec::<u8>::new();
assume_eq!(
    z.serialize(&mut buffer),
    Envelop(    // L0
        NonEnvelop("(f32)"),    // S1
        Envelop("114.514"), // L2 V3 D4
        NonEnvelop("(f32)"),    // S5
        Envelop("350234.0"), // L6 V7 D8
    ),  // D9
);
buffer.clear();
assume_eq!(
    z.serialize_typed(&mut buffer, Name { head_offset: 0, tail_offset: 0 }),
    Envelop(
        NonEnvelop("MyType"),
        Envelop(
            NonEnvelop("(f32)"),
            Envelop("114.514"),
            NonEnvelop("(f32)"),
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
        NonEnvelop("z: MyType"),    // S9
        Envelop(    // L10
            NonEnvelop("(f32)"),    // S11
            Envelop("114.514"), // L12 V13 D14
            NonEnvelop("(f32)"),    // S15
            Envelop("350234.0"), // L16 V17 D18
        ),  // D19
    ),  // D20
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
            NonEnvelop("z: MyType"),
            Envelop(
                NonEnvelop("(f32)"),
                Envelop("114.514"),
                NonEnvelop("(f32)"),
                Envelop("350234.0"),
            ),
        ),
    )
);
```

`Envelop(NonEnvelop(b<(f32)>)Envelop(b<114.514>)NonEnvelop(b<(f32)>)Envelop(b<350234.0>))` comes from `serialize` of `MyType`, `Signature` MUST be determined and serialized by a `MyType` value holder -- the `to_serialize`'s `z` here.

An empty `Name`(`head_offset == tail_offset`) writes no `name` and no separator, so the signature is the bare type describer, e.g. `MyType`.

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
assume_eq!(
    MyEnum::UnitLike.serialize(&mut buffer),
    Envelop("UnitLike") // L0 V1 D2
);
```

Rust solves the size of such type by padding and then all variants can be Sized on stack, i.e. for the example above, it would be size 12 align 4. Similarly, `serialize` MUST return `[Option<(Offset, Offset)>; n]` where `n` is the greatest possible `n` over ALL variants, using a phantom offset (`None`) as the placeholder for padding. i.e. for the `MyEnum` here, it needs `[Option<(Offset, Offset)>; 13]` to store its serialization slices, while for UnitLike, it has 3 Somes and 10 Nones in the returned result of `serialize`.

Specially, for a NOT Sized type(serialization not sized, not Rust Layout, semantical instead of transparent inner raw value), e.g. Iterable(Vec, Map, LinkedList etc.), or dyn Trait pointer(&dyn Trait, &mut dyn Trait, Box<dyn Trait> etc.), sdave-codec or sdave-derive cannot provide a light, safe, idiomatic way to handle them without (a) heap memory copying/sorting; OR (b) relying on an unsafe procedural that can always produce it.

### `dyn Trait`

A `dyn` field is enabled by `#[sdave_dyn(..)]` on the trait; what the attribute does is described once in
`sdave-derive/README.md`. Its wire consequence: serializing a `Box<dyn Trait>` / `&dyn Trait` writes one
Envelop whose payload is the concrete type's `Signature` NonEnvelop followed by the concrete value's
Envelop, flattened in place so the `dyn` field still takes exactly one limiter/value/delimiter triple,
like any other Sized value.

sdave-codec won't duplicate the type marker for each element. It instead designs: (NonEnvelop(`Signature`) ++ ) Envelop(empty payload) ++ Envelop(element0) ++ Envelop(element1) ++ ..... <==> Iterable. This differs real empty v.s. semantic empty, e.g. `[]` and `[""]`. `[Empty Envelop, [Envelop ......]]`. These Envelops are expected to be serialized sequentially in buffer when returned to its caller(application or some recursive from other `serialize` that depends on it), they would together take 1 LimiterSlice slot and 1 ValueSlice slot and 1 DelimiterSlice slot to locate them in buffer -- thus it won't break the deserialization -- but MUST require being flattened in-place(within implementation of such `serialize`) (for efficiency, it's recommended to serialize all elements of this Iterable, then flatten once by extending the buffer from the slice, since the flattened MUST have the exact same len as before, len of it times of swap would solve, then drop the unflattened one)(requires mem move, but is considered acceptable).

Specially, `&str` MUST NOT be considered Iterable: it is the raw-text leaf shape (payload = the UTF-8 bytes), not a sequence.

### Field attributes

`#[derive(Serialize, Deserialize)]` reads two field attributes, `#[default(<expr>)]` and
`#[skip(<expr>)]`. Their semantics are documented once, in `sdave-derive/README.md`; what belongs here is
the wire consequence:

- `#[skip(<expr>)]` — the type has one field fewer on the wire: no `Signature`, no slot.
- `#[default(<expr>)]` — nothing changes on the wire; only the reader may go without the field.

## Capability boundary

What the codec and the derive support today. Every *runtime* claim below is pinned
by a test file of the codec: `sdave-codec/tests/primitive.rs` (the primitives),
`iterable.rs`, `composite.rs` (hand-written impls), `derive.rs` (the derive),
`features.rs` (the feature-gated shapes) and `boundary.rs` (the error paths). The
compile-time gaps are pinned differently: they are the absence of an impl (or a
macro rejection), which no test can observe at run time.

### Supported (always available)

No feature can take these away. The structural ones say how values are arranged rather than
how a leaf is spelt, and `str` is the raw-text leaf: its payload already *is* the bytes, so
there is nothing about it to make optional.

| shape | `Serialize` | `Deserialize` | notes |
| --- | --- | --- | --- |
| tuples `(A, B, …)`, arities 1–12 | yes | yes | wire-identical to a tuple struct; positional on the way back |
| `()` | yes | yes | an empty-payload Envelop |
| `str`, `&str` | yes | no | the payload IS the string bytes, with no formatting or parsing in either direction; a `str` is deliberately NOT an iterable (`String` is the deserializable counterpart) |
| `&T`, `Box<T, A>`, `&[T]`, `Box<[T]>` | yes | no | forwards to the referent's `Serialize` |
| `[T]` | yes | no | the iterable wire shape; a slice is unsized, so `Vec<T, A>` is its deserializable counterpart |
| `[T; N]` | yes | yes | the same iterable wire shape with a statically known element count |
| a `#[derive(Serialize, Deserialize)]` struct/enum | yes | yes | named fields are matched by `Signature`, so field order is irrelevant |
| `dyn Trait`, `Box<dyn Trait>` | yes | yes (`Box`) | needs `#[sdave_dyn(..)]`, whose requirements are in `sdave-derive/README.md` |

### Supported (one feature each)

Each shape is behind its **own** Cargo feature, so an application pulls in only the
wire shapes it means to accept. `primitive` and `vec` are on by default.

| feature | shape |
| --- | --- |
| `primitive` *(default)* | the readable primitives: `u8`..`u128`, `usize`, `i8`..`i128`, `isize`, `f32`, `f64`, `bool`, `char` |
| `vec` *(default)* | `Vec<T, A>` |
| `vec_deque` | `VecDeque<T>` |
| `linked_list` | `LinkedList<T>` |
| `btree_set` | `BTreeSet<T>` |
| `option` | `Option<T>` |
| `result` | `Result<T, E>` |
| `boxed` | `Box<T, A>` **deserializer** (the `Serialize` side is always on) |
| `rc` | `Rc<T>` |
| `arc` | `Arc<T>` |
| `cow` | `Cow<'_, B>` (serializes as `B`; deserializes to the owned form) |
| `cell` | `Cell<T>` |
| `unsafe_cell` | `UnsafeCell<T>` |
| `string` | `String` (the owned, deserializable counterpart of `str`) |

#### Readable leaf vs raw bytes

`primitive` is the one gate that is *on* rather than off by habit: a number, `bool` or
`char` is spelt as its `Display` text and read back with `FromStr`, which is what makes a
document human-readable — and what makes a `u32` payload its decimal digits instead of
its four raw bytes.

An application that does not need that readability turns the feature off and brings a leaf of its own.
The structural vocabulary does not change: tuples, `()`, arrays and slices are always on, and each of
them carries *whatever* leaf it is given — `[T; N]` is first-class structural support for any `T` with
the codec impls, exactly like a tuple of `T`s. What changes is which leaves exist: `[u8; N]` stops
compiling when `primitive` does, because `u8` is one of the gated leaves, while `[RawByte; N]` (or a
tuple of them) works as soon as `RawByte` does.

Writing that leaf is the one thing the application owns: `sdave-codec/tests/composite.rs` is a worked
example of a hand-written `Serialize`/`Deserialize` pair (`frame_slice` out,
`single_item_buffer_as_envelop_payload` in). And the gate keeps the choice explicit — with the feature
off there is no `Serialize`/`TypeName` impl for `u32`, so a field that still names `u32` fails to
compile instead of quietly shipping text:

```
error[E0277]: the trait bound `u32: sdave_codec::TypeName` is not satisfied
```

Nothing structural changes: tuples, `()`, arrays and slices keep their impls and their wire
shapes, and `str` stays raw. They compose with whichever leaves exist, so the gate only decides
which leaf a container can carry.

```toml
# readable documents: the default
sdave-codec = "0.4"

# raw bytes only: no `Display`/`FromStr` leaf. Re-list any other shape you still want,
# because `default-features = false` drops `vec` along with `primitive`.
sdave-codec = { version = "0.4", default-features = false, features = ["vec"] }
```

### NOT supported

- **Compile-time gaps** (the trait is simply not implemented, never a runtime
  capability): a shape whose feature is off (e.g. `u32` without `primitive`);
  `HashMap`/`HashSet` (they need `std`, and the codec is `no_std`); unions;
  references-as-output (`&T` has no deserializer, and neither does `&str`).
- **Recursive / self-referential owned types** (a hand-built linked list or tree)
  do NOT derive, whatever the features are. A field's compile-time `SLICES` is
  needed to size the type's own `Slices` array, so a type that recurses through a
  container (`children: Vec<Self>`) trips the type-level constant cycle (`E0391`);
  a *generic* recursive type compiles its impl but overflows (`E0275`) on use.
  Model recursive data as a `Vec` of a non-recursive element type, or hand-write
  its `Serialize`/`Deserialize` (as for unions).
- **An empty enum** (`enum Never {}`) derives `SLICES = 0` and a `match self {}`, which
  stable Rust rejects for a `&Self` scrutinee, so that shape is not supported yet.
- **A blanket `IntoIterator` impl is impossible**: `impl<T: IntoIterator> Serialize
  for T` overlaps every impl on a foreign type (`u32`, `Vec`, `[T; N]`, …) and is
  rejected by coherence (`E0119`). The containers above are enumerated instead.
- **`#[sdave_dyn]`**: a `dyn Trait` is bounded by what the attribute can claim, and the
  attribute's own requirements are `sdave-derive/README.md`'s.

### Malformed buffers

A malformed buffer never panics; it maps to a distinct `Error`:

- `EmptyLimiterPairs` — the given `limiter_pairs` set is empty.
- `NoSerializableLimiterPair` — no pair could frame the payload under the strategy.
- `UnexpectedItem` — the buffer is not the expected Envelop / signature / value.
- `UnconfirmedDelimiter` — a `Variant::V2` Envelop's delimiter is unconfirmed and
  the buffer was not declared final.
- `InvalidPayload` — a payload is not valid for the target type.
- `ArithmeticOverflow` — a well-formed decimal overflows the target integer (only
  reachable with the `primitive` feature).

## MSRV

**Rust 1.100+** — the allocator-parameterised core requires `allocator_api`. Until that stable
release lands, nightly is acceptable (verified: it compiles ungated on `1.101.0-nightly`); do not
use `#![feature(…)]` in library code, so the same source keeps working on stable 1.100.
