# Cluster 5a Review Report — orbit propagation math

**File**: crates/csp-sim/src/orbit.rs
**Reviewer**: Ruthless Reviewer 5a
**Date**: 2026-10-25
**Mode**: Static only (no cargo runs — tree mid-edit)

---

### [SEVERITY: MEDIUM] Finding 1: Scan publishes `Exit{root}` transitions that `rebase` structurally refuses — and the shipped data makes the entire root-exit path (incl. §2.9 debris-escape deletion) unreachable dead code
**File**: crates/csp-sim/src/orbit.rs:1232-1247 (also 998-1008, 242-258)
**Problem**: `soi_candidates` admits the vessel-parent's own boundary as an exit candidate purely on `soi_m: Option<f64>` being `Some` — it never checks that the parent itself has a parent. But `rebase`'s Exit arm requires the old parent to have a grandparent and returns `Recoverable("soi exit: parent has no parent")` otherwise. The publisher and the applier therefore disagree on the root's boundary: for any model whose root body carries a finite `soi_m`, the scan schedules `Exit{root}` events that only lib.rs's debris intercept (`debris_since.is_some()`, lib.rs:7425-7435) can consume; a player vessel exiting gets a transition the seam refuses. Meanwhile the shipped home system has `solen: soi_m: None` (data/systems/home/solen/body.ron), so no root-exit candidate is ever enumerated — confirming from the publisher side that §2.9's escape-past-root deletion is dead code in the shipped system (lib.rs's residence walk at 5560-5570 gates on `soi_m.is_some_and(...)` too, so neither the event path nor the stamp path can fire it). csp-data's validator does NOT reject a finite root `soi_m` (validate.rs only checks positivity), so the broken configuration is data-reachable, not hypothetical.
**Evidence**:
```rust
// orbit.rs:1234-1237 — parent candidate admitted with no grandparent check
if let Some(b) = self.bodies.get(&s.parent)
    && let Some(cand) = soi_candidate(&s.parent, b.soi_m, true)?
{
    out.push(cand);
}
// orbit.rs:1005-1008 — rebase refuses exactly that candidate's transition
let grandparent = old_parent
    .parent
    .clone()
    .ok_or(SimError::Recoverable("soi exit: parent has no parent"))?;
```
**Impact**: (a) Shipped data: debris that escapes the system is never deleted — it sails on rails past the boundary forever, and the §2.9 feature silently never executes. (b) Any validator-legal system with a finite root SOI: a non-debris vessel crossing it hits `Recoverable` at fire time — rolled back, degraded, re-detected, forever (an elliptic conic whose apoapsis exceeds the root SOI churns one degraded event pair per orbit; positions stay formally consistent only because there is no other frame). The module doc's design assumption ("the star's infinite SOI contributes no exit candidate", line 90-91) is enforced nowhere.
**Suggestion**: Enforce the assumption the docs already state. Either (1) `from_body_defs` rejects `def.parent.is_none() && def.soi_m.is_some()` the way csp-data already rejects `OrbitOnRoot` for orbit elements, making the asymmetry unconstructible; or (2) gate the parent-exit candidate in `soi_candidates` on `b.parent.is_some()` so the scan never publishes a transition `rebase` refuses, and route §2.9 debris escape exclusively through lib.rs's residence-walk distance predicate (which already exists at lib.rs:5560-5570 and does not need a scheduled event). Pick one; today the two pub APIs in this file carry contradictory contracts.

### [SEVERITY: MEDIUM] Finding 2: Documented scan-exhaustion fallback ("clamp the horizon and retry") does not exist; the shipped fallback re-anchors past the dropped crossing, so a failed post-transition publish can strand a vessel in the wrong frame permanently
**File**: crates/csp-sim/src/orbit.rs:76-83 (module doc), 112-127 (`SOI_SCAN_MAX_SAMPLES` doc), 1153-1159 (`next_soi_event_within` doc)
**Problem**: The module's contract states twice that `Recoverable` on sample-budget exhaustion is safe because "the caller's fallback is the §2.6.5 re-issue pattern: clamp the horizon and retry" (lines 81-83, 124-126). The shipped caller has no such fallback: `World::schedule_next_soi` (lib.rs:4455) propagates with `?`, and `fire_due_events` maps the error to the degraded channel — rolling back the entire event application via `ev_shadow` (including an already-applied `apply_soi_transition`) and re-publishing a bare `SoiRescan` at `event.tick + DEGRADED_RESCAN_TICKS`. That rescan's origin sits *after* the crossing that was just dropped: `next_soi_event_within` only scans `(now, horizon]` by its own (correct) contract, so a one-shot crossing — a hyperbolic flyby `Enter{child}` — is never re-detected. The vessel keeps the pre-transition conic indefinitely: positions evaluated in the wrong parent's frame, wrong μ, silently wrong trajectory. orbit.rs's core invariant — exhaustion/errors exist precisely so a crossing is "never silently hidden" (line 81: "never `Ok(None)`, which would silently hide a crossing") — is defeated downstream while this file's docs assert the protection exists. (Budget exhaustion itself is indeed unreachable via `schedule_next_soi` — `scan_horizon`'s cap keeps `Σ span/h_c + 2k` under `SOI_SCAN_MAX_SAMPLES` for any realistic body count — but any other scan `Recoverable`, e.g. a `state_at` Kepler-iteration cap, reaches the same path.)
**Evidence**:
```rust
// orbit.rs:80-83 — the promised fallback
/// `Ok(None)`, which would silently hide a crossing in the unscanned
/// suffix — and the caller's fallback is the §2.6.5 re-issue pattern:
/// clamp the horizon and retry.
```
vs lib.rs:7310-7318 (actual): `self.apply_soi_transition(...)?;` then `self.schedule_next_soi(...)?` — both inside one `fire_event` whose failure restores `ev_shadow` and re-publishes `SoiRescan` at `event.tick.saturating_add(DEGRADED_RESCAN_TICKS)` — behind the dropped crossing.
**Impact**: Low-frequency (needs a scan error on the post-transition rescan) but unbounded-consequence: a flyby vessel is stranded on the old conic with no self-heal (the old conic never re-enters the child's SOI). The documented contract is false, which is worse than no contract: a future maintainer will rely on "clamp and retry" existing. Borderline-low likelihood, rated MEDIUM for the silent-permanent-wrongness tail plus the false doc in a file whose header says its docs "ARE the contract".
**Suggestion**: Make one of the two true. Either implement the documented fallback in `schedule_next_soi` (on scan error, retry once with the horizon halved before propagating), or — better, since exhaustion is already impossible there — have the degraded republish route through `resolve_soi_residence`-style point-in-time re-derivation (which re-applies a *completed* transition from the conic's own state) instead of a behind-the-crossing rescan, and rewrite the orbit.rs doc lines 80-83/124-126 to describe the mechanism that actually ships.

---

## Quality verdict (honest, 5 lines)

1. The numerical core — universal-variable Kepler (Laguerre–Conway with maintained bracket + bisection fallback), Stumpff series/closed-form split, the f&g reconstruction, and the elliptic/parabolic/hyperbolic element conventions (including the `a := −p` parabolic encoding and the retrograde-equatorial ω convention) — is correct as far as static analysis can establish; every formula was re-derived and each degenerate branch (e→0, e→1, i→0/π, α→0, dt→0, dt<0) either has an explicit guard or degrades to a loud `Recoverable`, never silent NaN.
2. NaN production paths are systematically intercepted (`soi_candidate`, the angle funnel in `periapsis_state`, `brent`'s residual check, `propagate`'s finiteness gates) — the loud-NaN discipline is real, not aspirational.
3. Both findings are contract-level, not math-level: the publisher/applier asymmetry on root SOI boundaries (Finding 1, which also confirms from the scan side that §2.9 escape deletion is dead code in the shipped data) and a documented scan-failure fallback that does not ship (Finding 2).
4. No f64/f32 mixing, no unit/sign errors found; tick-lattice differencing (`tick_delta_seconds` via i128) is exemplary.
5. Static-only caveat: no cargo run (tree mid-edit by instruction); the two findings rest on code reading cross-checked against lib.rs call sites (lib.rs:4455, 7143-7260, 7405-7447, 5560-5570) and the shipped data files, not on execution.
