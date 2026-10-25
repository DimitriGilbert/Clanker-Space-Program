# Ruthless Review 5: crates/csp-sim/src/transfer.rs

Session: 2026-10-25. STATIC REVIEW ONLY (no cargo run).
Assigned file: crates/csp-sim/src/transfer.rs (2,284 lines).


### [SEVERITY: MEDIUM] Finding 1: A collinear-endpoint cell kills the entire porkchop scan / plan
**File**: crates/csp-sim/src/transfer.rs:785 (also :789)
**Problem**: `leg_at` propagates per-cell Lambert errors, so one geometrically degenerate grid cell aborts the whole scan, contradicting its own documented cell-vs-error contract.
**Evidence**: `crates/csp-sim/src/transfer.rs:785`:
```rust
if let Some(sol) = lambert(r1, r2, tof_s, mu_p, way)? {
```
and `:789` `for sol in lambert_multi(r1, r2, tof_s, mu_p, way, revs)? {`. `lambert` returns `Err(SimError::Recoverable("lambert: collinear endpoints"))` from `lambert_geom` (transfer.rs:170-175) whenever the sampled endpoints sit within ~1.4e-7 rad of conjunction or opposition (Δν ≈ 0 or π). `leg_at`'s doc (transfer.rs:742-745) states: "`Ok(None)` when nothing solves (an infeasible cell, not an error)" — collinearity is a property of the sampled cell, not a broken input, yet the `?` turns it into a scan-wide `Err`. `lambert_multi`'s velocity-recovery `Err`s (transfer.rs:566, "infeasible solution point" / "degenerate g coefficient") escape the same way.
**Impact**: `porkchop_scan` and `plan_transfer` fail wholesale (Recoverable) instead of marking one cell `leg: None`. A same-plane body pair sweeps Δν through 0/π across a departure window; a sampled tick landing within ~3e-7 rad of alignment kills the query. At the `PORKCHOP_MAX_CELLS` ceiling (65,536 cells) the expected hit rate is ~0.6% per scan — rare, but deterministic: an affected route always fails, every time. Note the exact 180° Hohmann phasing is *excluded* by this error, so cells at the physically most interesting phase can never be evaluated (near-180° works only outside the 1.4e-7 rad sliver).
**Suggestion**: In `leg_at`, treat the geometry errors as cell infeasibility: either map `lambert`'s collinear/non-positive-tof `Err`s to "no leg for this cell" (match on the recoverable collinear case before `?`), or change `lambert`/`lambert_geom` to return `Ok(None)` for collinear endpoints since callers with valid mu/tof can only hit it from sampled geometry. Do the same for the `lambert_multi` velocity-recovery errors inside the scan loop.

### [SEVERITY: MEDIUM] Finding 2: `max_revs` is unvalidated — the "bounded work" contract is false
**File**: crates/csp-sim/src/transfer.rs:788 (query field :627, cap claim :813-815)
**Problem**: `PorkchopQuery::max_revs` is a caller-controlled `u32` with no upper bound, but `porkchop_scan`'s doc claims "Bounded work: the cell product is capped at [`PORKCHOP_MAX_CELLS`] and each cell costs a bounded number of Lambert solves (`2·(1 + max_revs)`)".
**Evidence**: `crates/csp-sim/src/transfer.rs:788-792`:
```rust
for revs in 1..=q.max_revs {
    for sol in lambert_multi(r1, r2, tof_s, mu_p, way, revs)? {
```
Every multi-rev call runs a 64-iteration golden-section branch-min search (`multi_rev_z`, transfer.rs:412-436, ~130 residual evals) plus up to two 64-iteration Brent solves and `lands_on` propagations — *even when the branch's TOF minimum is far above `tof`* (common for interplanetary cells, where N≥1 branch minima exceed the sampled durations). With `max_revs = 1000` and a modest 24×24 grid: 576 cells × 2 ways × 1000 revs × ~200 evaluations ≈ 2.3e8 Stumpff evaluations per query; `u32::MAX` makes it effectively unbounded.
**Impact**: A single planning query with a large `max_revs` hangs the calling thread for minutes-to-forever — inside code the docs explicitly frame as bounded (and which `lib.rs` deliberately keeps out of the command apply path for exactly this reason). No wrong output, but a trivially reachable denial of compute.
**Suggestion**: Validate `max_revs` in `porkchop_scan_inner` alongside the existing grid checks — a small hard cap (e.g. reject `max_revs > 15`, or > some `LAMBERT_MAX_REVS` const documented against the N³ growth of branch TOF minima) with a `Recoverable` error, and fix the doc to state the bound. Optionally short-circuit a branch when `T(z_lo + w)`-based reasoning already puts the branch minimum above `tof`.

### [SEVERITY: MEDIUM] Finding 3: Long-way Lambert is silently unavailable for short TOFs — the "solution exists for every tof > 0" contract is false, and the failure burns ~50 evals
**File**: crates/csp-sim/src/transfer.rs:225-239 (`tof_residual`) and :366-381 (`zero_rev_z` hyperbolic expansion)
**Problem**: For `LambertWay::Long` (A < 0) with short time-of-flight, the true root lies at ψ below roughly −1e4…−1e5, where (a) the two terms of `√μT = x³S + A√y` catastrophically cancel (the regime the module doc itself flags at :55-62) and (b) `stumpff` overflows (`cosh(√−z)` → +inf for z < −(710)² ≈ −5.04e5, crates/csp-sim/src/orbit.rs:309-328). In the overflow region `tof_residual`'s extension computes `y = A·(−inf)/inf = NaN`, so the `y <= 0.0` pocket branch is skipped and the whole region reads `f64::MAX` — positive, never flipping.
**Evidence**: `crates/csp-sim/src/transfer.rs:229-237`:
```rust
let (c2, c3) = stumpff(z);
if c2 > 0.0 {
    let y = g.r1n + g.r2n + g.a * (z * c3 - 1.0) / libm::sqrt(c2);
    if y <= 0.0 { return -tof; }
}
f64::MAX
```
For A < 0, y → +∞ as z → −∞, so the `y ≤ 0` pocket (which guarantees the Short-way flip) never exists; the Long-way flip depends on the cancellation-corrupted `T` happening to evaluate below `tof` before overflow — past the corruption threshold (~e^{t/4}·2⁻⁵² ≫ tof, i.e. |z| beyond ~10⁴–10⁵ depending on tof) it cannot. `zero_rev_z` then loops `step *= 4.0` up to `1e30` (~50 residual evaluations, all `f64::MAX`) before returning `None` (transfer.rs:370-380).
**Impact**: `lambert(..., Long)` returns `Ok(None)` for problems that provably have a zero-rev solution, contradicting the module doc (:47-50 "a solution exists for every `tof > 0`") and `LambertWay`'s contract (:113-117 "A porkchop evaluates both and keeps the cheaper"). Realistic interplanetary queries (r ~ 1e10 m, tof ≥ days) keep roots near z ≈ 0 and are unaffected — the Short way is always safe (its pocket edge sits at |z| ~ 10²–10³, far from overflow) — but short-range/high-speed Long-way legs (r ~ 1e7–1e8 m, tof ~ hours) can silently vanish from a porkchop surface, degrading optimality with no diagnostic. Not a wrong-answer path: `z_solved` + `lands_on` gate what is returned.
**Suggestion**: (1) In `tof_residual`'s `None` arm, treat `z < 0` with non-finite `y` as the pocket (`-tof`) rather than `f64::MAX` — the infeasible-for-this-way region at negative z reads "T below any target", which restores the sign flip for Long; roots that survive are still gated by `z_solved` + `lands_on`, so no garbage can leak. (2) Bail the expansion early once `step` passes the known overflow scale (|z| > 1e6) instead of grinding to 1e30. (3) Alternatively, amend the module/`LambertWay` docs to state the Long-way short-TOF exclusion explicitly.

---
## Verdict

The planner's answer-correctness chain is genuinely solid — every returned Lambert root is gated by `z_solved` AND a propagation `lands_on` check, so wrong-answer paths are essentially closed, and the departure-node/burn-frame round-trip through `maneuver.rs` is exact. What remains is availability and contract honesty, not correctness of output.

transfer.rs (M5 Lambert/porkchop planner, pure queries over `PatchedConics`) and transfers.rs (M6-D ResourceTransfer/crew-transfer *process*) are two unrelated, both-live modules (`pub mod transfer;` lib.rs:75, `pub mod transfers;` lib.rs:93) — zero code duplication, but the near-identical names are a live import/grep hazard (a bare `transfer::` grep matches both; importing the wrong one still compiles wherever both are in scope).
Duplication risk is naming-only today; renaming `transfers.rs` → `resource_transfers.rs` (or the planner → `trajectory.rs`) would remove the trap.

REQUEST CHANGES — findings 1-3 (all MEDIUM): scan-killing per-cell Lambert `Err`s, unvalidated `max_revs` breaking the bounded-work claim, and the systematically-missing Long-way short-TOF solve contradicting three documented contracts.
