# Verified Report 3: csp-data validator + datahash

Verification agent: v3 | Date: 2026-10-25 | All findings re-checked against HEAD (1e8c6da) source.

Concurrent-edit check: datahash.rs was last touched by b644a73 ("feat(art): real assets"), which only appended `"csar"` to `ART_EXTENSIONS` — no case folding was added, so no finding is fixed by concurrent edits. validate.rs was last touched by e169c08 (Phase 12), before this review.

---

### Finding 1: Variant stat-multipliers never checked for StatPath resolvability — CONFIRMED
**Original**: `check_variants` checks only id/dup/positivity; a variant naming a stat the part lacks passes CI and fails `resolve_part` with `UnresolvableStatPath`.
**Verification**:
- validate.rs:2555-2572: the `stat_multipliers` loop checks only `DuplicateVariantStat` (2557) and `m > 0` finite (2564). No `base_value` call. Existing variant tests (validate.rs:4606, 4615) cover only dup and nonpositive multipliers — none cover resolvability.
- Resolver side is exactly as claimed: resolve.rs:2148-2177 `resolve_built_part` passes `&variant.stat_multipliers` into `fold_multipliers`, which at resolve.rs:962-968 returns `UnresolvableStatPath { source }` for any path failing `stat_path_resolves` (= `StatPath::base_value(def).is_some()`, resolve.rs:849-851). So the variant-resolvability gate exists only on the resolver.
- The fix predicate exists and is used for the parallel case: validate.rs:3513 (`check_tree_tracks`) calls `t.stat_path.base_value(&p.def)` for upgrade tracks, with its own doc note that it mirrors `compose_upgrade_levels` (resolve.rs:893-899). Note: the original's "60 lines away" is wrong — `check_tree_tracks` is ~950 lines away — but the substance (predicate already in the file, applied to tracks, not variants) is accurate.
- Impact confirmed: the def loads, all data tests pass, the failure surfaces at manufacture/build via a typed resolve error.

### Finding 2: No loc-key validator for colonies/recipes; embedded-demo gate omits them — CONFIRMED (with colony-demo nuance)
**Original**: Every other LocKey-carrying category has a `validate_*_loc_keys` fn; colony/recipe display keys are unchecked by any validator, and demo.rs's `loc_cases` omits both categories.
**Verification**:
- lib.rs:114-120: loc-key validator exports are exactly system/site/part/experiment/techtree/contract (+ generic `validate_loc_keys`/`validate_loc`). `grep -n "pub fn validate_.*loc" validate.rs` returns the same six — no colony/recipe variant exists anywhere in the crate.
- colony.rs:191 and recipe.rs:45 both carry `pub display_name: LocKey`.
- Desktop side is covered only by hand-rolled test loops: tests/colonies.rs `shipping_recipe_loc_keys_resolve` (its own doc comment admits "no dedicated validator exists for recipes, so the gate asserts it directly") and `home_colony_loc_keys_resolve`. These run against repo `data/` only.
- demo.rs:78-97: `loc_cases` has exactly 6 categories — colony and recipe absent, while recipes load at demo.rs:38 and colonies at demo.rs:39. The embedded recipes all carry loc keys (apps/demo/data/recipes/*.ron: `recipe.smelt_ore.name`, `recipe.isru_propellant.name`, `recipe.electrolysis.name`) and no test anywhere gates them (grep of crates/csp-view/tests finds no recipe/display_name loc check). A missing key in the reduced embedded loc table ships silently.
- Nuance: the embedded set ships **no colonies.ron at all** (apps/demo/data/systems/home/ contains only sites.ron, solen/, terrin/; the FILES manifest in csp-view/src/demo.rs has no colonies entry; load.rs:499-505 returns an empty `ColonyFile` for a missing file). So the demo-gate half is live today only for recipes; the colony half is a latent gap (real the moment a colony is embedded). Validator-family gap itself is fully real.

### Finding 3: Orbit geometry half-checked — periapsis-in-surface, child-SOI-swallows-parent, sibling overlap all pass — CONFIRMED
**Original**: Only rule is child apoapsis < parent's authored `soi_m`; no periapsis-vs-radius, no own-SOI-vs-own-orbit, no sibling-overlap check; rule silently skipped when parent has no `soi_m`.
**Verification**:
- validate.rs:1251-1299 `check_orbit_rules`: beyond positivity/finite/eccentricity-range/period-consistency, the only geometry rule is 1276-1286 (`apoapsis = a*(1+e) >= parent.soi_m` → `SoiTooSmall`).
- `grep -n "soi_m" validate.rs`: consulted only at 1206 (self positivity) and 1277-1285 (the one rule). Nothing compares `soi_m` to `semi_major_axis_m`, `radius_m` (BodyDef field exists, system.rs:187), or a sibling's orbit. No `periapsis` computation exists anywhere in the file (only contracts' `periapsis_alt_m` band and `arg_periapsis_rad` finite check); no `sibling` set-level pass (`check_graph` does tree-structure only: star-root, parent-exists, orbit-present).
- Silent skip confirmed: line 1277 `if let Some(soi) = p.def.soi_m` — a parent with moons and `soi_m: None` gets no orbit-geometry check at all (only its own `soi_m` positivity check, which is also skipped for None). Shipped-data tests don't backstop this: tests/home_system.rs has zero `soi` assertions.
- Sim assumption confirmed: csp-sim/src/orbit.rs:222-258 — `SoiCandidate` per body + parent, nested-sphere signed-distance scan, with degenerate radii only caught as `Recoverable`, not non-nested geometry.

### Finding 4: Propellant-sum check skips reporting when sum overflows to +Inf — CONFIRMED (line cite corrected)
**Original**: Validator's `sum.is_finite() && (sum-1).abs() > TOL` is inverted relative to resolve.rs's mirror; +Inf sum validates green, resolve errors.
**Verification**:
- validate.rs:2794-2800: `if sum.is_finite() && (sum - 1.0).abs() > PROPELLANT_SUM_TOL` — with `sum = +Inf`, `is_finite()` is false, condition false, no error pushed.
- Preconditions hold: check_resource_list (2296-2317) → part_positive (2278-2288) accepts any finite positive fraction; `1e308 + 1e308 = +Inf` (IEEE754 overflow), and the sum check runs after the per-entry gate (2793 → 2794).
- Mirror confirmed at resolve.rs:1190-1192 (not 1193-1195 as the original cites): `if !(sum.is_finite() && (sum - 1.0).abs() <= PROPELLANT_SUM_TOL) { return Err(...) }` — +Inf errors there. Same def, divergent gates. LOW is the right severity (needs absurd values), but the inversion is real.

### Finding 5: `surface_resources` ResourceId keys never resolved — CONFIRMED
**Original**: Only ResourceId cross-ref the validator skips; keys discarded at the png-name check.
**Verification**:
- validate.rs:1243-1245: `for (_, map) in &b.def.surface_resources { check("surface_resources.png", &map.png) }` — key pattern-matched away.
- system.rs:214: `pub surface_resources: Vec<(ResourceId, ConcentrationMapRef)>`.
- validate.rs:1014: `pub fn validate_system(sys: &LoadedSystem)` — takes no ResourceFile, so nothing else in the fn could check it.
- Dormancy confirmed: repo-wide grep finds `surface_resources` only in the schema (system.rs), the validator's png check + tests (validate.rs:5460), site.rs:71's doc ("schema-only ... `SiteDef::resource_yields` ... stand-in"), and bench/test constructors. No sim/view consumption. Typo'd id ships green today.

### Finding 6: Experiment with empty `caps` validates green — CONFIRMED
**Original**: Per-cap checks never establish `caps` is non-empty; a kind that can run nowhere passes.
**Verification**:
- validate.rs:3150-3180 (pre-loop: schema version, stem match, dup id, transmit_ratio band) and 3181-3212 (per-cap loop) contain no `caps.is_empty()` gate; the function ends at 3213. An empty list simply has no entries to check.
- experiment.rs:68-74: `cap_for` iterates `self.caps` → returns `None` for every context when empty; research.rs:729-732 turns that into `SimError::Recoverable("experiment: no cap authored for context")` — the experiment can never pay out anywhere.
- The suggested mirror exists verbatim: validate.rs:1992-1994 `if !d.outputs.iter().any(|(_, q)| *q > 0.0) { bad(..., "recipe produces nothing", ...) }`.

### Finding 7: datahash art-extension match is case-sensitive — CONFIRMED (concurrent edit did not fix)
**Original**: `rest.rsplit('.').next()` compared to lowercase `ART_EXTENSIONS` without case folding; `art/x.PNG` gets hashed, flipping `data_set_hash` on art-only changes.
**Verification**:
- datahash.rs:166-174 `in_scope`: `ART_EXTENSIONS.contains(&ext)` — no `to_ascii_lowercase` anywhere in the file; list is all-lowercase (line 30-32).
- Concurrent-edit check: `git show b644a73 -- crates/csp-data/src/datahash.rs` is a one-line diff adding `"csar"` to `ART_EXTENSIONS` only. Case folding was not added; the gap persists at HEAD (1e8c6da). Not FIXED-BY-CONCURRENT-EDIT.
- No test covers uppercase extensions (the file's own test module has only the framing-injectivity test; no datahash test file exists under tests/). Conservative direction (hashes more, never less) — LOW agreed.

---

## Clean-claim spot-checks (report's "honest quality verdict")

- Typed accumulating errors: confirmed. Every validator takes `out: &mut Vec<ValidationError>` and pushes; a grep for `return Err` in validate.rs finds zero hits — no first-error abort anywhere.
- No vacuous gates: spot-checked the bands that could plausibly be dead (`EccentricityOutOfRange` 0.0..1.0 at 1265, propellant-sum, situation-mask, caps-overlap at 3204-3210, `soi_m` positivity at 1206) — all are real predicates exercised by rejection tests. No always-true gate found.
- "~70 rejection-style tests": validate.rs has 54 `#[test]`s plus 7 shipped-data test files — approximately right.
- deny_unknown_fields: present in 12 of csp-data/src's definition modules — consistent with the claim.

## Summary

7 confirmed, 0 dismissed, 0 fixed by concurrent edit.

- All three MEDIUMs are real omissions with the exact resolver-mirror/coverage-gap shape described; each fix is small and the pattern to copy exists in the same file.
- Two citation inaccuracies in the original, neither material: Finding 1's "60 lines away" (actually ~950, validate.rs:3513 vs 2555) and Finding 4's "resolve.rs:1193-1195" (actually 1190-1192).
- One impact nuance: Finding 2's demo-gate half is live only for recipes today (the embedded set ships no colonies.ron — load.rs:499-505 returns an empty set for the missing file); the colony half is latent until a colony is embedded, but the missing-validator claim itself is fully accurate.

Verdict: REQUEST CHANGES stands (Findings 1-3).
