//! shared helpers for SDAVE integration tests, built on the public API only.
#![allow(dead_code)]

use sdave::*;
use std::num::NonZeroUsize;

pub fn nz(x: usize) -> NonZeroUsize {
    NonZeroUsize::new(x).unwrap()
}

pub fn off(x: usize) -> Offset {
    // SAFETY: test offsets are far below usize::MAX.
    unsafe { NonMaxUsize::new(x) }
}

pub fn pair(limiter: &[u8], delimiter: &[u8], least_repeat: usize) -> LimiterPair<u8> {
    LimiterPair {
        least_repeat: Some(nz(least_repeat)),
        limiter: limiter.to_vec(),
        delimiter: delimiter.to_vec(),
    }
}

/// the README example limiter pair set, minus ('♦','♣',1) which doesn't fit a u8 buffer.
pub fn std_pairs() -> Vec<LimiterPair<u8>> {
    vec![
        pair(b"^", b"~", 2),
        pair(b"?", b"!", 2),
        pair(b"%", b"%", 2),
        pair(b"{", b"}", 2),
    ]
}

/// a flattened snapshot of one iterated State, for easy assertions.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Unknown,
    Partial(usize),
    Ne(usize, usize),
    PhantomE,
    Env(usize, usize, usize, Option<usize>),
}

pub fn item_of(s: &State) -> Item {
    match s {
        State::Unknown => Item::Unknown,
        State::PartialLimiterSlice { head_offset } => Item::Partial(head_offset.get()),
        State::NonEnvelop {
            head_offset,
            tail_offset,
        } => Item::Ne(head_offset.get(), tail_offset.get()),
        State::E { envelop } => {
            if envelop.is_phantom() {
                Item::PhantomE
            } else {
                Item::Env(
                    envelop.head_offset().get(),
                    envelop.payload_head_offset().get(),
                    envelop.payload_tail_offset().get(),
                    envelop.tail_offset().map(|t| t.get()),
                )
            }
        }
    }
}

/// collect every iterated item, stepping the parser as needed.
pub fn collect<T: Sized + PartialEq + Clone>(parser: &mut FlatParser<T>) -> Vec<Item> {
    parser.parse_incremental();
    parser.iter().map(|s| item_of(&s)).collect()
}

pub fn collect_buf(variant: Variant, buf: &[u8], pairs: &[LimiterPair<u8>]) -> Vec<Item> {
    let mut parser = FlatParser::new(buf, variant, pairs.to_vec());
    collect(&mut parser)
}

/// collect only the real (non-phantom) archived Envelopes.
pub fn envelops_of(items: &[Item]) -> Vec<(usize, usize, usize, Option<usize>)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Env(h, ph, pt, t) => Some((*h, *ph, *pt, *t)),
            _ => None,
        })
        .collect()
}
