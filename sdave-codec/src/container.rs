//! The containers and wrappers the derive never generates.
//!
//! Nothing here allocates: the codec's containers are the ones with a fixed shape, and the
//! allocator-requiring ones (`Vec`, `String`) live in `sdave-alloc` so that this crate stays usable
//! with no allocation story at all.

use alloc::boxed::Box;

use crate::{Error, Payload, Serialize, Shape, Sink, depth_of};

/// The unit value: an empty payload, framed like a zero-field tuple struct.
impl Serialize for () {
    const SHAPE: Shape = Shape::Tuple;
    const DEPTH: u16 = 0;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        Payload::payload(self, sink)
    }
}

/// A tuple: one envelope holding one item envelope per element, in declaration order. No item is
/// labelled — the declaration supplies each element's type — and arity is capped at 12.
macro_rules! tuple {
    ($($field:ident),+ $(,)?) => {
        #[allow(non_snake_case)]
        impl<$($field: Serialize),+> Serialize for ($($field,)+) {
            const SHAPE: Shape = Shape::Tuple;
            const DEPTH: u16 = depth_of(&[$(<$field as Serialize>::DEPTH),+]);

            fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
                let ($($field,)+) = self;
                $( sink.item_run($field)?; )+
                Ok(())
            }
        }
    };
}

tuple!(A);
tuple!(A, B);
tuple!(A, B, C);
tuple!(A, B, C, D);
tuple!(A, B, C, D, E);
tuple!(A, B, C, D, E, F);
tuple!(A, B, C, D, E, F, G);
tuple!(A, B, C, D, E, F, G, H);
tuple!(A, B, C, D, E, F, G, H, I);
tuple!(A, B, C, D, E, F, G, H, I, J);
tuple!(A, B, C, D, E, F, G, H, I, J, K);
tuple!(A, B, C, D, E, F, G, H, I, J, K, L);

/// `Option`: the variant name alone for `None`, the variant plus one associated-value envelope for
/// `Some`.
impl<T: Serialize> Serialize for Option<T> {
    const SHAPE: Shape = Shape::Enum;
    const DEPTH: u16 = depth_of(&[<T as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        match self {
            Some(value) => sink.variant_with("Some", value),
            None => sink.variant("None"),
        }
    }
}

/// `Result`: the variant name, plus one associated-value envelope for the payload it carries.
impl<T: Serialize, E: Serialize> Serialize for Result<T, E> {
    const SHAPE: Shape = Shape::Enum;
    const DEPTH: u16 = depth_of(&[<T as Serialize>::DEPTH, <E as Serialize>::DEPTH]);

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        match self {
            Ok(value) => sink.variant_with("Ok", value),
            Err(error) => sink.variant_with("Err", error),
        }
    }
}

/// A `Box` is transparent: the value it points at keeps its own shape, framing and depth, so
/// `Box<T>` and `T` differ on the wire only in the marker of the declaration that names them.
impl<T: Serialize + ?Sized> Serialize for Box<T> {
    const SHAPE: Shape = T::SHAPE;
    const DEPTH: u16 = <T as Serialize>::DEPTH;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        (**self).append(sink)
    }
}

/// A reference is transparent, exactly like a `Box`.
impl<T: Serialize + ?Sized> Serialize for &T {
    const SHAPE: Shape = T::SHAPE;
    const DEPTH: u16 = <T as Serialize>::DEPTH;

    fn append(&self, sink: &mut Sink<'_>) -> Result<(), Error> {
        (**self).append(sink)
    }
}
