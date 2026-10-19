# SDAVE

**S**treamable **D**elimiter-**A**daptive **V**erbatim **E**nvelope protocol.

SDAVE is a **fundamental**, flat (nesting-agnostic, payload-agnostic) framing
protocol. A serialize-time-defined, configurable delimiter slice is reserved
from the payload, and the payload is decoded verbatim -- byte-identical to what
was written.

It separates serialized (pending (still-streaming)) Envelops from (pending)
NonEnvelops out of **one** buffer (channel) that mixes them together, and those
NonEnvelops are considered as expected.

The core, the codec and the derive macro are all `#![no_std]`. The core does
**no** allocation at all: any heap allocation is delegated to the allocator the
application explicitly passes in, and the codec allocates only what the value it
hands back owns -- the three `Vec`s a `FlatDeserializer` owns are **lent** down
the nesting and taken back on return, so allocation follows the returned data
rather than the document size or the nesting depth
(see: [`sdave-codec/README.md`](sdave-codec/README.md), *Allocation model*).
They are generic over the basic data unit `T` (`Clone + PartialEq + Sized`) and
the allocator `A` (`Allocator + Clone`). Buffers are addressed by `Offset`
(a `NonMaxUsize`: a `usize` that can hold every value except `usize::MAX`).

## Crates

| crate | description |
| --- | --- |
| `sdave` | the flat framing core: `LimiterPair`, `Variant`, `Item`, `Envelop`/`NonEnvelop`, `FlatDeserializer`/`FlatDeserializerState`/`InternalState`, `Slices`/`Cursor2D`, `can_serialize`/`get_all_serializable_limiter_pairs`, and the heap-free collector `Stack`. |
| `sdave-codec` | the typed codec: `Serialize`/`Deserialize`, `TypeName`/`NamePolicy`. |
| `sdave-derive` | `#[derive(Serialize)]`, `#[derive(Deserialize)]`, plus the `#[sdave_dyn]`, the field attributes `#[skip(<expr>)]` and `#[default(<expr>)]`. |
| `sdave-derive-impl` | the codegen behind macros; for insight; not stable. |

The core owns the framing and the heap-free collector; the codec owns the typed
wire rules and the allocator discipline. The wire rules themselves are stated
once, in `sdave-codec/README.md`.

## Motivation

Existing serialization protocols include JSON, XML, TOML, YAML, Protobuf,
Rust raw strings, traditional C strings, Java strings, etc.

Can be summarized to four fundamental payload-protocols:

1. **Fixed reserved slices + payload escape mechanism** -- fixed reserved
   slices is reserved from the payload, plus payload escape mechanisms.
   - The payload may **NOT** be **byte-identical** to what was written.
   - `O(m * 2^n)` escapers are needed for `n` levels of nested serialized
     payload with `m` escaper-requiring slices: mental burden for payload
     writers, and wasted IO when heavily nested content is serialized in
     the payload.
2. **Length-prefix** -- the payload length MUST be defined ahead of the
   payload.
   - The full payload must be known before serialization; incremental
     payload streaming byte-identical to what was read is impossible.
3. **Strict static structure** (e.g. Protobuf) -- markers and build-time
   defined static paths.
   - Even worse for incremental payload streaming.
4. **Configurable delimiter slice** -- a configurable delimiter slice is
   reserved from the payload.
   - Undefined behaviour when the payload collides with the configured
     delimiter slice; the writer had better know what can appear in an
     incremental streaming payload.

For LLM agentic harness usages, an LLM roughly thinks about what it is
going to call **before** making the real call, and can consider delimiter
slice definition within thinking -- so serialize agent tool call into
SDAVE could work.

## Design

### Limiter slice / Delimiter slice

A `LimiterPair` is a triple of `(least_repeat, limiter, delimiter)`:

- `limiter` -- an immutable slice of `T`, e.g. a UTF-8 1B/2B/3B char in a `u8`
  buffer.
- `delimiter` -- another immutable slice of `T`.
- `least_repeat` -- the minimum number of repeats.

An Envelop is framed as `least_repeat` (or more) repeats of the `limiter`,
then the payload, then the same number of repeats of the `delimiter`.
Repeating the delimiter slice solves the `payload–delimiter slice` corruption
issue, otherwise the payload is broken by SDAVE when there's a conflict between
payload and delimiter slice.

A pair with an empty `limiter`/`delimiter`, or with `least_repeat == None`, is
ignored (it can never match).

### Payload nesting

An Envelop's payload may itself be a channel: the frames inside it are ordinary
Envelops and NonEnvelops built by the same rule, so composition needs no second
primitive -- there is no length prefix and no escaper. Two uses are load-bearing
in `sdave-codec`: an iterable is `Envelop(marker) ++ Envelop(element0) ++ …`,
and a **positional composite** (a tuple struct or tuple variant) is one Envelop
whose payload is the sequential field Envelops, identified by its type text
(`Pair(f32, f32)`) instead of a per-field signature.

`can_serialize` and the framing helpers take such a payload as a list of
logical slices (`&[Option<&[T]>]`), not as one contiguous region
(see: [Serializer limiter pair select](#serializer-limiter-pair-select)). 

A serializer can frame an unflattened result and leave the bytes where they were
written. That is deliberate -- flattening mid-serialization is an extra copy of
every payload, the expensive part that a nesting-heavy document would pay at
every level. Whether a value is flattened at all is the caller's (i.e. the typed
layer's) decision, never a requirement of the frame.

### Serializer limiter pair select

The serializer, which may pick any pair that frames a payload, is supported by
two helpers over a given buffer tail and payload:

- `can_serialize` -- reports, for a single pair, the minimal working repeat
  (guaranteed `>= least_repeat`), or `None` if that pair cannot frame the
  payload without conflicts.
- `get_all_serializable_limiter_pairs` -- collects every pair that can frame the
  payload, each carrying its minimal working repeat.

Minimising repeats matters because a payload that collides with one pair's
delimiter forces that pair's delimiter slice to grow past the run inside the
payload; a nested document whose payloads collide with the first pair's
delimiter otherwise grows without bound with nesting depth. Which policy to
apply (first-match order, fewest repeats, smallest frame, ...) is left to the
application. It never changes how documents are decoded.

### Delimiter match algorithm

There's not a full winner, choose based on scene. The behaviour is selected per
deserializer via `Variant`.

#### `Variant::V1`

The very first full matched delimiter slice is just the delimiter slice. No
delimiter slice confirmation latency, but the tail of the payload MUST NOT
collide with the delimiter. This limiter pair mental burden addon is NOT
solvable on application level.

For V1, header and tailer of payload and tailer of existing contents (if NOT
ended by a complete V1 envelop) in the mixed buffer before this envelop can each
disable matching limiter pair. Two contiguous envelops can have the same limiter
pair.

Assume limiter pair set as the example above (`(limiter, delimiter, least_repeat)`),
e.g. `('^','~',2)`, `('?','!',2)`, `('%','%',2)`, `('{','}',2)`, `('♦','♣',1)`.

```
0%%A%%%%B%%%%C%%1 -> [0, payload A, payload B, payload C, 1]
0%%A%%%%B%%%%C%%1% -> [0, payload A, payload B, payload C, 1]
0%%%%%%B%%%%C%%1 -> [0, pending B%%%%C%%1]
0^^~~^^B~~{{C}}1 -> [0, payload (empty(payload_head_offset == payload_tail_offset)), payload B, payload C, 1]
1^^^echo^^hello~~~~~2 -> [1, payload echo^^hello, ~~2]
```

Assume a bad example of a limiter pair set containing A and B with
`(A.delimiter == B.limiter)`: `('^','~',2)`, `('~','%',2)`.

```
0^^A~~~~B%%~~C%%1 -> [0, payload A, payload B, payload C, 1]
```

Assume a bad example of a shared limiter `('^','~',2)`, `('^','~~',2)`, `('^','!',2)`.

```
1^^A!B!~~~~C -> [1, payload A!B!, ~~C] // when parser checks 2nd ~ behind B!, there's a valid delimiter slice.
```

#### `Variant::V2`

The very first full matched delimiter slice FOLLOWED by a NON-delimiter is the
delimiter slice. Tailer of payload won't disable any limiter pair, but
introduces delimiter slice confirmation latency. This latency is solvable on the
application level, by automatically pushing a non-delimiter to the mixed
channel.

For V2, header of payload and tail of existing contents in the mixed buffer
before this envelop can at most each disable one limiter pair. Thus at most 2
limiter pairs can be disabled for this envelop. Two contiguous envelops MUST
NOT share the same limiter pair if this limiter pair matches limiter start with
delimiter OR delimiter start with limiter. And V2 introduces a delimiter slice
confirmation latency. For streaming, a V2 Envelop reserves the delimiter from
the header of the item behind (NEVER contributes to mental burden as both
variants already require buffer MUST not end with limiter).

Assume limiter pair set as the example above.

```
0%%A%%%%B%%%%C%%1 -> [0, payload A%%, B, pending C%%1]
0{{A}}{{B}}??C!!1 -> [0, payload A, payload B, payload C, 1]
0{{A}}{{B}}??C!!! -> [0, payload A, payload B, pending C!]
1^^^echo^^hello~~~~~2 -> [1, payload echo^^hello~~, 2]
1^^^echo^^hello~~~~~ -> [1, pending echo^^hello~~ ]
```

Assume a bad example of a limiter pair set containing A and B with
`(A.delimiter == B.limiter)`: `('^','~',2)`, `('~','%',2)`.

```
0^^A~~~~B%%~~C%%1 -> [0, payload A~~, B%%, payload C, 1]
```

Assume a bad example of a shared limiter `('^','~',2)`, `('^','~~',2)`, `('^','!',2)`.

```
1^^A!B!~~~~C -> [1, payload A!B!~~, C] // when parser checks 4th ~ behind B!, it is terminated by next C, then SDAVE chooses the first matched LimiterPair. Application's responsibility/freedom to stabilize the behaviour by sorting the input LimiterPairs.
```

With `('^','~~',2)`, `('^','~',2)`, `('^','!',2)`:

```
1^^A!B!~~~~C -> [1, payload A!B!, C] // when parser checks 4th ~ behind B!, it is terminated by next C, then SDAVE chooses the first matched LimiterPair.
```

### API overview

- `Variant` -- selects the delimiter match algorithm (`V1` / `V2`, see above).
- `LimiterPair` -- the `(least_repeat, limiter, delimiter)` frame definition.
- `Item` -- either an `Envelop` or a `NonEnvelop` boundary.
- `Envelop` / `NonEnvelop` -- an Envelop is a payload framed by a matching
  limiter/delimiter pair; a NonEnvelop is a confirmed buffer slice that is not
  an Envelop. An Envelop is either archived (its `tail_offset` is `Some`, it
  will never grow again) or pending/unarchived (`tail_offset` is `None`).
- `FlatDeserializer` -- deserializes a `T` buffer, caching the confirmed items
  and driving the incremental deserialization. `iter()`/`iter_from()` yield
  `Item`s, eagerly deserializing as it goes; `deserialize_incremental()` /
  `deserialize_all()` drive it directly. Its storage (`archived_boundaries`,
  `repeat_map`, `limiter_pairs`) is lendable: `decouple`/`new_unchecked` move
  it, `take_*`/`swap_*` hand it to a nested reader and back, and `clone`
  copies only the position. A deserializer whose storage is lent out
  (`archived_boundaries == None`) panics on `iter`,
  `deserialize_incremental`, `deserialize_all` and `advance`. `advance` drops
  the boundaries the offset fully covers, truncates a straddling one, and
  shifts the rest.
- `FlatDeserializerState` / `InternalState` -- the deserialization state,
  decoupled from the buffer itself. It reports how far the buffer has been
  settled, whether the current item is archived, and whether the buffer is
  fully deserialized (`InternalState::is_clean` / `is_archived` /
  `is_exhausted`).
- `deserialize_incremental` -- the core step: advance the state by exactly one
  item (archive it, or settle more of the current unarchived item).
- `update_internal_cache` -- reset the per-pair scan cache (`bsize` and the
  repeat counts) and rewind a partially scanned item to the last settled
  boundary. This is what makes lending a deserializer's storage safe: the
  borrower's scan cannot survive as a stale count.
- `can_serialize` / `get_all_serializable_limiter_pairs` -- the serializer side
  helpers (see above).
- `Slices` / `Cursor2D` -- a read-only 2D cursor over a payload kept as a
  logical list of slices (`&[Option<&[T]>]`) rather than one contiguous region;
  it is what the `can_serialize` helpers read a payload with.
- `Stack` -- a native-stack-frame collector for a compile-time-unsized
  sequence: `Stack::with(source, producer, consumer, mutatable)` produces one
  `T` per call onto the current native frame, then consumes the whole sequence
  **once** from the deepest frame, where every node is still alive (`consumer`
  is not called for an empty stack). It is how a per-element slice list avoids
  a heap `Vec`.

### Q&A

Q: Why not force `limiter != delimiter`?

A: SDAVE is a fundamental general-purpose protocol, and a sized `T` may not have
that many reservable variants.

Q: Why does NonEnvelop exist?

A: Cases are that different info streams share the same channel. LLM output is
just a typical one.

Q: Why is `FlatDeserializer::archived_boundaries` an `Option`?

A: Because it is *lent* to a nested reader. `None` means "in flight", which is
why the accessors are `Option`/`unsafe`, and why driving a lent-out deserializer
panics instead of silently caching the borrower's items.

Q: Does the codec allocate per nesting level?

A: No. `Deserialize` receives a `&mut FlatDeserializer`, lends its three `Vec`s to
the next level and takes them back, and an iterable's element ranges are stacked on
native frames, so only the value returned to the caller (plus `[T; N]`'s element
list) is allocated -- `sdave-codec/README.md`, *Allocation model*.

Q: Why does `Deserialize::deserialize` take a `FlatDeserializer` instead of `&[T]`
with variant and limiter pairs?

A: The deserialize impl MUST need at least 3 `Vec`s for internal state, item boundaries,
limiter pairs(limiter pairs vec also contains internal cache state), if the fn is not
declarared with access to them, EACH nesting layer MUST allocate 3 `Vec`s -- the fragment
churn the design exists to avoid.

## Scope

SDAVE differs (pending) NonEnvelops from (pending) Envelops in a mixed buffer/channel.

Application can make use of unarchived Envelop for **well defined streamable
recoverable** payload usage. In case of that, it's the application's own
responsibility to verify and deal with pending payload and prepare
solutions for Errors (i.e. implementation layer protocol Error).
