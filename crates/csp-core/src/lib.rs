//! Core sim primitives: integer sim time, deterministic RNG streams.
//!
//! Rules (PLAN §1.1, §2.3, §2.7):
//! - No I/O, no wall clock, no `std::time`.
//! - Sim time is an integer tick count — never an `f64` accumulator.
//! - Any `f64` math that feeds sim state uses `libm` transcendentals only.

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

/// Named deterministic RNG streams (PLAN §2.7). No global RNG exists.
#[derive(Clone, Copy, Debug)]
pub enum StreamTag {
    Reliability,
    Ignition,
    Contracts,
    Generation,
}

/// Derive a deterministic ChaCha8 stream from
/// `(save_seed, stream_tag, entity_id, counter)`.
///
/// Counters are part of the save: reloading and re-flying an event does not
/// reroll it. Anti-save-scum by construction and required for replays.
pub fn stream(save_seed: u64, tag: StreamTag, entity_id: u64, counter: u64) -> ChaCha8Rng {
    // splitmix64-style word mixing — deterministic across platforms, unlike
    // `Hash`/`DefaultHasher` whose output is not specified stable.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for w in [save_seed, tag as u64, entity_id, counter] {
        x ^= w;
        x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        x ^= x >> 33;
    }
    ChaCha8Rng::seed_from_u64(x)
}

/// Convenience: draw a uniform `f64` in `[0, 1)` from a derived stream.
pub fn uniform(save_seed: u64, tag: StreamTag, entity_id: u64, counter: u64) -> f64 {
    let mut rng = stream(save_seed, tag, entity_id, counter);
    // 53-bit uniform — same construction as rand::random::<f64>.
    (rng.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_is_integral() {
        let mut t = SimTime::ZERO;
        t.advance(TICK_HZ);
        assert!((t.seconds() - 1.0).abs() < f64::EPSILON);
        assert_eq!(t.ticks(), 64);
    }

    #[test]
    fn streams_are_deterministic_and_distinct() {
        let a = uniform(42, StreamTag::Reliability, 7, 0);
        let b = uniform(42, StreamTag::Reliability, 7, 0);
        let c = uniform(42, StreamTag::Ignition, 7, 0);
        let d = uniform(43, StreamTag::Reliability, 7, 0);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert!((0.0..1.0).contains(&a));
    }
}
