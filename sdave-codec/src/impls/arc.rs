//! `Arc<T>` as a transparent wrapper, gated behind the `arc` feature.
//!
//! An `Arc<T>` serializes exactly like its referent `T` (sharing is not part of the
//! wire format), so `Arc<u32>` and `u32` are interchangeable on the wire.
//! Deserializing produces a fresh `Arc` (the container is `Global`-backed by
//! construction; `Arc<T, A>` would require the unstable `allocator_ext` feature).

use alloc::sync::Arc;

impl_shared_ptr!(Arc<T> => Arc::new);
