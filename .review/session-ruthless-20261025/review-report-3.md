# Cluster 3 Review: csp-data validator + datahash

Reviewer: ruthless-c3 | Date: 2026-10-25 | Mode: STATIC ONLY (no cargo)

Files: crates/csp-data/src/validate.rs, crates/csp-data/src/datahash.rs, crates/csp-data/tests/*

---

### [SEVERITY: MEDIUM] Finding 1: Variant stat-multipliers are never checked for resolvability — a variant can make a part unbuildable while validation is green
**File**: crates/csp-data/src/validate.rs:2555-2572 (`check_variants`)
**Problem**: `check_variants` validates variant id, cost multiplier, stat-path uniqueness and multiplier positivity, but never asks whether the `StatPath` a variant multiplies actually exists on the part. The resolver requires exactly that: csp-phys/src/resolve.rs:40-42 ("every `StatPath` a variant names must resolve on the def, else `ResolveError::UnresolvableStatPath`").
**Evidence**: `for (stat, m) in &v.stat_multipliers { ... }` only checks duplicates and `m > 0`. Contrast `check_tree_tracks` (validate.rs:3513) which already calls `t.stat_path.base_value(&p.def)` for upgrade tracks — the identical predicate exists and is used 60 lines away, just not for variants.
**Impact**: A part authored with e.g. `stat_multipliers: [("control_torque_nm", 1.2)]` but no `Control` module passes every §8.2 gate (validate_parts green, all data tests green), then `resolve_part` fails with `UnresolvableStatPath` at build time — a player-facing error the CI gate exists to prevent. This is precisely the "loads fine but breaks sim invariants" class.
**Suggestion**: In the `stat_multipliers` loop add `if stat.base_value(part).is_none() { out.push(BadModule/…) }` (a new stable reason token like `"variant multiplies a stat the part does not carry"`); add a rejection test mirroring `delta_track_nonpositive_base_is_rejected`.

### [SEVERITY: MEDIUM] Finding 2: No loc-key validator exists for `colonies.ron` / `recipes/*.ron` — and the embedded-demo gate omits them entirely
**File**: crates/csp-data/src/lib.rs:114-120 (export list), crates/csp-view/tests/demo.rs:78-97
**Problem**: Every other LocKey-carrying category has a `validate_*_loc_keys` fn (system, site, part, resource, experiment, techtree, contract). `ColonyDef::display_name` (colony.rs:191) and `RecipeDef::display_name` (recipe.rs:45) have none.
**Evidence**: lib.rs exports no `validate_colony_loc_keys`/`validate_recipe_loc_keys`. The FS-side data is covered only by hand-rolled assertions in crates/csp-data/tests/colonies.rs:58-73 and 103-117 (the test itself admits "no dedicated validator exists for recipes"). Worse, demo.rs loads colonies.ron (line 39) and validates it structurally (line 66), but its `loc_cases` table (lines 78-97) lists exactly 6 categories — colony and recipe keys are unchecked for the embedded set, whose loc table is a *reduced* copy where a missing key is most likely.
**Impact**: An unresolved colony/recipe display key in the demo/web data set ships silently and renders as the raw key in the UI; on the desktop side the guarantee rests on ad-hoc test loops rather than the validator family the §8.2 "ID stability" row promises.
**Suggestion**: Add `validate_colony_loc_keys(&ColonyFile, &LocFile)` and `validate_recipe_loc_keys(&LoadedRecipes, &LocFile)` following the `validate_site_loc_keys` pattern, export them, wire them into demo.rs's `loc_cases`, and replace the hand-rolled loops in tests/colonies.rs.

### [SEVERITY: MEDIUM] Finding 3: SOI/orbit geometry is only half-checked — periapsis inside the parent and child SOIs swallowing parents/siblings pass
**File**: crates/csp-data/src/validate.rs:1276-1286 (`check_orbit_rules`)
**Problem**: The only geometry rule is "child apoapsis `a·(1+e)` < parent's authored `soi_m`". Three patched-conics invariants are missing:
 (a) child periapsis `a·(1−e)` ≥ parent `radius_m` — a moon with `a` inside the parent's surface validates green;
 (b) a body's own `soi_m` vs its orbital distance — `soi_m ≥ a·(1−e)` means the child's SOI contains its parent (and the star), so every vessel near the parent is permanently inside the child's SOI;
 (c) sibling SOI overlap — two children of one parent with overlapping SOIs make §2.6.6's transition scan ambiguous.
**Evidence**: grep of validate.rs shows `soi_m` consulted only at lines 1277-1285; nothing compares `soi_m` to `semi_major_axis_m`, `radius_m`, or sibling orbits. The sim consumes these as nested boundaries (crates/csp-sim/src/orbit.rs:227-256 `SoiCandidate` per body + parent; §2.6.6 scan) and has no defense against non-nested spheres beyond burning its sample budget.
**Impact**: Subtly-bad authored geometry (exactly the class this validator exists to catch per PLAN §3.0's "a parent's SOI exceeds every child's apoapsis" family) loads green and produces degenerate SOI behavior in-game: instant/oscillating SoiTransition events or a moon rendered inside its planet. Also note the existing rule is silently skipped when the parent has no authored `soi_m` — a planet with moons and no `soi_m` gets no orbit-geometry check at all.
**Suggestion**: In `check_orbit_rules` add: `periapsis = a·(1−e)`; error if `periapsis <= parent.radius_m + body.radius_m`; error if `soi_m` of a parented body `>= periapsis` (it must nest strictly inside its own orbit); and in a set-level pass, error when two siblings' `(apoapsis-reached SOI extents)` overlap. At minimum implement (a) and (b), which need no new data.

### [SEVERITY: LOW] Finding 4: Propellant-sum check skips reporting when the sum overflows to +Inf — validator and its sim-side mirror disagree
**File**: crates/csp-data/src/validate.rs:2794-2800 (`check_engine_module`)
**Problem**: `if sum.is_finite() && (sum - 1.0).abs() > PROPELLANT_SUM_TOL` — when the fractions are individually finite/positive but so large the sum overflows (`1e308 + 1e308 = +Inf`), `sum.is_finite()` is false and the error is silently not reported.
**Evidence**: Each fraction first passes `check_resource_list`/`part_positive` (finite, > 0 — `1e308` qualifies). csp-phys/src/resolve.rs:1193-1195 mirrors the rule correctly: `if !(sum.is_finite() && (sum - 1.0).abs() <= PROPELLANT_SUM_TOL) { return Err(...) }`.
**Impact**: The two gates diverge on the same def: `validate_parts` green, `resolve_part` fails with "propellant mass fractions do not sum to 1" at build time. Absurd authoring values required, but the condition is simply inverted relative to its mirror.
**Suggestion**: `if !(sum.is_finite() && (sum - 1.0).abs() <= PROPELLANT_SUM_TOL)` — same shape as resolve.rs.

### [SEVERITY: LOW] Finding 5: `surface_resources` map keys are never resolved against `resources.ron` — the only ResourceId cross-ref the validator skips
**File**: crates/csp-data/src/validate.rs:1243-1245 (`check_file_refs`)
**Problem**: `BodyDef::surface_resources: Vec<(ResourceId, ConcentrationMapRef)>` (system.rs:214) is checked only for png-name tameness (`check("surface_resources.png", &map.png)`); the `ResourceId` keys are never checked with `res.contains`, unlike every other resource reference in the data set (tanks, propellants, drills, builds, site yields, colony modules, balance's bench resource).
**Evidence**: validate.rs:1243 iterates `for (_, map) in &b.def.surface_resources` and discards the key. `validate_system` takes no `ResourceFile`, so nothing else could have checked it.
**Impact**: Today the field is schema-only and unconsumed (site.rs:71-77 documents `SiteDef::resource_yields` as the stand-in "unauthored to date"), so a typo'd resource id is dead authored data that ships green — by the validator's own standard ("an unreachable cap is dead authored data"). When PNG concentration maps are eventually wired, the check will have been forgotten.
**Suggestion**: Either add the resolution check now (a small `validate_system_resources(sys, res)` companion, or extend `check_file_refs`'s caller with an optional registry), or make the dormancy explicit in `check_file_refs`'s doc so the gap is a decision, not an oversight.

### [SEVERITY: LOW] Finding 6: An experiment def with an empty `caps` list validates green — a kind that can run nowhere
**File**: crates/csp-data/src/validate.rs:3180-3212 (`validate_experiments`)
**Problem**: Per-cap checks (body known, situations non-empty, cap positive, coverage non-overlapping) never establish that `caps` is non-empty. A def with `caps: []` passes.
**Evidence**: The doc on `BadExperiment` and the cap loop itself target unreachable *entries*; an empty list has no entries to be unreachable. research.rs:729 (`let Some(cap) = def.cap_for(...) else { ... }`) then yields nothing for every (body, situation) — the experiment can never produce data anywhere.
**Impact**: Dead authored data shipping green; a part's `Experiment` module referencing the kind validates (the kind exists) but can never pay out — the same trap `UnknownExperimentKind` exists to prevent, one level deeper.
**Suggestion**: `if d.caps.is_empty() { push(BadExperiment { reason: "caps is empty — the experiment can run nowhere" }) }`, mirroring the recipes' "recipe produces nothing" rule at validate.rs:1992-1994.

### [SEVERITY: LOW] Finding 7: datahash art-extension match is case-sensitive — an uppercase art file silently enters the shipping hash
**File**: crates/csp-data/src/datahash.rs:166-174 (`in_scope`)
**Problem**: `rest.rsplit('.').next()` is compared against the lowercase `ART_EXTENSIONS` list without case folding. `data/art/terrain.PNG` (easily produced on macOS/Windows checkouts) is *not* excluded and gets hashed.
**Evidence**: `if let Some(ext) = rest.rsplit('.').next() && ART_EXTENSIONS.contains(&ext)` — no `to_ascii_lowercase`.
**Impact**: A texture-only change to an uppercase-extension art file flips `data_set_hash`, which keys §7.7 drift warnings and the bugreport fingerprint — a false "the data set changed" signal on files the scheme explicitly declares presentation-only. Conservative direction (hashes more, never less), so LOW.
**Suggestion**: `ART_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())` (allocate-free via `eq_ignore_ascii_pattern` or a lowercase-bytes match), plus a one-line test with `art/x.PNG` excluded and `biomes.PNG`-style map still hashed.

---
## Honest quality verdict

1. validate.rs is genuinely strong: typed accumulating errors (never first-error abort), full-context diagnostics, first-def-wins duplicate convention documented, deny_unknown_fields everywhere, and ~70 rejection-style unit tests plus shipped-data gates — the gaps found are omissions, not sloppiness.
2. The most material hole is that the validator family and the csp-phys resolver were designed as mirror gates but two rules (variant stat-path reachability, Inf-sum propellants) exist only on the resolver side, so those defects surface in-game instead of at CI.
3. Loc-key coverage and orbit-geometry coverage each miss one category (colonies/recipes; SOI nesting/periapsis) — both cheap to close with the patterns already in the file.
4. datahash.rs is the best file reviewed: injective framing (with a regression test proving the retired collision), symlink/non-UTF-8 refusal, deterministic ordering, content-only hashing; only the case-folding edge remains.
5. No CRITICAL or HIGH findings: nothing lets panic-inducing or replay-falsifying data through today; verdict is REQUEST CHANGES on the three MEDIUMs, all fixable in well under 200 lines.
