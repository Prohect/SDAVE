# sdave-alloc

The **ergonomic and allocator layer** of SDAVE: everything that needs an allocator, and nothing
that needs to make a decision.

**Status: to be built.** No code exists yet; this README is the specification.

## Position in the workspace

Depends on `sdave` (core) and `sdave-codec`. Nothing depends on it: `sdave-codec` must remain
usable without any allocation story, and `sdave-macro` never names it.

## What it owns

1. **The growable scratch.** The codec's plan/write needs storage for pending levels when a value's
   nesting is data-dependent (recursive/self-referential types). This crate provides the
   implementation of the codec's scratch abstraction over an application-supplied
   `A: Allocator`, alongside the blanket `&mut [Slot]` impl in the codec that a no-alloc caller
   can use instead.
2. **The retry helper.** The protocol the buffer model implies, made ergonomic:
   plan once → ask for the exact requirement → attempt a write into whatever buffer the caller
   already has (typically a stack array) → if it is short, allocate exactly that much through `A`
   and write again **with the same plan**. Because the requirement is exact, this terminates in one
   growth step; because the write is size-checked first, the caller's buffer is never left
   partially written.
3. **Document targets.** Writing a document directly into an allocator-backed container — the
   `Vec<T, A>`-shaped target — so an application's own allocator can back the output document.
4. **`Global` conveniences.** The `to_vec` / `to_string`-style calls and the `from_slice` /
   `from_str`-style counterparts that ordinary users expect, all bound to the global allocator, so
   that a user who does not care about allocators never types one.
5. **Allocator-requiring container impls** *(open — see below)*: the `Serialize`/`Deserialize`
   impls for `Vec<T>`, `String` and `Box<T>`, which cannot exist without an allocator. Putting them
   here is what lets `sdave-codec` stay allocation-free.

## What it must not own

Wire decisions of any kind: no naming policy, no marker rules, no shape rules, no framing
choices, no error vocabulary. It also never consumes the application's plan/state (SDAVE never
takes it), and it must not introduce an allocation into a path the caller believes is
allocation-free — every heap use here is visible as an `A` the caller supplied or as a documented
`Global`-bound convenience.

## The two caller shapes it must serve

| Caller | Needs from this crate |
| --- | --- |
| No-alloc / `no_std` embedded, fixed buffers on its stack | **nothing** — core + codec alone must suffice, with the blanket `&mut [Slot]` scratch and an explicit buffer |
| Ordinary application | the `Global` conveniences, so `to_vec`/`to_string`-style calls just work |

If the first row ever needs this crate, the layering has failed: report that as a defect in the
codec's scratch abstraction rather than growing this crate.

## Contracts it inherits

- **No partial output.** A short buffer is left byte-identical; the exact deficit is known before
  any byte is written.
- **The plan is the application's**, never taken or consumed by SDAVE.
- **Exact sizing, not estimation** — the whole reason the retry loop terminates in one step.
- **One naming policy, one config.** Conveniences must not introduce a second inner policy
  (the fixed inner policy of the pre-0.4 `dyn` implementation is gone by design).

## Open decisions

1. **Where the allocator-requiring container impls live** — here (recommended, so the codec stays
   allocation-free) or in `sdave-codec` behind an `alloc` feature.
2. **Whether `&str`/`String` text helpers belong here** or stay codec-side (`&str` needs no
   allocator).
3. **Names and shapes** of the scratch type, the document target, and the `Global` aliases.
4. **Whether a `Global` alias type is exported** (e.g. `type Std = core::alloc::Global`) or users
   write `Global` themselves.
5. **Whether the retry helper is a function, a builder, or a method on the document target.**

## MSRV

**Rust 1.100+**, inherited from the core: the allocator-parameterised types require
`allocator_api`. Nightly is acceptable until then; the crate must not need a feature gate on
`1.101.0-nightly` (verified for the core).
