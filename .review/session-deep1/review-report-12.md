# Deep Review — Cluster C12: weld→WeldedBody machinery (physics determinism)

**Scope:** `crates/csp-phys/src/{lib,backend,graph,part,tensor,weld,hull,merge,structural}.rs` — read in full. Cross-checked against `csp-core`'s `DQuat`/`DVec3`, `thermal.rs::joint_conductance`, and the `csp-sim` consumers (`joints.rs` latch ~820–1089, `lib.rs` breakup ~8600–8720, `simple.rs` contact generation ~1020–1520, 3079).

## Summary

The core determinism story is solid and verified:

- `weld` sums mass/CoM/inertia, emits force sites, hulls, and `part_index` strictly in ascending `PartNodeId` (the `solved` `BTreeMap` in `assemble`), independent of insertion order. Bit-identity is correctly qualified as *per id-labelling*.
- `SymMat3::rotate` — initially suspected of computing `RᵀIR` instead of `RIRᵀ` — was **empirically disproven**: `r[i][a]` in tensor.rs:60–73 is `R_{ai}` (component a of rotated basis i), so `out[a][b] = Σᵢⱼ I_ij R_ai R_bj = (RIRᵀ)_ab`. Correct; the fixed 0..3×0..3 accumulation order is bit-deterministic.
- `merge_graphs`' seam math is right: recorded edge `seam ∘ t'_root` with `seam = t_port⁻¹ ∘ T_rel` lands every secondary node at `T_rel ∘ t'_m`; the non-identity-root (fragment) case is handled and tested.
- `joint_conductance(k_a, k_b, …)` is bitwise-commutative in `k_a,k_b` (harmonic mean), so `build_thermal_net`'s re-root claim holds; edges are canonical `(min,max)` and sorted.
- `joint_loads`' physics is correct: `F_J = M_S·a(c_S) − ΣF_ext`; `τ_J` uses the proper moving-point moment equation `I_p α + ω×I_p ω + M(c−p)×a_p − Σ(r−p)×F`; subtree aggregates fold bottom-up in reversed preorder (children always precede parents).
- `hull::facet_planes`/`point_penetration` are deterministic (ascending triple order, first-keep dedup, `total_cmp` tie-breaks); degenerate inputs return `None`.
- Error paths are typed throughout `weld`/`merge` (§2.13); `Unreachable` covers cycles/orphans deterministically (ascending first-fail).
- The latch-side momentum hand-off in `joints.rs:902–921` is correct: `vel` is CoM velocity so `p` combines directly, and `L` is recomposed about the merged CoM in primary frame as `I_pω_p + r_p×m_p v_p + R_rel I_s R_relᵀ · (R_rel ω_s) + r_s×m_s v_s` — the standard origin-transfer sum, all in one frame.

## Findings

### [MEDIUM] Finding 1: Coincident AABB centres produce a *zero* attach axis — axial failures become undetectable

**File:** `crates/csp-phys/src/structural.rs:216–237`

**Problem:** When both node and parent have hull AABBs (the `(Some, Some)` arm) but their centres coincide (`sep == 0`), the dominant-axis test `sep.x.abs() >= sep.y.abs() && sep.x.abs() >= sep.z.abs()` is trivially true and yields `axis = DVec3::new(0.0.signum(), 0, 0) = (0,0,0)`. Then `axial_n = |F_J·0| = 0` identically and `shear_n = ‖F_J‖`.

**Evidence:** `DVec3::new(sep.x.signum(), 0.0, 0.0)` at line 220; `f64::signum(0.0) == 0.0`. The `_ =>` fallback arm (lines 227–236) — which handles *missing* AABBs by using the joint→subtree-CoM direction — is unreachable here because both AABBs exist.

**Impact:** A joint that can only fail on `max_axial_n` (shear/torque unlimited) never fails when the child's AABB centre coincides with the parent's — a real configuration for a small part nested at the centre of a symmetric parent's bounds. Notably the same coincident-centre case is *explicitly* treated as degenerate by `build_thermal_net` (weld.rs:689–691, `sep.norm_squared() < 1e-18 → no edge`), so the two subsystems disagree on the same geometry: thermal drops the coupling, structural silently rechannels the entire joint load into shear. Near-coincident centres additionally make the axis a noise-dominated arbitrary pick between the `>=` ties.

**Suggestion:** In the `(Some, Some)` arm, guard `sep.norm_squared()` at the same `1e-18` threshold the thermal net uses and fall through to the existing `_ =>` fallback axis when the centres coincide. A zero axis is never a meaningful attach direction.

### [MEDIUM] Finding 2: `joint_loads`/`failed_joints` return DFS preorder, not the documented ascending `PartNodeId`

**File:** `crates/csp-phys/src/structural.rs:114` (doc), 136–149 (order construction), 196–246 (emission), 253 (doc)

**Problem:** The doc for `joint_loads` promises "Returns loads in ascending `PartNodeId` order — the same order `failed_joints` and the split driver consume," and `failed_joints`' doc repeats "ascending `PartNodeId`". But `order` is built by stack-pop DFS preorder: root first, then each child's whole subtree before the next sibling (children pushed reversed so they pop ascending *within* a parent).

**Evidence:** For graph `{1: [2,3], 2: [5]}` the traversal emits `[1,2,5,3]`; `loads` (non-root only) = `[2,5,3]` — node 5 precedes node 3. There is no sort before `loads.push` (line 239) or in `failed_joints`' filter (lines 258–268).

**Impact:** Still deterministic (fixed order), so no determinism violation — but the documented contract is false, and a consumer that picks "the first failed joint" expecting the lowest id gets a different node. The one real consumer is safe by accident: `csp-sim/src/lib.rs:8626–8629` re-sorts `structural_hits` by `(depth, id)` before claiming subtrees — and its own comment (lib.rs:8613–8616) cites "`failed_joints`' ascending-id order" as the rationale, i.e. the sim authors already tripped on the assumption (had they relied on the documented order directly, an ancestor could double-claim a descendant's subtree → fabricated mass, the exact bug the sort prevents).

**Suggestion:** Either emit loads in ascending id (iterate `graph.nodes.keys()` in the load loop — the aggregates are all precomputed by then) or fix the docs to say "a fixed DFS preorder". Iterating by key is a one-line change and makes the contract true.

### [MEDIUM] Finding 3: `facet_planes` is O(n⁴) over an *unreduced* vertex set — an authored `Points` collider with interior vertices is a per-tick compute bomb

**File:** `crates/csp-phys/src/hull.rs:37–108` (`facet_planes`, O(n⁴) in vertex count); the non-reduction is deliberate at `crates/csp-phys/src/weld.rs:41–44` + `canonicalize` at :732–739; call sites `crates/csp-phys/src/simple.rs:1022, 1447–1460, 3079` and `crates/csp-phys/src/aero.rs:533`

**Problem:** `canonicalize` sorts and dedups but **does not hull-reduce** — interior vertices "may survive" by design (weld.rs:41–44). The facet detector is `C(n,3)` triples × an `n`-vertex one-side test = O(n⁴), justified in the doc by "n being a convex hull's extreme-point count (8 for the ubiquitous box)". That justification fails for `ColliderShape::Points`, which passes the authored set through verbatim (part.rs:163–165): a part authored with 200 vertices — say a tessellated fairing where most points are interior — produces ~1.6×10⁹ inner-loop steps **per facet-plane derivation**, and `facet_planes` is re-derived on demand in the contact path (per hull pair per tick in `generate_pair_contacts`/`any_vertex_penetrates` and the ray path), with no caching visible at the call sites.

**Evidence:** weld.rs:44 — "canonicalized but **not** hull-reduced: interior points a resolver supplied may survive"; hull.rs:8–12 — "O(n⁴) in the vertex count, n being a convex hull's extreme-point count" — the two statements conflict the moment a `Points` hull carries non-extreme vertices. There is also no vertex-count bound anywhere in `weld`'s input gate (`ColliderShape::is_valid` checks finiteness only, part.rs:180–186).

**Impact:** Craft with a fat authored `Points` hull stalls the physics step — deterministic, but a DoS-class perf cliff triggered by ordinary authored data, exactly the "unbounded cost inside a derived cache" the project elsewhere works to avoid. (The voxelizer path at aero.rs:524–533 has the same input but is at least one-shot per config change, not per tick.)

**Suggestion:** Cheapest fix: reduce the vertex set before facet detection — drop points that are not extreme (e.g., cull any vertex strictly inside every candidate facet plane's hull, or compute the hull at weld time and store only extremes in `CompoundCollider`). If minimality can't be assumed, cap `Points` length in the weld/resolver input gate (`ColliderShape::is_valid` or `validate_effective`) to a count the O(n⁴) sweep can afford, and document the bound.

### [LOW] Finding 4: `fragment_graph_of` / `failed_joints` panic on contract violation instead of typed errors

**File:** `crates/csp-phys/src/structural.rs:261` (`graph.nodes[&l.node]`), 321 (`graph.nodes[&id]`), 323 (`transforms[&id]`)

**Problem:** Three raw index panics in `pub` functions:

- `fragment_graph_of`: `graph.nodes[&id]` panics if `members` contains an id not in `graph`; `transforms[&id]` panics if `transforms` (caller-supplied, documented as "from `body_transforms`") is missing `root`. A bad `root` alone panics via `subtree_set` → `fragment_graph_of`.
- `failed_joints`: `graph.nodes[&l.node]` panics if `loads` was produced for a different graph than the one passed.

**Evidence:** Same file, `joint_loads` lines 174–186, carefully returns `WeldError::InternalInvariant` for *proven-present* lookups ("structural: non-root node missing a parent") — the crate's own convention — while these sibling functions raw-index.

**Impact:** §2.13 wants typed errors, never panics, on fallible paths. Inputs are currently always well-formed (subtree_set/body_transforms/same-graph loads), so this is a robustness/consistency gap rather than a live bug — a misuse crashes the sim instead of erroring.

**Suggestion:** `fragment_graph_of` is infallible by signature; either document panics as contract violations explicitly (and `debug_assert`/`assert` them for loudness), or return `Result<PartGraph, WeldError>` and reuse `InternalInvariant` like `joint_loads` does.

### [LOW] Finding 5: `merge_graphs` cannot detect a wrong `effective_base` — silent cross-wiring to primary `WeldPart`s

**File:** `crates/csp-phys/src/merge.rs:183–190`, 239–244

**Problem:** `effective_base` is trusted as `primary.parts.len()`. A too-small base remaps secondary `effective_index`s into the *primary's* slice range — in bounds, so `weld` cannot detect it; the merged body silently welds primary stats onto secondary nodes (wrong mass/CoM/inertia, no error). A too-large base at least errors (`EffectiveIndexOutOfRange`).

**Evidence:** `node.effective_index.checked_add(effective_base)` only guards `u32` overflow; there is no way to range-check without the parts slice, which the function deliberately excludes to stay "pure over exactly what it transforms" (doc line 175–178).

**Impact:** Caller-contract bug class: a wrong base yields a plausible-but-wrong `WeldedBody` — precisely the failure mode the crate otherwise works hard to type-error on. Current callers pass `parts.len()` correctly (`joints.rs:858–859`); the hazard is for future/manual callers.

**Suggestion:** Cheap hardening that keeps purity: document the failure mode at the parameter (currently only the formula is documented), or take `parts_primary: &[WeldPart]` solely for `parts_primary.len()` so the base cannot be misstated.

### [LOW] Finding 6: Secondary parent *cycles* escape `merge_graphs`' validation

**File:** `crates/csp-phys/src/merge.rs:88–96` (doc promise), 253–262 (pass 2)

**Problem:** `MergeError::SecondaryParent` is documented as covering "malformed input the merge reports here rather than letting the merged weld reject it as `Unreachable`" — but a secondary containing a detached parent cycle (`a→b→a`, unreachable from its root) passes: every non-root node's parent exists in the graph, so `remap.get(p)` succeeds and the cycle is copied into the merged graph. The defect then surfaces only at `weld(merged)` as `Unreachable`.

**Evidence:** The check at lines 254–259 validates parent *existence*, never reachability; there is no DFS over the secondary.

**Impact:** Error is still typed and deterministic — just reported one stage later than the doc implies, after a merge that "succeeded". Diagnostically it can confuse: `weld` reports the failure against the *merged* (remapped) ids, not the secondary's original ids.

**Suggestion:** Either validate secondary reachability up front (cheap DFS like `solve_transforms`'s) or narrow the doc claim to parent-existence only.

### [LOW] Finding 7: weld≡merge bit-identity only holds under exact-unit arithmetic — the compose association differs

**File:** `crates/csp-phys/src/merge.rs:245–252` (`seam.compose(node.transform)`), 40–53 (module doc claim); `crates/csp-phys/src/graph.rs:54–59` (`compose` renormalizes)

**Problem:** The recorded seam edge is `seam ∘ t'_root` = `(t_port⁻¹ ∘ rel) ∘ t'_root`, while the equivalent authored edge is `t_port⁻¹ ∘ (rel ∘ t'_root)` — the same nesting the merge test's `graph_ref` uses (merge.rs:576–578). `PartTransform::compose` renormalizes the quaternion product at each step, so composition is *not* associative: `(a∘b)∘c` and `a∘(b∘c)` agree only to rounding. With exact-unit/dyadic fixtures both associations produce identical bits (the test asserts bitwise equality and passes); with arbitrary real rotations they differ in the last ulp, so the stored edge transform — and therefore `weld(merged)` vs `weld(authored)` — is bit-identical only under the test's arithmetic, not in production.

**Evidence:** graph.rs:57 `(self.rotation * local.rotation).normalize()`; merge.rs:570 — the test itself scopes the claim: "the same value `seam ∘ t'_root` produces bitwise under exact-unit arithmetic". The module doc (merge.rs:50–53) states "weld(merged) == weld(from-scratch) bit-identically" without the qualifier.

**Impact:** In a live latch the two association orders differ by ~1 ulp in the seam quat — meaning an undock-and-compare / oracle test against a from-scratch graph would see a non-bitwise (but physically identical) difference. Not a correctness bug — the merged graph is internally consistent — but the named exit criterion is overstated, and any future bit-comparison oracle between "merged" and "authored" states will flake on non-dyadic geometry.

**Suggestion:** Document the exact-unit qualifier in the module doc, or compose the edge as `seam.compose(t'_root)` vs authored `t_port⁻¹.compose(rel.compose(t'_root))` — they already match; nothing to fix in code. If a bitwise oracle is wanted beyond the test fixture, the caller could pre-compose `rel.compose(t'_root)` and pass `attach`-frame seam — same association, but changes the seam's documented meaning.

### [LOW] Finding 8: `facet_planes` dedup can merge *distinct* facets at large hull scale

**File:** `crates/csp-phys/src/hull.rs:98–107`

**Problem:** Duplicate-plane merging compares `(kept.n − pl.n).norm() <= plane_eps && |kept.d − pl.d| <= plane_eps` where `plane_eps = 1e-9 · max(diag, 1 m)`. The normal-difference term mixes units (a dimensionless vector norm against a metre-scaled tolerance): for a 1 km-diagonal station hull, `plane_eps = 1e-6`, so two genuinely distinct facets whose normals differ by < 1e-6 rad **and** offsets differ by < 1 µm collapse into one. On a large tessellated hull (thin panel stacks, adjacent near-parallel faces), a real facet can be dropped → a manifold point the contact solve would have generated is lost.

**Evidence:** The comparison happens per candidate in insertion order (first kept wins); `plane_epsilon` scales purely with the AABB diagonal (hull.rs:134–144). A metre-scale box is unaffected (eps = 1e-9, far below facet spacing) — the hazard is only large-diagonal or deliberately fine-tessellated `Points` hulls (compounds Finding 3's input space).

**Impact:** Rare and geometric — lost contact facets on huge/fine hulls, not wrong contacts. Still deterministic.

**Suggestion:** Compare normals at an angle (dot ≥ 1 − θ_eps², θ fixed in radians, scale-free) and keep `d` at `plane_eps`; or document that facets closer than `plane_eps` in either sense are intentionally merged.

### [LOW] Finding 9: Weld input gate accepts non-unit `ForceSite::local_dir` — silent thrust scaling

**File:** `crates/csp-phys/src/weld.rs:409–415`; contract at `crates/csp-phys/src/part.rs:111–113`

**Problem:** The site gate rejects only non-finite or exactly-zero directions (`norm_squared() <= 0.0`); a direction of norm 2 welds through and — since thrust is `throttle·rated_n` *along* `local_dir` — silently doubles the applied force. Compare `NonUnitRotation`: a node rotation 1e-6 off unit is a *typed error* (weld.rs:377–379), while a 200%-norm site direction is fine.

**Evidence:** `site.local_dir.norm_squared() <= 0.0` is the only magnitude check. `part.rs` documents "local_dir is a unit direction supplied by the resolver — the weld rotates but never renormalizes it" — assumption enforced nowhere.

**Impact:** Only reachable via malformed `WeldPart` input (the resolver does emit unit dirs), but it is an inconsistent gate in a module whose entire stated purpose is rejecting bad physical data before it corrupts the body.

**Suggestion:** Reject `|norm² − 1|` beyond a tolerance (mirroring `UNIT_ROTATION_TOLERANCE`), or normalize and document. Symmetric with the rotation gate.

### [LOW] Finding 10: `point_penetration` boundary branch silently suppresses all candidates on thin slabs

**File:** `crates/csp-phys/src/hull.rs:195` (`df >= planes[i].thick * 0.5 - tol`)

**Problem:** A vertex near a facet's edge region (the boundary branch — needed for the aligned-boxes case) is only accepted while `depth_f < thick_f/2 − tol`. With `tol = vertex_margin` = `1e-3·diag` **floored at 1e-3 m** (hull.rs:127, `.max(1.0)`), any hull thinner than `2·tol` (~2 mm at metre scale, up to 2 m for a km-scale station) rejects every boundary candidate — only strictly-interior vertices (inside *all* facets) still generate contacts.

**Evidence:** The `thick*0.5 − tol` bound goes negative for `thick < 2·tol`; callers pass `vertex_margin(&penetrated_hull.vertices)` (simple.rs:1484, 1519, 1032).

**Impact:** Contacts at facet edges/corners on thin-plate hulls (a real part shape for panels/fairings) can be missed entirely — a vertex inside the slab but near an edge emits nothing. Genuinely interior points still work, so the failure is limited to near-edge penetration, and the margin floor makes it worse for small hulls (1 mm margin on a 1 cm part is 10% of its size). A documented heuristic, but the thin-slab corner case is unremarked.

**Suggestion:** Clamp the half-thickness bound at ≥ 0 (`(thick*0.5 − tol).max(0.0)`), or scale `tol` to `min(1e-3·diag, c·min_thickness)`; at minimum, document that slabs thinner than `2·margin` lose edge-entry contacts.

## Explicitly checked and found clean

- `SymMat3::rotate` computes `R·I·Rᵀ` correctly (empirically verified — `out[0][1] = −0.5` for `diag(1,2,3)` under 45° z); `shift`/`mul_vec`/`det`/`inverse` adjugate all hand-verified.
- `PartTransform::compose`/`inverse`/`normalized` math and the unit-quat gate; `seam.compose(t'_root)` produces `T_rel ∘ t'_m` for merged nodes (verified against the test's hand-computed chain). The root's stored quat is `normalized()` once; composed chains renormalize per step — unit drift cannot accumulate.
- Weld summation order is ascending `PartNodeId` for mass, CoM, inertia, sites, hulls, thermal — bit-identity per labelling holds; `canonicalize`'s `total_cmp` sort + `dedup` is deterministic even on ±0 (sort places −0.0 before +0.0, dedup keeps the −0.0 form — fixed either way).
- `merge_graphs` id remap (`max_primary+1 + old_id`) cannot collide; overflow checked; seam validated; error precedence deterministic; secondary root/parent checks cover existence (cycles deferred to weld — Finding 6).
- `build_thermal_net`: commutative conductance, canonical edge order, deterministic dominant-axis tie-break; re-root invariance claim verified; coincident-centre → no edge (the convention Finding 1 needs).
- `joint_loads` formulas, bottom-up aggregation order, massless-subtree skip, gravity cancellation — all correct; the `_ =>` fallback axis is sound (Finding 1 is only about it not being reached when `sep == 0`).
- `fragment_graph` preserves body-frame pose at the fragment root; `fragment_velocity` is the correct momentum-conserving stamp (`v + ω×(c_frag − c)`); fragment `ang_vel` stays body-frame consistent with the caller-stamped `rot` (fragment frame == parent body frame).
- `sim` consumer side: `joints.rs` seam `t_port⁻¹ ∘ T_rel` and aggregate-momentum stamp verified; `lib.rs` fragment claiming sorts `(depth, id)` and subtracts `assigned` deepest-first — no double-welded subtrees.
- `backend.rs` value types: `BodyHandle` positional contract, `Contact.impulse` semantics, `JointKind` `u16` discriminant all consistent.
- No `HashMap`, threads, unstable sorts, or non-`libm` float math anywhere in the reviewed files.
