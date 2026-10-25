# Ruthless Review — Cluster 4b: crates/csp-sim/src/lib.rs lines 10,743–21,484

Scope note: this range is entirely inside the `#[cfg(test)] mod tests` block that opens at line 10,459 — it is the sim's test suite (command drain, warp equivalence, SOI, physics bubble, thermal, breakup, docking, transfers, science, reliability, scripts, leases, fatal-rollback), not production code. Findings are therefore test-precision defects: crafted-gate cases that hit the wrong gate, tripwire assertions that cannot fail, and dead scaffolding. Every finding was verified against the production gates in the file's first half (grep cross-references cited below).

### [SEVERITY: MEDIUM] Finding 1: Crafted colony-fraction cases never reach the fraction-range restore gate they appear to test
**File**: crates/csp-sim/src/lib.rs:12202
**Problem**: `restore_rejects_crafted_snapshot_arenas` includes two `module_fractions` corruption cases. The fixture colony (`crafted_colony`, line 11,992) has `modules: Vec::new()` (line 12,001), so BOTH cases — `vec![2.0]` (out-of-range) and `vec![0.5]` (in-range) — have length 1 against 0 modules and are rejected by the length-parallelism gate at line 9,141 ("colony fractions/modules mismatch"), which runs BEFORE the range gate. The fraction-range half of the "incoherent colony solve" check (lines 9,149–9,151, `f.is_finite() && (0.0..=1.0).contains(f)`) is never exercised with matching lengths — the `vec![2.0]` case is a duplicate of `vec![0.5]`'s gate, not the range case it reads as. Verified: the only `module_fractions` test usages in the workspace are lines 12,202 and 12,210; the `t_eq_k = Some(NaN)` sibling case covers only the `||`-joined temperature half of the same gate.
**Evidence**:
```rust
// 12,201-12,202 (modules is empty per crafted_colony line 12,001)
|c: &mut Colony| c.t_eq_k = Some(f64::NAN),
|c: &mut Colony| c.module_fractions = vec![2.0],
...
// 12,210 — the "length" case; identical gate to 12,202
|c: &mut Colony| c.module_fractions = vec![0.5],
```
```rust
// production order, lines 9,141 then 9,149 — length fires first
if c.module_fractions.len() != c.modules.len() { ... }
if c.module_fractions.iter().any(|f| !(f.is_finite() && (0.0..=1.0).contains(f))) || ...
```
**Impact**: Deleting or weakening the fraction-range clause of `restore_colony_state` (e.g., dropping the `module_fractions.iter().any(...)` half of line 9,149) leaves the entire suite green. A crafted save carrying a 2.0 allocation fraction — state the solver can never produce — would then restore cleanly and diverge the hash lineage. In a suite whose stated purpose (doc at 12,132) is "every hashed payload a crafted snapshot could carry is refused", this is a hole in the fence that the case list pretends is closed.
**Suggestion**: Push one module into the crafted colony before the fraction cases (e.g., `c.modules.push(colonies::ColonyModule { def: Battery { capacity: 1.0 }, damaged: false })`), then: keep `vec![2.0]` (lengths now match → range gate fires), and change the length-mismatch case to `vec![]` on a one-module colony (or `vec![0.5, 0.5]`). Alternatively split into two fixtures so each case names its gate unambiguously.

### [SEVERITY: MEDIUM] Finding 2: "Ballistic plateau" velocity tripwires cannot fail — and the comment's arithmetic is off by 100x
**File**: crates/csp-sim/src/lib.rs:17515
**Problem**: In `dry_engine_produces_no_thrust`, the final "ballistic" assertion uses a `< 1.0 m/s` bound over a 64-tick (1 s) window, but a live 200 N burn on the ~500.5 kg craft adds only `200/500.5 * 1 s ≈ 0.4 m/s` — inside the bound. The comment claims "a live 200 N burn on a 500 kg craft would add ~40 m/s", which is wrong by a factor of 100 (40 m/s would require 100 s or 20 kN). The same pattern closes `jettisoned_tank_mid_burn_starves_engine` (line 17,796): 1000 N on the 500 kg post-jettison remnant is 2 m/s over the 1 s window against a `< 40.0` bound. In both tests the effective gates are the preceding `throttle == 0.0` / `rated_n == 0.0` record checks; the velocity assertions are dead as physics tripwires and would not catch a bug where the record reads zero while force is still integrated (exactly the failure class the "ballistic" language claims to guard).
**Evidence**:
```rust
// 17,512-17,519 — window is 64 ticks = 1 s; Δv from 200 N ≈ 0.4 m/s < 1.0
// Ballistic: speed can only change through gravity — over 1 s
// in a ~800 km orbit it shifts < 1 m/s; a live 200 N burn on a
// 500 kg craft would add ~40 m/s.
assert!(
    (b.vel.norm() - speed1).abs() < 1.0,
    "velocity must stop growing once dry ({speed1} → {})",
    b.vel.norm()
);
```
```rust
// 17,796-17,799 — 1000 N / 500 kg = 2 m/s over 1 s < 40.0
assert!(
    (speed - speed_burning).abs() < 40.0,
    "post-jettison the vessel only coasts ({speed_burning} → {speed})"
);
```
**Impact**: False confidence: the assertions document a physics-level safety net that mathematically cannot fire, and the 100x comment error will mislead the next maintainer into "tightening" nothing (or trusting the margin). A regression that keeps thrust force applied while zeroing the reported throttle/rating passes both tests.
**Suggestion**: Size the window or thrust so a live burn is decidable: e.g., extend the coast window to 1024 ticks (16 s → 6.4 m/s for the dry-engine case) and tighten the bound to ~1.0 m/s, or shrink the bound to `< 0.05 m/s` for the current 1 s window (ballistic gravity drift is ~0.007 m/s at that radius). Fix the comment to the real number (0.4 m/s), and apply the same to the jettison test (2 m/s actual).

### [SEVERITY: LOW] Finding 3: Dead `tracked` parameter in the §2.6.1 milestone equivalence test's `build` closure
**File**: crates/csp-sim/src/lib.rs:11562
**Problem**: `warp_x1_vs_x1e5_equivalence_10_sim_days` defines `let build = |tracked: bool| {...}` whose `if tracked` branch (lines 11,530–11,539) enqueues `SetVesselTracked`. Both invocations are `build(true)` (lines 11,562–11,563); the `false` arm is unreachable dead scaffolding.
**Evidence**:
```rust
let mut a = build(true);
let mut b = build(true);
```
**Impact**: Mild: the parameter implies an untracked equivalence variant exists; a reader auditing the milestone test's coverage matrix ("one tracked ⇒ Clamping") cannot tell whether the untracked shape was checked or dropped. No assertion is weakened — both runs legitimately track vessel 1.
**Suggestion**: Either drop the parameter (inline `true`) or, better, keep it and add the untracked twin comparison (`build(false)` vs `build(false)`) since the closure makes that a two-line addition and the untracked Silent-event path through a 10-day warp split is otherwise only covered by the short `soi_transitions_rebase_vessel_and_reschedule` test.

### [SEVERITY: LOW] Finding 4: Dead `d0` binding and self-arguing comment in `lab_scarce_regime_exhausts`
**File**: crates/csp-sim/src/lib.rs:19113
**Problem**: The test fetches `d0` (line 19,101), spends a five-line comment debating whether `D0 < T` (19,101–19,106), then discards the value with `let _ = d0;` while hand-trimming the pool to 2.0 units instead.
**Evidence**:
```rust
let d0 = w.vessels()[&VesselId(0)].science.units_at(&key); // 6.32 > T=5…
...
let _ = d0;
```
**Impact**: Noise in a suite whose comments elsewhere are load-bearing regression documentation; the musing comment ("D0 < T_eff? No — T is the lab's throughput...") reads like unresolved design doubt rather than spec.
**Suggestion**: Delete the `d0` binding, the `let _ = d0;`, and compress the comment to the operative fact: the scarce regime needs `D0 < T = 5`, so the pool is trimmed to 2.0 directly.

---

## Verdict

The back half is a single 10,700-line `#[cfg(test)]` module — all production code lives in the first half, so no shipping-path bug can live in this range. As a test suite it is well above professional median: exact assertions (bitwise `to_bits`/hash equality), fixtures that self-verify non-vacuity, full determinism discipline (libm-only math, BTree containers, no wall clock, fixed seeds), and named regression tests documenting pre-fix failure modes. The two MEDIUM findings are precision defects in that discipline — one crafted-gate case that hits the wrong restore gate (leaving the fraction-range clause genuinely untested) and physics tripwires whose bounds cannot fire, one with a 100x comment error. Nothing here supports "far below professional"; the risk is concentrated in the two uncovered/mis-covered clauses, both cheap to fix.
