//! The record body: a generated body announces its fields, and the serializer orders them.
//!
//! Named-field decoding matches **by name, in any order**, so a record's wire order is the
//! serializer's to choose — the derive supplies the fields and never fixes their order. The order
//! is ascending [`Serialize::DEPTH`] (shallowest field first), ties in declaration order: the
//! record's own frame closes last, and its payload ends in the last field's frame tail, which is a
//! delimiter run of whichever pair that field's frame ended on — so leaving the *deepest* field
//! last leaves the profile's earlier, policy-preferred pairs available for the record's own frame.
//!
//! Collection allocates nothing: a generated body passes a slot array it sized from its own
//! declaration (a stack array), and the codec selects the next field to append by scanning it.

use crate::{AppendValue, Error, ErrorKind, Serialize, Sink};

/// One announced record field, and its slot in the caller's array.
///
/// A generated body fills `[FieldSlot::EMPTY; field_count]`; the serializer then appends the
/// announced fields in its own order, marking each slot as it goes.
pub struct FieldSlot<'v> {
    name: &'static str,
    marker: &'static str,
    depth: u16,
    value: Option<&'v dyn AppendValue>,
    appended: bool,
}

impl<'v> FieldSlot<'v> {
    /// An empty slot.
    pub const EMPTY: FieldSlot<'v> = FieldSlot {
        name: "",
        marker: "",
        depth: 0,
        value: None,
        appended: false,
    };
}

/// The fields of a record, as announced by a generated body.
///
/// A field's value must outlive the whole `record` call: the serializer appends the announced
/// fields *after* the body returns, in its own order.
pub struct Record<'a, 'v> {
    slots: &'a mut [FieldSlot<'v>],
    len: usize,
}

impl<'a, 'v> Record<'a, 'v> {
    /// How many fields have been announced.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no field has been announced.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Announce a field: its name, its declared type's marker spelling, and its value.
    ///
    /// The call order is only the tie-break of the serializer's own ordering; it is never the wire
    /// order by itself.
    ///
    /// A field whose declared type is unsized — `str`, `[u8]`, `dyn Trait`, a DST's tail — is
    /// announced through a *sized reference to it*: the field's access yields `&str`/`&[u8]`/
    /// `&dyn Trait`, which are sized values of a transparent [`Serialize`] implementation, so the
    /// erasure's source is `&&str` and never an unsized referent. Unsizing `&V` to a trait object
    /// requires `V: Sized`, which is why this entry takes sized field types and why no entry can
    /// take an unsized one.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Invariant`] when the caller's slot array is smaller than its own field list — a
    /// generated body sizes the array from the declaration, so this is a bug, not input.
    pub fn field<V: Serialize>(
        &mut self,
        name: &'static str,
        marker: &'static str,
        value: &'v V,
    ) -> Result<(), Error> {
        let index = self.len;
        let erased: &'v dyn AppendValue = value;
        let slot = self
            .slots
            .get_mut(index)
            .ok_or(Error::new(ErrorKind::Invariant(
                "the record's slot array is smaller than its field list",
            )))?;
        *slot = FieldSlot {
            name,
            marker,
            depth: <V as Serialize>::DEPTH,
            value: Some(erased),
            appended: false,
        };
        self.len = index + 1;
        Ok(())
    }
}

impl Sink<'_> {
    /// Append a record body: `body` announces the fields, and the serializer appends them.
    ///
    /// The values announced must outlive this call, and the slot array must be as long as the
    /// field list.
    ///
    /// See the module documentation for the order and why it is this one.
    pub fn record<'v, F>(&mut self, slots: &mut [FieldSlot<'v>], body: F) -> Result<(), Error>
    where
        F: FnOnce(&mut Record<'_, 'v>) -> Result<(), Error>,
    {
        let mut record = Record { slots, len: 0 };
        body(&mut record)?;
        let count = record.len;
        for _ in 0..count {
            let index = select_next(record.slots, count);
            let slot = &mut record.slots[index];
            slot.appended = true;
            let name = slot.name;
            let marker = slot.marker;
            let value = slot.value.ok_or(Error::new(ErrorKind::Invariant(
                "an announced record field has no value",
            )))?;
            self.field_header(name, marker)?;
            self.item_erased(value)?;
        }
        Ok(())
    }
}

/// The next field to append: the shallowest not-yet-appended slot, ties in declaration order.
fn select_next(slots: &[FieldSlot<'_>], count: usize) -> usize {
    let mut best: Option<usize> = None;
    for index in 0..count {
        if slots[index].appended {
            continue;
        }
        match best {
            Some(current) if slots[current].depth <= slots[index].depth => {}
            _ => best = Some(index),
        }
    }
    // The caller runs this exactly `count` times, and every round marks one slot.
    best.expect("a not-yet-appended slot exists")
}
