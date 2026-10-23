# Cluster C13 Review — `crates/csp-phys/src/{simple,aero,thermal}.rs`

## Verdict

The determinism posture is genuinely clean: every iteration that feeds the tick
is fixed-order over `Vec`/`BTreeMap` (no `HashMap` anywhere), `libm`-only
transcendentals, cold-started PGS (no serialized solver state), fixed
`SOLVER_ITERATIONS = 8` for both passes, and the joint list is re-sorted by
`(body_a, body_b, anchor bits)` so caller enumeration order cannot leak in
(`simple.rs:915-926`). Bitwise-determinism tests exist for single-body,
two-vessel, and jointed runs. The prior "double-counted shielding" bug is
verifiably fixed: `area_m2` stores *total* projected area (`unblocked +
blocked`, `aero.rs:1227`), `A_eff = area·(1 − shielded)` recovers exactly the
unblocked share (`aero.rs:754`), and `thermal.rs:320` uses the identical
formula. Manifold construction is consistent between predicate and generation
(`any_vertex_penetrates`/`generate_pair_contacts` share `point_penetration` +
`vertex_margin`), impulse sign conventions check out on both directions, the
pair/joint write-backs conserve linear momentum by construction, and the CoM-
frame discipline (integrate CoM → rotate → re-derive `pos`) is applied
uniformly in `integrate_free`, ground position correction, and
`pair_position_correction`. Joint rows are correctly bilateral three-row
constraints with per-row GS re-evaluation, and the thermal merge-join /
pre-update-temperature conduction snapshot are correct.

Findings below are real but mostly robustness/modeled-boundary gaps — no
determinism break and no force-sign error found.

### [MEDIUM] Finding 1: `SoftCapture` reel-in rate has no stability bound — `pull_per_s·dt ≥ 2` produces a permanent rattle, and nothing validates against it

**File:** `crates/csp-phys/src/simple.rs:868-899` (target), `simple.rs:1712-1740` (row solve); validation side: `crates/csp-sim/src/joints.rs:218-221`

**Problem:** The joint's velocity target is `target = −pull·c` per axis
(clamped to `±max_correction`). The PGS solve drives the anchors' relative
point velocity to that target each tick, so anchor separation obeys the
explicit map `c_{n+1} ≈ c_n·(1 − pull·dt)`. That map is stable only for
`pull·dt < 2` — i.e. `pull_per_s < 128` at the pinned `dt = 1/64` — and only
non-oscillatory below `pull·dt < 1` (`pull_per_s < 64`). For
`pull_per_s ∈ [64, 128)` the reel-in converges with alternating overshoot; for
`pull_per_s ≥ 128` the unsaturated band diverges geometrically until the
component clamp engages, at which point the joint sits in a bounded limit
cycle — `|c|` oscillating around `≈ max_correction/pull` forever, injecting a
full-strength bilateral impulse every tick.

**Evidence:** `JointConstraint::new` computes
`(-pull_per_s * c.x).clamp(±max_correction_mps)` with no bound on `pull_per_s`
itself (`simple.rs:877-881`). `solve_joint_rows` applies `dl = (target[row] −
vrel·dir)·k` unclamped (`simple.rs:1722`), so `vrel` converges to `target`
inside the 8 iterations. On the sim side, `JointDef` comes from
`DockingConfig::joint_kind()` (`joints.rs:231-236`), whose `is_valid` accepts
any finite `soft_pull_per_s >= 0.0` (`joints.rs:218-219`) — a `balance.ron`
value of e.g. 200 passes validation and produces a joint that never converges:
the anchors rattle at ~`cap·dt` amplitude, the capture never reads "settled",
the joint dissolves on window expiry, and docking silently fails.

**Impact:** A tunable authored parameter can flip a joint's explicit scheme
unstable. The shipped default (`8.0`, `pull·dt = 0.125`) is safe, so this is
latent — it fires when balance is retuned, and the failure mode (a permanent
cm-scale buzz + failed dock) looks like a solver bug rather than bad data.

**Suggestion:** Enforce the stability bound where the parameter enters the
hashed config — e.g. `soft_pull_per_s <= ~0.5/dt` (32/s) in
`DockingConfig::is_valid` — or defensively clamp `pull_per_s` inside
`JointConstraint::new` to a documented stable range (`min(pull, 1/dt)`), which
keeps every value the layer accepts convergent.

### [MEDIUM] Finding 2: Thermal explicit Euler has no stability guard — pathological coupling or extreme temperatures diverge to ±inf and are serialized/hashed

**File:** `crates/csp-phys/src/thermal.rs:333-361` (conduction + integration)

**Problem:** The tick is raw explicit Euler: `new_t = t + dt·rates[i]/C` with
no clamp, sub-step, or finiteness check before `temps.insert(n.node, new_t)`
(`thermal.rs:356-357`). Two independent divergence paths exist:

- **Conduction:** for an edge `(a,b)`, the 2-node update has eigenvalue
  `1 − dt·G·(1/C_a + 1/C_b)`; it diverges (alternating growth) when
  `G·dt·(1/C_a + 1/C_b) > 2`, i.e. `G > 128·C_a·C_b/(C_a+C_b)` W/K.
  `weld` validates `heat_capacity > 0` and `conduction ≥ 0` *individually*
  (`weld.rs:426-437`) but never their ratio — a small-C part (e.g. `C = 10
  J/K`) joined to a high-k part (`k ~ 10⁴ W/K`) with a healthy shared-face
  fraction produces a divergent edge. The oscillation grows ~`|1−λdt|`ⁿ to
  ±inf within a few dozen ticks.
- **Radiation:** `T⁴` explicit overshoots zero when
  `dt·εσA·T³/C > 1` (`T ≳ ~2×10⁴ K` for typical authored values). Once `T < 0`,
  the `a4 − t⁴` term stays large-negative → monotone runaway to `−inf`.

`part_temps_k` is the vessel's *serialized* record (`thermal.rs:264-269`;
`csp-sim/src/lib.rs:3399-3406`) and enters `state_hash` via `h.f64` — ±inf or
NaN propagates silently into the determinism pipeline rather than failing
typed, and `t2*t2` on an infinite `t` yields `inf − inf = NaN` rates.

**Evidence:** module doc claims stability "because authored couplings satisfy
`G·dt/C ≪ 1` … at every temperature that survives a tick" (`thermal.rs:37-41`)
— an assumption, not an enforcement. Nothing anywhere checks `new_t` or
`rates[i]` for finiteness before writeback; lethality (`overheated`) is a
post-tick observation and doesn't bound the trajectory.

**Impact:** Reachability requires extreme authored data (or lethality-free
operation above ~10⁴ K), so it's config-dependent — but the failure is the
worst kind for this codebase: silent non-finite serialized state that hashes
fine and corrupts the oracle/hash-golden pipeline instead of producing a
`SimError`.

**Suggestion:** Cheap hardening that preserves determinism: (a) weld-side
bound — reject `joint_conductance` results where `G·dt·(1/C_a+1/C_b) ≥ 2`
(authored k/C pairs that can't be integrated at 64 Hz are bad data); and/or
(b) a per-tick `new_t.is_finite()` guard in `step` that clamps or skips the
writeback — a NaN is strictly worse than a clamped finite value. A
`max |ΔT|` sub-step would also work but changes results.

### [LOW] Finding 3: `ref_area_m2` sums *total* per-part projected area — occluded area is counted, so the craft silhouette is overestimated

**File:** `crates/csp-phys/src/aero.rs:1296-1300`

**Problem:** `ref_area` is `max over dirs` of `Σ_p area_m2[di]`, where
`area_m2` is *unblocked + occluded* by construction (`aero.rs:1227`). For
craft whose parts overlap along some direction (a payload behind a nose cone,
stacked tanks seen face-on), mutually occluded area is summed multiple times —
the "reference area" exceeds the craft's true projected silhouette. Since
`cd/cl/cm` knots divide by `A_ref` (`aero.rs:1332-1336`), all three
coefficients are systematically under-normalized for overlapped craft (the
discrepancy can be large for flat packed clusters).

**Evidence:** The true directional silhouette is `Σ_p area_m2[di]·(1 −
shielded[di])` (exactly the `A_eff` the force model uses); the code sums the
un-shielded total instead.

**Impact:** Diagnostic-only in v1 — the knot grid is a table artifact, and the
`cm_alpha` direct-vs-table test uses the *same* `ref_area` on both sides, so
it self-cancels. But any consumer reading `ref_area_m2`/the coefficient knots
as physical Cd/Cl/Cm gets biased values; if a later milestone uses `A_ref`
for an actual force computation the bias becomes live.

**Suggestion:** Either sum `area_m2[di]·(1 − shielded[di])` (the honest
craft silhouette), or document in the field comment that `ref_area_m2` is the
max *unoccluded-sum* convention, not a physical silhouette.

### [LOW] Finding 4: Zero-area-direction `cop` fallback is the vertex centroid (mislabelled "part CoM") and still enters barycentric interpolation

**File:** `crates/csp-phys/src/aero.rs:1216, 1234-1238`

**Problem:** When a direction has `a_dir == 0`, `cop[di]` falls back to
`verts.mean()` — the arithmetic mean of hull vertices, which the doc calls
"the part's body-frame CoM" (`aero.rs:652-654`). For a hull whose vertex
distribution isn't the mass distribution (asymmetric vertex sets), the vertex
centroid ≠ `part.com` — the comment is wrong, and the value is arbitrary.
More importantly, the fallback still participates in interpolation: at a
query `w` whose containing triangle mixes a zero-area vertex with real ones,
`cop = Σ w_i·cop_i` drags the application point toward the vertex centroid,
biasing the torque arm at table edges even though `A_eff > 0` overall.

**Evidence:** `part_com = verts.iter().fold(ZERO,+)/verts.len().max(1)`
(`aero.rs:1216`); `cop` interpolated unconditionally at `aero.rs:758` whenever
the blended `a_eff > 0` (`aero.rs:754-756`).

**Impact:** A bounded, deterministic CoP error near zero-area table vertices —
torque error of order `w_i·‖centroid − true cop‖·‖F‖`. No instability or
nondeterminism; a model-fidelity wart plus an incorrect comment.

**Suggestion:** Fall back to the nearest nonzero-area direction's `cop` (or
the part's true body-frame CoM via `transforms`), or weight `cop`
interpolation by per-direction unblocked area; fix the doc to say "vertex
centroid".

### [LOW] Finding 5: Vessel-vessel pair contacts reuse the *ground* friction coefficient `env.friction_mu`

**File:** `crates/csp-phys/src/simple.rs:1658` (pair friction limit), vs
`simple.rs:294` (`friction_mu` doc: "Coulomb μ for ground contact")

**Problem:** The pair manifold's Coulomb limit is `env.friction_mu·λ_n` —
the field is documented as ground-only. Hull-on-hull metal/composite contact
and rock/regolith are materially different contacts; a single coefficient is
now silently applied to both manifolds.

**Impact:** Deterministic and bounded — this is a modeling fidelity note, not
a correctness or determinism defect. If authored data later wants a different
vessel-vessel μ there is no channel for it.

**Suggestion:** Either document the deliberate sharing in `Environment`'s
doc, or add a separate pair-friction field when the data layer gains one.

### [LOW] Finding 6: Self-joints (`body_a == body_b`) are not rejected — they inject net torque with zero net linear impulse

**File:** `crates/csp-phys/src/simple.rs:910-934` (`generate_joint_constraints`)

**Problem:** The filter only bounds-checks handles. A `JointDef` with
`body_a == body_b` produces a constraint whose `+j`/`−j` linear impulses
cancel exactly but whose angular write-backs use different lever arms
(`r_a ≠ r_b` for distinct anchors) — a silent net angular velocity injection
from a degenerate input. The effective mass double-counts the body
(`inv_m_a + inv_m_b = 2/m`), so `dl` is also half the correct magnitude.

**Evidence:** `csp-sim` joints are canonically `vessel_a < vessel_b`
(`joints.rs:266-268`), so the sim can't emit this today — but `step`'s
contract says only "joints naming a handle outside `bodies` are skipped"
(`simple.rs:907-909`), leaving this reachable for any other caller.

**Impact:** None for current callers; a latent silent-spin bug for the API's
documented input domain.

**Suggestion:** Add `&& j.body_a != j.body_b` to the filter — one line, no
behavioural change for valid input.

### [LOW] Finding 7: A latched body with `throttle > 0` latch/unlatch cycles every `settle_ticks + 1` ticks — the backend never enforces the "latched ⇒ throttle 0" invariant it states

**File:** `crates/csp-phys/src/simple.rs:1963-1966` (latch), `1994-2003`
(throttle unlatch)

**Problem:** On latch, velocities zero and contacts clear — but `b.throttle`
is left untouched, even though the module doc cites §2.6.4's "a latched
vessel's throttle is zero". A resting body holding nonzero throttle (e.g. a
throttle command with no thrust sites, or sub-liftoff thrust) latches after
`settle_ticks`, then unlatches on the very next tick via the `throttle > 0`
check, resetting `settled_ticks` — a permanent 129-tick latch flicker: one
skipped integration tick per ~2 s cycle.

**Impact:** Deterministic and tiny (≈0.8% duty cycle of skipped integration),
self-correcting — arguably intended since a commanded throttle *should*
prevent latching. But the one-tick frozen pose is a real (if small)
discontinuity, and it contradicts the latch's own documented invariant; if
the World is expected to zero throttle on latch, that isn't wired here.

**Suggestion:** Either zero `b.throttle` at latch (the backend owns the flag,
so it can own the invariant), or exclude `throttle > 0` bodies from
*qualifying* in `update_settle` so the latch never engages — which also
removes the flicker.

### [LOW] Finding 8: `voxelize_craft` dims product can overflow `usize` for absurd-but-finite hull spans — debug panic / silent truncation in release

**File:** `crates/csp-phys/src/aero.rs:988-998`

**Problem:** `dims[i] = (ceil(span/edge) as usize).max(1)` — `f64→usize` `as`
saturates — and `dims[0]·dims[1]·dims[2] <= VOXEL_BUDGET` is checked with
plain `usize` multiplication. For a hull spanning ≳260 km/axis (finite, so it
passes weld), the product overflows: panic in debug builds (violating the
§2.13 no-panic posture) or a wrapped value in release that can spuriously
pass the budget check → `vec![EMPTY; wrapped]` allocates a wrong-sized grid →
silently wrong tables.

**Impact:** Reachable only with absurd geometry (a ~10⁶ m part) — no real
craft. But it's an unguarded overflow on a boundary that validates only
finiteness, exactly the class the fixed-caps rule is meant to close.

**Suggestion:** Compute the product in `u128` or checked/`saturating`
arithmetic so the budget test can never wrap; optionally bound `dims` early
(e.g. `span/edge` saturating to `VOXEL_BUDGET`).

## Notes (verified, no action)

- Contact ordering, impulse sign conventions, `+λn`/`-λn` split, latched-side
  static-collider handling, pair impulse write-back indices, and the
  `(body_id, contact_index)` ascending order are all internally consistent.
- `point_penetration`'s multi-facet boundary rule and the persistent-facet
  position re-measure (`solve_positions` pairs) use the same `d − n·p` depth
  convention as `hull::Plane`.
- `weights()` barycentric dual-basis math is correct (`e_i = d_j×d_k`,
  `w_i = (w·e_i)/det`) with deterministic first-match and a safe fallback.
- `update_settle`'s spinning-world residual gates are consistent with the
  integrator's own quaternion-update convention.
- `entry_face` axis-order sort is stable with lowest-index tie-break, matching
  its doc.
- `friction_basis`/`cross_section_area` reference-axis pick cannot produce a
  degenerate tangent for unit inputs (chosen axis is the least-aligned of
  X/Y ⇒ `|n·ref| ≤ 1/√2`).
- `skin_friction_cd`'s `Re < 10³ ⇒ 0` hard cut is a real Cf discontinuity but
  at `v ~ 1.5 cm/s` (ρ≈1.2, L≈1) — force magnitude is negligible there.
- `build_thermal_net`'s attach-axis/shared-face math in `weld.rs` matches the
  thermal module doc (canonical `(a<b)` edges, deterministic sort).
