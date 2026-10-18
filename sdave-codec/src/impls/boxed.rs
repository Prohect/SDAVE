//! `Box<T, A>` deserialization, gated behind the `boxed` feature.
//!
//! `Box<T, A>` already forwards `Serialize`/`TypeName` to `T` (always on, via the
//! codec's pointer forwarding); this adds the missing `Deserialize`, which
//! allocates a fresh box in the caller's allocator. It is what lets an owning
//! `Box<Leaf>` field round-trip.

use core::alloc::Allocator;

use alloc::boxed::Box;

use sdave::{FlatDeserializer, Offset};

use crate::{Error, deserialize::Deserialize, name::NamePolicy};

impl<T, A> Deserialize<u8, A> for Box<T, A>
where
    T: Deserialize<u8, A, Type = T>,
    A: Allocator + Clone,
{
    type Type = Box<T, A>;

    fn deserialize<N: NamePolicy>(
        deserializer: &mut FlatDeserializer<'_, '_, u8, A>,
        final_buffer: Option<Offset>,
        allocator: A,
    ) -> Result<Self::Type, Error> {
        let value = <T as Deserialize<u8, A>>::deserialize::<N>(
            deserializer,
            final_buffer,
            allocator.clone(),
        )?;
        Ok(Box::new_in(value, allocator))
    }
}
