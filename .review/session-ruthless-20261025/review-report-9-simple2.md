# Review Report 9 — simple.rs (second half, lines 1775–3548)

Reviewer: ruthless half-file pass. Scope: lines 1775–3548 ONLY. Static review (no cargo run).
Focus: integrator/energy, dt handling, NaN/inf, force/torque accumulation, inertia, determinism, placeholder math.

---
### [SEVERITY: HIGH] Finding 1: Baumgarte position pass caps linear travel but leaves the angular pseudo-impulse unbounded — orientation corruption for offset-mass craft
**File**: crates/csp-phys/src/simple.rs:1827-1857 (ground branch), 1876-1901 (pair branch, same pattern)

**Problem**: `MAX_TICK_POS_CORRECTION` bounds only the linear CoM excursion per body per tick. The angular half of the same pseudo-impulse — `dtheta_body = c.inv_i.mul_vec(r_body.cross(n_body * sigma))` (line 1847, and `pair_position_correction` line 961 via line 1896/1900) — has no bound at all, and it is then integrated with the first-order Euler quaternion step `q += 0.5·(dθ⊗q)` (lines 1849-1857), which is only valid for small `‖dθ‖`.

**Evidence**: The cap scales σ so that `sigma / mass_kg <= remaining` (line 1827-1832), i.e. `Σσ ≤ 0.2·m` per tick. But `kn = 1/K ≤ m` (K ≥ inv_m by construction, `Constraint::new`'s `effective`), so `σ ≤ β·depth·m` even *uncapped* whenever depth < 1 m, and `‖dθ‖ ≤ σ·‖r‖·λmax(I⁻¹)` can still be huge whenever `m·λmax(I⁻¹)·‖r‖ ≫ 1` — i.e. mass concentrated at the CoM with the contacting vertex on light, far-reaching geometry. Concrete: 20,000 kg pod at the CoM + 50 kg boom reaching 5 m, contact vertex at the boom tip penetrating 5 cm: `kn ≈ m = 20,000`, `σ = 0.2·0.05·20,000 = 2,000`, `σ/m = 0.01 m` — far under the cap, so the cap never engages — but `I ≈ 50·5² = 1,250`, giving `‖dθ‖ = σ·r/I = 2,000·5/1,250 = 0.8 rad` per vertex per iteration (8 iterations). Buried deeper than 1 m (crash landing, spawn inside terrain), σ saturates at `0.2·m` and `‖dθ‖` reaches ~16 rad for the same craft: the Euler step plus `normalize()` yields an effectively arbitrary orientation.

**Impact**: A probe/truss-style vessel (heavy core, light extremities) that touches down or crashes with centimetres of bite snaps its orientation by tenths of a radian in a single tick, and a buried one thrashes to pseudo-random orientations every tick while it climbs out at 0.2 m/tick — player-visible wreck jitter and bogus resting attitude. Uniform-inertia craft (rockets, cubes) are unaffected (`dθ ≈ β·depth·r/k²` is tiny), which is why the cube-based settle tests all pass.

**Suggestion**: Bound the angular excursion with the same budget discipline as the linear one: clamp σ per application so `‖I⁻¹(r×n·σ)‖ ≤ θ_max` (θ_max ≈ 0.05 rad), and/or add an angular term (`spent_ang += ‖dθ‖·‖r‖`) to the `spent` budget. Independently, replace the first-order `q += 0.5·(dθ⊗q)` in both `solve_positions` and `pair_position_correction` with an exact axis-angle rotation so any residual large increment is at least a valid rotation of the correct magnitude.

### [SEVERITY: MEDIUM] Finding 2: Soft-capture "closing-speed cap" is per-component — the documented `max_correction` speed cap is false on diagonal separations, and the test only exercises the axis-aligned case
**File**: crates/csp-phys/src/simple.rs:3383-3415 (`soft_capture_caps_closing_speed`); root cause in `JointConstraint::new` target construction (~line 894-903, first half)

**Problem**: The test's contract statement — "a far-separated pair closes at ≤ max_correction, never faster" (lines 3384-3385) — does not hold for the code it claims to verify. The joint target is built by clamping each *component* of `−pull_per_s·c` to `±max_correction_mps`, so a diagonal anchor separation `ĉ = (1,1,1)/√3` admits a relative velocity of `(−1,−1,−1)` m/s: a closing *speed* of √3·max_correction ≈ 1.73 m/s, 73% over the authored cap.

**Evidence**: The test constructs the two bodies at `(…, 0.0, 0.0)` and `(…, 8.0, 0.0)` (lines 3390-3391) — separation purely along y, the one axis where per-component and magnitude clamps coincide — so `assert!(closing <= 1.0 + 1e-9)` (line 3405) cannot detect the diagonal overshoot it documents. The assertion would still pass if the clamp were a magnitude clamp (correct) or per-component (broken-on-diagonals): the test does not discriminate.

**Impact**: Docking/capture approaches along a diagonal reel in at up to 1.7× the authored closing speed — larger capture impulse than the soft-capture design promises, and a doc invariant that is silently false.

**Suggestion**: Clamp the target vector's magnitude instead of its components (deterministic: `let t = -pull_per_s·c; let tl = t.norm(); if tl > max_correction_mps { t *= max_correction_mps / tl }`), or, if the per-component form is intended, fix the test's doc claim and add a diagonal-separation case asserting the true bound.

---
Out-of-scope notes (for the first-half owner, max 3):
1. Module doc (~line 1089) says contacts are "Cleared for latched bodies", but `generate_pair_contacts` pushes pair records onto a *latched* body's `contacts` whenever only one side is latched (not both) — a latched vessel can carry non-empty contact records on later ticks.
2. `JointConstraint::new` per-component target clamp is the root cause of Finding 2 (code lives in the first half; the false contract is asserted by the test in this half).
3. The pair position pass re-measures depth against the single generation facet plane only (line 1868) — a vertex that slides tangentially off that facet's polygon but stays beyond its plane keeps receiving correction; self-limiting but worth a doc note.

## Verdict (second half, lines 1775-3548)

The solver core in this half is unusually solid: dimensionally consistent pseudo-impulse position solve (σ = β·depth·kn against effective mass K⁻¹), a settle gate that closes the mid-slide latch hole, guarded divisions, NaN-free aero path, and genuinely meaningful determinism/latch/stacking tests. The two real defects are the unbounded angular half of the Baumgarte correction (Finding 1, orientation corruption on offset-mass craft — the cap's doc claim "bounds the total correction" is only true of translation) and the diagonal soft-capture cap the test doc overstates (Finding 2). REQUEST CHANGES on Finding 1; Finding 2 is a small fix or an honest doc/test correction.
