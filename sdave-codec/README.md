# sdave-codec

The **typed codec** layer of SDAVE: it turns a Rust value into SDAVE documents and back, on top of
the flat framing implemented in `sdave`.

**Status: to be implemented — and it goes first.** The macro crate (`sdave-macro`) derives `impl`s
that call *this* crate, so this crate's surface must exist, pass review, and be verified before any
macro work starts. The `sdave` core is implemented and covered by `SDAVE/tests/**` (69 passing
tests, 0 ignored, `cargo clippy` clean).

## Position in the workspace

| Crate | Owns | Depends on |
| --- | --- | --- |
| `sdave` (core) | flat framing: limiter pairs, the deserializer state machine, `can_serialize`, one-step `deserialize_incremental` | nothing |
| **`sdave-codec`** (this) | the typed codec: profile (`Config`), shape and marker rules, the single inner-to-outer serializer, the typed deserializer, errors, the `dyn` machinery | `sdave` |
| `sdave-alloc` | `Global` conveniences, `Vec`-shaped document targets, the growable scratch, the retry helper | `sdave`, `sdave-codec` |
| `sdave-macro` | the derives and the attribute macro; emits calls into this crate | nothing of ours (host-only proc-macro) |

The workspace root is `sdave/Cargo.toml`, whose `[workspace]` table currently lists **no members** —
this crate has to be added there, and it is the only place a manifest edit outside this crate is
needed.

## What this crate owns, and what it must not

**Owns:** `Config` (variant + ordered pairs + `Limits` + pair-select policy) and its fingerprint;
`Shape`; the `Serialize` / `Deserialize` traits; the **single inner-to-outer serializer**; the
typed deserializer over the core's state machine; `NamePolicy` / `ShortType` and marker equivalence;
`Payload` and `BasicType`; `Error` / `ErrorKind`; the `dyn` payload machinery; **writing the
framing bytes** (limiter slice, payload, delimiter slice) — the core deliberately has no serializer.

**Must not:** own IO or streams; keep a global or mutable name table; allocate behind the caller's
back — every allocation goes through an allocator the caller passed; define application protocol
grammars (the AK4H `ToolCall` wrapper is the application's); construct trait objects when decoding
a `dyn` field (that is the application's registry).

## The core primitives this crate is built on

Read `sdave/src/lib.rs` for exact signatures — this section fixes the *semantics* that matter, not
the spelling, so it cannot rot. The core offers:

1. **`LimiterPair<'l, T, A>`** — `least_repeat: Option<NonZeroUsize>` (private, accessor), `limiter`
   / `delimiter: OnceCell<&'l Vec<T, A>>` where the `OnceCell` wraps the *reference*; the pair is
   immutable once inited. An empty `limiter` or `delimiter` makes the pair **ignored** (inert in
   both directions — see the accessor docs), which is why the policy must never rely on one.
2. **`FlatDeserializer` + `FlatDeserializerState`** — the state machine, one item per
   `deserialize_incremental` step; `deserialize_all` drains to a fixpoint; `iter` / `iter_from`
   yield the item sequence and never expose an intermediate unarchived state; `advance` trims a
   consumed prefix (dropping covered items, truncating a straddling Envelop, re-inserting the
   phantom marker, shifting offsets); `decouple` hands out the parts and `new_unchecked` accepts
   them back; `repeat_map_on_cursor` reports per-pair live repeats (`None` = mismatch, which is
   also what an *ignored* pair reports); `is_limiter_pairs_safe` audits a pair list for the
   documented safety recommendation.
3. **`can_serialize(buffer, payload, variant, limiter_pair) -> Option<NonZeroUsize>`** — the
   encoder's primitive and **the only place the "disabled pair" outcome exists**: `Some(repeat)`
   is the minimal working repeat for appending an Envelop whose payload is `payload` into `buffer`;
   `None` means this pair cannot frame this payload in this context. `None` is per payload and per
   context: it never mutates a pair, a `Config`, or a fingerprint. Its documented `# Safety`
   contract is the caller's obligation (a clean item boundary context, and at most the confirmation
   of the last item of the existing contents).
4. **`get_all_serializable_limiter_pairs(...)`** — the same question asked of every pair, **written
   into a caller-supplied `Vec`** (no hidden allocation, no allocator argument). This is the input
   to pair selection; the policy below chooses among its results.
5. **`deserialize_incremental(buffer, &old_state) -> new_state`** — the free-function form of one
   step, taking the variant and pair list from the state.

## The serialize contract: one inner-to-outer pass

- **One pass.** Serialization walks the value **once**, bottom-up (inner to outer), and materializes
  each level's framing into the caller's buffer as that level closes. There is no separate size/plan
  pass and no separate write pass: the bytes *are* the plan, and the document's length is a
  by-product of the pass, not a number computed ahead of it.
- **The buffer is the caller's, and it may grow.** The caller hands a buffer — in practice a `Vec`
  over its own allocator. Appending to it is the caller's capability, and a reallocation is a
  **capability extension** the caller provides; it is not SDAVE allocating, and it does not make
  SDAVE require an allocator. SDAVE only ever appends to what it was handed, over the allocator the
  caller passed.
- **Framing matches `can_serialize`'s designed scene.** When a level closes, its framing is decided
  by `can_serialize(context = buffer[..payload_head], payload = buffer[payload_head..], variant,
  pair)` over the **real, already-written bytes**: the existing content is the *left* (the buffer)
  and the level's payload is the *right* (the payload) — the "insert an envelope at the end of the
  existing contents" scene the core documents, streaming small index to large index. The limiter
  slice is placed before the payload and the delimiter slice after it.
- **Field order is the serializer's choice.** Named-field decoding matches **by name, in any order**,
  so a record's fields may be written in any order whatsoever. The codec orders them by the
  **structure-depth tree** (and other factors where needed) so that the sequence of `can_serialize`
  contexts is favourable; no order is ever required to be the declaration order, and decoding is
  unaffected.
- **`A` is never hidden.** The core's types are parameterised by an allocator, and the caller passes
  one explicitly wherever the core needs it; this crate allocates nothing on its own. Container
  impls that *require* an allocator (`Vec<T>`, `String`, `Box`) are intended for `sdave-alloc`
  (see *Still open*) so this crate can be used with no allocation story at all.
- **Object-safety drives the `dyn` design.** The `dyn` payload trait is a supertrait of an object
  trait, so its methods must be **type-erased** — no `NamePolicy` and no allocator generics. The
  write side therefore passes a sink that carries the naming policy *dynamically*, which is why no
  fixed inner policy (the old `DynNames`) is needed, and why the same config serves the whole
  document.
- **Pair selection** is a policy over `get_all_serializable_limiter_pairs`' results, carried on the
  profile: `FirstMatch` (default; first pair in profile order that can frame), `LeastRepeat`
  (fewest delimiter repeats), `SmallestFrame` (smallest framed envelope; ties by repeats, then
  order). Minimising repeats is what stops a nested document from growing without bound when its
  payloads collide with the first pair's delimiter. The policy never affects decoding.

## The wire contracts this crate must uphold

**Markers.** A marker is the compiler-reported type name (`type_name`) filtered by the
application's `NamePolicy`, **verbatim otherwise**. Rules: each constructor's own fully-qualified
path is looked up; a whitelisted constructor emits its last identifier, everything else keeps its
full path; entries are type *exemplars* (`ShortType::of::<Vec<()>>()` selects `Vec`, covering every
`Vec<T>`); matching uses the complete constructor path, never a suffix; shortening does not
propagate across nesting; entries cannot supply replacement names; two constructors that would
abbreviate to the same short name are an `InvalidNamePolicy` error rather than an order-based
resolution; the policy is a compile-time constant, immutable for the life of a serializer. Sender
and receiver must agree: an incoming marker is compared against the policy-rendered expectation in
both directions, with the documented tuple-comma-padding equivalence and no other normalisation.

Because markers are compiler-derived, **a marker is stable only for a given build**; a type whose
marker must survive a rebuild or a `std`↔`no_std` move is declared in `SHORT_TYPES`. Do not promise
more than that. (There is no wire-format version field: the format is versioned by the library
version, and mixed-version data is unsupported — say so rather than trying to detect it.)

**`dyn` fields.** The field header keeps the **verbatim** spelling (`Box<dyn …>` and `&dyn …` differ
on the wire); the **value bytes carry the concrete type's marked document** — identity is
*delivered, not erased* — and decoding it requires the application's registry, because a trait
object cannot be constructed here.

**Shape mapping.**

| Declaration | Wire shape | Framing |
| --- | --- | --- |
| Named struct | `Record` | one record envelope of `Name: Type` field runs |
| Unit struct | `Record` | one empty record envelope |
| Tuple struct / newtype / zero-field tuple struct | `Tuple` | one value envelope holding positional item envelopes, one per field |
| Unit variant `V` | `Enum` | variant envelope only |
| Single-field variant `V(T)` | `Enum` | variant envelope + one associated-value envelope holding `T`'s payload |
| Multi-field variant `V(A, B)` | `Enum` | variant envelope + one associated-value envelope holding a positional tuple body |
| Named-field variant `V { x, y }` | `Enum` | variant envelope + one associated-value envelope holding a context-known record envelope |

`V`, `V()`, and `V {}` keep **distinct** framing — a zero-field tuple variant is not a unit variant.
Tuple arity is capped at 12. Nested aggregates elide the redundant marker only when the surrounding
context already supplies the type (a field header declares the child's type); a standalone document
root always emits its own marker.

**Leaves.** Canonical decimal integers (`-001` decodes to `-1`), finite-only floats, `true`/`false`,
a single-scalar `char`, borrowed `&str`, byte payloads, `NonMaxUsize` and the standard nonzero
types, `()`, `Option<T>`, `Box<T>`, `Result<T, E>`, tuples. `Vec<T>` frames a mandatory empty header
envelope followed by one item envelope per element.

**Decoding rules.** Named fields are matched **by name, in any order**; duplicate, unknown and
missing fields are errors; every field header's marker is validated before its value run is read;
defaults run only after the record is confirmed complete and at most once each; unknown variants,
wrong arity, nonempty or missing list headers and truncated frames are errors; decoded leaves are
borrowed verbatim from the input wherever possible.

**Profile identity.** `Config` carries the variant, the ordered pair list, `Limits`, and the
pair-select policy. Its **fingerprint** is a stable digest over exactly those, with a documented
byte encoding; adding a field changes every digest, so either the fingerprint carries a version
component or it is documented as a **version-tagged** value that may not be compared across library
versions (see *Still open*).

**Errors.** Typed `ErrorKind` set, each carrying an absolute input offset plus an outer→inner path
(`Field` / `Variant` / `Index`); `impl core::error::Error` (stable since 1.81, so no `std` needed).
Host source paths (`file!()`/`line!()`) exist only as a **feature-gated trace** capability: absent
from `Display` and from `Eq` by default, and never part of the observable contract.

## Obligations toward the derive (the macro-facing surface)

This is the part `sdave-macro` will be written against, so it must be decided here and kept stable:

1. **A per-type shape constant** the derive can set (`Record` / `Tuple` / `Enum`) so the framing is
   known before any value is inspected — that is what makes empty vectors and unit variants
   decodable.
2. **One callable append entry per type**, which a generated body calls for each of its children and
   which frames the level as it closes. The **serializer** owns the order in which a record's fields
   are appended (see *Field order is the serializer's choice*); the derive supplies the fields and
   never fixes their order.
3. **A type-erased write handle** (the sink carrying the naming policy dynamically) so a generated
   `dyn` delegation is object-safe.
4. **Field-header emission and marker validation on decode** — the derive only maps a declaration
   to a shape; it must not compose headers, compare markers or touch leaf bytes itself.
5. **A `BasicType` escape hatch** for `#[sdave(basic)]` (fallible, must not panic, works with the
   enclosing envelope's payload rather than adding an outer marker).
6. **A default-provider hook** for absent named fields, run only after the record is confirmed
   complete and at most once per field, with `#[sdave(default)]` / `#[sdave(default = path)]`
   meaning exactly what the macro README says.
7. **A stable emitted path** the derive can name — `::sdave_codec` by default, redirectable with
   `#[sdave(crate = …)]` (settled; see below). Everything an expansion names must be reachable at
   this crate's root, including the handful of core items an expansion touches.
8. **Container and leaf impls for everything the derive never generates** — see the shape table.

## Decisions taken as settled for implementation

These were resolved in the 0.4.0 design thread and are to be implemented as written; the ones I
recommended and the maintainer has not contradicted are marked *(recommended)* — escalate rather
than deviate silently if one turns out not to work.

- Capability tiers: `core` (no_std, caller-supplied `A`), `alloc`, `macro`; **no `std` crate**, and
  errors ride `core::error::Error`.
- **One inner-to-outer pass** into the caller's buffer; that buffer may grow, and a reallocation is
  a capability extension the caller provides, not an SDAVE allocation. There is no plan pass and no
  scratch. *(settled by the maintainer)*
- **Field order is the serializer's choice**, sorted by the structure-depth tree; decoding is
  order-independent. *(settled by the maintainer)*
- `dyn` fields: verbatim header spelling, concrete **marked** document in the value bytes,
  type-erased sink so no fixed inner policy is needed.
- Markers: verbatim compiler spellings, the policy as the only naming authority, `ShortType` as the
  opt-in stability mechanism; no canonicalisation, no cross-build stability promise.
- No wire-format version field; the format is versioned by the library version.
- Host-path tracing is feature-gated and non-observable.
- **Emitted path:** an expansion names the runtime through one path, this crate
  (`::sdave_codec`), with a serde-style `#[sdave(crate = …)]` override; there is **no facade
  crate** — the core cannot re-export the codec without inverting the layering — so this crate is
  what the derives name, and it must re-export at its root everything a derive needs.

## Still open (decide while implementing, or escalate)

1. **The fixed-buffer contract** — what a caller that hands a *fixed* slice (rather than a growable
   buffer) gets when it is too short: an exact deficit with no partial output, or a documented
   requirement to size it first. A growable-buffer caller simply grows.
2. **Enumeration order** — the secondary factors beyond the structure-depth tree that order a
   record's fields, and how a field's depth is known for a recursive type.
3. **Whether `deserialize_all`-style drain helpers survive** now that the application drives
   segments itself.
4. **Whether `Variant` keeps exactly V1/V2.**
5. **Fingerprint versioning** — a version component, or documented version-tagged values.
6. **Where the allocator-requiring container impls live** — `sdave-alloc` *(recommended)* or here
   behind an `alloc` feature.

## MSRV

**Rust 1.100+** — the allocator-parameterised core requires `allocator_api`. Until that stable
release lands, nightly is acceptable (verified: it compiles ungated on `1.101.0-nightly`); do not
use `#![feature(…)]` in library code, so the same source keeps working on stable 1.100.
