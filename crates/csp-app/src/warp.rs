//! The warp controller (PLAN §2.6.3): lives entirely in `csp-app`, observes
//! wall-clock frame cost, and emits `SimCommand::SetWarp` /
//! `SimCommand::AdvanceTo`. The sim **never measures anything** — feeding
//! measured frame time into sim state would break T1 replay, so everything
//! here is wall-side session state whose only sim contact is logged
//! commands.

use csp_core::TICK_HZ;

/// The §2.6.2 ladder: ×1 real time, ×2 physics warp (the v1 cap — ×3/×4
/// stay locked until measured per-tick cost on the reference machine is
/// ≤0.5 ms for the current scene, §2.6.2), then rails ×10 … ×10⁵ (the
/// §2.6.2 ceiling: ~18 sim-minutes per 60 fps frame). The ladder index is
/// what the player steps through; the value is what `SetWarp` logs — the
/// sim stores it verbatim and interprets nothing (§2.6.3), so the log reads
/// as literal multipliers.
///
/// Regime gating (bubble occupied ⇒ physics warp, no-thrust/no-atmosphere
/// ⇒ rails) arrives with the bubble in a later milestone; M0's sim has no
/// bubble, so every rung is admissible.
pub const WARP_LADDER: [u64; 7] = [1, 2, 10, 100, 1_000, 10_000, 100_000];

/// §2.6.3: `budget_fraction` — the share of the frame budget sim stepping
/// may consume before the controller derates.
const BUDGET_FRACTION: f64 = 0.5;
/// Reference frame budget for the §2.6.3 inequality (60 fps).
const FRAME_BUDGET_MS: f64 = 1000.0 / 60.0;
/// §2.6.3 hysteresis: promotion requires the inequality to hold with this
/// margin…
const PROMOTE_MARGIN: f64 = 1.5;
/// …for this many consecutive frames, to prevent oscillation.
const PROMOTE_FRAMES: u32 = 30;

/// Nanoseconds per wall second — the integer quantum [`Self::target_tick`]
/// accumulates in.
const NANOS_PER_SEC: u128 = 1_000_000_000;

/// §2.6.3's controller. Owns the warp ladder, the request/effective split,
/// the demotion hysteresis, and the integer tick accumulator. None of this
/// is sim state — the sim sees only the `SetWarp`/`AdvanceTo` commands the
/// caller enqueues from `decide`/`target_tick` results.
pub struct WarpController {
    /// `WARP_LADDER` index the player asked for (manual `,`/`.` steps or
    /// panel buttons — UI requests land a frame later per §2.5).
    requested: usize,
    /// `WARP_LADDER` index the budget currently allows — `≤ requested`
    /// under auto-derate, and the level `target_tick` advances at.
    effective: usize,
    /// Net ladder steps input asked for since the last [`Self::decide`].
    pending_step: i32,
    /// Consecutive frames [`Self::promote_candidate`] has satisfied the
    /// margined §2.6.3 inequality — the streak is bound to one rung: a
    /// frame whose margined ceiling differs resets it, never extends it.
    promote_streak: u32,
    /// The ladder rung `promote_streak` is counting for. `None` when no
    /// promotion is currently building.
    promote_candidate: Option<usize>,
    /// Per-tick wall cost of the last `step_frame` — the caller divides
    /// the frame's whole step cost by the ticks it advanced, because
    /// §2.6.3's inequality is written in per-tick units. Stays wall-side;
    /// never sim state.
    measured_step_ms: f64,
    /// Fixed-point sim progress: `Σ dt_wall_ns × multiplier` as a `u128`.
    /// Integer accumulation — never float — so frame-rate jitter can't
    /// drift the target the way an `f64` accumulator would (§2.6.3's
    /// spirit: measured time drives a logged command, not hidden state).
    scaled_nanos: u128,
}

impl WarpController {
    /// Fresh controller: requested = effective = ×1 (real time, §2.6.2's
    /// always-allowed regime — matching the sim's boot warp level).
    pub fn new() -> Self {
        Self {
            requested: 0,
            effective: 0,
            pending_step: 0,
            promote_streak: 0,
            promote_candidate: None,
            measured_step_ms: 0.0,
            scaled_nanos: 0,
        }
    }

    /// Queue a manual ladder step (winit `,`/`.` keys, or the panel's
    /// `−`/`+` buttons one frame later via `UiOutbox`). Folded into
    /// `requested` by the next [`Self::decide`].
    pub fn request_step(&mut self, step: i32) {
        self.pending_step = self.pending_step.saturating_add(step);
    }

    /// §2.6.3 `observe`: record the per-tick step cost (ms) measured from
    /// the last `step_frame` — the caller passes
    /// `frame_step_ms / ticks_advanced`, the quantity the §2.6.3
    /// inequality is written in. `dt_wall` rides the call for the
    /// contract's shape — the demotion rule as written is a function of
    /// the step measurement and the fixed frame budget, not the
    /// instantaneous dt.
    pub fn observe(&mut self, measured_step_ms: f64, _dt_wall: f64) {
        self.measured_step_ms = measured_step_ms.max(0.0);
    }

    /// The §2.6.3 inequality at rung `idx`, optionally `margin`-scaled for
    /// the promotion side: `per_tick_ms × TICK_HZ × multiplier × margin <
    /// BUDGET_FRACTION × FRAME_BUDGET_MS`. `measured_step_ms` is per-tick
    /// step cost, so the left side extrapolates one measured tick to a
    /// rung's full per-second sim load.
    fn affordable(&self, idx: usize, margin: f64) -> bool {
        self.measured_step_ms * TICK_HZ as f64 * WARP_LADDER[idx] as f64 * margin
            < BUDGET_FRACTION * FRAME_BUDGET_MS
    }

    /// Largest rung `≤ requested` satisfying [`Self::affordable`] — 0 when
    /// even ×1 misses the budget (a pathological scene; the sim still runs).
    fn ladder_ceiling(&self, margin: f64) -> usize {
        (0..=self.requested)
            .rev()
            .find(|&k| self.affordable(k, margin))
            .unwrap_or(0)
    }

    /// §2.6.3 `decide`: fold pending manual steps into `requested`, demote
    /// `effective` immediately to the largest affordable rung, and promote
    /// only when the margined inequality has held [`PROMOTE_FRAMES`]
    /// consecutive frames (the 1.5×/30-frame hysteresis that prevents
    /// oscillation — per the plan's letter it also applies to manual
    /// step-ups, so a request engages after ~0.5 s at 60 fps).
    ///
    /// Returns `Some(multiplier)` when the emitted level changed; the
    /// caller logs `SimCommand::SetWarp(multiplier)` so the decision is a
    /// replayable command, never a measurement.
    pub fn decide(&mut self) -> Option<u32> {
        if self.pending_step != 0 {
            let req = self.requested as i64 + i64::from(self.pending_step);
            self.requested = req.clamp(0, WARP_LADDER.len() as i64 - 1) as usize;
            self.pending_step = 0;
        }
        // Demotion is immediate — the player must not overshoot because a
        // frame stalled.
        let mut next = self.effective.min(self.ladder_ceiling(1.0));
        if next < self.requested {
            let margined = self.ladder_ceiling(PROMOTE_MARGIN);
            if margined > next {
                // §2.6.3's "30 consecutive frames" binds the streak to ONE
                // candidate rung: a frame whose margined ceiling differs
                // restarts the count at 1 — "a promotion direction held"
                // across different rungs is not the hysteresis the plan
                // wrote.
                if self.promote_candidate == Some(margined) {
                    self.promote_streak += 1;
                } else {
                    self.promote_candidate = Some(margined);
                    self.promote_streak = 1;
                }
                if self.promote_streak >= PROMOTE_FRAMES {
                    next = margined;
                    self.promote_candidate = None;
                    self.promote_streak = 0;
                }
            } else {
                self.promote_candidate = None;
                self.promote_streak = 0;
            }
        } else {
            self.promote_candidate = None;
            self.promote_streak = 0;
        }
        let changed = next != self.effective;
        self.effective = next;
        changed.then(|| WARP_LADDER[next] as u32)
    }

    /// §2.5's `warp_controller.target_tick(tick, dt_wall)`: the absolute
    /// integer frame target at the effective multiplier. `dt_wall` is
    /// already clamped by the caller (§2.5's `min(dt, 100ms)`); it is
    /// quantized once into integer nanoseconds and accumulated as
    /// `ns × multiplier` in `u128` — integer math, never float
    /// accumulation — then floored to whole ticks. Per-frame quantization
    /// loses ≤1 ns, a bounded sub-nanosecond drift rather than the
    /// unbounded growth of a float accumulator.
    ///
    /// `current_tick` keeps the result ≥ the sim clock (the clock cannot
    /// retreat, §2.3); saturation to `u64::MAX` is unreachable — at ×10⁵
    /// the accumulator needs ~10²⁰ wall-years — and the sim's
    /// `MAX_TICKS_PER_FRAME` back-pressure bounds any real overshoot.
    pub fn target_tick(&mut self, current_tick: u64, dt_wall: f64) -> u64 {
        let dt_nanos = (dt_wall.max(0.0) * NANOS_PER_SEC as f64) as u64;
        self.scaled_nanos = self
            .scaled_nanos
            .saturating_add(u128::from(dt_nanos) * u128::from(self.multiplier()));
        let ticks = self.scaled_nanos * u128::from(TICK_HZ) / NANOS_PER_SEC;
        let target = u64::try_from(ticks).unwrap_or(u64::MAX);
        target.max(current_tick)
    }

    /// The emitted multiplier — `WARP_LADDER[effective]`.
    pub fn multiplier(&self) -> u64 {
        WARP_LADDER[self.effective]
    }

    /// The multiplier the player asked for — the panel shows it when the
    /// §2.6.3 derate is holding `effective` below it.
    pub fn requested_multiplier(&self) -> u64 {
        WARP_LADDER[self.requested]
    }
}

impl Default for WarpController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Zero measured cost → every rung affordable → promotion engages the
    /// full request after the §2.6.3 hysteresis window.
    #[test]
    fn manual_steps_engage_after_hysteresis() {
        let mut c = WarpController::new();
        c.request_step(1);
        assert_eq!(c.decide(), None); // still ×1 during the window
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        // First decide applied the request; the (PROMOTE_FRAMES)th
        // additional call promotes to ×2 once.
        assert_eq!(c.multiplier(), 2);
        c.request_step(WARP_LADDER.len() as i32); // clamp at the ceiling
        c.decide();
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
        // Stepping down is immediate — no hysteresis on demotion.
        c.request_step(-1);
        assert_eq!(c.decide(), Some(10_000));
    }

    /// §2.6.3 demotion: a huge measured step cost collapses the allowed
    /// ladder to ×1, and recovery waits out the margined 30-frame window.
    #[test]
    fn derate_demotes_and_recovers() {
        let mut c = WarpController::new();
        c.request_step(6); // request ×10⁵
        for _ in 0..=PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
        // A stall: measured step cost blows the budget at every rung.
        c.observe(1.0, 0.016);
        assert_eq!(c.decide(), Some(1)); // immediate demotion
        assert_eq!(c.multiplier(), 1);
        // Cost returns to normal — promotion still waits the window.
        c.observe(0.0, 0.016);
        assert_eq!(c.decide(), None);
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
    }

    /// §2.6.3's hysteresis binds the streak to ONE candidate rung: a
    /// margined ceiling that alternates between two rungs keeps
    /// restarting the count — "30 consecutive frames" means the same
    /// rung held, not any promotable direction held.
    #[test]
    fn promote_streak_is_bound_to_one_rung() {
        let mut c = WarpController::new();
        c.request_step(6); // request the ×10⁵ ceiling
        c.decide();
        // Alternate the margined ceiling between rung 5 (×10⁴, at a
        // per-tick cost of 5µs) and rung 4 (×10³, at 50µs): neither holds
        // for 30 consecutive frames, so nothing promotes.
        for i in 0..80 {
            let per_tick_ms = if i % 2 == 0 { 5e-6 } else { 5e-5 };
            c.observe(per_tick_ms, 0.016);
            c.decide();
        }
        assert_eq!(c.multiplier(), 1);
        // Hold the ×10⁴ candidate steady — it engages after the window.
        c.observe(5e-6, 0.016);
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 10_000);
    }

    /// §2.5/§2.6.3 target math: integer accumulation — 60 frames of
    /// exactly 1/60 s at ×1 yield 64 ticks; at ×10 the same wall time
    /// yields 640. Targets are monotonic and never below the clock.
    #[test]
    fn target_tick_is_integer_accumulated() {
        let mut c = WarpController::new();
        let dt = 1.0 / 60.0;
        let mut t = 0;
        for _ in 0..60 {
            t = c.target_tick(0, dt);
        }
        // 60 × 16 666 666 ns quantization = 999 999 960 ns → 63.999… → 63.
        // Documented bounded truncation, not float drift.
        assert_eq!(t, 63);
        // Monotonic: a zero-dt frame can't retreat, and never sits below
        // the sim clock even if the clock ran ahead via AdvanceTo.
        assert_eq!(c.target_tick(70, 0.0), 70);
        // Warp up to index 2 (×10): another wall second adds ~640 ticks to
        // the accumulated scaled time.
        c.request_step(2);
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 10);
        for _ in 0..60 {
            t = c.target_tick(0, dt);
        }
        // scaled = 999_999_960 + 60×166_666_660 ns → 703.999… ticks → 703.
        assert_eq!(t, 703);
    }
}
