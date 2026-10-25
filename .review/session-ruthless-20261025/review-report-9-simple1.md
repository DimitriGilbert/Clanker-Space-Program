# Review Report 9: simple.rs lines 1-1774 (craft rigid-body integrator)
Reviewer: ruthless-subagent-9 | Date: 2026-10-25 | STATIC ONLY

## Findings

### [SEVERITY: HIGH] Finding 1: Gyroscopic term integrated explicitly — provable monotone rotational-energy pump on every tick
**File**: crates/csp-phys/src/simple.rs:1365-1366
**Problem**: `integrate_free` updates body-frame angular velocity with the gyroscopic term evaluated at the *old* ω:
```rust
let gyro = b.ang_vel.cross(b.inertia_local.mul_vec(b.ang_vel));
b.ang_vel = b.ang_vel + inv_i.mul_vec(tau_body - gyro) * PHYSICS_DT;
```
**Evidence**: With τ = 0 (torque-free tumble — the state of all debris/spent stages/unloaded stations), ω+ = ω + dt·a where a = I⁻¹(ω×Iω). The rotational energy change is ΔE = dt·(ω·I·a) + ½dt²·(a·I·a); the first term is ω·(ω×Iω) = 0, so ΔE = ½dt²·aᵀIa > 0 **strictly, every tick**, for any ω not aligned with a principal axis. Numbers: a tumbling long stage (I = diag(1e5,1e6,1e6), ω = (0.05,0.2,0.1) rad/s) gains ~5e-7 of its energy per tick → ×1.12 per game-hour; scale ω to a post-collision tumble (0.5 rad/s, ΔE/E ∝ ω²) and it is ×e^11/hour — visible spin-up within minutes. Nothing downstream renormalizes or damps this (no clamp on ω, and vacuum has no drag).
**Impact**: Player-visible: torque-free tumbling craft slowly spin themselves up over a session; the §2.9 torque-free-precession oracle check degrades with horizon length; determinism is fine but the *physical* answer drifts unboundedly.
**Suggestion**: Midpoint the gyro term: `let a0 = inv_i.mul_vec(tau - w.cross(I.w)); let w_mid = w + a0 * (0.5*PHYSICS_DT); let a1 = inv_i.mul_vec(tau - w_mid.cross(I.mul_vec(w_mid))); w += a1 * PHYSICS_DT;` — same cost order, fixed iteration count, still deterministic; the O(dt²) energy pump becomes O(dt³) alternating-sign. (Renormalizing L·E is heavier and changes the pinned dynamics.)

### [SEVERITY: MEDIUM] Finding 2: No CCD/substepping — contact generation point-samples the tick; fast hulls tunnel through vessels, fast landers bury metres deep
**File**: crates/csp-phys/src/simple.rs:221 (fixed `PHYSICS_DT`, no substep), 120-127 (doc overclaim), 239 (`MAX_TICK_POS_CORRECTION` = 0.2)
**Problem**: All contact generation (`generate_contacts` :1391, `generate_pair_contacts` :1443) sees only tick-boundary poses, and no body velocity is clamped or substepped.
**Evidence**: Pair contact is vertex-inside-facet only. A 0.5 m debris cube at 100 m/s relative velocity travels 1.56 m/tick — it can go from strictly-outside to strictly-outside in consecutive ticks, producing zero manifold points ever; the module doc's "is caught when a vertex crosses" (line 124-125) is false once per-tick displacement exceeds feature thickness. Ground side: a 2 km/s reentry sinks vertices ~31 m in one tick; the velocity solve zeroes vn (restitution 0) but Baumgarte recovery is capped at 0.2 m/tick → a survivable fast touchdown sits buried for ~155 ticks (2.4 s), rising 0.2 m per tick while gravity fights it.
**Impact**: Hypervelocity debris/impacts ghost straight through station hulls with no impulse (no breakup, no collision sound/log); hard landers visibly sink metres into terrain and crawl back out over seconds.
**Suggestion**: Speculative-contact margin: generate pair/ground candidates with `depth > -|v_rel|·dt` (slop proportional to relative speed) and clamp, or substep phases 1–4 with `n = ceil(|v|max·dt / feature_min)` capped for determinism (fixed per-body n, fixed order). Both keep the pinned 1/64 s tick.

### [SEVERITY: MEDIUM] Finding 3: `facet_planes` and per-hull AABBs rebuilt per body-pair — O(N²) reconstruction of per-body static data every tick
**File**: crates/csp-phys/src/simple.rs:1471-1504 (inside the `for i { for j }` loop), 1033-1065 (`any_vertex_penetrates`)
**Problem**: `aabbs_a`, `aabbs_b`, `planes_a`, `planes_b` are computed inside the pair loop for **both** bodies of each pair, but facet planes depend only on one body's canonicalized body-frame hull vertices — invariant across every pair involving that body and across ticks (until reweld).
**Evidence**: `facet_planes(&h.vertices, plane_epsilon(&h.vertices))` (lines 1483-1504) takes only the body's own hull. The doc itself calls the detector "the same O(n⁴) detector" (line 81). For N vessels with H hulls of n vertices, the tick pays O(N²·H·n⁴) where O(N·H·n⁴) suffices; 20 vessels × 8-vertex hulls ≈ 3×10⁷ redundant operations per tick at 64 Hz. The predicate path (`any_vertex_penetrates` :1045-1048) is worse per call: it re-detects `q`'s facet planes for *every hull of `p`* on *every invocation*, and `vessel_contact` is the world's warp-refusal/save-boundary predicate called between steps for pairs.
**Impact**: Bubble-scale contact cost grows quadratically in vessel count for pure re-computation of identical planes; frame budget eaten before the solver even runs in crowded bubbles.
**Suggestion**: Hoist per-hull planes and world AABBs out of the pair loop — build once per body per tick (or cache on the body as a derived cache beside `aero`, invalidated on reweld) and index into it from the pair loop; same for `any_vertex_penetrates` (cache per `q` per call).

### [SEVERITY: LOW] Finding 4: Module doc contradicts the settle implementation (stale claim about inertial-frame latch)
**File**: crates/csp-phys/src/simple.rs:143-146
**Problem**: The contact-model doc says "The settle latch still reads the stored inertial-frame ‖v‖, so on a spinning world a co-rotating body above `settle_v` legitimately never latches — rotating-frame settle is M2-C World wiring."
**Evidence**: `update_settle` (line ~1952 onward) implements exactly the rotating-frame gates the doc defers: `v_rel = b.vel − body_spin×(com − center)`, co-rotating `w_rel`, and pose deltas residualized against expected co-rotation — a landed body on a spinning world *does* latch.
**Impact**: A maintainer auditing latch behaviour on rotating worlds from the module doc would conclude co-rotating bodies never latch and might "fix" the (correct) implementation; doc/code divergence on a §2.6.4 terminal-state contract.
**Suggestion**: Replace the parenthetical with one sentence stating the latch gates are measured in the co-rotating frame (cross-reference `update_settle`).

## Verdict (simple.rs lines 1-1774)
Front half is disciplined: CoM-frame integration, fixed iteration orders, guarded effective masses, and a correct two-body pair solve.
Blocker-free, but Finding 1 is a real integrator defect with a closed-form proof of monotone energy growth — fix before long-horizon scenarios matter.
Findings 2-3 are consequence-bounded (tunneling at high Δv, quadratic contact-generation waste) and fixable without touching the pinned tick.
Docs are unusually good but one paragraph (Finding 4) has rotted against its own implementation.
Verdict: REQUEST CHANGES (Finding 1; 2-3 strongly recommended).
