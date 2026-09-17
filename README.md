# SDAVE

**S**treamable **D**elimiter-**A**daptive **V**erbatim **E**nvelope protocol.

SDAVE is a **fundamental**, flat (nesting-agnostic, payload-agnostic)
serialization protocol optimized for LLM agentic harness usage. It reserves
a serialize-time-defined delimiter slice from the payload and decodes the
payload verbatim — byte-identical to what was written. It tells serialized
envelopes and potentially streaming envelopes and non-envelope slices from
one slice(channel) mixing them together, and those non-envelope slices won't
be considered as unexpected results.

## Motivation

Existing serialization protocols include JSON, XML, TOML, YAML, Protobuf,
Rust raw strings, traditional C strings, Java strings, etc.

Can be summarized to four fundamental payload-protocols:

1. **Fixed reserved slices + payload escape mechanism** — fixed reserved
   slices is reserved from the payload, plus payload escape mechanisms.
   - The payload may **NOT** be **byte-identical** to what was written.
   - `O(m * 2^n)` escapers are needed for `n` levels of nested serialized
     payload with `m` escaper-requiring slices: mental burden for payload
     writers, and wasted IO when heavily nested content is serialized in
     the payload.
2. **Length-prefix** — the payload length MUST be defined ahead of the
   payload.
   - The full payload must be known before serialization; incremental
     payload streaming byte-identical to what was read is impossible.
3. **Strict static structure** (e.g. Protobuf) — markers and build-time
   defined static paths.
   - Even worse for incremental payload streaming.
4. **Configurable delimiter slice** — a configurable delimiter slice is
   reserved from the payload.
   - Undefined behaviour when the payload collides with the configured
     delimiter slice; the writer had better know what can appear in an
     incremental streaming payload.

For LLM agentic harness usages, an LLM roughly thinks about what it is
going to call **before** making the real call, and can consider delimiter
slice definition within thinking — so serialize agent tool call into
SDAVE could work.

## Design

```rust
/// `T`: basic data unit of a buffer.
/// `limiter`/`delimiter` a slice of vec of T, e.g. UTF-8 2B char in a u8 buffer.
/// Technically, limiter can equal to (a repeating unit/a sequence of)delimiter: it would cause conflicts between empty Envelop and long limiter slice; in case of that, SDAVE always consider the slice as long limiter slice, and an empty-payload-Envelop is impossible; it's not recommended.
/// Technically, one limiter can has multiple delimiters in different LimiterPairs, it's not recommended.
/// Technically, one delimiter can has multiple limiters in different LimiterPairs, it's not recommended.
struct LimiterPair<T: Sized>{
    /// 0 would behave like 1, match `limiter`/`delimiter` first, then guard on `least_repeat` where assertion(repeat>=1) is always true.
    least_repeat: NonZeroUsize,
    limiter: &[T],
    delimiter: &[T],
}
enum State {
    /// empty, or `s`[..] is a potential limiter slice, or `s`[..] MUST be a limiter slice, but repeats not well defined by being terminated by a not repeating T.
    Empty,
    N(NonEnvelop),
    Y(Envelop),
}
struct NonEnvelop{
    head_offset: usize,
    /// can monotonic increase in next updates, `s`[head_offset..tail_offset] MUST NOT 1.contains valid limiter slice; 2.ends with potential limiter slice.
    tail_offset: usize,
    /// true means this NonEnvelop is confirmed archived, would never grow again(there MUST be a valid limiter slice behind tail_offset).
    ok: bool,
}
struct Envelop {
    head_offset: usize,
    /// `payload_head_offset` could be Some when and only when limiter slice well defined(at least one non-limiter T(either payload head or delimiter not eq to limiter) exists following limiter slice).
    payload_head_offset: Option<NonZeroUsize>,
    /// can monotonic increase in next updates, `s`[payload_head_offset..payload_tail_offset] MUST NOT 1.contains valid delimiter slice; 2.ends with potential delimiter slice.
    payload_tail_offset: Option<NonZeroUsize>,
    /// Some(offset) means this Envelop is confirmed archived, would never grow again.
    /// An archived Envelop, with limiter != delimiter, can has empty payload, e.g. `^^~~`, in case of which, both payload offset being None. An empty Envelope is an empty Envelope, not a NonEnvelop.
    /// It could be an NonEnvelop or an Envelop behind.
    tail_offset: Option<NonZeroUsize>,
}
// not own the buffer so that Application can full control buffer's lifetime.
// SAFETY: SDAVE never owns buffer in this demo, can not confirm the buffer is monotonic recieving or offset MUST be updated to existing State.
impl Envelop{
    unsafe fn offset(&mut self, offset: usize);
}
impl NonEnvelop{
    unsafe fn offset(&mut self, offset: usize);
}

unsafe fn parse<T: Sized>(v: Variant, s: &[T], limiter_pairs: &[LimiterPair<T>]) -> State;
/// returned State's head_offset != state's head_offset means state is confirmed Ok, and thus returned State start alfter tail_offset of state.
/// for a streaming `s` already containing non-empty contents, never pass an Empty state or it reparse from offset 0. 
unsafe fn parse_incremental<T: Sized>(v: Variant, s: &[T], limiter_pairs: &[LimiterPair<T>], state: &State) -> State;
```

### Limiter slice / Delimiter slice

- The limiter pair is one of a configurable, SDAVE-parse-time-fixed set,
  e.g. `[('^', '~', 2), ('?', '!', 2), ('%', '%', 2), ('{', '}', 2), ('♦', '♣', 1)]`.

Note: In case of agentic harness, for u8 buffer, since ASCII only use 0x00..0x7F,
and LLM can output UTF-8, it's best practice to reserved UTF-8 only chars;
it's recommended to only reserve limiterPairs whose limiter != delimiter,
and the limiterPair set match eq(len(intersect(all limiter,all delimiter)),0)
and set.any(|p|set.any(|p1|p.as_ptr()!=p1.as_ptr&&(p.limiter==p1.limiter||
p.delimiter==p1.delimiter))).is_none();

The limiter slice is `<$limiter>[...]`: least_repeat or more repeats of a limiter.
The delimiter slice is then the other delimiter of same pair repeat same times.
Repeating the same delimiter solves `payload–delimiter slice` corruption issue,
otherwise the payload is broken by SDAVE when there's conflict between payload
and delimiter slice.

#### Delimiter match algorithm

There's not a full winner, choose based on scene.

##### Variant1

The every first full matched delimiter slice is just the delimiter slice. No
delimiter slice confirmation latency, but tail of payload MUST NOT collide with
delimiter. This limiter pair mental burden addon is NOT solvable on application level.

For variant1, head and tail of payload and tail of existing contents(if NOT ended
by a complete variant1 envelope) in the mixed buffer before this envelope can at
most each disable one limiter pair. Thus at most 3 limiter pairs can be disabled
for this envelope. Two contiguous envelopes can have same delimiter pair.

assume limiter pair set same as example every above.
`0%%A%%%%B%%%%C%%1` -> [0, payload A, payload B, payload C, 1]
`0%%A%%%%B%%%%C%%1%` -> [0, payload A, payload B, payload C, 1]
`0%%%%%%B%%%%C%%1` -> [0, pending B%%%%C%%1]
`0^^~~^^B~~{{C}}1` -> [0, payload (offset range None), payload B, payload C, 1]
`1^^^echo^^hello~~~~~2` -> [1, payload echo^^hello, ~~2]

assume a bad example of a limiter pair set containing A and B with (A.delimiter == B.limiter)
[('^', '~', 2), ('~', '%', 2)].
`0^^A~~~~B%%~~C%%1` -> [0, payload A, payload B, payload C, 1]

assume a bad example of a shared limiter
[('^', '~', 2), ('^', "~~", 2), ('^', '!', 2)].
`1^^A!B!~~~~C` -> [1, payload A!B!, ~~C] // when parser check 2rd ~ behind B!, there's a valid delimiter slice. 

##### Variant2

The every first full matched delimiter slice FOLLOWED by a NON-delimiter is the
delimiter slice. Tail of payload won't disable a limiter pair, but would introduce
delimiter slice confirmation latency. This latency is solvable on application
level, by automatically push a non-delimiter to mixed channel.

For variant2, head of payload and tail of existing contents in the mixed buffer
before this envelope can at most each disable one limiter pair. Thus at most 2
limiter pairs can be disabled for this envelope. Two contiguous envelopes MUST
NOT share same delimiter pare(won't contribut 2 to 3). And variant2 introduce
a dilimiter slice confirmation latency.

assume limiter pair set same as example every above.
`0%%A%%%%B%%%%C%%1` -> [0, payload A%%, B, pending C%%1]
`0{{A}}{{B}}??C!!1` -> [0, payload A, payload B, payload C, 1]
`0{{A}}{{B}}??C!!1` -> [0, payload A, payload B, payload C, 1]
`0{{A}}{{B}}??C!!!`  -> [0, payload A, payload B, pending C!]
`1^^^echo^^hello~~~~~2` -> [1, payload echo^^hello~~, 2]
`1^^^echo^^hello~~~~~` -> [1, pending echo^^hello~~ ]

assume a bad example of a limiter pair set containing A and B with (A.delimiter == B.limiter)
[('^', '~', 2), ('~', '%', 2)].
`0^^A~~~~B%%~~C%%1` -> [0, payload A~~, B%%, payload C, 1]

assume a bad example of a shared limiter
[('^', '~', 2), ('^', "~~", 2), ('^', '!', 2)].
`1^^A!B!~~~~C` -> [1, payload A!B!~~, C] // when parser check 4th ~ behind B!, it is terminated by next C, then SDAVE choose the first matched LimiterPair. Application's responsibility/freedom to stablize the behaviour by sort the input LimiterPairs.

[('^', "~~", 2), ('^', '~', 2), ('^', '!', 2)].
`1^^A!B!~~~~C` -> [1, payload A!B!, C] // when parser check 4th ~ behind B!, it is terminated by next C, then SDAVE choose the first matched LimiterPair. 

### Q&A

Q Why not force limiter != delimiter?
A SDAVE is a fundamental general purpose protocol, Sized T may not have that much reservable variants.

Q Why is NonEnvelope existing?
A Cases are that different info stream share same channel. LLM output is just a typical one.

## Scope

A buffer can be [content_slice0 /* State::EMPTY */, OK(SDAVE0),content_slice1, OK(SDAVE1), PENDING(SDAVE2)].

SDAVE differs NonEnvelope and Envelope and pending potential Envelope from
mixed buffer/channel.

It's application's responsibility to explain/make use of NonEnvelope
and Envelope and pending potential Envelope.

Application can make use of PENDING(payload_head_offset) for
**well defined streamable recoverable** payload usage. In case
of that, it's application's own responsibility to verify and
deal with potentially pending payload and prepare solutions for
false-positive envelope.
