//! The scalar leaves: `Serialize` for the primitives, delegating to [`crate::Payload`].
//!
//! A leaf owns no framing: the envelope around it belongs to its field header or item position, so
//! the append entry emits the payload and nothing else. A leaf's shape is [`Shape::Tuple`] with no
//! items — the same empty-body framing a zero-field tuple struct has — and its structure depth is
//! `0`, so a record orders it before any aggregate field.

use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize,
};

use crate::{Error, NonMaxUsize, Payload, Serialize, Shape, Sink};

macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(
        impl Serialize for $ty {
            const SHAPE: Shape = Shape::Tuple;
            const DEPTH: u16 = 0;

            fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
                Payload::payload(self, sink)
            }
        }
    )*};
}

leaf!(
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    bool,
    char,
    f32,
    f64,
    str,
    [u8],
    NonMaxUsize,
    NonZeroU8,
    NonZeroU16,
    NonZeroU32,
    NonZeroU64,
    NonZeroU128,
    NonZeroUsize,
    NonZeroI8,
    NonZeroI16,
    NonZeroI32,
    NonZeroI64,
    NonZeroI128,
    NonZeroIsize,
);
