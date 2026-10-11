//! The **typed codec** layer of SDAVE: it turns a Rust value into SDAVE documents and back, on top
//! of the flat framing implemented in `sdave`.
//!
//! - a **profile** ([`Config`]) carries the variant, the ordered limiter pairs, the [`Limits`], the
//!   [`PairSelect`] policy and the [`NamePolicy`], plus a stable [`Fingerprint`];
//! - a **shape** ([`Shape`]) and a **structure depth** fix each type's framing before any value is
//!   inspected;
//! - a value is written by **one inner-to-outer pass** ([`Config::append`]) that frames each level
//!   as it closes, through a type-erased [`Sink`] carrying the naming policy dynamically;
//! - a value is read back by [`Deserialize`] over the core's deserializer state machine;
//! - every failure is a typed [`ErrorKind`] with an absolute input offset and an outer→inner path.
//!
//! This crate is `#![no_std]` and allocates nothing on its own: the document target is the
//! caller's, and `A` is never hidden.
//!
//! # One pass, no plan, no scratch
//!
//! Serialization walks the value **once**, bottom-up, appending into the caller's target. A level
//! closes by *inserting* its limiter slice before its own payload and its delimiter slice after it —
//! the payload is already in the target, so room for the limiter is made by moving that payload up.
//! Every framing decision is taken on those real bytes by [`sdave::can_serialize`], with the
//! existing content as its `buffer` (the left) and the level's payload as its `payload` (the
//! right): the "insert an envelope at the end of the existing contents" scene the core documents,
//! streaming small index to large index. The bytes *are* the plan, and the document's length is a
//! by-product of the pass.
//!
//! The target is the caller's storage and the codec never grows it. A target that may grow — backed
//! by the caller's own allocator — grows, and that is a capability extension the caller provides,
//! not the codec allocating; a fixed target reports [`ErrorKind::TargetExhausted`], and because one
//! pass learns a document's length only as it produces it, that error names a *lower bound* on the
//! length rather than an exact deficit.
//!
//! # The wire grammar
//!
//! ```text
//! document       = marked(root value)                        -- a standalone root
//! marked(v)      = marker-text, frame(body(v))               -- the root, and a dyn payload
//! frame(payload) = limiter * repeat, payload, delimiter * repeat
//!
//! body(Record)   = ( field-name, ":", marker-text, frame(body(field)) )*     -- the serializer's order
//! body(Tuple)    = frame(body(item))*                        -- declaration order, no names
//! body(Enum)     = frame(variant-name), frame(body(value))?  -- the tail only when it has fields
//! body(List)     = frame(""), frame(body(item))*             -- the mandatory empty list header
//! body(leaf)     = the leaf's text, verbatim
//! ```
//!
//! Markers appear exactly where a *name* does: at the document root, inside a record field header,
//! and in a `dyn` payload's value bytes. A nested aggregate therefore emits no marker of its own —
//! the field header or the declaration already supplies its type — which is also why a tuple item
//! and a list item are not labelled.
//!
//! A record's *field order* is the serializer's: named-field decoding matches by name in any order,
//! so [`Record`] appends the announced fields in ascending [`Serialize::DEPTH`] — shallowest first,
//! ties in declaration order.
//!
//! # `dyn` payloads
//!
//! A `dyn` field delivers identity instead of erasing it: the field header keeps the compiler
//! spelling of the *field type* (`Box<dyn …>` and `&dyn …` differ on the wire), and the value bytes
//! carry the *concrete* type's marked document. An application's object trait takes [`DynSerialize`]
//! as a supertrait — its [`DynSerialize::append_dyn`] is type-erased, and the [`Sink`] carries the
//! naming policy dynamically, so no fixed inner policy exists. On the read side the codec only
//! locates the concrete marker ([`DynPayload`], [`Config::locate_dyn_payload`]); resolving it and
//! constructing the value is the application's registry.
//!
//! # Wire stability
//!
//! There is **no wire-format version field**: the format is versioned by the library version, and
//! mixed-version data is unsupported. A marker is the compiler's own spelling filtered by the
//! [`NamePolicy`], so it is stable only for a given build; a type whose marker must survive a
//! rebuild or a `std`↔`no_std` move is declared as a [`ShortType`] entry instead.

#![no_std]
// `Error` is deliberately non-allocating: it carries a fixed-capacity outer→inner path (`Path`) by
// value, so a failure can say *where* it happened without an allocator the caller never handed us.
// Clippy's `result_large_err` advice — box the error — would allocate behind the caller's back,
// which this crate's contract forbids; the lint's size threshold is a heuristic, not a defect.
#![allow(clippy::result_large_err)]

extern crate alloc;

mod append;
mod basic;
mod config;
mod container;
mod decode;
mod dyn_payload;
mod encode;
mod error;
mod leaf;
mod name;
mod payload;
mod record;
mod shape;
mod target;
mod unit;

pub use crate::basic::{Basic, BasicType};
pub use crate::config::{Chosen, Config, Fingerprint, Limits, PairSelect};
pub use crate::decode::{
    DecodeSource, Deserialize, FieldHeader, option_or, option_or_default,
};
pub use crate::dyn_payload::{
    DYN_PAYLOAD_DEPTH, DynPayload, DynSerialize, locate_dyn_marker, locate_dyn_payload,
};
pub use crate::encode::{AppendValue, DocumentTarget, Serialize, Sink, deepest_of, depth_of};
pub use crate::error::{Error, ErrorKind, Limit, PATH_CAPACITY, Path, Step};
pub use crate::name::{
    NamePolicy, ShortType, marker_eq_ascii, own_path, short_name, units_eq_ascii,
};
pub use crate::payload::Payload;
pub use crate::record::{FieldSlot, Record};
pub use crate::shape::Shape;
pub use crate::target::{SliceTarget, insert_frame};
pub use crate::unit::Unit;

// An expansion names the runtime through this crate alone, so the handful of core items it — or a
// generated body — touches must be reachable at this crate's root as well.
pub use sdave;
pub use sdave::{
    DecoupledFlatDeserializer, DecoupledFlatDeserializerState, Envelop, FlatDeserializer,
    FlatDeserializerIter, FlatDeserializerState, InternalState, LimiterPair, NonEnvelop,
    NonMaxUsize, Offset, State, Variant, can_serialize, deserialize_incremental,
    get_all_serializable_limiter_pairs,
};
