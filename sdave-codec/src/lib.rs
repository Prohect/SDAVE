#![no_std]
#![warn(missing_docs)]
// Every block of unsafe code is already explicit, including inside `unsafe fn`.
#![deny(unsafe_op_in_unsafe_fn)]

//! Typed codec layer for SDAVE.
//!
//! This crate turns Rust values into SDAVE documents and back. It sits on top of
//! the flat framing implemented by [`sdave`], whose whole API it re-exports.
//!
//! The [`Serialize`]/[`Deserialize`] traits are generic over the basic data unit
//! `T` and the allocator `A`; the implementations this crate provides all target
//! `T = u8` and stay generic over `A`.
//!
//! The contract — the wire format, which half of it each shape provides, the Cargo
//! feature that gates each one, the error list and the MSRV — is written once, in
//! the crate `README.md`. This module keeps only what rustdoc needs: the map below
//! and the one example worth compiling.
//!
//! # Module map
//!
//! - `serialize` — the [`Serialize`] trait plus the framing primitives every impl
//!   builds on (`frame_slice`, `frame_region`, `frame_payload`, `flatten_in_place`, …);
//! - `deserialize` — the [`Deserialize`] trait;
//! - `impls` — the shape impls: the leaf shapes and the feature-gated ones;
//! - `iterable` — the iterable wire shape, shared by `[T]`, `[T; N]` and the heap
//!   containers;
//! - `name` — [`TypeName`] / [`NamePolicy`] rendering;
//! - `error` — [`Error`].
//!
//! # `dyn Trait`
//!
//! A trait opts into `dyn Trait` support with `#[sdave_dyn(..)]` from
//! `sdave-derive`; the attribute, what it emits and what it requires are documented in
//! `sdave-derive/README.md`. The doctest below pins its most important compile-time
//! consequence:
//!
//! ```compile_fail
//! use sdave_derive::{Deserialize, Serialize, sdave_dyn};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Claimed;
//!
//! #[sdave_dyn(Claimed)]
//! trait Tagged {}
//! impl Tagged for Claimed {}
//!
//! struct Unclaimed;
//! impl Tagged for Unclaimed {} // not claimed -> compile error
//! ```

extern crate alloc;

mod deserialize;
mod error;
mod impls;
mod iterable;
mod name;
mod serialize;

pub use sdave::*;

pub use crate::{deserialize::*, error::*, impls::*, iterable::*, name::*, serialize::*};

/// Implementation details used by `sdave-derive`'s generated code.
///
/// Not part of the public API; path-stable aliases only.
#[doc(hidden)]
pub mod __private {
    pub use alloc::alloc::Global;
    pub use alloc::boxed::Box;
    pub use alloc::vec::Vec;
}
