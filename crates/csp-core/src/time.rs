//! The integer sim clock (PLAN §2.3). Never an `f64` accumulator.

use serde::{Deserialize, Serialize};

use crate::SimError;

/// Sim tick rate. The physics dt is immutably `1/TICK_HZ` seconds for the
/// entire life of the project — warp never changes it (PLAN §2.6).
pub const TICK_HZ: u64 = 64;

/// Simulation time as an integer tick count (PLAN §2.3). Ordering and
/// equality are total. Epoch 0 = game start; UT seconds = ticks / `TICK_HZ`.
/// `serde(transparent)` keeps the wire spelling a bare `u64` (§7.2/
/// §7.6); serialization never feeds the §7.1 hash.
///
/// The field is private on purpose: [`SimTime::try_advance`] is the only
/// mutator, and it is checked — direct field writes would bypass its
/// overflow contract and wrap silently in release.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SimTime(u64);

impl SimTime {
    pub const ZERO: Self = Self(0);

    /// Construct from a raw tick count — the only constructor besides
    /// [`Self::ZERO`]. Unchecked by design: a tick count is just a `u64`,
    /// and the fallible contract lives on the mutation path
    /// ([`Self::try_advance`]), not on construction.
    pub const fn from_ticks(ticks: u64) -> Self {
        Self(ticks)
    }

    /// Advance by `ticks` — the only mutator, and it is fallible (§2.13:
    /// `step_frame` returns `Result<_, SimError>`).
    ///
    /// Leaves the clock untouched and returns `SimError::Fatal` on overflow;
    /// the tick boundary then aborts and rolls back per policy. There is no
    /// infallible variant on purpose: a saturating clock would silently
    /// freeze sim time, and a `u64` counter at 64 Hz lasts ~9 billion years,
    /// so overflow is an invariant violation, never a runtime case.
    pub fn try_advance(&mut self, ticks: u64) -> Result<(), SimError> {
        self.checked(ticks).map(|t| {
            self.0 = t;
        })
    }

    fn checked(&self, ticks: u64) -> Result<u64, SimError> {
        self.0
            .checked_add(ticks)
            .ok_or(SimError::Fatal("sim tick counter overflow"))
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

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn time_is_integral() {
        let mut t = SimTime::ZERO;
        t.try_advance(TICK_HZ).unwrap();
        assert!((t.seconds() - 1.0).abs() < f64::EPSILON);
        assert_eq!(t.ticks(), 64);
    }

    #[test]
    fn try_advance_accumulates_exactly() {
        let mut t = SimTime::from_ticks(u64::MAX - 4);
        t.try_advance(4).unwrap();
        assert_eq!(t.ticks(), u64::MAX);
    }

    #[test]
    fn try_advance_overflow_is_fatal_and_preserves_clock() {
        let mut t = SimTime::from_ticks(u64::MAX - 4);
        assert!(matches!(t.try_advance(8), Err(SimError::Fatal(_))));
        assert_eq!(t.ticks(), u64::MAX - 4); // untouched on failure
    }
}
