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
/// Regime gating landed with the M2-C bubble as a soft gate: the frame
/// loop evaluates `World::rails_warp_refusal` on post-step state every
/// frame, and while it reports a refusal (a bubble-resident vessel under
/// thrust, in unsettled contact, or in atmosphere) the controller is
/// capped at rung 1 — the predicate's conditions are the §2.6.2 RAILS
/// row's allowed-when (×10…×10⁵); ×2 physics warp is the "bubble
/// occupied" regime they describe, so the floor stops there instead of
/// pinning ×1 (a fired §2.6.5 `Clamping` event still drops to rung 0)
/// — see `App::on_step_report`. The check reads state the step just
/// produced, so a refusal that appears mid-step lands at the NEXT
/// frame's command drain: a documented one-frame lag, bounded because
/// the refusal conditions persist until commanded away.
///
/// The persistence is also what closes the gate against `decide`'s
/// promotion side: the frame loop re-invokes [`WarpController::demote`]
/// EVERY frame the refusal holds — including at-or-below the floor
/// where the demote is a no-op emit — and that heartbeat re-clears any
/// §2.6.3 promotion streak aimed ABOVE the floor, so `decide` (which
/// runs pre-step) can never rebuild the 30-frame window into a gated
/// rung. A streak at-or-below the floor survives on purpose — rung 1
/// is a rung the refusal itself allows, so ×2 can still engage while
/// the predicate holds.
pub const WARP_LADDER: [u64; 7] = [1, 2, 10, 100, 1_000, 10_000, 100_000];

/// §2.6.3: `budget_fraction` — the share of the frame budget sim stepping
/// may consume before the controller derates.
const BUDGET_FRACTION: f64 = 0.5;
/// The reference frame rate the budget is written against (60 fps).
const FRAME_RATE_FPS: f64 = 60.0;
/// Reference frame budget for the §2.6.3 inequality (60 fps).
const FRAME_BUDGET_MS: f64 = 1000.0 / FRAME_RATE_FPS;
/// §2.6.3 hysteresis: promotion requires the inequality to hold with this
/// margin…
const PROMOTE_MARGIN: f64 = 1.5;
/// …for this many consecutive frames, to prevent oscillation.
const PROMOTE_FRAMES: u32 = 30;

/// Nanoseconds per wall second — the integer quantum [`Self::target_tick`]
/// accumulates in.
const NANOS_PER_SEC: u128 = 1_000_000_000;

/// Nanoseconds per sim tick — `NANOS_PER_SEC / TICK_HZ`, exact
/// (10⁹ = 64 × 15 625 000). The quantum a [`Self::needs_rebase`]
/// re-anchor scales `current_tick` by.
const NANOS_PER_TICK: u128 = NANOS_PER_SEC / TICK_HZ as u128;

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
    /// Set by every downward step — `decide`'s immediate demotion,
    /// [`Self::demote`], and the settings cap that routes through it —
    /// and by [`Self::rebase`] when the app swaps worlds out from under
    /// the accumulator (load/revert/new game). The scaled-time backlog
    /// `scaled_nanos` banked at the previous multiplier is not owed to
    /// the sim: a sustained-×10⁵ backlog outruns the sim's per-call
    /// tick budget, so without a re-anchor the clock keeps grinding
    /// toward the stale absolute target for minutes after the HUD
    /// already shows ×1. The next [`Self::target_tick`] re-anchors
    /// `scaled_nanos` at the live sim clock (it owns `current_tick`),
    /// which is legal because a smaller `AdvanceTo` is absorbed, never
    /// a retreat — csp-sim's `advance_to_never_retreats_the_clock`.
    ///
    /// Armed at construction too: the first `target_tick` of a fresh
    /// controller anchors at the live clock rather than zero. The
    /// zero anchor only ever produced `target = current_tick` — a
    /// `step_frame` call that is no tick boundary and therefore
    /// **drains nothing** — so a world whose clock did not start at 0
    /// (the starter seed's build-delivery step to `complete_tick`, any
    /// restored world) would sit frozen, every queued command
    /// undrained, until enough wall time accumulated to pass the
    /// jump. That is the shipped "pad craft will not launch" dead
    /// sim: `SetThrottle` queued, never applied, `throttle_queued`
    /// then suppressing the unlatch re-send forever.
    needs_rebase: bool,
    /// The settings-applied ladder ceiling (M10 "max time warp"): a QoL
    /// cap on `requested` — the §2.6.3 inequality still applies
    /// underneath it. `WARP_LADDER.len() - 1` until the settings limit it.
    cap_idx: usize,
}

impl WarpController {
    /// Fresh controller: requested = effective = ×1 (real time, §2.6.2's
    /// always-allowed regime — matching the sim's boot warp level). The
    /// accumulator starts unanchored — `needs_rebase` arms the first
    /// [`Self::target_tick`] to anchor at whatever clock the world
    /// actually holds (the startup seed's delivery step leaves it far
    /// past 0).
    pub fn new() -> Self {
        Self {
            requested: 0,
            effective: 0,
            pending_step: 0,
            promote_streak: 0,
            promote_candidate: None,
            measured_step_ms: 0.0,
            scaled_nanos: 0,
            needs_rebase: true,
            cap_idx: WARP_LADDER.len() - 1,
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
    /// the promotion side: `per_tick_ms × ticks_per_frame × margin <
    /// BUDGET_FRACTION × FRAME_BUDGET_MS`. `measured_step_ms` is per-tick
    /// step cost, and at multiplier `k` the sim executes `TICK_HZ × k`
    /// ticks per wall-second — `TICK_HZ × k / FRAME_RATE_FPS` per frame —
    /// so the left side is the per-frame sim-stepping cost the
    /// `BUDGET_FRACTION` share of the frame budget must cover.
    /// (The pre-fix form compared ms-per-second against ms-per-frame —
    /// a 60× unit error that made promotion past ×1 unreachable on any
    /// real scene.)
    fn affordable(&self, idx: usize, margin: f64) -> bool {
        let ticks_per_frame = TICK_HZ as f64 * WARP_LADDER[idx] as f64 / FRAME_RATE_FPS;
        self.measured_step_ms * ticks_per_frame * margin < BUDGET_FRACTION * FRAME_BUDGET_MS
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
    /// step-ups, so a request engages after ~0.5 s at 60 fps). Every
    /// downward step — manual `−`, auto-derate, a `requested` clamp —
    /// also arms [`Self::needs_rebase`] so the emitted target re-anchors
    /// at the live clock instead of dragging the banked backlog.
    ///
    /// Returns `Some(multiplier)` when the emitted level changed; the
    /// caller logs `SimCommand::SetWarp(multiplier)` so the decision is a
    /// replayable command, never a measurement.
    pub fn decide(&mut self) -> Option<u32> {
        if self.pending_step != 0 {
            let req = self.requested as i64 + i64::from(self.pending_step);
            self.requested = req.clamp(0, self.cap_idx as i64) as usize;
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
        if next < self.effective {
            // A downward step drops the scaled-time backlog banked at
            // the higher multiplier — the sim never owed it, and a
            // stale absolute target would keep `step_frame` at its
            // per-call budget long after the HUD shows the lower rung.
            self.needs_rebase = true;
        }
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
    /// the accumulator needs ~10²⁰ wall-years. [`Self::needs_rebase`]
    /// re-anchors the accumulator at `current_tick` first: a demotion
    /// drops the backlog the previous multiplier banked, which stays
    /// legal because the emitted `AdvanceTo` only ever moves the clock
    /// forward — a target at or below it is absorbed, never a retreat.
    ///
    /// A behind-clock accumulator re-anchors on sight: `scaled_nanos`
    /// IS the pre-quantization target stream, so the live clock can
    /// never legitimately sit ahead of it (it produced every target the
    /// clock advanced toward). Behind means the accumulator was never
    /// anchored to this world — a fresh controller, or a world the
    /// caller swapped without rebasing — and letting it lag would emit
    /// `target = current_tick` frames that are not tick boundaries:
    /// `step_frame` drains nothing below its target, so the queue
    /// (throttle included) never applies. The self-heal is the same
    /// write `needs_rebase` performs; the flag covers the opposite
    /// direction (a stale *forward* backlog after a demotion or world
    /// swap), which only the caller can distinguish from honest
    /// accumulation.
    pub fn target_tick(&mut self, current_tick: u64, dt_wall: f64) -> u64 {
        let anchor = u128::from(current_tick) * NANOS_PER_TICK;
        if self.needs_rebase || self.scaled_nanos < anchor {
            // Re-anchor at the live clock. `NANOS_PER_TICK` is exact,
            // so the floored tick target lands back on `current_tick`
            // — no round-trip loss beyond the usual ≤1-tick
            // quantization.
            self.scaled_nanos = anchor;
            self.needs_rebase = false;
        }
        let dt_nanos = (dt_wall.max(0.0) * NANOS_PER_SEC as f64) as u64;
        self.scaled_nanos = self
            .scaled_nanos
            .saturating_add(u128::from(dt_nanos) * u128::from(self.multiplier()));
        let ticks = self.scaled_nanos * u128::from(TICK_HZ) / NANOS_PER_SEC;
        let target = u64::try_from(ticks).unwrap_or(u64::MAX);
        target.max(current_tick)
    }

    /// §2.6.5 `Clamping` / §2.6.2 rails-refusal demotion: drop `effective`
    /// to ladder index `rung` NOW — the §2.6.3 hysteresis deliberately does
    /// not apply downward (a fired Clamping event means the player could
    /// already be overshooting). An in-flight promotion streak aimed
    /// ABOVE `rung` is cleared — unconditionally, even when `rung` is
    /// at-or-above `effective` and there is nothing to emit. The frame
    /// loop re-invokes `demote` every frame a gate holds, so that no-op
    /// call is the heartbeat: while the gate holds, `decide` (which
    /// runs pre-step) can never accumulate the 30-frame promotion
    /// window into a rung the gate forbids — a persistent refusal
    /// cannot be promoted through. A streak at-or-below `rung`
    /// survives on purpose: the demote declares `rung` reachable, and
    /// for the rails refusal that floor is rung 1 — ×2 physics warp,
    /// the "bubble occupied" regime the predicate describes. Clearing
    /// the streak that reaches it would hold `effective` below the
    /// floor the gate itself set, keeping the one allowed promotion
    /// unreachable for the refusal's whole life.
    /// `requested` is untouched: a one-shot event should not rewrite
    /// player intent (re-promotion still waits the hysteresis once the
    /// refusal clears), and a persistent refusal re-demotes every frame
    /// it is checked, so it cannot oscillate either.
    ///
    /// A real demotion also arms [`Self::needs_rebase`]: the scaled-time
    /// backlog banked at the dropped rung is not owed to the sim.
    ///
    /// Returns `Some(multiplier)` when the emitted level changed → the
    /// caller logs `SimCommand::SetWarp` so the demotion is replayable.
    pub fn demote(&mut self, rung: usize) -> Option<u32> {
        let rung = rung.min(WARP_LADDER.len() - 1);
        // Cleared before the early return on purpose — see the doc: the
        // gate heartbeat calls `demote` every frame it holds, and a
        // streak aimed above the floor must not survive it. A streak
        // bound for `rung` itself (or lower) is a promotion the gate
        // allows — killing it would pin `effective` under the floor.
        if self.promote_candidate.is_some_and(|c| c > rung) {
            self.promote_streak = 0;
            self.promote_candidate = None;
        }
        if rung >= self.effective {
            return None;
        }
        self.effective = rung;
        self.needs_rebase = true;
        Some(WARP_LADDER[rung] as u32)
    }

    /// Re-anchor the accumulator at the next [`Self::target_tick`]'s
    /// live clock — the app calls this when the world under the
    /// controller was swapped (a loaded save, a revert, a new game):
    /// the banked `scaled_nanos` describes the OLD world's clock, so a
    /// stale *forward* backlog would otherwise read as a legitimate
    /// target and the sim would sprint to it (a behind-clock backlog
    /// `target_tick` already self-heals; only the caller can tell a
    /// forward one is stale).
    pub fn rebase(&mut self) {
        self.needs_rebase = true;
    }

    /// The M10 "max time warp" settings cap: clamp `requested` to the
    /// largest rung whose multiplier is `≤ cap` (a stored value between
    /// rungs rounds down; `0` means ×1 — the sim cannot be frozen by a
    /// settings file). If the live `effective` sits above the new cap it
    /// demotes NOW — no hysteresis downward, same rule as
    /// [`Self::demote`] — and returns `Some(multiplier)` so the caller
    /// logs `SimCommand::SetWarp` and the demotion stays replayable.
    pub fn set_cap_multiplier(&mut self, cap: u64) -> Option<u32> {
        self.cap_idx = WARP_LADDER
            .iter()
            .rposition(|&m| m <= cap.max(1))
            .unwrap_or(0);
        self.requested = self.requested.min(self.cap_idx);
        self.demote(self.cap_idx)
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

    /// The multiplier at ladder rung `idx` (clamped) — notices name the
    /// rung a request asked for before it engages.
    pub fn multiplier_at(&self, idx: usize) -> u64 {
        WARP_LADDER[idx.min(WARP_LADDER.len() - 1)]
    }

    /// The ladder index `requested` would sit at once `pending_step`
    /// plus `step` more requests fold in — capped at the settings
    /// ceiling like `decide`'s fold. Press-time feedback needs this:
    /// "what is this key actually asking for" is answered before the
    /// request spends a frame in the queue.
    pub fn request_preview(&self, step: i32) -> usize {
        (self.requested as i64 + i64::from(self.pending_step) + i64::from(step))
            .clamp(0, self.cap_idx as i64) as usize
    }

    /// Whether the settings cap sits below the ladder top — a "dead"
    /// step-up names the cap when it, not the ladder, did the clamping.
    pub fn capped_below_ladder_top(&self) -> bool {
        self.cap_idx < WARP_LADDER.len() - 1
    }

    /// The largest rung `decide`'s promotion could reach at the current
    /// measured cost — the §2.6.3 margined inequality over the WHOLE
    /// ladder ([`Self::ladder_ceiling`] is bounded by `requested`; this
    /// is not). A step-up request above this can never engage while the
    /// measured cost holds — the caller surfaces that as a limit notice
    /// at the press instead of a silently dead key.
    pub fn promotable_ceiling(&self) -> usize {
        (0..WARP_LADDER.len())
            .rev()
            .find(|&k| self.affordable(k, PROMOTE_MARGIN))
            .unwrap_or(0)
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
    /// Inputs are honest scene costs — the pre-fix tests passed only
    /// because they fed values 1000× too small (5 ns mislabeled as µs):
    /// at the corrected per-frame inequality a heavy scene legitimately
    /// derates and a real one re-promotes to a real rung.
    #[test]
    fn derate_demotes_and_recovers() {
        let mut c = WarpController::new();
        c.request_step(6); // request ×10⁵
        for _ in 0..=PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
        // A stall: 8 ms/tick misses the budget even at ×1
        // (8 × 64 / 60 = 8.53 > 8.33 ms/frame) — every rung derates.
        c.observe(8.0, 0.016);
        assert_eq!(c.decide(), Some(1)); // immediate demotion
        assert_eq!(c.multiplier(), 1);
        // Cost returns to a real scene's: 0.672 ms/tick is the committed
        // 200-part-atmo reference number (perf-log-devbox-release.ron) —
        // promotion still waits the window, and the margined ceiling at
        // that cost is ×2, not the requested ×10⁵.
        c.observe(0.6719, 0.016);
        assert_eq!(c.decide(), None);
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 2);
    }

    /// The §2.6.3 inequality against the committed perf log
    /// (`docs/perf/perf-log-devbox-release.ron`): the 200-part-atmo
    /// scene promotes to ×2 physics warp, the cheap 500-rails map
    /// scene reaches ×10, and the 1000-part-orbit station correctly
    /// stays at ×1. Pre-fix all three were locked at ×1 — promotion
    /// needed ≤43 µs/tick against real scenes costing 0.15–4.6 ms.
    #[test]
    fn reference_scenes_reach_real_rungs() {
        let promote_to = |per_tick_ms: f64, request: i32| -> u64 {
            let mut c = WarpController::new();
            c.request_step(request);
            c.observe(per_tick_ms, 1.0 / 60.0);
            for _ in 0..=PROMOTE_FRAMES {
                c.decide();
            }
            c.multiplier()
        };
        // sim.200-part-atmo = 0.6719 ms/tick → ×2 (the ×10 rung fails
        // the 1.5× promotion margin: 10.75 > 8.33 ms/frame).
        assert_eq!(promote_to(0.6719, 6), 2);
        // sim.map-500-rails = 0.145 ms/tick → ×10 (margined ×10 is
        // 2.32 ms/frame; margined ×100 would be 23.2 — denied).
        assert_eq!(promote_to(0.145, 6), 10);
        // sim.1000-part-orbit = 4.6344 ms/tick → stays ×1 (even ×2
        // costs 9.9 ms/frame unmargined).
        assert_eq!(promote_to(4.6344, 6), 1);
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
        // Alternate the margined ceiling between rung 4 (×10³, at a
        // per-tick cost of 5 µs = 5e-3 ms) and rung 3 (×10², at 50 µs):
        // neither holds for 30 consecutive frames, so nothing promotes.
        // (The pre-fix test fed 5e-6/5e-5 ms — 5/50 ns mislabeled as µs,
        // 1000× too small; at honest µs values the alternating ceiling
        // is ×10³↔×10² under the corrected per-frame inequality.)
        for i in 0..80 {
            let per_tick_ms = if i % 2 == 0 { 5e-3 } else { 5e-2 };
            c.observe(per_tick_ms, 0.016);
            c.decide();
        }
        assert_eq!(c.multiplier(), 1);
        // Hold the ×10³ candidate steady — it engages after the window.
        c.observe(5e-3, 0.016);
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 1_000);
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
        // the sim clock even if the clock ran ahead via AdvanceTo. A
        // clock AHEAD of the accumulator is the stale state the
        // self-heal covers — the call re-anchors scaled_nanos at
        // 70 ticks (the backlog the clock already passed is not owed).
        assert_eq!(c.target_tick(70, 0.0), 70);
        // Warp up to index 2 (×10): another wall second adds ~640 ticks to
        // the accumulated scaled time — now anchored at 70.
        c.request_step(2);
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 10);
        for _ in 0..60 {
            t = c.target_tick(0, dt);
        }
        // scaled = 70×15_625_000 + 60×166_666_660 ns → 709.999… ticks → 709.
        assert_eq!(t, 709);
    }

    /// The shipped dead-sim regression: a fresh controller facing a
    /// world whose clock did NOT start at 0 (the starter seed's
    /// build-delivery step jumps it to `complete_tick`) must anchor
    /// `scaled_nanos` at the live clock on the first `target_tick` —
    /// a zero-anchored accumulator only ever emits
    /// `target = current_tick`, a `step_frame` call that is not a
    /// tick boundary and therefore drains NOTHING (the queued
    /// `SetThrottle` never applied: the frozen pad). Same self-heal
    /// covers an accumulator left behind by a later world jump —
    /// and `rebase` re-anchors a stale FORWARD backlog the caller
    /// flags after a world swap.
    #[test]
    fn target_tick_anchors_to_the_live_clock() {
        let mut c = WarpController::new();
        // A world already at tick 4941 (the seeded build delivery):
        // the first real frame must emit a target AHEAD of the
        // clock, not the clock itself.
        let t = c.target_tick(4941, 1.0 / 60.0);
        assert!(t > 4941, "first target must advance past 4941, got {t}");
        // From there the accumulator tracks wall time normally.
        let t2 = c.target_tick(4942, 1.0 / 60.0);
        assert!(t2 > t, "targets keep advancing");
        // A behind-clock accumulator self-heals without the flag:
        // pretend the world jumped forward again (a second seed, a
        // later save) — the next target re-anchors, not freezes.
        let mut c2 = WarpController::new();
        let _ = c2.target_tick(10, 1.0 / 60.0); // anchored at 10
        let jumped = c2.target_tick(9_999, 1.0 / 60.0);
        assert!(jumped > 9_999, "a world jump re-anchors, got {jumped}");
        // `rebase` drops a stale FORWARD backlog: at ×1 the banked
        // scaled time is the absolute target stream — after the
        // caller flags a world swap the accumulator re-anchors at
        // the new (earlier) clock instead of sprinting to the old
        // world's horizon.
        let mut c3 = WarpController::new();
        for _ in 0..120 {
            let _ = c3.target_tick(0, 1.0 / 60.0);
        }
        c3.rebase();
        let t3 = c3.target_tick(4, 1.0 / 60.0);
        assert!(
            t3 <= 5,
            "rebase anchors at the new clock (4 + this frame's dt), got {t3}"
        );
    }

    /// The M10 settings cap: `requested` clamps to the largest rung `≤`
    /// the cap, a lowered cap demotes `effective` immediately (logged by
    /// the caller as `SetWarp`), and between-rungs values round down.
    #[test]
    fn cap_multiplier_clamps_requests_and_demotes() {
        let mut c = WarpController::new();
        c.set_cap_multiplier(1_000);
        c.request_step(WARP_LADDER.len() as i32); // try for the ceiling
        c.decide();
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        // Capped at ×10³ — the ladder top stays out of reach.
        assert_eq!(c.multiplier(), 1_000);
        assert_eq!(c.requested_multiplier(), 1_000);
        // Between-rungs stored values round down to the rung ≤ cap —
        // the live ×10³ already satisfies it, so nothing demotes.
        assert_eq!(c.set_cap_multiplier(5_000), None);
        assert_eq!(c.multiplier(), 1_000);
        // A cap below the live rung demotes NOW (no hysteresis downward).
        assert_eq!(c.set_cap_multiplier(10), Some(10));
        assert_eq!(c.multiplier(), 10);
        // And zero cannot freeze the sim — it floors at ×1.
        assert_eq!(c.set_cap_multiplier(0), Some(1));
        assert_eq!(c.multiplier(), 1);
        // Re-widening the cap lets requests climb again.
        c.set_cap_multiplier(100_000);
        c.request_step(1);
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 2);
    }

    /// Press-time feedback helpers: `request_preview` folds `pending_step`
    /// into `requested` exactly like `decide` (settings cap included),
    /// and `promotable_ceiling` answers "could this ever engage" over
    /// the WHOLE ladder at the measured cost — the signal a
    /// limited/dead press is narrated from in `warp_step_request`.
    #[test]
    fn press_preview_and_promotable_ceiling() {
        let mut c = WarpController::new();
        assert_eq!(c.request_preview(3), 3);
        c.request_step(2);
        // The queued step counts too — the preview is the post-fold rung.
        assert_eq!(c.request_preview(1), 3);
        c.set_cap_multiplier(100);
        assert_eq!(c.request_preview(9), 3); // clamped at the ×100 cap
        assert!(c.capped_below_ladder_top());
        assert_eq!(c.multiplier_at(3), 100);
        // Zero measured cost → every rung promotable.
        assert_eq!(c.promotable_ceiling(), WARP_LADDER.len() - 1);
        // The committed 200-part-atmo reference cost: margined, only
        // ×2 clears the §2.6.3 inequality — a ×10 request could never
        // engage while the scene stays this heavy.
        c.observe(0.6719, 0.016);
        assert_eq!(c.promotable_ceiling(), 1);
    }

    /// §2.6.5 Clamping demotion: drops `effective` immediately, keeps
    /// `requested`, and re-promotion still waits the §2.6.3 hysteresis.
    #[test]
    fn demote_drops_effective_now_and_repromotes_via_hysteresis() {
        let mut c = WarpController::new();
        c.request_step(6); // request ×10⁵
        for _ in 0..=PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
        // A Clamping event fired → demote to ×1, returns the new level.
        assert_eq!(c.demote(0), Some(1));
        assert_eq!(c.multiplier(), 1);
        assert_eq!(c.requested_multiplier(), 100_000); // intent preserved
        // No second event: nothing to emit.
        assert_eq!(c.demote(0), None);
        // Re-promotion needs the full 30-frame margined window again.
        assert_eq!(c.decide(), None);
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
    }

    /// §2.6.2's gate vs §2.6.3's hysteresis: while `rails_warp_refusal`
    /// reports, `App::run_frame` re-invokes `demote(0)` EVERY frame —
    /// a no-op emit at rung 0, but the heartbeat must still clear the
    /// promotion streak. `decide` runs pre-step: if the streak survived
    /// the no-op demote it would rebuild to [`PROMOTE_FRAMES`] and emit
    /// `SetWarp(100_000)` under the refusal every ~30 frames.
    #[test]
    fn persistent_refusal_blocks_repromotion() {
        let mut c = WarpController::new();
        c.request_step(6); // request ×10⁵
        for _ in 0..=PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
        // The refusal fires → the first demote emits ×1 once.
        assert_eq!(c.demote(0), Some(1));
        // …and holds: every later frame calls demote(0) again (nothing
        // to emit) after decide() ran. No `SetWarp` promotion may leave
        // the controller for the refusal's whole duration.
        for _ in 0..3 * PROMOTE_FRAMES {
            assert_eq!(c.decide(), None, "promotion emitted while the refusal held");
            assert_eq!(c.demote(0), None); // heartbeat: nothing to emit
            assert_eq!(c.multiplier(), 1);
        }
        // Release: `requested` survived intact, so the honest
        // re-promotion path runs its hysteresis and engages.
        for _ in 0..PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
    }

    /// §2.6.3's accumulator vs the sim's per-call tick budget: at ×10⁵
    /// each frame accrues ~1667 sim-s of target while `step_frame`
    /// covers at most 1080 sim-s a call, so the clock lags the emitted
    /// target by a growing backlog. Stepping down must re-anchor
    /// `scaled_nanos` at the live clock — the backlog is not owed to
    /// the sim — or the HUD shows ×1 while the sim keeps grinding out
    /// the banked target for minutes.
    #[test]
    fn step_down_rebases_the_target() {
        /// Mirrors `csp_sim::MAX_TICKS_PER_FRAME` — the per-call advance
        /// budget that lets the sim clock lag the emitted warp target.
        /// (The controller is sim-free by design; the test only needs a
        /// per-frame cap well under the ×10⁵ accrual.)
        const FRAME_BUDGET: u64 = 69_120;
        let mut c = WarpController::new();
        c.request_step(6); // ×10⁵
        for _ in 0..=PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
        let dt = 1.0 / 60.0;
        // Two wall-seconds at ×10⁵: the target outruns the clock.
        let mut clock = 0_u64;
        for _ in 0..120 {
            let target = c.target_tick(clock, dt);
            clock += FRAME_BUDGET.min(target - clock);
        }
        // The banked backlog is the pre-fix failure mode — assert the
        // setup really built one.
        let stale = c.target_tick(clock, 0.0);
        assert!(
            stale - clock > FRAME_BUDGET,
            "setup must outrun the clock: stale {stale}, clock {clock}"
        );
        // Player steps back to ×1: the next target re-anchors at the
        // live clock — this frame's ×1 accrual only — instead of
        // dragging the ×10⁵ backlog.
        c.request_step(-6);
        assert_eq!(c.decide(), Some(1));
        let t = c.target_tick(clock, dt);
        assert_eq!(t, clock + 1, "target must re-anchor at the clock");
        // Normal accumulation resumes from the re-anchored base, never
        // below the clock.
        assert_eq!(c.target_tick(clock + 1, 0.0), clock + 1);
    }

    /// Same re-anchor on the §2.6.2/§2.6.5 demotion path — a refusal or
    /// a fired Clamping event drops `effective` through `demote`, and
    /// the banked backlog drops with it.
    #[test]
    fn demote_rebases_the_target() {
        const FRAME_BUDGET: u64 = 69_120; // csp_sim::MAX_TICKS_PER_FRAME
        let mut c = WarpController::new();
        c.request_step(6);
        for _ in 0..=PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
        let dt = 1.0 / 60.0;
        let mut clock = 0_u64;
        for _ in 0..120 {
            let target = c.target_tick(clock, dt);
            clock += FRAME_BUDGET.min(target - clock);
        }
        assert!(c.target_tick(clock, 0.0) - clock > FRAME_BUDGET);
        // The refusal/clamping demotion → ×1 emits once…
        assert_eq!(c.demote(0), Some(1));
        // …and the next frame's target is the live clock plus this
        // frame's ×1 slice, not the banked ×10⁵ remainder.
        assert_eq!(c.target_tick(clock, dt), clock + 1);
    }

    /// The demote heartbeat's floor semantics: a gate at `rung` kills
    /// a promotion streak aimed ABOVE it every frame (a held refusal
    /// cannot be promoted through) but leaves a streak at-or-below the
    /// floor alive — rung 1 under a rails refusal is §2.6.2's legal
    /// "bubble occupied" ×2, so its promotion must still complete.
    /// Killing it would pin `effective` under the floor the gate set.
    #[test]
    fn demote_clears_only_streaks_above_the_floor() {
        let mut c = WarpController::new();
        c.request_step(1); // aim at ×2
        c.decide(); // the rung-1 streak begins
        for _ in 1..PROMOTE_FRAMES {
            // The heartbeat at floor 1 must not kill the ×2 promotion.
            assert_eq!(c.demote(1), None);
            c.decide();
        }
        assert_eq!(c.multiplier(), 2, "the rung-1 streak survives demote(1)");
        // A streak aimed ABOVE the floor dies under the same heartbeat:
        // ×10⁵ engaged, the refusal's floor drops the controller to
        // ×2, and no re-promotion into the gated rungs can complete.
        let mut c = WarpController::new();
        c.request_step(6);
        for _ in 0..=PROMOTE_FRAMES {
            c.decide();
        }
        assert_eq!(c.multiplier(), 100_000);
        assert_eq!(c.demote(1), Some(2));
        for _ in 0..2 * PROMOTE_FRAMES {
            c.decide();
            c.demote(1);
        }
        assert_eq!(
            c.multiplier(),
            2,
            "streaks into the gated rungs never complete"
        );
    }
}
