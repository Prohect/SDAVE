//! Micro-benchmark: checked vs `_unchecked` parse over a large mixed channel.
//!
//! Run with: `cargo run --release -p sdave --example bench`

use std::num::NonZeroUsize;
use std::time::Instant;

use sdave::{
    LimiterPair, State, Variant, parse, parse_incremental, parse_incremental_unchecked,
    parse_unchecked,
};

fn pairs() -> Vec<LimiterPair<'static, u8>> {
    [
        ('\u{2666}', '\u{2663}'),
        ('\u{25b6}', '\u{25c0}'),
        ('\u{2605}', '\u{2606}'),
    ]
    .into_iter()
    .map(|(l, d)| LimiterPair {
        least_repeat: NonZeroUsize::new(2).unwrap(),
        limiter: l.to_string().leak().as_bytes(),
        delimiter: d.to_string().leak().as_bytes(),
    })
    .collect()
}

/// A mixed channel: plain content peppered with short limiter runs (below
/// `least_repeat`) plus envelopes with multi-byte (de)limiters.
fn build_buffer() -> Vec<u8> {
    let mut s = Vec::with_capacity(4 << 20);
    while s.len() < (4 << 20) {
        s.extend_from_slice(
            "some plain content with a lone \u{2666} char and more text; ".as_bytes(),
        );
        s.extend_from_slice(
            "\u{2666}\u{2666}the payload of an envelope\u{2663}\u{2663}".as_bytes(),
        );
        s.extend_from_slice("tail content \u{25b6} not enough repeats; ".as_bytes());
        s.extend_from_slice("\u{25b6}\u{25b6}\u{25b6}p\u{25c0}\u{25c0}\u{25c0}".as_bytes());
    }
    s
}

fn drive(v: Variant, s: &[u8], pairs: &[LimiterPair<u8>], unchecked: bool) -> usize {
    let mut segments = 0;
    let mut st = unsafe {
        if unchecked {
            parse_unchecked(v, s, pairs)
        } else {
            parse(v, s, pairs)
        }
    };
    loop {
        if matches!(st, State::Empty) {
            break;
        }
        segments += 1;
        if !st.is_archived() {
            break;
        }
        st = unsafe {
            if unchecked {
                parse_incremental_unchecked(v, s, pairs, &st)
            } else {
                parse_incremental(v, s, pairs, &st)
            }
        };
    }
    segments
}

fn main() {
    let pairs = pairs();
    assert!(sdave::limiter_pairs_recommended(&pairs));
    let s = build_buffer();
    println!("buffer: {} bytes", s.len());
    for v in [Variant::V1, Variant::V2] {
        // The bench doubles as a smoke test: checked and unchecked MUST agree.
        assert_eq!(drive(v, &s, &pairs, false), drive(v, &s, &pairs, true));
        for unchecked in [false, true] {
            // Warmup.
            let segments = drive(v, &s, &pairs, unchecked);
            let t = Instant::now();
            const ROUNDS: usize = 20;
            for _ in 0..ROUNDS {
                std::hint::black_box(drive(v, &s, &pairs, unchecked));
            }
            let per_round = t.elapsed() / ROUNDS as u32;
            println!(
                "{v:?} {}: {per_round:?}/round ({:.1} MiB/s, {segments} segments)",
                if unchecked { "unchecked" } else { "checked  " },
                s.len() as f64 / per_round.as_secs_f64() / (1 << 20) as f64,
            );
        }
    }
}
