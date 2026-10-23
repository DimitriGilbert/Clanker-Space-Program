# Verified Review — Cluster C12: weld→WeldedBody machinery

Verification of `.review/session-deep1/review-report-12.md` against
`crates/csp-phys/src/{lib,backend,graph,part,tensor,weld,hull,merge,structural}.rs`
(actual tree: `crates/csp-phys/`, not `csp-phys/` as the task stub implied) plus the
cited `csp-sim`/`csp-data`/`csp-core` cross-references. Every cited line was re-read;
one claim was additionally checked empirically (`f64::signum`).

**Verdicts: 1 DISMISSED · 9 CONFIRMED (2 with corrections).**

---

### Finding 1 — coincident AABB centres → "zero attach axis" — **DISMISSED** (mechanism false; residual LOW issue remains)

The finding's central mechanism is wrong. `sep.x` is a bare `f64`
(`csp-core/src/vec3.rs:18–22`), and **`f64::signum(+0.0) == 1.0`** — verified
empirically (`signum(+0.0) = 1`, `signum(-0.0) = -1`; and `x - x` yields `+0.0`
under round-to-nearest, so exactly-coincident centres give `sep = (+0.0,+0.0,+0.0)`).

What actually happens at `structural.rs:216–226` when `sep == 0`:

- the dominant-axis test `0 >= 0 && 0 >= 0` is true — that part is correct;
- `axis = DVec3::new(signum(+0.0), 0, 0) = (1,0,0)` — **unit +X, not the zero vector**;
- `axial_n = |F_J·x̂|`, `shear_n = ‖F_J − x̂(F_J·x̂)‖ = √(F_y²+F_z²)` — a perfectly
  normal decomposition along a *fixed arbitrary* axis.

So `axial_n` is **not** identically 0 and axial-limited joints are **not**
undetectable — they are judged against +X regardless of the real attach geometry.
The claimed impact ("a joint that can only fail on `max_axial_n` never fails")
does not hold.

What survives of the finding (downgrade to LOW):

- The `(Some, Some)` arm never reaches the `c_s − p_j` fallback
  (structural.rs:227–236), so degenerate geometry gets an arbitrary axis —
  confirmed.
- `build_thermal_net` treats the same coincident-centre case as degenerate and
  drops the edge (`weld.rs:689–691`, `sep.norm_squared() < 1e-18 → continue`),
  while structural silently assigns +X — the subsystem disagreement is real.
- Near-coincident centres pick a deterministic-but-arbitrary dominant axis on
  `>=` ties — real, though "noise-dominated" overstates it (the pick is a pure
  function of `sep`).

Corrected description: coincident centres yield attach axis **+X** — an
arbitrary, geometry-independent direction that misclassifies the axial/shear
split rather than suppressing it.

### Finding 2 — `joint_loads`/`failed_joints` emit DFS preorder, not ascending `PartNodeId` — **CONFIRMED**

- `structural.rs:136–149`: children map built over `BTreeMap` (ascending), pushed
  reversed onto the stack so they pop ascending *within* a parent — but the
  traversal is stack-pop DFS: a child's whole subtree precedes the next sibling.
  For `{1:[2,3], 2:[5]}` the trace is `order = [1,2,5,3]`, `loads = [2,5,3]` —
  node 5 precedes node 3. Exactly as reported.
- Doc claims at `structural.rs:113–114` ("ascending `PartNodeId` order — the same
  order `failed_joints` and the split driver consume") and `:249–251` are false.
  `failed_joints` (`:258–268`) preserves `loads` order; no sort anywhere.
- Consumer check confirmed: `csp-sim/src/lib.rs:8626–8629` re-sorts
  `structural_hits` by `(depth, id)` before claiming subtrees; the comment at
  `:8610–8617` cites "`failed_joints`' ascending-id order" — the sim authors did
  rely on the (false) documented order, then defended against it with the
  depth-sort anyway. The other consumer (`lib.rs:3375–3387`) hands `failed`
  through wholesale into the same re-sorted path. Both safe.
- Worth noting: DFS preorder guarantees ancestors-before-descendants, which is
  exactly the property the split driver's subtree-claiming needs — the *actual*
  order is safer for this consumer than the *documented* one would be. The
  defect is purely the false contract (a "first failed joint = lowest id"
  assumption would silently get a different node).

### Finding 3 — `facet_planes` is O(n⁴) over an unreduced vertex set — **CONFIRMED** (one call-site and a magnitude correction)

- O(n⁴) confirmed: `hull.rs:43–45` enumerates C(n,3) triples and `:55–62` scans
  all n vertices per triple.
- Non-reduction confirmed: `weld.rs:41–44` ("canonicalized but **not**
  hull-reduced"), `canonicalize` at `weld.rs:732–739` sorts+dedups only;
  `ColliderShape::Points` passes the authored set verbatim (`part.rs:164`).
- No vertex-count bound confirmed: `ColliderShape::is_valid` checks finiteness
  only (`part.rs:180–186`); the upstream `csp-data` validator enforces only a
  *minimum* of 4 points (`csp-data/src/validate.rs:1897–1907`). Nothing caps n.
- Per-tick re-derivation confirmed at production sites: `simple.rs:1022`
  (`any_vertex_penetrates`, per overlapping hull pair) and `simple.rs:1442–1463`
  (`generate_pair_contacts`, planes recomputed per hull per body pair per tick,
  no caching). `aero.rs:533` is the one-shot voxelizer — the report itself
  flagged it as such.
- **Correction:** `simple.rs:3079` is inside `#[cfg(test)] mod tests`
  (`mod tests` at :2123, `max_vertex_penetration` at :3075) — not a production
  call site. The per-tick exposure rests on :1022 and :1442–1463 alone, which is
  sufficient.
- **Correction:** the magnitude example is ~6× overstated: n=200 gives
  C(200,3)·200 ≈ 2.6×10⁸ inner steps, not ~1.6×10⁹ (that figure needs n≈340).
  Conclusion unchanged — O(n⁴) on authored input is a per-tick compute bomb.

### Finding 4 — `fragment_graph_of`/`failed_joints` panic on contract violation — **CONFIRMED**

- `structural.rs:261` `graph.nodes[&l.node]`, `:321` `graph.nodes[&id]`,
  `:323` `transforms[&id]` — all `BTreeMap` `Index` impls, which panic on absent
  keys. Reachable exactly as described: `members` containing a non-graph id,
  `transforms` missing `root`, a bad `root` (panics at `:321` inside
  `fragment_graph_of` after `subtree_set` returns `{root}`), or `loads` from a
  different graph.
- The contrast is real: `joint_loads` (:174–186) returns
  `WeldError::InternalInvariant` for lookups an earlier pass proved, while these
  sibling `pub` functions raw-index. Consistency/robustness gap, not a live bug —
  LOW stands.

### Finding 5 — `merge_graphs` cannot detect a wrong `effective_base` — **CONFIRMED**

- `merge.rs:239–244`: `effective_index.checked_add(effective_base)` guards `u32`
  overflow only. A too-small base remaps secondary indices into the primary's
  slice range — in bounds, so `weld` cannot flag it; mass/CoM/inertia of the
  wrong parts weld silently onto secondary nodes. A too-large base errors
  (`EffectiveIndexOutOfRange`). Exactly as reported.
- The purity rationale is documented (`merge.rs:173–178`); the failure mode is
  not. Only current caller `csp-sim/src/joints.rs:852–859` passes
  `u32::try_from(craft_p.parts.len())` — correct. Caller-contract hazard, LOW
  stands.

### Finding 6 — secondary parent cycles escape `merge_graphs` validation — **CONFIRMED**

- `merge.rs:253–262` validates parent *existence* only (`remap.get(p)`); no
  reachability walk over the secondary. A detached cycle (`a→b→a`, unreachable
  from `secondary.root`) has all parents present → copied verbatim into the
  merged graph → `weld(merged)` reports `Unreachable` against the *remapped*
  ids. Exactly as reported.
- The doc promise at `merge.rs:88–96` ("malformed input the merge reports here
  rather than letting the merged weld reject it as `Unreachable`") overstates
  the check. Error stays typed and deterministic — LOW stands.

### Finding 7 — weld≡merge bit-identity holds only under exact-unit arithmetic — **CONFIRMED**

- `graph.rs:57`: `compose` renormalizes `(self.rotation * local.rotation)` every
  step → composition is non-associative to rounding (the crate's own test
  `compose_is_associative_to_rounding`, `graph.rs:243–265`, documents "agree
  only to rounding").
- Association mismatch confirmed: merge stores `seam.compose(node.transform)` =
  `(t_port⁻¹ ∘ rel) ∘ t'_root` (`merge.rs:252`; live seam built as
  `t_port.inverse().compose(rel)` at `joints.rs:857`), while the authored
  equivalent is `t_port⁻¹ ∘ (rel ∘ t'_root)` (`merge.rs:576–578`).
- Test scoping confirmed: `merge.rs:568–570` — "the same value `seam ∘ t'_root`
  produces bitwise under exact-unit arithmetic"; fixtures are dyadic offsets +
  axis flips.
- Module doc `merge.rs:50–53` states "weld(merged) == weld(from-scratch)
  bit-identically" with no exact-unit qualifier — overstatement confirmed.
  Production consequence: ~1 ulp in the seam quat vs a from-scratch oracle —
  physically identical, internally consistent. LOW stands.

### Finding 8 — `facet_planes` dedup can merge distinct facets at large scale — **CONFIRMED**

- `hull.rs:98–107`: dedup compares `(kept.n − pl.n).norm() <= plane_eps &&
  |kept.d − pl.d| <= plane_eps`, first-kept wins in ascending-triple order;
  `plane_eps = 1e-9·max(AABB diag, 1 m)` (`hull.rs:143`).
- Unit analysis confirmed: the normal-difference term is dimensionless but
  tolerance scales with hull size. For a 1 km-diagonal hull, `plane_eps = 1e-6`:
  two genuinely distinct facets with normals within ~1e-6 rad **and** offsets
  within 1 µm collapse to one plane → a real contact facet is dropped. Rare and
  geometric; deterministic; lost manifold points rather than wrong contacts.
  LOW stands.

### Finding 9 — weld input gate accepts non-unit `ForceSite::local_dir` — **CONFIRMED**

- `weld.rs:409–415` rejects only non-finite or `norm_squared() <= 0.0` — a
  norm-2 direction welds through and scales thrust (`F = throttle·rated_n`
  along `local_dir`, `part.rs:100–101`).
- Contract at `part.rs:110–113` ("unit direction… the weld rotates but never
  renormalizes it") is enforced nowhere; the asymmetric `NonUnitRotation` gate
  is at `weld.rs:377–379` (`UNIT_ROTATION_TOLERANCE = 1e-9`, `graph.rs:108`).
- Reachable only via malformed `WeldPart` input (resolver emits unit dirs) —
  inconsistent gate, LOW stands.

### Finding 10 — `point_penetration` boundary branch suppressed on thin slabs — **CONFIRMED**

- `hull.rs:195`: boundary candidates rejected when `df >= thick·0.5 − tol`.
  `tol = vertex_margin` = `1e-3·max(diag, 1.0)` → floor 1e-3 m (`hull.rs:127`).
  For `thick < 2·tol` the bound is ≤ 0, so `df > 0` already fails the `df <
  bound` test — *every* boundary-branch candidate is rejected; only the
  strictly-interior MTD branch (:181–192) still produces contacts.
- Production callers pass `vertex_margin(&hull.vertices)`: `simple.rs:1032`,
  `:1484`, `:1519` confirmed. (`simple.rs:3092` passes `plane_epsilon` instead —
  but that is test code, `mod tests` at :2123.)
- Consequence as described: vertex contacts entering through a facet edge on a
  hull thinner than `2·tol` (≥ ~2 mm everywhere, up to 2 m on a km hull) emit
  nothing. Deterministic, heuristic — LOW stands.

---

## Spot-check of the report's "clean" claims

- `SymMat3::rotate` (`tensor.rs:58–82`): `r[i][a]` is component `a` of rotated
  basis `i`, `out[a][b] = Σ I_ij R_ai R_bj = (R·I·Rᵀ)_ab` — correct, fixed
  accumulation order. Clean claim holds.
- Weld summation order: `assemble` iterates the `solved` `BTreeMap`
  (`weld.rs:541,564`) — strictly ascending `PartNodeId` for mass, CoM, inertia,
  sites, hulls, `part_index`, thermal. Holds.
- `joint_conductance` (`thermal.rs:369–375`): `2·k_a·k_b/(k_a+k_b)·fraction` —
  IEEE `*`/`+` are commutative, so bitwise-commutative in `k_a,k_b`. Holds.
- Thermal net: coincident-centre skip at `weld.rs:689–691`, canonical `(min,max)`
  edges sorted at `:706–720`. Holds.
- `merge_graphs` seam math and remap: verified against `merge.rs:245–262` and
  the hand-computed test at `:375–421`. Holds.
- Latch momentum hand-off (`joints.rs:897–922`): `vel` is CoM velocity; `L`
  recomposed about the merged CoM in primary frame via origin-transfer sum —
  matches the report's reading. Holds.

## Summary

| # | Finding | Verdict |
|---|---------|---------|
| 1 | Zero attach axis on coincident centres | **DISMISSED** — `f64::signum(+0.0)=1.0` yields axis +X, not (0,0,0); `axial_n` is a normal (if arbitrary-axis) reading, never identically 0. Residual: arbitrary-axis degenerate case inconsistent with thermal's edge-drop — LOW. |
| 2 | `joint_loads` order is DFS preorder, not ascending | **CONFIRMED** — doc contract false; consumers safe via `(depth,id)` re-sort at csp-sim lib.rs:8626–8629. |
| 3 | `facet_planes` O(n⁴) on unreduced set | **CONFIRMED** — corrections: simple.rs:3079 is test-only; n=200 is ~2.6×10⁸ not ~1.6×10⁹ steps. |
| 4 | Raw-index panics in `pub` functions | **CONFIRMED** — LOW, contract-violation reachability only. |
| 5 | `effective_base` undetectable when too small | **CONFIRMED** — LOW, caller-contract hazard. |
| 6 | Secondary parent cycles pass merge | **CONFIRMED** — LOW, deferred to weld as `Unreachable`. |
| 7 | Bit-identity only under exact-unit arithmetic | **CONFIRMED** — doc overstatement; ~1 ulp vs from-scratch oracle. |
| 8 | Dedup merges distinct facets at 1 km scale | **CONFIRMED** — LOW, mixed-unit tolerance. |
| 9 | Non-unit `local_dir` accepted | **CONFIRMED** — LOW, malformed-input only. |
| 10 | Thin-slab boundary suppression | **CONFIRMED** — LOW, heuristic corner case. |
