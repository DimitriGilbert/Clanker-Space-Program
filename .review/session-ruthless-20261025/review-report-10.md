# Ruthless Review — Cluster 10 (physics domain modules)

Reviewer: ZCode (static review only, no cargo). Date: 2026-10-25.
Scope: crates/csp-phys/src/{aero,weld,merge,thermal,readout,structural,part,profile,graph,hull,tensor,backend,lib}.rs + tests/{resolve_parts,session}.rs

Findings appended below as confirmed.

---

### [SEVERITY: MEDIUM] Finding 1: ConfigMask's 64-node positional cap silently no-ops aero configuration changes on large crafts
**File**: crates/csp-phys/src/aero.rs:189 (with crates/csp-sim/src/lib.rs:6338-6346, crates/csp-sim/src/joints.rs:1033-1050)
**Problem**: `ConfigMask` is a `u64` bitset over node *positions* (ascending `PartNodeId`). `ConfigMask::active` returns `true` unconditionally for `i >= 64`, and every sim-side mask writer skips the bit write when the target position is `>= 64` ("the write is skipped rather than overflowing the shift" — lib.rs:6340). There is no warning, log, or editor surface anywhere for this.
**Evidence**: aero.rs:189-194:
```rust
pub fn active(&self, i: usize) -> bool {
    if i >= 64 { return true; }
    self.0 & (1u64 << i) != 0
}
```
csp-sim/src/lib.rs:6344 (`if ... && j < 64 { retained_mask |= 1u64 << j; }`) and joints.rs:1046 (`if j < 64 { mask |= bit << j; }`) drop the bit otherwise. The plan (PLAN.md §2.4.1) makes `ConfigMask` load-bearing exactly for "fairing deployment, cargo-bay doors, landing gear" and its own rationale is "One table would leave a jettisoned fairing's payload permanently shielded."
**Impact**: On any craft with more than 64 parts, a deployable/jettisonable part whose node sits at ascending-position >= 64 can never be aero-deactivated: jettisoning that fairing leaves the table built with it present — the fairing keeps producing drag *and* keeps shielding the payload (payload keeps reduced drag and zero convective heating) exactly as before the jettison. The nearest-Hamming fallback cannot fix it (no mask expresses the intent). Station-scale craft (the plan's explicit 1000-part target) are entirely outside maskable range. The bound is documented in the aero.rs module doc, but nothing tells the player their config change did nothing.
**Suggestion**: Minimum: log a `Recoverable`/warning when a config change targets a node at position >= 64 (both in `set_aero_config` and the jettison/merge remaps). Proper fix: pack the mask over deployable parts only (bitset `Vec<u64>` or renumber maskable nodes into low positions), which keeps the u64 for the common case while lifting the hard craft-size cliff.

### [SEVERITY: LOW] Finding 2: `CraftAeroTables::cm_alpha` silently returns the wrong stability derivative for out-of-range Mach index
**File**: crates/csp-phys/src/aero.rs:808-812
**Problem**: `cm_alpha(mach_idx)` computes `(cm_knots[5 + mach_idx] - cm_knots[mach_idx]) / (AoA[1]-AoA[0])`. `cm_knots` is AoA-major with stride `AERO_MACH_KNOTS.len()` = 5 and 30 entries. There is no bounds validation of `mach_idx`.
**Evidence**: For any `mach_idx >= 5` the two indices still land in-bounds (until 26) but address a *different AoA pair* — e.g. `mach_idx = 6` returns the slope between AoA rows 1 and 2, labeled as the 0°–15° derivative the doc promises. Only `mach_idx >= 26` panics.
**Impact**: No production caller yet, but this is the public API the plan names for SAS/autopilot stability derivatives ("pitch/yaw stability derivatives (Cm_α) are obtained by differencing this table" — PLAN.md §2.4.1); a future caller iterating the wrong range gets plausible-but-wrong derivatives with no error. Classic silent-wrong-answer trap.
**Suggestion**: Validate `mach_idx < AERO_MACH_KNOTS.len()` and return 0.0 (or `Option<f64>`/debug_assert) otherwise; the knot-grid stride should be a named constant rather than the inline `AERO_MACH_KNOTS.len() + mach_idx`.

### [SEVERITY: LOW] Finding 3: `thermal::step` discards the overheat (lethality) report whenever any node diverges
**File**: crates/csp-phys/src/thermal.rs:369-394
**Problem**: Overheat crossings are accumulated in `overheated` during the integration loop, but if *any* node's Euler update lands non-finite, the function returns `Err(SimError::Recoverable(...))` and the whole `overheated` vec is thrown away — including crossings already detected for healthy nodes.
**Evidence**: thermal.rs:383-393: `} else { diverged = true; } ... if diverged { return Err(...); } Ok(overheated)`. With a persistently divergent net (the exact corrupt/hand-built input the writeback guard exists for — the weld's `UnstableThermalCoupling` gate only covers weld-formed edges), the withheld node's temperature never changes, so *every* tick diverges again and the `Err` return permanently suppresses the §3.4 thermal explosion trigger for every other part on the vessel.
**Impact**: A corrupt-save thermal net (one bad edge) plus an overheating part elsewhere → the overheating part never explodes; the player sees a vessel that survives past `max_temp_k` indefinitely while a Recoverable error scrolls logs. Narrow (requires a net the weld wouldn't emit), which is why this is LOW.
**Suggestion**: Report lethality independently of the divergence: either apply the overheat detection before the `Err` return (return the list inside the error, or take a `&mut Vec<PartNodeId>` out-param), or run the lethality check over the withheld (unchanged, finite) temperatures so persistent divergence still lets a genuinely-overheat part fire.

### [SEVERITY: LOW] Finding 4: `merge_graphs`' `effective_base` precondition is debug-only — release builds silently alias secondary parts onto primary `WeldPart`s
**File**: crates/csp-phys/src/merge.rs:235-241
**Problem**: The one precondition that cannot be checked downstream (`effective_base >= max(primary effective_index) + 1`) is enforced by `debug_assert!` only. The module's own discipline is "Errors instead of panicking on every malformed input ... never a half-written graph", and every other malformed input here is a typed `MergeError`; this one panics in debug and *silently misindices* in release.
**Evidence**: merge.rs:235-241:
```rust
debug_assert!(
    primary.nodes.values().all(|n| n.effective_index < effective_base),
    "merge: effective_base below the primary's used effective indices"
);
```
A too-small base in release makes every secondary node's `effective_index + base` land on a primary `WeldPart` — wrong mass/CoM/inertia/collider for the absorbed vessel, detected by nothing downstream.
**Impact**: Today the only in-tree caller passes `parts.len()`, so it is latent; but a debug/release behavior *difference* in the merge path contradicts the project's own debug-byte-compare / cross-profile hash-equivalence discipline (§7.5), and the check the assert performs is fully computable from the arguments that are in signature.
**Suggestion**: Promote to a real typed error: `MergeError::EffectiveBase { max_index, base }` returned from the same predicate. Zero cost, keeps release and debug identical, and the error map already exists.

---

## Quality verdict (honest, 5 lines)

1. The physics core is genuinely solid: hand-computed mass-property tests (weld, tensor), momentum-conserving splits (structural), energy-conserving conduction (thermal), table-vs-direct Cm cross-checks (aero) — the hard math checks out; no CRITICAL or HIGH findings.
2. Note for the coordinator: the session context was wrong — weld.rs is NOT the joint/constraint solver; it is a pure merger. The actual PGS impulse solver lives in simple.rs (out of this cluster's file list), so "iteration caps / energy injection" could not be audited here.
3. readout.rs agrees with the VAB-side numbers (hand-Tsiolkovsky verified in-test) and with session.rs's supply-region/fire-and-separate rule — the booster-drains-sustainer behavior is a consistent, documented design, not a bug.
4. thermal.rs's model is stable by construction (weld-time coupling gate + NaN writeback guard); no radiator/ablator exists but PLAN.md doesn't promise them for vessels (§3.5.1's radiators are the colony model) — not a contradiction.
5. Residual risk concentrates in silent-degradation edges (Findings 1, 3, 4) — all narrow, all cheap to fix; nothing here blocks merge.

Summary: 4 findings — 0 CRITICAL, 0 HIGH, 1 MEDIUM, 3 LOW. Verdict: APPROVE.
