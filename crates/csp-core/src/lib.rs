#![forbid(unsafe_code)]

//! Core sim primitives: integer sim time, deterministic RNG streams, canonical
//! state hashing.
//!
//! Rules (PLAN §1.1, §2.3, §2.7):
//! - No I/O, no wall clock, no `std::time`.
//! - Sim time is an integer tick count — never an `f64` accumulator.
//! - Any `f64` math that feeds sim state uses `libm` transcendentals only.
//! - No `HashMap`/`HashSet` in anything that iterates into state or a hash.

use rand_chacha::ChaCha8Rng;
use rand_core::{Rng, SeedableRng};

/// Sim tick rate. The physics dt is immutably `1/TICK_HZ` seconds for the
/// entire life of the project — warp never changes it (PLAN §2.6).
pub const TICK_HZ: u64 = 64;

/// Simulation time as an integer tick count. Ordering and equality are total.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SimTime(pub u64);

impl SimTime {
    pub const ZERO: Self = Self(0);

    pub fn advance(&mut self, ticks: u64) {
        self.0 += ticks;
    }

    /// Whole ticks elapsed.
    pub fn ticks(&self) -> u64 {
        self.0
    }

    /// Elapsed sim seconds (derived; the stored value stays integral).
    pub fn seconds(&self) -> f64 {
        self.0 as f64 / TICK_HZ as f64
    }
}

// ---------------------------------------------------------------------------
// Deterministic RNG streams (PLAN §2.7). No global RNG exists.
// ---------------------------------------------------------------------------

/// Version of the key-derivation scheme. Bumped if the KDF ever changes;
/// recorded in the save header so old saves can be detected.
pub const KDF_VERSION: u8 = 1;

const KDF_CONTEXT: &str = "csp/rng-stream/v1";

/// Named deterministic RNG streams.
#[derive(Clone, Copy, Debug)]
pub enum StreamTag {
    Reliability,
    Ignition,
    Contracts,
    Generation,
}

/// Full key for one deterministic draw (PLAN §2.7). `counter` indexes into the
/// stream — reseeding per counter, never advancing shared stream position, so
/// a stored counter replays the exact draw.
#[derive(Clone, Copy, Debug)]
pub struct StreamKey {
    pub tag: StreamTag,
    pub entity: u64,
    pub counter: u64,
}

/// Derive the full 32-byte ChaCha key via BLAKE3 derive-key mode.
/// Own KDF — never `seed_from_u64`, whose output is a rand_core
/// implementation detail and not value-stable across versions.
pub fn derive_key(save_seed: u64, key: StreamKey) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key(KDF_CONTEXT);
    h.update(&KDF_VERSION.to_le_bytes());
    h.update(&save_seed.to_le_bytes());
    h.update(&(key.tag as u64).to_le_bytes());
    h.update(&key.entity.to_le_bytes());
    h.update(&key.counter.to_le_bytes());
    *h.finalize().as_bytes()
}

/// A deterministic ChaCha8 stream for one `StreamKey`.
pub fn stream(save_seed: u64, key: StreamKey) -> ChaCha8Rng {
    ChaCha8Rng::from_seed(derive_key(save_seed, key))
}

/// Convenience: draw a uniform `f64` in `[0, 1)` from a derived stream.
pub fn uniform(save_seed: u64, key: StreamKey) -> f64 {
    let mut rng = stream(save_seed, key);
    (rng.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

// ---------------------------------------------------------------------------
// Canonical state hashing (PLAN §7.1). Hand-written visitor — never serde.
// ---------------------------------------------------------------------------

/// Version of the hash canonicalization scheme. Recorded next to every golden.
pub const HASH_SCHEMA_VERSION: u32 = 1;

/// Canonical byte-stream hasher over BLAKE3 (PLAN §7.1).
///
/// Rules: dense arena index order only; all floats via `to_bits()` with
/// `-0.0` normalized to `0.0`; NaN is forbidden — `f64` panics in debug on NaN.
pub struct StateHasher(blake3::Hasher);

impl StateHasher {
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.update(&v.to_le_bytes());
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.0.update(&[v as u8]);
        self
    }
    /// `to_bits`, `-0.0 -> 0.0`; NaN panics (debug only — NaN in state is a bug).
    pub fn f64(&mut self, v: f64) -> &mut Self {
        debug_assert!(!v.is_nan(), "NaN must never enter sim state");
        let v = if v == 0.0 { 0.0 } else { v };
        self.0.update(&v.to_bits().to_le_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.u64(v.len() as u64);
        self.0.update(v);
        self
    }
    pub fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }
    /// Some(T) hashes tag 1 + T; None hashes tag 0. Canonical Option.
    pub fn opt<T>(&mut self, v: &Option<T>, f: impl Fn(&mut Self, &T)) -> &mut Self {
        match v {
            Some(x) => {
                self.u32(1);
                f(self, x);
            }
            None => {
                self.u32(0);
            }
        }
        self
    }

    pub fn finish(&self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

impl Default for StateHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tag: StreamTag) -> StreamKey {
        StreamKey {
            tag,
            entity: 7,
            counter: 0,
        }
    }

    #[test]
    fn time_is_integral() {
        let mut t = SimTime::ZERO;
        t.advance(TICK_HZ);
        assert!((t.seconds() - 1.0).abs() < f64::EPSILON);
        assert_eq!(t.ticks(), 64);
    }

    #[test]
    fn streams_are_deterministic_and_distinct() {
        let a = uniform(42, key(StreamTag::Reliability));
        let b = uniform(42, key(StreamTag::Reliability));
        let c = uniform(42, key(StreamTag::Ignition));
        let d = uniform(43, key(StreamTag::Reliability));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert!((0.0..1.0).contains(&a));
    }

    /// Hard-coded expected KDF output: a dependency or KDF change fails loudly.
    #[test]
    fn kdf_golden() {
        let k = derive_key(
            42,
            StreamKey {
                tag: StreamTag::Reliability,
                entity: 7,
                counter: 0,
            },
        );
        let expect: [u8; 8] = EXPECTED_KDF_PREFIX;
        assert_eq!(&k[..8], &expect);
    }

    #[test]
    fn hash_canonicalizes_floats() {
        let mut a = StateHasher::new();
        let mut b = StateHasher::new();
        a.f64(-0.0);
        b.f64(0.0);
        assert_eq!(a.finish(), b.finish());
    }
}

// golden prefix for kdf_golden — filled by first run, then frozen.
#[cfg(test)]
const EXPECTED_KDF_PREFIX: [u8; 8] = [185, 230, 217, 66, 114, 51, 125, 180];
