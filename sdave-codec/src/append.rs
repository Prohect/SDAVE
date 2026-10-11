//! The public append entries: one inner-to-outer pass, into the caller's target.

use core::alloc::Allocator;
use core::any::type_name;

use crate::{Config, DocumentTarget, Error, Serialize, Sink, SliceTarget, Unit};

impl<'l, 'v, T, A> Config<'l, 'v, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    /// Append `value` to `target` as a standalone document, and return its end offset.
    ///
    /// One pass, inner to outer: each level is framed as it closes, from the bytes the pass has
    /// already written. A standalone document root always emits its own marker before its
    /// envelope.
    ///
    /// The target is the caller's storage and the codec never grows it: a target that may grow —
    /// backed by the caller's own allocator — simply grows, and that is a capability extension the
    /// caller provides, not the codec allocating; a fixed target reports
    /// [`crate::ErrorKind::TargetExhausted`].
    ///
    /// # Errors
    ///
    /// [`crate::ErrorKind::TargetExhausted`] when a fixed target runs out; `Allocation` when a
    /// growing target's allocator refuses; [`crate::ErrorKind::LimitExceeded`] for a bound of
    /// [`Config::limits`]; [`crate::ErrorKind::Unframable`] when no pair can frame a level's
    /// payload; [`crate::ErrorKind::NotRepresentable`] or [`crate::ErrorKind::InvalidUtf8`] when a
    /// payload cannot be written in the document's unit; and the leaf errors of [`Serialize`].
    ///
    /// On any error the pass is rolled back: the target is truncated to where it started, so a
    /// target that may grow is left exactly as it was, and a fixed target's pre-existing content is
    /// untouched because everything the pass wrote lay beyond it.
    pub fn append<V: Serialize + ?Sized>(
        &self,
        value: &V,
        target: &mut dyn DocumentTarget,
    ) -> Result<usize, Error> {
        let start = target.cursor();
        let outcome = {
            let mut sink = Sink::new(&mut *target, self.name_policy(), self.limits());
            (|| -> Result<(), Error> {
                sink.marker(type_name::<V>())?;
                sink.item_run(value)
            })()
        };
        match outcome {
            Ok(()) => Ok(target.cursor()),
            Err(error) => {
                let _ = target.truncate(start);
                Err(error)
            }
        }
    }

    /// Append `value` to a fixed `[T]`, after the caller's `context_len` units of existing content,
    /// and return the document's end offset.
    ///
    /// The document occupies `units[context_len..end]`.
    ///
    /// # Errors
    ///
    /// [`crate::ErrorKind::TargetExhausted`] names the room the slice had and how far the pass
    /// reached — a *lower bound* on the document's length, because a single pass learns the length
    /// only as it produces it. A caller that wants the document anyway retries with a larger slice,
    /// or with a target it can grow.
    pub fn append_to_slice<V: Serialize + ?Sized>(
        &self,
        value: &V,
        units: &mut [T],
        context_len: usize,
    ) -> Result<usize, Error> {
        let mut target = SliceTarget::new(units, context_len, self);
        self.append(value, &mut target)
    }
}
