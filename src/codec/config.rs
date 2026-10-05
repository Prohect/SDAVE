use std::{collections::BTreeSet, num::NonZeroUsize};

use crate::{LimiterPair, Variant, error_new};

use super::{Error, ErrorKind, NamePolicy, Result, names::Names};

/// Per-document bounds shared by all nested codecs and basic helpers.
///
/// `max_elements` counts real envelopes (including headers), not phantoms.
/// Delimiter work is a conservatively charged upper bound on framing scans,
/// including verification, rather than a wall-clock timeout.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    pub max_depth: usize,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_elements: usize,
    pub max_delimiter_work: usize,
    pub max_repeat: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_input_bytes: 8 * 1024 * 1024,
            max_output_bytes: 8 * 1024 * 1024,
            max_elements: 100_000,
            max_delimiter_work: 64 * 1024 * 1024,
            max_repeat: 4096,
        }
    }
}

/// A stable, cross-run digest of a [`Config`].
///
/// [`std::hash::Hash`] with the standard library's default hasher is not
/// reproducible across processes or Rust releases. This digest is, so it can be
/// persisted and compared. See [`Config::fingerprint`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConfigFingerprint(u64);

impl ConfigFingerprint {
    /// The raw digest value.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// An immutable framing/limits configuration for the typed codec.
///
/// The initial typed profile deliberately supports only distinct single UTF-8
/// scalar units, with disjoint limiter/delimiter sets and no ASCII formatting
/// units. This rejects ambiguous profiles and guarantees representable empty
/// frames. The fundamental parser still accepts its broader general profiles.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Config {
    variant: Variant,
    pairs: Vec<LimiterPair<u8>>,
    limits: Limits,
}

impl Config {
    pub fn new(variant: Variant, pairs: Vec<LimiterPair<u8>>) -> Result<Self> {
        let config = Self {
            variant,
            pairs,
            limits: Limits::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    pub fn with_limits(mut self, limits: Limits) -> Result<Self> {
        self.limits = limits;
        self.validate()?;
        Ok(self)
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    pub fn limiter_pairs(&self) -> &[LimiterPair<u8>] {
        &self.pairs
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// A stable digest over this profile: `variant`, the ordered limiter pairs
    /// and `limits`.
    ///
    /// The standard library's `Hash`/`DefaultHasher` are not reproducible across
    /// processes or Rust releases; this digest is, so it can be persisted and
    /// compared — for example to detect that a reloaded document or session was
    /// authored under a different framing profile. Pair order is significant
    /// (SDAVE selects the first matching pair), so it contributes to the digest.
    /// The dependency-free FNV-1a encoding below is part of the public contract
    /// and MUST remain stable across releases; use `Hash` for in-memory
    /// comparison and this only when a value must survive serialization.
    pub fn fingerprint(&self) -> ConfigFingerprint {
        let mut hash = Fnv1a::new();
        hash.write(&[self.variant as u8]);
        for value in [
            self.limits.max_depth,
            self.limits.max_input_bytes,
            self.limits.max_output_bytes,
            self.limits.max_elements,
            self.limits.max_delimiter_work,
            self.limits.max_repeat,
        ] {
            hash.write_usize(value);
        }
        hash.write_usize(self.pairs.len());
        for pair in &self.pairs {
            match pair.least_repeat {
                Some(repeat) => {
                    hash.write(&[1]);
                    hash.write_usize(repeat.get());
                }
                None => hash.write(&[0]),
            }
            hash.write_bytes(&pair.limiter);
            hash.write_bytes(&pair.delimiter);
        }
        ConfigFingerprint(hash.finish())
    }

    fn validate(&self) -> Result<()> {
        if self.pairs.is_empty() || self.pairs.len() > 32 {
            return Err(error_new!(ErrorKind::InvalidProfile(
                "expected 1..=32 active pairs".into(),
            )));
        }
        let mut units = BTreeSet::new();
        for pair in &self.pairs {
            let repeat = pair.least_repeat.ok_or_else(|| {
                error_new!(ErrorKind::InvalidProfile(
                    "disabled pairs are not a typed profile".into(),
                ))
            })?;
            if repeat.get() > self.limits.max_repeat {
                return Err(error_new!(ErrorKind::InvalidProfile(
                    "minimum repeat exceeds max_repeat".into(),
                )));
            }
            for unit in [&pair.limiter, &pair.delimiter] {
                let text = std::str::from_utf8(unit).map_err(|_| {
                    error_new!(ErrorKind::InvalidProfile(
                        "units must be UTF-8 scalars".into(),
                    ))
                })?;
                let mut chars = text.chars();
                let ch = chars
                    .next()
                    .ok_or_else(|| error_new!(ErrorKind::InvalidProfile("empty unit".into())))?;
                if chars.next().is_some() || matches!(ch, ' ' | '\t' | '\r' | '\n') {
                    return Err(error_new!(ErrorKind::InvalidProfile(
                        "each unit must be one non-formatting UTF-8 scalar".into(),
                    )));
                }
                if !units.insert(unit.as_slice()) {
                    return Err(error_new!(ErrorKind::InvalidProfile(
                        "limiter and delimiter units must all be distinct".into(),
                    )));
                }
            }
        }
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        let pairs = [('♦', '♣'), ('♥', '♠'), ('★', '☆')]
            .into_iter()
            .map(|(limiter, delimiter)| LimiterPair {
                least_repeat: NonZeroUsize::new(1),
                limiter: limiter.to_string().into_bytes(),
                delimiter: delimiter.to_string().into_bytes(),
            })
            .collect();
        Self {
            variant: Variant::V2,
            pairs,
            limits: Limits::default(),
        }
    }
}

pub(super) struct Context {
    pub config: Config,
    pub names: Names,
    pub budget: Budget,
}

impl Context {
    pub fn new<P: NamePolicy>(config: Config) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            names: Names::new::<P>()?,
            config,
            budget: Budget::default(),
        })
    }

    pub fn reset(&mut self) {
        self.budget = Budget::default();
    }

    pub fn enter(&mut self) -> Result<()> {
        if self.budget.depth >= self.config.limits.max_depth {
            return Err(self.limit("nesting depth", self.config.limits.max_depth));
        }
        self.budget.depth += 1;
        Ok(())
    }

    pub fn leave(&mut self) {
        self.budget.depth -= 1;
    }

    pub fn input(&self, length: usize) -> Result<()> {
        if length > self.config.limits.max_input_bytes {
            Err(self.limit("input bytes", self.config.limits.max_input_bytes))
        } else {
            Ok(())
        }
    }

    pub fn output(&self, length: usize) -> Result<()> {
        if length > self.config.limits.max_output_bytes {
            Err(self.limit("output bytes", self.config.limits.max_output_bytes))
        } else {
            Ok(())
        }
    }

    pub fn element(&mut self) -> Result<()> {
        if self.budget.elements >= self.config.limits.max_elements {
            return Err(self.limit("envelopes", self.config.limits.max_elements));
        }
        self.budget.elements += 1;
        Ok(())
    }

    pub fn work(&mut self, amount: usize) -> Result<()> {
        let total =
            self.budget.work.checked_add(amount).ok_or_else(|| {
                self.limit("delimiter work", self.config.limits.max_delimiter_work)
            })?;
        if total > self.config.limits.max_delimiter_work {
            return Err(self.limit("delimiter work", self.config.limits.max_delimiter_work));
        }
        self.budget.work = total;
        Ok(())
    }

    pub fn remaining_work(&self) -> usize {
        self.config.limits.max_delimiter_work - self.budget.work
    }

    pub fn repeat(&self, repeat: usize) -> Result<()> {
        if repeat > self.config.limits.max_repeat {
            Err(self.limit("delimiter repeats", self.config.limits.max_repeat))
        } else {
            Ok(())
        }
    }

    pub fn limit(&self, resource: &'static str, limit: usize) -> Error {
        error_new!(ErrorKind::LimitExceeded { resource, limit })
    }
}

#[derive(Default)]
pub(super) struct Budget {
    depth: usize,
    elements: usize,
    work: usize,
}

/// Dependency-free FNV-1a, used only for the stable [`ConfigFingerprint`].
struct Fnv1a(u64);

impl Fnv1a {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn write_usize(&mut self, value: usize) {
        self.write(&(value as u64).to_le_bytes());
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_usize(bytes.len());
        self.write(bytes);
    }

    fn finish(self) -> u64 {
        self.0
    }
}
