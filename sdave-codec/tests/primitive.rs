//! The primitives: the scalar codec (round trips, the `Display`-payload contract,
//! the reserved formatter bounds) plus tuples and `()`.
//!
//! The scalar half needs the `primitive` feature; the test target pins it on (see
//! `Cargo.toml`), because every payload in this suite is written through it.

mod common;

use std::alloc::Global;

use common::{document, many_pairs, payload_of, primitive_pairs, round_trip, round_trip_with};
use sdave_codec::*;
use sdave_derive::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Round trips.
// ---------------------------------------------------------------------------

#[test]
fn scalars_round_trip_at_their_extremes() {
    assert_eq!(round_trip(0u8), 0);
    assert_eq!(round_trip(u8::MAX), u8::MAX);
    assert_eq!(round_trip(u16::MAX), u16::MAX);
    assert_eq!(round_trip(0u32), 0);
    assert_eq!(round_trip(u32::MAX), u32::MAX);
    assert_eq!(round_trip(u64::MAX), u64::MAX);
    assert_eq!(round_trip(u128::MAX), u128::MAX);
    assert_eq!(round_trip(usize::MAX), usize::MAX);
    assert_eq!(round_trip(i8::MIN), i8::MIN);
    assert_eq!(round_trip(i16::MIN), i16::MIN);
    assert_eq!(round_trip(i32::MIN), i32::MIN);
    assert_eq!(round_trip(i64::MIN), i64::MIN);
    assert_eq!(round_trip(i128::MIN), i128::MIN);
    assert_eq!(round_trip(isize::MIN), isize::MIN);
    assert_eq!(round_trip(114.514f32), 114.514f32);
    assert_eq!(round_trip(f64::MAX), f64::MAX);
    assert_eq!(round_trip(f64::MIN), f64::MIN);
    assert_eq!(round_trip(f64::MIN_POSITIVE), f64::MIN_POSITIVE);
    assert_eq!(round_trip(f64::from_bits(1)), f64::from_bits(1));
    assert_eq!(round_trip(f32::MAX), f32::MAX);
    assert_eq!(round_trip(f32::MIN_POSITIVE), f32::MIN_POSITIVE);
    assert_eq!(round_trip(f32::from_bits(1)), f32::from_bits(1));
    assert!(round_trip(true));
    assert!(!round_trip(false));
    assert_eq!(round_trip('\0'), '\0');
    assert_eq!(round_trip('x'), 'x');
    assert_eq!(round_trip('🦀'), '🦀');
}

// ---------------------------------------------------------------------------
// Tuples and `()` (treated as primitives).
// ---------------------------------------------------------------------------

#[test]
fn tuples_round_trip() {
    let pairs = many_pairs();
    assert_eq!(round_trip_with((42u32,), &pairs), (42u32,));
    assert_eq!(round_trip_with((42u32, true), &pairs), (42, true));
    assert_eq!(round_trip_with((1u32, false, 'x'), &pairs), (1, false, 'x'));
    // A nested tuple and a long one.
    assert_eq!(
        round_trip_with(((1u32, 2u32), (true,)), &pairs),
        ((1, 2), (true,))
    );
    let twelve = (
        1u8, 2u8, 3u8, 4u8, 5u8, 6u8, 7u8, 8u8, 9u8, 10u8, 11u8, 12u8,
    );
    assert_eq!(round_trip_with(twelve, &pairs), twelve);
}

#[test]
fn tuple_and_tuple_struct_share_a_wire_shape() {
    // A tuple and an identically-shaped tuple struct serialize identically, so a
    // value written as one deserializes as the other.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Pair(u32, bool);

    let pairs = many_pairs();
    let mut working = pairs.clone();
    let flat = document(&(7u32, true), &mut working);
    let decoded = <Pair as Deserialize<u8, Global>>::deserialize::<NoShortening>(
        &flat,
        Variant::V1,
        &pairs,
        None,
        Global,
    )
    .unwrap();
    assert_eq!(decoded, Pair(7, true));
}

#[test]
fn unit_round_trips() {
    assert_eq!(round_trip(()), ());
}

// ---------------------------------------------------------------------------
// `str` is serialize-only and its payload is verbatim.
// ---------------------------------------------------------------------------

#[test]
fn str_payload_is_verbatim_even_when_it_collides_with_the_delimiter() {
    let pairs = primitive_pairs();
    assert_eq!(payload_of("hello", &pairs), b"hello");
    assert_eq!(payload_of("", &pairs), b"");
    assert_eq!(payload_of("🦀", &pairs), "🦀".as_bytes());
    // The payload contains the delimiter unit `~~`, which forces the serializer to
    // grow the delimiter repeat so the frame stays unambiguous.
    assert_eq!(payload_of("hello ~~ world", &pairs), b"hello ~~ world");
}

// ---------------------------------------------------------------------------
// Formatting boundary.
//
// Primitives are formatted into a stack buffer sized to their longest `Display`
// result. `i128::MIN` is the longest integer one at exactly 40 bytes; the float
// extremes are far longer because Rust formats them positionally, so their buffers
// are much larger.
// ---------------------------------------------------------------------------

#[test]
fn primitive_display_boundary() {
    // The longest integer `Display` result: exactly 40 bytes, so it must fit
    // without an off-by-one.
    let payload = payload_of(&i128::MIN, &primitive_pairs());
    assert_eq!(payload.len(), 40);
    assert_eq!(payload, b"-170141183460469231731687303715884105728");
}

// ---------------------------------------------------------------------------
// Manual float `Display` length check.
//
// `impl_primitive!` sizes each primitive's formatter buffer to that primitive's
// longest `Display` result (`f32 => 49`, `f64 => 328`). Those bounds are derived,
// not measured, so this ignored test measures them on the machine at hand: it
// builds values from raw bit patterns (never from decimal literals), keeps only
// the longest result it saw, and prints just that conclusion. Formatting millions
// of floats costs nothing as long as nothing is printed per value.
//
// Enumerating the whole bit space is only viable for `f32` (and even then far too
// slow), so this is a sweep: every exponent combined with the extreme mantissas
// (which a random stream would rarely hit exactly) and a pseudo-random stream,
// both signs. The maxima sit on negative values, one byte longer than their
// positive counterparts because of the sign.
//
// Run it with:
//   cargo test -p sdave-codec --test primitive -- --ignored --nocapture float_display_len_boundary
// ---------------------------------------------------------------------------

/// The `f32` formatter bound reserved by `impl_primitive!` in
/// `sdave-codec/src/impls.rs`; keep in sync.
const F32_DISPLAY_BOUND: usize = 49;
/// The `f64` formatter bound reserved by `impl_primitive!` in
/// `sdave-codec/src/impls.rs`; keep in sync.
const F64_DISPLAY_BOUND: usize = 328;

/// `f32` bit patterns worth pinning down by hand: `±0`, the
/// subnormal/normal/overflow boundaries, `±inf`, `NaN`, `±1` and pi.
const F32_NOTABLE: [u32; 12] = [
    0x0000_0000, // +0
    0x8000_0000, // -0
    0x0000_0001, // smallest positive subnormal
    0x007F_FFFF, // largest subnormal
    0x0080_0000, // smallest positive normal
    0x7F7F_FFFF, // largest finite
    0x7F80_0000, // +inf
    0xFF80_0000, // -inf
    0x7FC0_0000, // NaN
    0x3F80_0000, // 1
    0xBF80_0000, // -1
    0x4049_0FDB, // pi
];

/// `f64` bit patterns worth pinning down by hand; see [`F32_NOTABLE`].
const F64_NOTABLE: [u64; 12] = [
    0x0000_0000_0000_0000, // +0
    0x8000_0000_0000_0000, // -0
    0x0000_0000_0000_0001, // smallest positive subnormal
    0x000F_FFFF_FFFF_FFFF, // largest subnormal
    0x0010_0000_0000_0000, // smallest positive normal
    0x7FEF_FFFF_FFFF_FFFF, // largest finite
    0x7FF0_0000_0000_0000, // +inf
    0xFFF0_0000_0000_0000, // -inf
    0x7FF8_0000_0000_0000, // NaN
    0x3FF0_0000_0000_0000, // 1
    0xBFF0_0000_0000_0000, // -1
    0x4009_21FB_5444_2D18, // pi
];

/// The mantissa patterns swept for *every* exponent: the extremes, which a
/// pseudo-random stream would rarely hit exactly. The exponent decides where the
/// decimal point lands, the mantissa how many significant digits are needed.
const F32_SWEEP_MANTISSAS: [u32; 3] = [0, 1, 0x007F_FFFF];
const F64_SWEEP_MANTISSAS: [u64; 3] = [0, 1, 0x000F_FFFF_FFFF_FFFF];

/// Pseudo-random mantissas drawn per exponent and sign.
const SAMPLES_PER_EXPONENT: u32 = 1024;

#[test]
#[ignore = "manual diagnostic: reports the longest sampled float Display result"]
fn float_display_len_boundary() {
    /// xorshift64, so the sweep is reproducible.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Format `bits` into the reused heap `buf` and record the worst length seen,
    /// for every value and for the non-negative ones only. Keeping both makes the
    /// sign byte's contribution visible: a negative result is its non-negative
    /// counterpart plus `-`.
    fn probe_f32(bits: u32, buf: &mut String, all: &mut (usize, u32), positive: &mut (usize, u32)) {
        use core::fmt::Write as _;

        buf.clear();
        write!(buf, "{}", f32::from_bits(bits)).unwrap();
        let len = buf.len();
        if len > all.0 {
            *all = (len, bits);
        }
        if bits < 1 << 31 && len > positive.0 {
            *positive = (len, bits);
        }
    }

    fn probe_f64(bits: u64, buf: &mut String, all: &mut (usize, u64), positive: &mut (usize, u64)) {
        use core::fmt::Write as _;

        buf.clear();
        write!(buf, "{}", f64::from_bits(bits)).unwrap();
        let len = buf.len();
        if len > all.0 {
            *all = (len, bits);
        }
        if bits < 1 << 63 && len > positive.0 {
            *positive = (len, bits);
        }
    }

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut buf = String::new();
    let mut f32_all = (0usize, 0u32);
    let mut f32_positive = (0usize, 0u32);
    let mut f64_all = (0usize, 0u64);
    let mut f64_positive = (0usize, 0u64);

    for bits in F32_NOTABLE {
        probe_f32(bits, &mut buf, &mut f32_all, &mut f32_positive);
    }
    for exponent in 0u32..=0xFF {
        for sign in [0u32, 1 << 31] {
            for mantissa in F32_SWEEP_MANTISSAS {
                let bits = sign | (exponent << 23) | mantissa;
                probe_f32(bits, &mut buf, &mut f32_all, &mut f32_positive);
            }
            for _ in 0..SAMPLES_PER_EXPONENT {
                let mantissa = rng.next_u64() as u32 & 0x007F_FFFF;
                let bits = sign | (exponent << 23) | mantissa;
                probe_f32(bits, &mut buf, &mut f32_all, &mut f32_positive);
            }
        }
    }

    for bits in F64_NOTABLE {
        probe_f64(bits, &mut buf, &mut f64_all, &mut f64_positive);
    }
    for exponent in 0u64..=0x7FF {
        for sign in [0u64, 1 << 63] {
            for mantissa in F64_SWEEP_MANTISSAS {
                let bits = sign | (exponent << 52) | mantissa;
                probe_f64(bits, &mut buf, &mut f64_all, &mut f64_positive);
            }
            for _ in 0..SAMPLES_PER_EXPONENT {
                let mantissa = rng.next_u64() & 0x000F_FFFF_FFFF_FFFF;
                let bits = sign | (exponent << 52) | mantissa;
                probe_f64(bits, &mut buf, &mut f64_all, &mut f64_positive);
            }
        }
    }

    eprintln!(
        "f32 worst: len={} bits={:#010x} value={}",
        f32_all.0,
        f32_all.1,
        f32::from_bits(f32_all.1)
    );
    eprintln!(
        "f32 worst non-negative: len={} bits={:#010x}",
        f32_positive.0, f32_positive.1
    );
    eprintln!(
        "f64 worst: len={} bits={:#018x} value={}",
        f64_all.0,
        f64_all.1,
        f64::from_bits(f64_all.1)
    );
    eprintln!(
        "f64 worst non-negative: len={} bits={:#018x}",
        f64_positive.0, f64_positive.1
    );
    eprintln!("reserved: f32={F32_DISPLAY_BOUND} f64={F64_DISPLAY_BOUND}");

    assert!(
        f32_all.0 <= F32_DISPLAY_BOUND,
        "sampled f32 Display needs {} bytes, over the reserved {F32_DISPLAY_BOUND}",
        f32_all.0
    );
    assert!(
        f64_all.0 <= F64_DISPLAY_BOUND,
        "sampled f64 Display needs {} bytes, over the reserved {F64_DISPLAY_BOUND}",
        f64_all.0
    );
}
