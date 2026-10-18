//! `Rc<T>` as a transparent wrapper, gated behind the `rc` feature.
//!
//! An `Rc<T>` serializes exactly like its referent `T` (sharing is not part of the
//! wire format), so `Rc<u32>` and `u32` are interchangeable on the wire.
//! Deserializing produces a fresh `Rc` (the container is `Global`-backed by
//! construction; `Rc<T, A>` would require the unstable `allocator_ext` feature).

use alloc::rc::Rc;

impl_shared_ptr!(Rc<T> => Rc::new);
