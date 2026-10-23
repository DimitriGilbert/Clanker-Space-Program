# Deep Review — Cluster C05: `orbit.rs` / `transfer.rs` / `maneuver.rs`

Reviewer: logic + determinism pass. All three files read in full; call sites
in `lib.rs` (`schedule_next_soi`, `resolve_soi_residence`,
`apply_soi_transition`, `apply_set_maneuver`, `fire_maneuver_start`,
`drive_node_burn`) and `csp-core` (`SimTime`, `DQuat`) checked where needed.

The code is unusually careful: epoch-anchoring is clean (no per-tick anomaly
re-stamp anywhere — partition independence holds by construction), all
transcendentals route through `libm`, iteration counts are fixed, solver
failure paths are honest `Recoverable`, the retrograde-equatorial ω
convention is correct and tested, and the Lambert lands-on propagation check
is the right defense for the documented cancellation regime. Findings below
are the residual real problems.

---

### [MEDIUM] Finding 1: `predict_encounter` chaining drops the post-rebase `suppress` — the documented composition returns `Err` on the wrong-side artifact

**File:** `crates/csp-sim/src/transfer.rs:1162-1170` (hard-coded `None`),
`crates/csp-sim/src/orbit.rs:1120-1134` (the mechanism it needs)

**Problem.** `predict_encounter`/`predict_encounter_excluding` call
`next_soi_event_within` with `suppress: None`. The documented chaining
recipe — "Feed `rebased` back into `predict_encounter` at `now = event.tick`
to chain multi-encounter predictions" (transfer.rs:1131-1133) — breaks
whenever the Brent-refined event tick lands on the *early* side of the true
crossing (the "≤1 tick, either side" contract permits this). The rebased
conic is then stamped on the wrong side of the boundary it "just crossed",
so the chained scan's first-stride detection re-fires the same boundary in
the same direction:

- After `Enter{B}`: conic parented to `B` but still outside `B`'s SOI →
  degenerate `Enter{B}` (own-parent) detection → `rebase` fails with
  `"soi enter: target is not a child of the vessel's parent"` → the error
  propagates.
- After `Exit{B}`: conic re-parented to the grandparent but still inside
  `B`'s SOI → `Exit{B}` (non-parent) detection → `rebase` fails with
  `"soi exit: event body is not the vessel's parent"` → propagates.

This is exactly the artifact the `suppress` parameter exists for: the live
sim passes the just-fired `(body, direction)` in `schedule_next_soi`
(`lib.rs:6571-6576`, `3988-3995`), and the test
`first_stride_suppression_drops_wrong_side_redetection` proves the artifact
occurs in practice. `exclude` cannot cover the Enter case — exclusion only
applies while the conic starts *inside* (`d < 0` at `now`); the Enter
artifact has `d > 0`.

**Evidence.** Verified by reproduction: the suppression test's own fixture
(vessel parented to `m` at `r = 2.44e6`, just outside `r_soi = 2.43e6`,
inbound) passed to the public `predict_encounter` returns

```
Err(Recoverable("soi enter: target is not a child of the vessel's parent"))
```

instead of the real subsequent event. (`encounter_prediction_chains_
through_rebase` passes only because its Brent happened to land at/after the
crossing; ~half of marginal landings land early.)

**Impact.** The documented `predict_encounter` → re-plan correction loop —
stated as *required, not optional* for `departure_node` — can spuriously
`Err` on a chained call, aborting the correction loop instead of returning
the real next encounter.

**Suggestion.** Expose the suppression through the query (accept an optional
`(body, direction)` parameter, or derive it from the previous prediction:
pass `(event.body, event.direction)` on chained calls), or internally drop
first-stride `Enter{conic.parent}` detections — a conic parented to B while
starting outside B's SOI is already the degenerate state the mechanism
exists to cover.

---

### [MEDIUM] Finding 2: `departure_node` can return `tick ≤ clock` — `SetManeuver`'s silent clamp then produces a wrong-longitude burn

**File:** `crates/csp-sim/src/transfer.rs:1355-1378`

**Problem.** `burn_s = dep_s + dm/n − period` deliberately places the burn
up to one parking period *before* `plan.departure_tick` ("node at window"
convention). The only refusal is `burn_s < 0` (pre-epoch) — the function is
a pure query with no `now`/clock parameter. When a plan's departure is
within ~one parking period of the present, `node.tick` lands at-or-before
the sim clock. `apply_set_maneuver` then applies the
may-not-rewrite-the-past clamp `tick.max(clock + 1)` (`lib.rs:~5077`) —
silently — and the node's prograde/normal/radial components, computed in
`burn_frame` at the *designed* burn longitude, are resolved against the
clamped tick's frame: a different orbit point, an arbitrary inertial Δv, a
garbage ejection burning the full Δv.

**Evidence.** `node.tick <= plan.departure_tick` is the only bound asserted
(tests + docs); nothing bounds it below the caller's clock, and the
honesty-limit documentation lists the instantaneous-egress and
finite-burn limits but not the stale-tick trap.

**Impact.** Silent wrong-direction ejection whenever a caller plans a
departure closer than ~1 parking period — plausible for a low-alt parking
orbit (~30–90 min period) with a nearby window.

**Suggestion.** Add an `earliest_tick` (caller's `now`) parameter and refuse
`Recoverable` when `burn_tick ≤ earliest_tick`, or return the required lead
time so callers can enforce it. At minimum document that `node.tick` must be
checked against the sim clock before `SetManeuver`.

---

### [LOW] Finding 3: degenerate-frame checks test exact `h ≤ 0` while docs claim `r×v ≈ 0`

**File:** `crates/csp-sim/src/maneuver.rs:149-157`;
`crates/csp-sim/src/orbit.rs:671-675`

**Problem.** `burn_frame` rejects only `hn <= 0.0` (exact zero/non-finite);
`cartesian_to_elements` likewise `h <= 0.0`. Both docstrings describe the
rejection domain as `r × v ≈ 0`. A *near*-radial trajectory (tiny positive
`h`, e.g. `r×v` at rounding-noise scale) sails through: `burn_frame` returns
a normal direction dominated by rounding error, so a `dv_normal` node
component resolves along an essentially arbitrary axis;
`cartesian_to_elements` produces degenerate-but-finite parabolic elements
(`e ≈ 1` → `a := −p` with `p ≈ 0`) instead of `Recoverable`. Consistent
convention, but dishonest vs the documented `≈`.

**Impact.** Silent wrong-direction node frames / garbage elements in the
near-rectilinear corner. Rare in gameplay; exact-radial is caught.

**Suggestion.** Either a relative gate (`hn < rtol·rn·vn`) or tighten the
docs to "exactly zero". If a relative gate is added, apply it in both places
so the frame convention stays uniform.

---

### [LOW] Finding 4: `brent_root`'s machine term degrades the unconditional "≤ 1 tick" bound at extreme ticks

**File:** `crates/csp-sim/src/orbit.rs:1402-1404`

**Problem.** `tol1 = 2·f64::EPSILON·|b| + 0.5` is in raw tick units; the
bracket exits when `|c−b| ≤ 2·tol1 ≈ 1 + 4·eps·|b|`. Verified numerically:
at `|b| = 4e14` the bound is already ~1.36 ticks; at `1e16` it is ~10 ticks.
The module docs and `SoiEvent` doc claim "within 1 tick" unconditionally.

**Impact.** Practically unreachable (4e14 ticks ≈ 200k sim-years at 64 Hz),
but the contract is unconditional and the fix is trivial.

**Suggestion.** Run the refinement in offset units (`x − a_ticks`) so the
machine term stays sub-tick, or restate the bound as `≤ 1 + 4·eps·|t|` ticks.

---

### [LOW] Finding 5: `lambert_multi` silently swallows `lambert_velocities` failures that `lambert` propagates

**File:** `crates/csp-sim/src/transfer.rs:561-567` vs `526-532`

**Problem.** `for z in multi_rev_z(...) { if let Ok(sol) =
lambert_velocities(...) && lands_on(...) }` — a solved `z` that fails
velocity recovery (degenerate `g`, non-finite `v1`/`v2`) is dropped
silently, shrinking the result set. `lambert` (zero-rev) propagates the same
condition as `Err(Recoverable("lambert: degenerate g coefficient"))`. No
wrong answer is returned, but a solver defect in the multi-rev path becomes
invisible — the honest-answer discipline elsewhere in the module argues for
surfacing it.

**Suggestion.** Propagate the error, or at least assert/count dropped
candidates so a silent zero-result is distinguishable from "branch had no
roots".

---

### [LOW] Finding 6: NaN `soi_m` silently produces "no events" (and can burn the whole sample budget)

**File:** `crates/csp-sim/src/orbit.rs:772-773`, `1260-1278`, `1311-1324`

**Problem.** A `soi_m = NaN` candidate: `stride_ticks` computes
`NaN.ceil().max(1.0) = 1` (Rust `f64::max` discards the NaN), and
`signed_distance` returns `sep − NaN = NaN` — every sign comparison is false
→ "no crossing" on a 1-tick stride grid. On short horizons this silently
reports `Ok(None)` (transitions disabled for that boundary, no error); on
long horizons it burns `SOI_SCAN_MAX_SAMPLES` at stride 1 → `Recoverable`
via the budget. Inconsistent with the deliberately-tested NaN-angle
handling, which errors loudly (`nan_angle_orbit_state_fails_next_soi_event`).

**Suggestion.** Validate `r_soi.is_finite()` at candidate enumeration (or in
`stride_ticks`/`signed_distance`) so a corrupt body record errors once,
loudly — matching the NaN-angle precedent.

---

## Checked and clear (no findings)

- **Epoch-anchoring / partition independence:** `state_at` is a pure
  `(elements, epoch, t)` closed form; `tick_delta_seconds` differences the
  integer lattice in `i128`; no `advance`-style re-stamp exists anywhere.
  The `M = M0 + n·dt` path re-derives from the fixed epoch every call —
  ×1 vs ×10⁵ warp cannot drift.
- **Solver edge cases:** parabolic `a := −p` encoding is classified
  identically in `periapsis_state`, `wrap_mean_anomaly`,
  `cartesian_to_elements`, `speed_bound`, `mean_motion`, and
  `encounter_approach` (shared `E_PARABOLIC_TOL`); the `±inf` residual and
  NaN paths in `solve_universal` both route to bracket/bisection or honest
  `Recoverable`; period-reduction skip for unrepresentable `a³` is correct;
  retrograde-equatorial ω via `q̂ = ŵ×n̂` is correct (the deleted `atan2(e_y,
  e_x)` shortcut's `2ω` bug is documented and covered).
- **Lambert validation:** `z_solved` + `lands_on` (independent propagation
  check) is the right belt-and-suspenders; multi-rev golden-section +
  bracket structure is sound; extension plateaus carry only sign, and the
  ≤1-pocket geometry keeps root coverage complete.
- **SOI stale-clock handling:** the scan is time-pure; `schedule_next_soi`
  clamps `now ≥ clock` and `resolve_soi_residence` covers the `(clock,
  epoch]` window for future-stamped conics — the two mechanisms compose
  correctly.
- **Burn executor bookkeeping:** `full = f_max·PHYSICS_DT/m` is literally
  the integrator's `throttle·F/m·dt`; the final-tick throttle ratio delivers
  the exact remainder; `drive_node_burn`'s stalled-burn path (no thrust/mass)
  is honest. `DQuat::from_axis_angle` normalizes internally, so
  `align_rotation`'s non-unit cross-product axis is fine.
- **Determinism:** libm-only transcendentals, fixed iteration caps,
  `BTreeMap` ordering, no `HashMap`/randomness/`f64::sqrt` intrinsics, no
  unwraps/panics in shipping code. `f64::mul_add` is correctly-rounded either
  way — deterministic.
- **`departure_node` documented limits:** instantaneous-egress and
  up-to-one-period-early firing are stated plainly; the required correction
  loop is flagged as non-optional (the trap in Finding 2 is the part *not*
  documented).
