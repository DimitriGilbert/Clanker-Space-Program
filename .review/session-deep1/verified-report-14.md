# Verified report — Cluster C14 (resolve / session / readout / profile)

Source of truth: `crates/csp-phys/src/{resolve,session,readout,profile}.rs`,
plus `crates/csp-phys/src/{part,graph,weld}.rs`,
`crates/csp-core/src/quat.rs`, `crates/csp-data/src/{part,validate}.rs`,
`crates/csp-sim/src/{manufacture.rs,lib.rs}`. Every cited line range was
re-read; each finding's mechanism was traced end-to-end.

Verdicts: **8 CONFIRMED, 0 DISMISSED.** Minor clarifications noted where
the report slightly over- or under-stated reachability.

---

## Finding 1 [MEDIUM] — `expected_hazard` drops invalid specs and reads NaN/∞ qual units as the best knot — **CONFIRMED**

Verified mechanics, all as reported:

- `profile.rs:206-208` — `if !spec.is_valid() { continue; }`: an invalid
  spec contributes exactly `0.0` to `lambda_total`, so
  `confidence() = exp(-Λ)` (profile.rs:251-257) inflates rather than
  fails. No counter, no `Result`, no worst-case substitution.
- `profile.rs:209-210` — `quals.get(&spec.family).map_or(0.0, |q| q.units())`
  never gates the record. `FamilyQualification::is_valid`
  (resolve.rs:327-332) exists and *is* called by the resolver
  (`resolve_reliability`, resolve.rs:1595-1600 → `InvalidQualification`)
  but not here.
- NaN/`+∞` units fall through `qual_factor` (resolve.rs:1091-1105):
  `units <= knots[0].qualification_units` is false for NaN and ∞, every
  `units <= b.qualification_units` window test is false, so the function
  returns `knots.last().hazard_multiplier` — the maximum-units end of the
  curve. Per `csp-data` `QualKnot` docs (part.rs:608-619), the first
  knot is the unqualified hazard factor and "lower is a better-proven
  family", so the last-knot reading is the fully-qualified (best)
  reading. **Clarification:** this affects `+∞` units too, not just NaN —
  slightly broader than reported. Strictly, "best" depends on a monotone-
  decreasing curve; the validator enforces only knots strictly increasing
  in units with multipliers ∈ (0,1] (validate.rs:2299-2320), not
  decreasing multipliers — but the documented convention is decreasing,
  so "most optimistic reading" holds for any sanely authored curve.
  Negative units are read pessimistically (≤ first knot → first knot).
- Resolver-produced specs can be invalid, exactly as claimed:
  `resolve_reliability` gates only *finiteness* on the passthrough fields
  (infant/dormant resolve.rs:1617-1622, regime threshold/multiplier
  1623-1632, failure-mode weights 1633-1638 — vacuous on an empty
  `failure_modes`), while `ReliabilitySpec::is_valid`
  (resolve.rs:1237-1256) demands `infant_mortality ∈ [0,1)`,
  `dormant_stress ∈ [0,1]`, non-empty `failure_modes` with positive
  weights, `threshold ≥ 0`, `multiplier ≥ 1`, `rated_burn_s` finite
  non-negative. An unvalidated def with `infant_mortality: 1.5`,
  `dormant_stress: 2.0`, `failure_modes: []`, a `threshold: -1` or
  `multiplier: 0.5` regime rule, or a NaN `rated_burn_s` (ungated in
  `resolve_module`, resolve.rs:1063) resolves successfully →
  `ReliabilitySpec::from_effective` (resolve.rs:1208-1232) carries it →
  `is_valid()` false → `expected_hazard` silently drops it.
- `is_valid` asymmetry at resolve.rs:1252 confirmed:
  `r.multiplier >= 1.0` lacks `is_finite()`, so `f64::INFINITY` passes
  while NaN fails. Consequence as reported: `stress *= ∞` → `Λ = ∞` →
  `confidence = 0` (conservative). **Clarification:** only reachable via
  a `ReliabilitySpec` not produced by `resolve_part` — the resolver gates
  multiplier finiteness at resolve.rs:1626, so resolved specs can't
  carry an ∞ multiplier.
- `is_valid`-as-refusal in the sim confirmed at both cites:
  `csp-sim/src/manufacture.rs:147`
  (`!craft.reliability.iter().all(|s| s.is_valid())` → `SimError::Recoverable`)
  and `csp-sim/src/lib.rs:4563`
  (`craft.reliability.iter().any(|s| !s.is_valid())` → same). The
  plan-side/sim divergence is real.

## Finding 2 [MEDIUM] — resolver under-gates its own output contract — **CONFIRMED**

Every listed ungated output was verified in source:

- `resolve_module` (resolve.rs:1044-1079) emits `isp_vac_s`, `isp_sl_s`,
  `rated_burn_s` × multipliers, `throttle_range`, `gimbal_deg`,
  `propellants` verbatim — zero validation. `isp ≤ 0`/NaN, negative
  `rated_burn_s`, inverted throttle range, negative/NaN propellant
  fractions all pass. The only engine-side gate anywhere is
  `thrust_vac_n` finiteness/`≥0` at resolve.rs:1412-1414 (inside the
  `Engine` arm of `resolve_weld_input`).
- The readout junk-row trace is exact: `isp_vac_s = 0` →
  `mdot = thrust/(0·G0) = ∞` (readout.rs:253) → `mass_flow_kg_s = ∞`
  (261) → row `isp_vac_s = thrust/(G0·∞) = 0` (263); demand `∞·frac`;
  `burn = avail/∞ = 0` → `burn_time_s = 0` (294-303);
  `propellant_burned_kg = (∞·0).min(avail) = NaN.min(avail) = avail` =
  the full attached tank contents (304-307 — `f64::min` discards NaN);
  `delta_v = 0` via the `ratio > 1 && isp > 0` gate (310). The row is
  exactly as self-contradictory as reported.
- Engine `propellants` ResourceIds: never checked against `resources.ron`
  — cloned verbatim (resolve.rs:1064); `propellant_mass`/
  `resolve_weld_input` gate only `Tank` capacities (resolve.rs:1355-1374,
  1448-1466). In `analyze`, an unknown propellant reads
  `available.get(r) → None → 0` → `burn = 0` — silent zero-Δv vs the
  typed `UnknownResource` a tank gets. The csp-data validator *does*
  gate engine propellant IDs + positive fractions summing to 1
  (`check_resource_list`, validate.rs:1722-1744, called at 2188-2195).
- `build_resources` amounts ungated: `u * f.mass` emitted unchecked
  (resolve.rs:1024-1028); only resource *existence* is checked
  (1712-1719).
- `resolve_envelopes` (resolve.rs:1540-1576): zero validation —
  `structural.max_*`, `thermal.heat_capacity/max_temp/emissivity/
  conduction`, aero curves, `attach_nodes.position_m`, and
  `size_m` (1750) all pass through ungated. Weld then refuses the same
  fields under `WeldError::InvalidPart` (weld.rs:425-449: heat capacity
  `>0`, max_temp `>0`, emissivity `(0,1]`, conduction `≥0`,
  structural `≥0` finite). Error-type split confirmed.
- Degenerate collider confirmed: `ColliderShape::Points([])` passes
  `is_valid` vacuously (part.rs:180-185 — `all()` on empty is `true`),
  `collider_inertia` returns `SymMat3::ZERO` for empty `Convex`
  (resolve.rs:996-998), and `collider_z_min` `None` only errors inside
  the `Engine` arm (resolve.rs:1410-1411) — a non-engine empty-`Convex`
  part resolves with a zero-vertex collider despite `InvalidStat`'s doc
  claiming "degenerate collider" (resolve.rs:571).
- `by_id` `BTreeMap` duplicate `part_id`: `.collect()` keeps the last
  def (resolve.rs:1807-1811) — confirmed.
- Doc/posture claims confirmed: `EffectivePart` doc "every number is
  finite" (resolve.rs:446-447) is falsified by NaN attach positions /
  envelopes / module fields; the defence-in-depth posture exists
  (resolve.rs:1611-1615 passthrough comment, 552-554) but is applied
  unevenly.

## Finding 3 [MEDIUM] — `compute_attach` never checks `position_m`; a NaN attach transform is storable and serializes to an unloadable file — **CONFIRMED**

Traced end-to-end:

- `compute_attach` (session.rs:997-1057) gates `direction`
  finiteness/`≥MIN_DIRECTION_NORM` (1032-1045) and `diameter_m`
  finiteness/match (1046-1055) — nothing checks `position_m`, and the
  `Ok(snap_transform(&pn, &cn))` result is returned unvalidated (1056).
- `snap_transform` (session.rs:684-688) computes
  `offset = p_p − rotation·p_c`; a non-finite `cn.position_m` or
  `pn.position_m` yields NaN offset. `.normalized()` only normalizes
  the quaternion (graph.rs:95-100) — NaN offset survives.
- `add_part` stores it unconditionally (session.rs:981-989);
  `reattach` likewise (session.rs:1099-1105). `set_transform` gates
  `transform.is_valid()` (session.rs:1205) — the asymmetry is real.
- `PartTransform::is_valid` = finite offset + unit rotation
  (graph.rs:122-124); a NaN-offset transform fails it.
- `validate` (session.rs:1386-1457) never inspects stored transforms —
  `is_launchable()` (session.rs:479-481) holds.
- `to_bytes` writes `n.transform` verbatim (session.rs:1580);
  `from_file` rejects the same record as `BadTransform`
  (session.rs:1627-1629). **A session can save a file it cannot reload —
  confirmed.**
- Weld rejects the transform at weld.rs:371-378 (`InvalidPart` /
  `NonUnitRotation`) — the later-layer, differently-typed failure the
  report describes.
- `snap_transform` is `pub` with no stated precondition in its doc
  (session.rs:679-683). A zero direction handed in directly:
  `from / from.norm()` → NaN axis (session.rs:667) — the fn-level gate
  only exists in `compute_attach`.
- Validator-side gate confirmed: `check_attach_nodes` calls
  `check_vec3_finite` on `attach_nodes.position_m`
  (validate.rs:1866; `BadAttachNode` variant at validate.rs:336-338 —
  the report's "336-344" cites the variant, the actual check is at 1866).
- Minor note: `pn.position_m`/`cn.position_m` can also go NaN via a
  stored NaN `built.scale` in a crafted file (`scaled_attach_node`
  multiplies, session.rs:634-644) — `check_built` runs at add-time for
  the child, and `validate` re-checks stored builds, so this second
  channel is narrower but consistent.

## Finding 4 [LOW] — `upgrade_levels_at_build` never gated at the session boundary — **CONFIRMED**

- `check_built` (session.rs:697-732) has exactly two arms: scale rule
  (703-724) and variant membership (725-730). No iteration of
  `built.upgrade_levels_at_build`.
- All session entry/mutation paths run only `check_built`: `add_root`
  (950), `add_part` (980), `set_scale` (1235), `set_variant` (1260);
  `validate` re-runs it (1397-1399). A `TrackLevels` with NaN/≤0
  `multiplier` or a `StatPath` the def lacks is accepted, serialized
  (`to_bytes` copies `built` verbatim, 1582), and reported launchable.
- `resolve_parts`/`to_part_graph` then fails at `fold_multipliers`
  (resolve.rs:886-898: `UnresolvableStatPath` /
  `InvalidStat` — the exact two conditions the report names).
- `StatPath::base_value` exists and is the right shared predicate
  (resolve.rs:773-775, 817) — the suggested fix is implementable.

## Finding 5 [LOW] — `apply_wear`/`wear_multiplier` can emit non-finite hazard — **CONFIRMED**

- `wear_multiplier` (resolve.rs:1121-1123): `1 + operating_s/base_mtbf_s`,
  no output gate; doc'd as infallible with input-only preconditions.
- `apply_wear` (resolve.rs:1133-1144) gates `usage.is_valid()` (1134)
  but writes `r.wear_multiplier` and
  `r.hazard_per_s = base_hazard · qual_factor · wear_multiplier` (1141-1142)
  unconditionally.
- Reachability confirmed: `base_mtbf_s = 1e-300` passes both gates
  (`>0` at 1589; `1/mtbf = 1e300` finite at 1605);
  `PartUsage::is_valid` (resolve.rs:382+) accepts `operating_s` up to
  `f64::MAX` (finite, ≥0). `1.8e308 / 1e-300` overflows → `wear_multiplier
  = ∞` → `hazard_per_s = ∞` on the `EffectivePart`, falsifying the type
  doc's "every number is finite" (resolve.rs:446).
- Display-only caveat confirmed: `WEAR_HAZARD_STUB` doc
  (resolve.rs:190-199) states the M9b sim model never reads these
  fields — "resolution-time display projections … no sim path reads
  them".

## Finding 6 [LOW] — propellant draw order is ascending `PartNodeId` — **CONFIRMED**

- Consumption loop (readout.rs:320-335) iterates `attached` — a
  `BTreeSet`, ascending `PartNodeId` — taking `have.min(need_units)`
  per tank until the burn is met. The `available` pool (282-291) is
  order-insensitive; only the drain attribution is ordered.
- Separation happens *after* the burn within the same stage iteration
  (341-359): `separated_ids` leave `attached` carrying their `tanks`
  remainder. Which tanks were drained is determined purely by id
  allocation order — confirmed arbitrary in exactly the sense reported
  (departing-tank-drained vs staying-tank-drained differ in all later
  stages' propellant while stage-`s` Δv is identical).
- The only documented edge in the module doc/inline comments is the
  massless-resource one (readout.rs:270-281) — draw order is
  undocumented, as claimed.
- The two-stage test (`two_stage_readout_separates_mass_and_propellant`,
  readout.rs:575-652) asserts `propellant_burned_kg == 2000.0` — *total*
  consumption of both tanks — so the ordering is indeed untested.

## Finding 7 [LOW] — empty session validates launchable but commit refuses — **CONFIRMED**

- `validate` (session.rs:1386-1457) iterates `self.nodes` for errors;
  `report_stage_gaps` returns early on empty `populated`
  (session.rs:749-751); `validate_staging` pushes nothing. Zero errors →
  `Validation::is_launchable() == true` (session.rs:479-481).
- `to_part_graph` returns `Err(SessionError::EmptySession)` at
  session.rs:1364; `analyze` short-circuits to a bare `CraftAnalysis`
  for empty sessions (readout.rs:148-150). The seam is real.

## Finding 8 [INFO] — `insert_stage` `u32::MAX` saturation silently merges stages — **CONFIRMED**

- `n.stage = n.stage.saturating_add(1)` (session.rs:1322): a part at
  `u32::MAX` cannot move while parts at `u32::MAX − 1` move into
  `u32::MAX` — two firing groups merge with no warning. Saturation is
  documented in the fn doc (session.rs:1317-1318).
- **Clarification:** the report says "only reachable via a crafted
  file" — it is also reachable through the public `set_stage(id,
  u32::MAX)` API (session.rs:1271-1278), which accepts any `u32`. A
  crafted file is not required, though realistic editor use makes it
  unlikely. INFO severity unaffected.

---

## Spot-checks of the report's "verified as correct" notes

All spot-checks pass:

- `DQuat::from_axis_angle` normalizes its axis internally
  (quat.rs:43-49) — `rotation_arc`'s unnormalized `f.cross(t)` is safe;
  zero-axis yields NaN only if callers feed it.
- `qual_factor` equal-knot 0/0 lerp unreachable: the earlier window
  claims `units == shared knot` (and a duplicated `knots[0]` is caught
  by the first early-return), confirmed at resolve.rs:1093-1104.
- `compose` renormalizes (graph.rs:54-59); `insert` returns the
  displaced node (graph.rs:191-193).
- Readout NaN-safety for resolver-produced inputs:
  `!burn.is_finite() → 0` (readout.rs:300-302), `dry_end` `max(1e-9)`
  (308), `ratio > 1 && isp > 0` (310), `wet > 0` on TWR (314).
- `MissionProfile::evaluate` `reference_g` rescale
  `twr·G0/ref_g.max(1e-9)` (profile.rs:138-142) — NaN-safe.
- `from_file` ordering (duplicates → transform → root → edges →
  reachability → `next_id`) verified at session.rs:1621-1660+.
