//! The profile: `Config`, its `Limits`, its pair-select policy and its fingerprint.

use alloc::vec::Vec;
use core::alloc::Allocator;
use core::num::NonZeroUsize;

use sdave::{LimiterPair, Variant, can_serialize};

use crate::{Error, ErrorKind, NamePolicy, Unit};

/// How the serializer picks a limiter pair among the pairs that can frame a payload.
///
/// The policy never affects decoding: the parser always takes the first matching pair in profile
/// order, so the wire is unambiguous whatever the serializer chose.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PairSelect {
    /// The first pair, in profile order, that can frame the payload. Historical behaviour, and the
    /// one that agrees with the parser's own order sensitivity.
    #[default]
    FirstMatch,
    /// The pair whose framing needs the fewest delimiter repeats. Minimising repeats is what stops
    /// a nested document from growing without bound when its payloads collide with the first
    /// pair's delimiter.
    LeastRepeat,
    /// The pair whose framed envelope is smallest in units; ties fall back to the fewest repeats,
    /// then to profile order.
    SmallestFrame,
}

impl PairSelect {
    fn code(self) -> u8 {
        match self {
            PairSelect::FirstMatch => 0,
            PairSelect::LeastRepeat => 1,
            PairSelect::SmallestFrame => 2,
        }
    }
}

/// Bounds a profile imposes on a document.
///
/// A bound of `0` means *no bound*, so [`Limits::UNBOUNDED`] — the default — constrains nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Limits {
    /// Maximum nesting depth of frames.
    pub max_depth: usize,
    /// Maximum total document length, in units.
    pub max_len: usize,
    /// Maximum delimiter/limiter repeat of a single frame.
    pub max_repeat: usize,
}

impl Limits {
    /// No bound at all.
    pub const UNBOUNDED: Limits = Limits {
        max_depth: 0,
        max_len: 0,
        max_repeat: 0,
    };

    /// The three bounds, in order.
    pub const fn new(max_depth: usize, max_len: usize, max_repeat: usize) -> Self {
        Limits {
            max_depth,
            max_len,
            max_repeat,
        }
    }
}

/// The profile: variant + ordered limiter pairs + `Limits` + pair-select policy + naming policy.
///
/// One `Config` serves a whole document: markers are rendered through its naming policy, framing
/// through its variant and pair list, and the pair-select policy decides which pair frames a
/// payload. The profile is immutable once built.
pub struct Config<'l, 'v, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    variant: Variant,
    limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
    limits: Limits,
    pair_select: PairSelect,
    name_policy: NamePolicy,
    fingerprint: Fingerprint,
}

impl<'l, 'v, T, A> Config<'l, 'v, T, A>
where
    T: Unit,
    A: Allocator + Clone,
{
    /// Build a profile.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidNamePolicy`] when the naming policy abbreviates two distinct
    /// constructors to the same short name, and
    /// [`ErrorKind::Invariant`] when the pair list is empty (a profile that can frame nothing
    /// would make the core's deserializer panic, so it is rejected here instead).
    #[allow(clippy::ptr_arg)]
    pub fn new(
        variant: Variant,
        limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
        limits: Limits,
        pair_select: PairSelect,
        name_policy: NamePolicy,
    ) -> Result<Self, Error> {
        name_policy.validate()?;
        if limiter_pairs.is_empty() {
            return Err(Error::new(ErrorKind::Invariant(
                "a profile needs at least one limiter pair",
            )));
        }
        let fingerprint = fingerprint(variant, limiter_pairs, limits, pair_select);
        Ok(Config {
            variant,
            limiter_pairs,
            limits,
            pair_select,
            name_policy,
            fingerprint,
        })
    }

    /// The delimiter match algorithm variant.
    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// The ordered limiter pairs.
    pub fn limiter_pairs(&self) -> &'v Vec<LimiterPair<'l, T, A>, A> {
        self.limiter_pairs
    }

    /// The profile's bounds.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The pair-select policy.
    pub fn pair_select(&self) -> PairSelect {
        self.pair_select
    }

    /// The naming policy.
    pub fn name_policy(&self) -> &NamePolicy {
        &self.name_policy
    }

    /// The profile's fingerprint.
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// The pair and repeat that frame `payload` given `context`, chosen by the pair-select policy.
    ///
    /// `context` is the document content that will precede this frame. Per
    /// [`sdave::can_serialize`]'s contract it must end at a clean item boundary; violating that
    /// cannot cause undefined behaviour, but it can produce a frame the deserializer will not
    /// re-parse.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Unframable`] when no pair of the profile can frame this payload here.
    pub fn choose(&self, context: &[T], payload: &[T]) -> Result<Chosen<'l, T>, Error> {
        let mut best: Option<(Chosen<'l, T>, usize)> = None;
        for (pair_index, pair) in self.limiter_pairs.iter().enumerate() {
            // Safety: the context is a clean item boundary of the document being written — the
            // caller's pre-existing channel content, or content this codec framed itself.
            let Some(repeat) = (unsafe { can_serialize(context, payload, self.variant, pair) })
            else {
                continue;
            };
            let limiter: &'l [T] = pair.limiter();
            let delimiter: &'l [T] = pair.delimiter();
            let frame = repeat
                .get()
                .saturating_mul(limiter.len().saturating_add(delimiter.len()));
            let chosen = Chosen {
                pair_index,
                repeat,
                limiter,
                delimiter,
            };
            match self.pair_select {
                PairSelect::FirstMatch => return Ok(chosen),
                PairSelect::LeastRepeat => {
                    if best
                        .as_ref()
                        .is_none_or(|(current, _)| repeat < current.repeat)
                    {
                        best = Some((chosen, frame));
                    }
                }
                PairSelect::SmallestFrame => {
                    if best.as_ref().is_none_or(|(current, best_frame)| {
                        frame < *best_frame || (frame == *best_frame && repeat < current.repeat)
                    }) {
                        best = Some((chosen, frame));
                    }
                }
            }
        }
        match best {
            Some((chosen, _)) => Ok(chosen),
            None => Err(Error::new(ErrorKind::Unframable)),
        }
    }
}

/// The pair and repeat [`Config::choose`] selected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chosen<'l, T> {
    /// The index of the pair in the profile's ordered list.
    pub pair_index: usize,
    /// The number of limiter (and, at the tail, delimiter) repeats of the frame.
    pub repeat: NonZeroUsize,
    /// The chosen pair's limiter unit slice.
    pub limiter: &'l [T],
    /// The chosen pair's delimiter unit slice.
    pub delimiter: &'l [T],
}

impl<T> Chosen<'_, T> {
    /// The frame's own size: `repeat * (limiter + delimiter)`, excluding the payload.
    pub fn frame_len(&self) -> usize {
        self.repeat
            .get()
            .saturating_mul(self.limiter.len().saturating_add(self.delimiter.len()))
    }
}

/// A stable digest of a profile's wire-relevant identity.
///
/// # Encoding
///
/// The digest is FNV-1a (64-bit) over this exact byte stream:
///
/// ```text
/// tag     u32 le          length of b"sdave-codec:profile"
///         bytes           b"sdave-codec:profile"
/// u16 le                  ENCODING_VERSION
/// u8                      variant          (1 = V1, 2 = V2)
/// u8                      pair_select      (0 = FirstMatch, 1 = LeastRepeat, 2 = SmallestFrame)
/// u64 le                  limits.max_depth
/// u64 le                  limits.max_len
/// u64 le                  limits.max_repeat
/// u32 le                  pair count
/// per pair, in profile order:
///     u8                  least_repeat present (0 | 1)
///     u64 le              least_repeat (0 when absent)
///     u32 le              limiter unit count
///     u32 le * count      each limiter unit's Unit::code
///     u32 le              delimiter unit count
///     u32 le * count      each delimiter unit's Unit::code
/// ```
///
/// The version tag is hashed in, so **adding a covered field changes every digest** — and a
/// fingerprint is a **version-tagged value**: never compare fingerprints produced by different
/// library versions. The naming policy is deliberately *not* covered: the README defines the
/// fingerprint over the variant, the pair list, `Limits` and the pair-select policy, and a marker
/// disagreement is caught by marker validation instead.
///
/// Unit counts and text lengths are encoded as `u32`, so a single buffer of more than
/// `u32::MAX` units is outside the encoding's domain.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint(u64);

impl Fingerprint {
    /// The version of the byte encoding above, hashed into every digest.
    pub const ENCODING_VERSION: u16 = 1;

    /// A fingerprint with the given digest.
    pub const fn new(digest: u64) -> Self {
        Fingerprint(digest)
    }

    /// The FNV-1a 64 digest of `bytes`, the primitive the profile encoding is built from.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Hasher::new();
        hasher.bytes(bytes);
        Fingerprint(hasher.0)
    }

    /// The digest.
    pub const fn get(&self) -> u64 {
        self.0
    }

    /// The digest, little-endian.
    pub const fn to_le_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl core::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Fingerprint({:016x})", self.0)
    }
}

impl core::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// The tag that opens every fingerprint's byte stream.
const TAG: &[u8] = b"sdave-codec:profile";

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a, 64-bit.
struct Hasher(u64);

impl Hasher {
    fn new() -> Self {
        Hasher(FNV_OFFSET_BASIS)
    }

    fn byte(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.byte(*byte);
        }
    }

    fn u8(&mut self, value: u8) {
        self.byte(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    /// A length-prefixed byte string.
    fn tagged(&mut self, bytes: &[u8]) {
        self.u32(bytes.len() as u32);
        self.bytes(bytes);
    }
}

fn variant_code(variant: Variant) -> u8 {
    match variant {
        Variant::V1 => 1,
        Variant::V2 => 2,
    }
}

fn fingerprint<'l, 'v, T, A>(
    variant: Variant,
    limiter_pairs: &'v Vec<LimiterPair<'l, T, A>, A>,
    limits: Limits,
    pair_select: PairSelect,
) -> Fingerprint
where
    T: Unit,
    A: Allocator + Clone,
{
    let mut hasher = Hasher::new();
    hasher.tagged(TAG);
    hasher.u16(Fingerprint::ENCODING_VERSION);
    hasher.u8(variant_code(variant));
    hasher.u8(pair_select.code());
    hasher.u64(limits.max_depth as u64);
    hasher.u64(limits.max_len as u64);
    hasher.u64(limits.max_repeat as u64);
    hasher.u32(limiter_pairs.len() as u32);
    for pair in limiter_pairs {
        match pair.least_repeat() {
            Some(repeat) => {
                hasher.u8(1);
                hasher.u64(repeat.get() as u64);
            }
            None => {
                hasher.u8(0);
                hasher.u64(0);
            }
        }
        hasher.u32(pair.limiter().len() as u32);
        for unit in pair.limiter() {
            hasher.u32(unit.code());
        }
        hasher.u32(pair.delimiter().len() as u32);
        for unit in pair.delimiter() {
            hasher.u32(unit.code());
        }
    }
    Fingerprint(hasher.0)
}
