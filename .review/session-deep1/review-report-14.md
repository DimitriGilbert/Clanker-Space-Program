# Cluster C14 — Deep review: resolve / session / readout / profile

Scope: `crates/csp-phys/src/{resolve,session,readout,profile}.rs`,
`crates/csp-phys/tests/{resolve_parts,session}.rs`. Read fully; supporting
code consulted: `part.rs` (`ColliderShape::is_valid`, `WeldPart` gate doc),
`graph.rs` (`PartTransform`), `weld.rs` (input gate), `csp-core/quat.rs`
(`from_axis_angle` normalizes its axis — so `rotation_arc`'s unnormalized
`f.cross(t)` axis is safe), `csp-data` `part.rs`/`validate.rs` (to calibrate
which gates the resolver defends in depth), `csp-sim` (who consumes
`ReliabilitySpec::is_valid`).

The frozen-vs-live split is implemented correctly: `BuiltPart` freezes
`upgrade_levels_at_build` (composed effect, resolver never touches the
tree) while `FamilyQualification`/`PartUsage` are resolver inputs —
`resolve_parts` reads the family's live units per call and
`ReliabilitySpec` carries the curve (not a point value) so §3.4 can
re-evaluate on live units. Resolver order matches the module doc
(identity → scale → variant → reachability → stats → weld view →
qualification → wear). Craft-file versioning (magic + bincode + schema
refusal + structural validation) and `PartNodeId` stability
(`next_id` serialized, `NextIdCollision` check, no reuse) are solid.
Readout boundary cases (zero demand, starved stages, dry-end clamp,
NaN-free TWR/Δv paths) are handled. Findings below are the real gaps.

---

### [MEDIUM] Finding 1: `expected_hazard` silently drops spec-invalid parts and reads NaN qualification units as the best knot — the "conservative approximation" is optimistic

**File:** `crates/csp-phys/src/profile.rs:205-211` (skip at 206-208, qual read at 209-210), `crates/csp-phys/src/resolve.rs:1091-1105` (`qual_factor` fallthrough), `crates/csp-phys/src/resolve.rs:1237-1256` (`ReliabilitySpec::is_valid`)

**Problem.** `expected_hazard` runs `if !spec.is_valid() { continue; }` —
a malformed spec contributes **zero** hazard instead of failing or
defaulting to the worst case, so `confidence()` inflates silently. And
`quals.get(&spec.family).map_or(0.0, |q| q.units())` does not gate the
record: a NaN `units()` reaches `qual_factor`, where every `units <= x`
comparison is false, so evaluation falls through the window loop and
returns `knots.last().hazard_multiplier` — the *most qualified* (best)
factor. Compare the resolver, which gates the same inputs
(`InvalidQualification`, `InvalidUsage`) instead of silently degrading.

**Evidence.** The conditions `is_valid` enforces — `infant_mortality ∈
[0,1)`, `dormant_stress ∈ [0,1]`, non-empty positive `failure_modes`,
`regime_stress` threshold ≥ 0 and multiplier ≥ 1 — are *not* enforced by
`resolve_reliability` (resolve.rs:1616-1638 gates only finiteness of the
same fields). So `resolve_part` → `ReliabilitySpec::from_effective` can
produce an invalid spec from a successfully resolved part (e.g.
`failure_modes: []`, `infant_mortality: 1.5` on an unvalidated def).
`csp-sim` treats `is_valid` as a **refusal** gate
(`csp-sim/src/manufacture.rs:147`, `csp-sim/src/lib.rs:4563` — a craft
with an invalid spec is not launchable), so the planning-side number and
the sim disagree by construction: the VAB's confidence reads "fine" for
a craft the sim would refuse to spawn. Also, `is_valid` itself is
asymmetric at resolve.rs:1252: `r.multiplier >= 1.0` passes `f64::INFINITY`
(∞ is finite-comparison-true) while NaN fails — an ∞ multiplier yields
`stress = ∞` → `Λ = ∞` → `confidence = 0`, a conservative outcome but
an inconsistent gate.

**Impact.** Design-AI/editor confidence overestimates survivability for
exactly the malformed-input cases the typed-error architecture exists to
surface; a corrupt serialized `FamilyQualification` (NaN units) reads as
a fully-qualified family — the most optimistic possible reading.

**Suggestion.** Mirror the resolver's input gates: validate each
`FamilyQualification` (`q.is_valid()`, or treat invalid as 0 units —
pessimistic, not optimistic), and replace the silent `continue` on
`!spec.is_valid()` with either a `Result`/`Option` return or a documented
worst-case contribution (e.g. skip but expose a `skipped: usize`/`Option<f64>`
on the API so callers cannot mistake a dropped part for a healthy one).
Add `r.multiplier.is_finite()` to `ReliabilitySpec::is_valid`'s
regime-rule clause for symmetry.

---

### [MEDIUM] Finding 2: The resolver under-gates its own output contract — `EffectivePart` is not guaranteed weldable, spec-valid, or readout-sane

**File:** `crates/csp-phys/src/resolve.rs:447-449` (`EffectivePart` doc:
"every number is finite, `mass_kg > 0`"), `resolve.rs:1044-1079`
(`resolve_module` — zero validation), `resolve.rs:1540-1576`
(`resolve_envelopes` — zero validation), `resolve.rs:1024-1028`
(`build_resources` amounts unchecked), `crates/csp-phys/src/part.rs:180-186`
(`ColliderShape::is_valid` vacuous on `Points([])`), `crates/csp-phys/src/weld.rs:397-448` (weld's stricter per-part gate)

**Problem.** The module advertises "one resolver, one place" and claims
`weld_input` "feeds `weld` verbatim", but the resolved `EffectivePart`
only gates: mass, cost, collider finiteness, thrust, MTBF/hazard,
qual output, usage/qual inputs, tank-capacity sign, and
tank/build-resource existence. The following pass through ungated and
can violate *downstream* contracts:

- **Structural/thermal bounds the weld requires.** Weld rejects
  `heat_capacity_j_per_k <= 0`, `max_temp_k <= 0`, `emissivity ∉ (0,1]`,
  `conduction < 0`, `structural.* < 0 or non-finite` (weld.rs:422-448).
  `resolve_envelopes` multiplies but never checks — an unvalidated def
  (`emissivity: 0`, NaN `max_axial_n`, or a NaN `ScaleExponents`
  exponent reaching `s^e`) resolves successfully and then fails at weld
  with a *different* error type (`WeldError::InvalidPart` vs
  `ResolveError`), splitting the "one place" gate.
- **Engine stats besides thrust.** `isp_vac_s`/`isp_sl_s ≤ 0` or NaN,
  `rated_burn_s < 0`, `throttle_range` inverted, negative propellant
  fractions all pass `resolve_module` verbatim. `readout::analyze`
  degrades them to junk rows rather than errors: `isp = 0` → `mdot =
  ∞` → `isp_vac_s = 0`, `mass_flow_kg_s = ∞`, and
  `propellant_burned_kg = (NaN).min(avail)` = the full tank contents with
  `burn_time_s = 0` (readout.rs:253-307, `f64::min` discards the NaN) —
  a self-contradictory row, all from a resolvable `EffectivePart`.
- **Engine propellant `ResourceId`s are never checked against
  `resources.ron`.** `propellant_mass`/`resolve_weld_input` check Tank
  and build-material resources (`UnknownResource`), but engine
  `propellants` are cloned verbatim — an engine burning a nonexistent
  resource resolves fine, then silently reads `mass_per_unit → None →
  burn = 0` in `analyze` instead of the typed error its tank sibling
  gets.
- **`build_resources` amounts.** `u * f.mass` is emitted ungated — a
  negative or NaN authored amount produces a negative/NaN BOM on the
  `EffectivePart` (existence is checked, the quantity is not).
- **Degenerate collider.** `Points([])` passes `ColliderShape::is_valid`
  (`all()` on empty is true) despite `InvalidStat`'s doc claiming
  "degenerate collider" is gated — an empty `Convex` resolves to a
  zero-vertex, zero-inertia collider (only an `Engine` module trips
  `collider_z_min`'s `None`).
- **Unreachability beyond stat paths.** `attach_nodes` positions,
  `size_m`, aero curve coefficients, `gimbal_deg`, `power_w`,
  `CrewSeat`/module passthroughs — none checked; NaN values reach the
  serialized `EffectivePart`. Additionally, `resolve_parts`' `by_id`
  `BTreeMap` silently keeps the *last* def on a duplicated `part_id`
  (resolve.rs:1807-1811) — only the `csp-data` validator prevents
  shadowing.

**Evidence.** Resolve.rs gates at 1396-1414 (collider, thrust), 1723-1734
(mass, cost), 1589-1648 (reliability); nothing validates the
`resolve_module`/`resolve_envelopes` outputs, and
`ResolveError::InvalidStat`'s own doc (resolve.rs:569-577) advertises
gates ("negative … capacity, degenerate collider") that are only
partially implemented. The csp-data validator *does* cover every item
above (isp positivity validate.rs:2148-2149, propellant fractions,
attach-node finiteness, thermal bounds, non-empty failure modes) — the
issue is that the resolver's stated defence-in-depth posture
("the resolver cannot assume validation ran", resolve.rs:1611-1615)
is applied unevenly.

**Impact.** For validated authored data: none today. For the explicitly
supported unvalidated channel (hand-built defs, serialized defs, corrupt
saves), `resolve_parts` success does **not** imply weld success or a
spec-valid/readout-sane part — errors surface later, typed differently,
or silently (junk readout rows, skipped specs per Finding 1).

**Suggestion.** Either gate the resolved envelopes/modules in
`resolve_built_part` (a post-pass checking the same bounds the weld
requires — cheap, one place) or narrow the `EffectivePart` doc to name
the fields it guarantees vs. the fields the weld re-gates; make
`UnknownResource` cover engine propellant resources for consistency with
tank/build resources; make `ColliderShape::Points` emptiness an
`InvalidStat` (or fix the `InvalidStat` doc); at minimum, keep the
per-field gate list and the doc's "every number is finite" claim in
lock-step.

---

### [MEDIUM] Finding 3: `compute_attach` validates direction/diameter but never `position_m` — a NaN attach position stores an invalid transform, and `to_bytes` then writes a file `from_bytes` refuses

**File:** `crates/csp-phys/src/session.rs:1019-1056` (`compute_attach`),
`session.rs:684-688` (`snap_transform`), `session.rs:1200-1214`
(`set_transform` *does* gate `is_valid`), `session.rs:1386-1456`
(`validate` — no transform check), `session.rs:1568-1616` (`to_bytes` /
`from_bytes`), `session.rs:1627-1629` (`BadTransform` on load)

**Problem.** The snap path gates `direction` finiteness/norm and
`diameter_m` finiteness/match, but not the attach-node `position_m` —
and it stores `snap_transform`'s output unconditionally. `set_transform`
refuses a non-unit/NaN transform (the invariant's stated reason), yet
`add_part`/`reattach` can install one: a def whose named attach node has
a non-finite `position_m` (direction and diameter can be perfectly
valid) produces `offset = p_p − rotation·NaN` → NaN transform → stored
on the node. Three downstream asymmetries result:

- `body_transforms`/`to_part_graph` propagate it into a `PartGraph` the
  weld rejects (`weld.rs:371` transform gate) — i.e. the "welds
  verbatim" pipeline fails at a later layer, typed differently.
- `validate` never checks stored transforms, so `is_launchable()` holds.
- `to_bytes` serializes it happily; `from_bytes` then rejects the same
  bytes as `BadTransform` — **a session can produce a craft file it
  cannot reload**, and the corrupt file *does* exist on disk.

`snap_transform` is `pub` with no input gate at all — callers must know
to pre-check direction norms (the doc comment doesn't state the
precondition; a zero direction yields `from/from.norm()` = NaN axis in
`rotation_arc`).

**Evidence.** `compute_attach` checks `pn.direction.is_finite()`/`norm`
(1032-1045) and `pn.diameter_m.is_finite()` (1046-1055), then returns
`Ok(snap_transform(&pn, &cn))` with no `position_m` or transform
validity check (1056). Contrast `set_transform`'s explicit
`transform.is_valid()` refusal (1205) and `from_file`'s
`BadTransform` gate (1627-1629) — the same check exists at write- and
manual-set boundaries but not at placement.

**Impact.** Only reachable through an unvalidated def (the validator
rejects non-finite attach positions, validate.rs:336-344 BadAttachNode)
— but that is precisely the channel every other session gate defends.
Consequence: invalid transform lands in memory, in the file, and in the
graph; the craft "saves" but cannot load.

**Suggestion.** Check `position_m.is_finite()` alongside the direction/
diameter gates in `compute_attach` (and/or assert
`snap_transform(…).is_valid()` before storing — one line), document
`snap_transform`'s precondition in its doc, and consider a transform
validity pass in `validate` so `is_launchable` matches what
`to_part_graph` will weld.

---

### [LOW] Finding 4: `upgrade_levels_at_build` is never gated at the session boundary — `validate().is_launchable()` does not imply `resolve_parts` succeeds

**File:** `crates/csp-phys/src/session.rs:697-732` (`check_built`),
`session.rs:1389-1400` (`validate` re-runs `check_built`),
`crates/csp-phys/src/resolve.rs:873-905` (`fold_multipliers` — the gate
that actually fires)

**Problem.** `check_built` re-checks the `BuiltPart`'s scale rule and
variant membership but ignores `upgrade_levels_at_build` entirely. Both
session entry paths (`add_root`, `add_part`) take a caller-constructed
`BuiltPart` — a `TrackLevels` with a NaN/≤0 `multiplier` or a `StatPath`
the def lacks is accepted, placed, serialized, and reported launchable;
`resolve_parts`/`to_part_graph` then fails at commit with
`UnresolvableStatPath`/`InvalidStat`. A crafted file reaches the same
hole (the file layer can't gate semantics — by design — but `validate`
is the layer that re-checks semantics, and it skips this one).

**Evidence.** `check_built` has arms for scale (703-724) and variant
(725-730) only. The resolver's contract for the same field is
`fold_multipliers`' reachability + positive-finite gate — there is no
cheaper "all keys resolve" session-side equivalent even though
`StatPath::base_value`/`stat_path_resolves` is exactly the predicate
needed.

**Impact.** Limited to callers who hand-construct `BuiltPart` (the
manufacture path fills it via the gated `compose_upgrade_levels`) and
crafted files. Still: two of the session's stated jobs — "the
editor-side boundary where a bad record is refused before it becomes a
node" (check_built doc) and the launchable verdict — silently defer to
commit for one field only.

**Suggestion.** Extend `check_built` (renamed or not) to iterate
`built.upgrade_levels_at_build`: `stat_path.base_value(def).is_some()`
and `multiplier.is_finite() && > 0` — the same two conditions
`fold_multipliers` enforces, reusing `StatPath::base_value` so the
predicates cannot drift.

---

### [LOW] Finding 5: `apply_wear`/`wear_multiplier` can emit non-finite hazard from finite, individually-valid inputs

**File:** `crates/csp-phys/src/resolve.rs:1121-1144`

**Problem.** `wear_multiplier` computes `1 + operating_s / base_mtbf_s`
with no output gate. `base_mtbf_s` is gated `> 0` and `1/mtbf` finite —
so `mtbf ≈ 1e-300` is legal — and `operating_s` merely finite — so
`operating_s / base_mtbf_s` overflows to `∞`. `apply_wear` then writes
`hazard_per_s = base_hazard · qual · ∞ = ∞` into `EffectivePart`
unconditionally, breaking the "every number is finite" guarantee the
type doc claims (resolve.rs:447). `PartUsage::is_valid` accepts
`operating_s = 1.8e308` and the resolved `base_mtbf_s` can legitimately
be tiny via a `BaseMtbfS` multiplier (gate is only `> 0`).

**Evidence.** `wear_multiplier` (1121-1123) is documented as infallible
with preconditions on *inputs*; `apply_wear` (1140-1143) checks
`usage.is_valid()` but never the computed `wear_multiplier`/
`hazard_per_s`. The MTBF gate at 1589 and the hazard gate at 1605 both
pass `mtbf = 1e-300` (its reciprocal `1e300` is finite).

**Impact.** Extreme-input-only (a ~zero MTBF or ~1e300 s of operation),
but the failure mode is exactly the NaN/∞ leak the rest of
`resolve_reliability` works to prevent — one `is_finite` check on the
result closes it. Note the field is display-only post-M9b, so impact is
a misleading readout row, not a sim decision.

**Suggestion.** In `apply_wear`, after computing, reject non-finite
`wear_multiplier`/`hazard_per_s` with `InvalidStat`/`InvalidUsage`, or
clamp at a documented bound; either restores the finiteness invariant.

---

### [LOW] Finding 6: Propellant draw order is ascending `PartNodeId`, not topology- or separation-aware — partial burns misattribute the drain

**File:** `crates/csp-phys/src/readout.rs:282-291` (`available` pool),
`readout.rs:318-335` (consumption loop)

**Problem.** A firing stage's burn is correctly sized
(`available/demand` min), but the *source* of the consumed units is
`for id in &attached` — ascending node id. When a stage burns only part
of its supply and that stage's decouplers then sever subtrees, which
tanks the fuel came out of is arbitrary:

- If a departing tank is drained, it leaves *empty* — that propellant is
  consumed, and the staying tanks retain theirs.
- If a staying tank is drained, the departing tank leaves *full* — the
  staying craft retains less.

Both orderings produce identical stage-`s` Δv (the mass accounting is
right) but different propellant remaining for all later stages, decided
by `next_id` allocation order, not by any physical rule ("drain
departing tanks first", "drain upper tanks first") and not documented.
The module doc owns up to the massless-resource edge but not this one.

**Evidence.** The consumption loop (320-335) iterates `attached` (a
`BTreeSet`, so ascending `PartNodeId`) taking `have.min(need)` per tank;
`separated_ids` carry their `NodeMass.tanks` remainder away but are
simply dropped from `attached` — so the split is fully determined by
which side of the severed edges the low ids happened to land. The
two-stage test (readout.rs:638-651) only exercises total consumption,
which hides the ordering.

**Impact.** Multi-stage readouts where a stage's engines burn partially
before separating (the common booster-with-fuel-left case) report later
stages' propellant — and therefore their Δv — depending on construction
order. Same-type row data differs for semantically identical crafts with
different id layouts; KSP-style priority (feed engines from what
separates, or by stage/depth) is the defensible order.

**Suggestion.** Drain tanks in subtrees being separated by this stage's
decouplers *first* (they leave anyway and the doc already credits the
departing half with feeding the burn), then the rest — one partition of
`attached` by `separated` membership before the ascending-id walk; and
document the order in the module doc either way.

---

### [LOW] Finding 7: `validate()` on an empty session reports launchable while the commit path refuses it

**File:** `crates/csp-phys/src/session.rs:1386-1457` (`validate` — no
emptiness check), `session.rs:1363-1364` (`to_part_graph` →
`EmptySession`)

**Problem.** `validate` produces no errors for `nodes.is_empty()`
(nothing to check, no stages to report), so `is_launchable()` returns
`true` — but the very next step (`to_part_graph` → `EmptySession`, and
`analyze`'s documented "empty = bare table" convention) treats an empty
craft as unlaunchable.

**Evidence.** `Validation::is_launchable` is `self.errors.is_empty()`
(479-481); for an empty session nothing pushes an error.
`SessionError::EmptySession` exists precisely for this state but is only
raised by `to_part_graph`.

**Impact.** Trivial: a consumer gating "can I launch?" on
`validate().is_launchable()` is told yes for a craft with zero parts.
Harmless for the VAB (which presumably hides the button) but a real
contract seam in the two-tier check.

**Suggestion.** Push `SessionError::EmptySession` into `errors` when
`self.nodes.is_empty()` (or `root.is_none()`), matching `to_part_graph`.

---

### [INFO] Finding 8: `insert_stage` saturation at `u32::MAX` silently merges two stages

**File:** `crates/csp-phys/src/session.rs:1319-1325`

**Problem.** `insert_stage(at)` saturates `n.stage` at `u32::MAX`; a part
already at `u32::MAX` cannot move, while parts at `u32::MAX - 1`
move *into* `u32::MAX` — two formerly distinct firing groups collapse
into one with no warning. Only reachable via a crafted file (validate's
bounded gap reporting already anticipates `u32::MAX` staging), and the
saturation is documented — flagged so the merge is a conscious choice.

**Evidence.** `n.stage = n.stage.saturating_add(1)` (1322) — no record of
the collision; `validate` sees the result as one populated stage.

**Impact.** A crafted high-stage craft loses a firing group on insert.
Cosmetic-grade.

**Suggestion.** Either accept the merge (current, documented) or fold it
into a `SessionWarning`; a saturating `u32::MAX` stage is already
validate-visible as a giant gap, so no action is strictly required.

---

## Notes on things verified as correct (no action)

- `rotation_arc`/`orthogonal_axis`: `DQuat::from_axis_angle` normalizes
  the axis internally (quat.rs:43-49); near-parallel/antiparallel cases
  are eps-banded; `orthogonal_axis` picks the least-aligned basis, so the
  antiparallel axis is never degenerate for a finite input.
- `reroot` path reversal: per-edge transform inversion and
  `parent_node`/`child_node` role swap verified algebraically — nodes
  stay coincident, directions stay opposed; new root gets the folded
  body transform. The weld is preserved (tests/session.rs:100-123).
- `missing_propellants` flood: fired decouplers enter the region (they
  stay with the parent side) while both child- and parent-traversal of
  the severed edge are blocked — matches readout's remove-on-fire model.
- `qual_factor` equal-knot handling: the earlier window always claims
  `units` equal to a duplicated knot position, so the 0/0 lerp is
  unreachable; output gate bounds the rest.
- `from_file` ordering: duplicates → transform → root existence →
  parentless/dangling/root-edge → reachability (unreachable ≡ cycle,
  since every node has a parent and the parent set is complete) →
  `next_id > max_id`. Complete and deterministic.
- `compose_upgrade_levels`: level overrun, unresolvable path, non-positive
  base/composed effect all typed; multiplier/delta fold order documented
  and self-consistent; `levels` retained as informational.
- Readout math: `dry_end` clamp (1e-9), `ratio > 1 && isp > 0` gate,
  `wet > 0` guard on TWR, `burn = ∞ → 0` on empty/zero demand — no NaN/∞
  escapes for resolver-produced inputs; `Isp_eff` weighting matches the
  doc's `ΣF/(g₀·Σṁ)`.
- `MissionProfile::evaluate` `reference_g` rescale
  (`twr_surface·G0/ref_g` with a `max(1e-9)` floor) is correct and
  NaN-safe.
