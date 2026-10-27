# Review C06 — Orbit + maneuver + bubble (`csp-sim/src/{orbit,maneuver,bubble}.rs`)

Scope per map §4/C06. All findings verified against the current working tree (HEAD 7dd4b86 + uncommitted);
supporting evidence in `csp-sim/src/lib.rs` read only where my files' contracts are consumed (executor,
auto-rails, SOI apply). Solver behavior verified empirically (temporary probe tests, reverted):
round-trip sweep e ∈ {0.95…0.99999, 1±5e-10, 1.001…3} × dt ∈ [1e-1, 1e9] s on a Terrin re-entry conic
(r_p = 640 km) stays inside the §2.8 1e-6 budget — worst observed 8.5e-7 at e = 1+5e-10, dt = 1e8 s
(beyond the PRD's open-orbit budget horizon of 1e7 s). The numeric core is solid; findings are
contract/coverage-level, not math-level.

### [HIGH] Finding 1: §2.6.6 item 4's "`h` is reported in the debug overlay" is coded but not wired
**File**: crates/csp-sim/src/orbit.rs:1293 (`scan_strides`), sole consumer crates/csp-sim/src/lib.rs:14274 (a test)
**Problem**: PLAN §2.6.6 item 4 (PLAN.md:585-587) makes the scan stride the *honesty surface* of the
SOI miss guarantee: "a residence shorter than `h` may be missed — `h` is reported in the debug overlay."
`PatchedConics::scan_strides` exists, is documented as "the number to show", and is shared with the
scan's own stride computation — but no production code path calls it. There is no debug overlay in
csp-app/csp-view/csp-ui, and the bugreport bundle does not carry it.
**Evidence**: `rg scan_strides crates/` → orbit.rs (definition + tests) and lib.rs:14274 (`scan_strides_reports_the_scans_own_stride`-adjacent test) only.
**Impact**: The §2.6.6 guarantee ("never misses a residence longer than h") is unobservable. A player
whose Rask encounter silently fails to appear (sub-h grazing residence, orbit.rs's own
`soi_graze_residence_below_h_is_missed` test proves this happens by design) has no way to see why,
and a bugreport cannot include the stride. Per the owner's rule, coded-but-not-wired = HIGH.
**Suggestion**: One row in the map's vessel-info panel (or the bugreport snapshot): per-candidate
`body → stride_ticks` for the focused vessel, straight from `scan_strides`.

### [MEDIUM] Finding 2: Executor burn starts AT the node tick — no ignition lead, so `predict_maneuver` systematically over-promises for finite burns
**File**: crates/csp-sim/src/lib.rs:6698 (event scheduled at `node.tick`), :6787-6796 (armed/evaluated at `node.tick`), :9230-9261 (`drive_node_burn`: full throttle until countdown ends); prediction crates/csp-sim/src/maneuver.rs:201-222
**Problem**: The map's predicted post-burn conic (`predict_maneuver`, consumed by csp-view/src/map.rs:1701/1963) applies the whole Δv impulsively at `node.tick`. The executor materializes at `node.tick` and burns forward from there, so the effective impulse sits at `node.tick + T_burn/2`. Nothing computes a half-burn lead (verified: no "lead"/"ignition" logic in map.rs/csp-ui).
**Evidence**: For a low-TWR ejection (e.g. 300 m/s²-weak upper stage, 1000 m/s over 300 s in LEO,
period ~5.8 ks) the effective impulse trails the node by ~150 s ≈ 9° of arc — the achieved conic
misses the predicted encounter by a first-order, burn-length-proportional amount. The in-repo
executor test (lib.rs:17158) only pins 1e-3 agreement on a ~2 s burn, where the bias is invisible.
**Impact**: Encounter/transfer planning through nodes degrades exactly when burns are long (the
interesting missions). Players must correct with additional nodes without being told the prediction
is biased. maneuver.rs:196-197 documents "finite-burn accuracy" but the start-at-node choice doubles
the mismatch vs the standard KSP half-before convention, for zero savings.
**Suggestion**: At `SetManeuver` (or at arm time, when mass/thrust are exactly known), schedule the
`ManeuverNodeStart` at `node.tick − ceil(dv·m/(2·F_net))` ticks; keep `node.tick` as the impulse
reference `predict_maneuver` already uses.

### [MEDIUM] Finding 3: No thrust of any kind off the construction-time dominant body — moon missions are ballistic flybys (no orbit insertion, no return burn, no landing)
**File**: crates/csp-sim/src/maneuver.rs:38-43 (documented v1 scope), gates at crates/csp-sim/src/lib.rs:6652-6656 (`SetManeuver`), :6779-6783 (node start), :6531-6535 (`ToBubble`); player-facing refusal csp-view/src/lib.rs:2646
**Problem**: All three paths require `orbit.parent == physics.dominant`, and `dominant` is fixed for
the world's life (set at construction, no switch command exists). Inside Rask's SOI a vessel can
never become physical, never schedule a node, never latch. The moon (in v1 scope: PLAN.md:1561-1562
"Minimum shippable v1 = … M5(reduced: home + moon only) … Home planet + moon") is therefore
flyby/orbit-on-rails only: no braking burn into Rask orbit, no return node from Rask SOI, no landing.
**Evidence**: PLAN.md:1501's M5 as-shipped note sanctions the *landing* deferral ("landing needs a
`dominant=duva` bubble world — tracked for post-v1") and the `duva_reachable_in_sim_end_to_end` test
(lib.rs:18269) demonstrates the sanctioned bar: node + burn at home, rails to the target. But the
note defers "landable" only; the inability to *thrust at all* while parent ≠ dominant (orbit
insertion, powered return) is beyond the note's deferral and is not on the §6.4 cut list.
**Impact**: A Rask mission must be planned as a pure ballistic free-return from Terrin orbit; any
arrival-speed mismatch cannot be corrected at the moon. Playable but narrow; the player discovers it
via the "VESSEL NOT RECOVERABLE — NOT ON RAILS HERE" notice after arriving.
**Suggestion**: At minimum, document the flyby-only moon scope in the map/planner copy (where the
Rask transfer is offered); the real fix (dominant-switch bubble world) is already tracked post-v1 —
consider also allowing `SetManeuver` to arm a node while parent ≠ dominant if the node point
evaluates in the parent's own frame, deferring the executor constraint to the (future) re-centered
bubble, or explicitly extending the M5 as-shipped note to cover "no thrust off-dominant".

### [LOW] Finding 4: §2.8 item 4's "log the elements" on solver non-convergence never happens — and orbit.rs's own doc claims it does
**File**: crates/csp-sim/src/orbit.rs:48-52 (doc: "the elements are logged at the call site"), actual caller crates/csp-sim/src/lib.rs:7745-7756 (`advance_orbits`)
**Problem**: PLAN §2.8 item 4 (PLAN.md:664-665): "On non-convergence: return `SimError::Recoverable`,
log the elements, and fall back to the previous valid state." The fallback half is structurally
sound (epoch-anchored state never written on Err), but every caller pushes only the `&'static str`
error onto `degraded`; no call site logs or attaches the failing elements, so the doc's claim is
aspirational.
**Impact**: A production solver failure (the warp path's worst nightmare) yields a bare
"kepler solve: iteration cap (64) exceeded" with no reproducible inputs — exactly the diagnostic
§2.8 item 4 was written to guarantee.
**Suggestion**: When `advance_orbits` catches a non-Fatal `state_at` error, format the vessel id +
elements into the notification ring / log channel the report already carries (one `format!` at one
call site; no error-type change needed).

### [LOW] Finding 5: `align_rotation`'s near-aligned shortcut leaves up to 1.4e-3 rad of pointing slop in the executor
**File**: crates/csp-sim/src/maneuver.rs:259-261
**Problem**: `cos > 0.999_999 ⇒ IDENTITY` skips the exact rotation whenever the net thrust axis is
within ~1.4e-3 rad of `burn_dir`, capping the executor's attitude error at 0.08° (delivered Δv
direction error up to ~0.14% transverse — ~1.4 m/s sideways on a 1 km/s burn). The exact branch is
~1e-15; the shortcut saves nothing measurable at one call per tick.
**Evidence**: `drive_node_burn` (lib.rs:9251) sets `rot = align_rotation(f̂, burn_dir)` wholesale each
tick, so the slop is bounded but systematic whenever the pair lands in the shortcut band.
**Impact**: Small but avoidable Δv-direction error in the one mechanism whose entire job is exact
direction; also inherited by `csp-bench`'s scripted controller via the same helper.
**Suggestion**: Tighten the identity threshold to `cos > 1 − 1e-12` (or always take the axis-angle
path; the anti-parallel case still needs its branch).

## PRD coverage table (§2.8 + §2.6.6 + bubble §2.3/§2.6.4)

| Requirement | Status | Evidence |
|---|---|---|
| §2.8 `OrbitState`/`OrbitModel`/`PatchedConics` | **WIRED** (trait's `advance` deliberately absent — documented §2.6.1 partition-independence rationale at orbit.rs:275-284; better than spec) | orbit.rs:218-299, 839-1075 |
| §2.8 universal solver items 1-3 (period reduction, Stumpff series, Laguerre–Conway n=5 + bracketing, cap 64) | **WIRED** — implemented literally | orbit.rs:348-533 |
| §2.8 item 4 non-convergence → `Recoverable` + previous-valid fallback | **WIRED** except "log the elements" (Finding 4) | orbit.rs:455-533; lib.rs:7745-7756 |
| §2.8 round-trip budget (1e-6 pos+vel, all conic classes) + e×Δt property test | **WIRED** — test grid per spec; empirical probe beyond the grid stays in budget (worst 8.5e-7) | orbit.rs:1598-1657; probe (session log) |
| §2.6.6 items 1-3, 5 (stride formula, bracketing, Brent ≤1 tick cap 64, graze detect/non-detect tests) | **WIRED** — plus a sample-budget back-pressure extension consistent with §2.6.5 | orbit.rs:1183-1508; tests 1853-1925, 2415-2446 |
| §2.6.6 item 4 "h reported in the debug overlay" | **DEGRADED — coded, unwired** (Finding 1) | orbit.rs:1293 |
| §2.8/M5 maneuver nodes + executor | **WIRED at the dominant body; DEGRADED elsewhere** (Finding 3; landing deferral sanctioned by PLAN.md:1501 note, thrust-gap flagged) | maneuver.rs (all); lib.rs:6597-6837, 9189-9261 |
| Executor burn timing vs impulsive prediction | **PARTIAL** — no ignition lead (Finding 2) | lib.rs:6698/9230 |
| §2.3 bubble radius 20 km + 10% hysteresis (18 km rejoin) | **REINTERPRETED** as an altitude rail floor `radius + max(atm_top, 20 km)` (bubble.rs:413-418; gate lib.rs:4819-4827). No oscillation risk: the auto-rails edge is one-directional and periapsis-gated; the conic, once railed with `r_p > floor`, cannot dip back. Acceptable reading; noting the deviation. | bubble.rs:402-422 |
| §2.6.4 surface latch record `(lat, lon, alt, orientation)`, exact unlatch, no drift | **WIRED** — capture/restore share the ω·t-seconds phase with the backend terrain (`set_sim_time`→`seconds()`, simple.rs:1322-1324); CoM-slot velocity convention documented and correct; `surface_upright` verified orthonormal (+Z→up, +Y→north, +X→east) | bubble.rs:231-394 |
| State carryover bubble↔rails↔latched | **WIRED by documented convention** — rails/latched carry no attitude; materialization takes weld-default rot/ang_vel and the executor owns attitude from tick 1 (maneuver.rs:52-56, lib.rs:6803-6810). Thermal record rides the vessel, frozen on rails (documented v1 approximation). | bubble.rs:44-58; maneuver.rs:13-31 |
| Determinism (libm-only, fixed caps, no hash-order iteration) | **WIRED** — no non-libm intrinsics in the three files (grep-verified); iteration caps 64; BTreeMap candidate order; `state_at` bitwise-pure (test). Solver outputs reach hashed state only through rebase/ToRails conversions. | all three files |

Playability chain verdict (mandate item 5): launch → auto-rails (75 km-circular canonical LEO rails:
floor = 70 km altitude periapsis) → de-orbit node → burn → `finish_burns` refuses the sub-floor
conic and leaves the vessel physical → re-entry with drag/heating → latch/recover: **works end to
end**. SOI rebase composition is exact algebra at a ≤1-tick-located crossing with wrong-side
suppression and an epoch-ahead residence walk; solver failure degrades without wedging (republished
`SoiRescan`). No stuck-state found.

## Top 3 issues
1. **Finding 1 (HIGH)** — §2.6.6 item 4's stride report is coded (`scan_strides`) with zero production consumers; the miss-guarantee is unobservable.
2. **Finding 2 (MEDIUM)** — Executor burns start at the node tick with no half-burn lead; `predict_maneuver`'s conic (the map's planning surface) is biased first-order in burn length.
3. **Finding 3 (MEDIUM)** — No thrust of any kind off the dominant body: moon missions are ballistic flybys (no orbit insertion/return burn); landing deferral is sanctioned, the thrust gap is not.
