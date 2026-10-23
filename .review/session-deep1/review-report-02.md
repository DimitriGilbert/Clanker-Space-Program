# Deep Review — Cluster C02 (csp-data authored-data schemas)

Scope: `crates/csp-data/src/{lib,ids,loc,system,site,balance,resource,recipe,biome,techtree,part,colony,contract,experiment}.rs`, cross-read against `load.rs`, `validate.rs`, `datahash.rs`, the authored `data/` set, and the sim-side consumers needed to judge each invariant (csp-sim orbit/contracts/research/reliability/colonies, csp-phys resolve).

Verdict up front: the schema layer is unusually well-documented and the invariants it *does* check are tight and verified. The real problems are (a) serde's default tolerance for unknown keys silently dropping misspelled `Option`/`default` fields, (b) two whole classes of cross-reference/loc validation that simply don't exist — one of which is already hiding a live broken datum, (c) a contract-generation clamp that produces exactly the unsatisfiable offers its doc says it prevents, and (d) a set of smaller gaps in schema-version discipline, path-ref sanity, and id hygiene.

---

### [HIGH] Finding 1: Unknown RON keys are silently ignored — misspelled `Option`/`#[serde(default)]` fields drop data with zero diagnostics
File: `crates/csp-data/src/*.rs` — every `#[derive(Deserialize)]` def, e.g. `system.rs:165` (`BodyDef`), `site.rs:74` (`resource_yields`), `part.rs:753` (`PartDef`), `contract.rs:115` (`deadline_days`), `loc.rs:12`, `techtree.rs:75`

Problem
No data-file type carries `#[serde(deny_unknown_fields)]`, and serde's default is to *ignore* unknown struct keys. Combined with `Option<T>` fields implicitly deserializing to `None` when absent (and `#[serde(default)]` fields to their default), a misspelled key does not error — it silently erases the datum. The codebase knows this pattern: `csp-bench`'s scenario schema uses `deny_unknown_fields` with the explicit comment "a misspelled key is a parse error" (`crates/csp-bench/src/scenario.rs:325-328`), and `csp-server` uses it too (`dashboard.rs:70`); the convention was simply never applied to the data files it protects most.

Evidence
Verified empirically against the pinned `ron = "0.12.2"`:

```text
struct S { a: u32, b: Option<u32> }
ron::de::from_str::<S>("(a: 1, bb: Some(5))") -> Ok(S { a: 1, b: None })   // misspelled Option key: silently dropped
ron::de::from_str::<S>("(a: 1, zzz: 9)")   -> Ok(...)                     // unknown key: silently ignored
```

Concrete damage paths, all of which still validate green afterwards:
- `biome_mapx:` / `atmosphere:` misspelled on `BodyDef` → `None` → no biome/atmosphere checks run; a body silently loses its atmosphere or its entire surface-science context.
- `insolation_w_m2:` misspelled → `None` → `body_light` (`csp-sim/src/colonies.rs:411`) defaults `day_insolation_w_m2` to 0 → every solar module on that body produces nothing, silently.
- `soi_m:` misspelled → `None` → the child-apoapsis-inside-parent-SOI rule at `validate.rs:1061` is skipped entirely.
- `deadline_days:` misspelled on a contract → `None` → the offer never expires and its failure penalty can never fire.
- `resource_yields:` misspelled (it's `#[serde(default)]`, `site.rs:74`) → empty list → all drills at that site produce 0, legally.
- `orbit:`/`parent:` misspelled *is* caught (`OrbitMissing`/`RootNotStar`) — the dangerous case is precisely the fields whose `None` is meaningful.

Impact
Authored-data typos become silent semantic changes that survive `load_*` + `validate_*` + the data-set hash (the hash covers bytes, not semantics). This is the widest single integrity hole in the schema layer.

Suggestion
Add `#[serde(deny_unknown_fields)]` to every data-file type. It composes correctly with `#[serde(default)]` and with the sites v1→v2 upgrade (a v1 file simply lacks the newer field). If the project ever wants forward-compatible "ignore new fields" files, scope the decision per file type — but silent-by-default is the wrong posture for data that feeds the determinism fingerprint.

---

### [HIGH] Finding 2: Experiment and site `LocKey`s have no validator — and the seeded experiment's `display_name` is already unresolved
File: `crates/csp-data/src/experiment.rs:53`, `site.rs:51`; `lib.rs:108-113` (export list has `validate_loc_keys`, `validate_part_loc_keys`, `validate_techtree_loc_keys`, `validate_contract_loc_keys` — nothing for experiments or sites)

Problem
Two `LocKey`-carrying def families have *no* loc-resolution validator: `ExperimentDef::display_name` and `SiteDef::display_name`. The test layer papers over part of this (`tests/colonies.rs:58-102` hand-checks recipe and colony names), but nothing anywhere checks experiment or site names. This isn't theoretical — the shipped data is broken right now.

Evidence
`data/experiments/exp.materials_study.ron:10` declares `display_name: "exp.materials_study.name"`. `grep -c "exp.materials_study" data/loc/en.ron` → **0**. The key does not exist; nothing failed any gate. (The site names `site.terrin-launch` etc. happen to resolve — `en.ron:84-86` — but nothing enforces that they continue to.)

Impact
User-facing strings fall back to raw key text; the §8.2 "loc keys resolve" rule has no enforcement surface for two def families, and the live proof that the gap bites is already in `data/`.

Suggestion
Add `validate_experiment_loc_keys(&LoadedExperiments, &LocFile)` and extend `validate_sites` (or add a companion) to check `SiteDef::display_name`; add `"exp.materials_study.name"` to `en.ron`.

---

### [HIGH] Finding 3: Satellite-contract apoapsis clamp `ra = rp` produces effectively-unsatisfiable offers — the degenerate band the doc says it avoids
File: `crates/csp-data/src/contract.rs:138-150` (schema doc); `crates/csp-sim/src/contracts.rs:243-249` (draw + clamp), `:735-737` (predicate); `crates/csp-data/src/validate.rs:2895-2922` (validator checks each range independently, never cross-band, never vs SOI)

Problem
The draw is `rp ∈ per_band`, `ra ∈ apo_band`, then `if ra < rp { ra = rp }`. The completion predicate is exact float comparison with no tolerance: `r_p − R ≥ min_periapsis_alt_m && r_a − R ≤ max_apoapsis_alt_m` (`contracts.rs:737`). When the clamp fires, `min_periapsis == max_apoapsis == A`, so the predicate reduces to `r_p ≥ A+R && r_a ≤ A+R`, and since `r_a ≥ r_p` always, to `r_p = r_a = A+R` *exactly* — a perfectly circular orbit at a precisely drawn altitude. Achieving float-exact `a(1−e) = a(1+e) = A+R` from maneuver integration is measure-zero; the contract can never complete, the deadline fires, and `failure_penalty_funds` drains. The schema doc says the clamp exists because "a degenerate band would make an unsatisfiable contract" — the clamp creates exactly that.

Evidence
`contracts.rs:245-249`:
```rust
let mut ra = apoapsis_alt_m.0 + next01(rng) * (apoapsis_alt_m.1 - apoapsis_alt_m.0);
if ra < rp { ra = rp; }
```
Authored bands `data/contracts/contract.satellite.ron:15-16`: `per ∈ [80k, 250k]`, `apo ∈ [150k, 1500k]` → P(ra < rp) ≈ 2.2% of generated offers are dead on arrival. The validator checks each `(lo, hi)` independently (`0 < lo ≤ hi`) but never `per_lo < apo_hi` overlap, and never bounds the bands against the body's `soi_m` — a second way to author unsatisfiable satellite/part-test orbit contracts (periapsis above SOI is physically unreachable).

Impact
A steady trickle of offers a player cannot possibly complete; each one converts to a failure penalty. Also, bands authored entirely above a body's SOI pass validation while naming a regime no orbit can reach.

Suggestion
Clamp `ra = rp + δ` for a small documented `δ` (any `δ > 0` restores a nonzero-measure band), or draw `ra` from `[max(apo_lo, rp), apo_hi]` and require `apo_hi > per_lo` in the validator; add a validator rule bounding altitude bands against the drawn bodies' `soi_m`.

---

### [MEDIUM] Finding 4: `balance.ron` has no semantic validator at all — 40+ tunables and an unresolvable `ResourceId` load green
File: `crates/csp-data/src/balance.rs:55-248`, `load.rs:349-353`, `lib.rs:108-113` (no `validate_balance` exists)

Problem
`load_balance` checks only RON shape + `schema_version`. Unlike every other file family, there is no `validate_balance` — no finite/positive/range checks on any of ~45 fields, and `reliability_bench_resource: ResourceId` (`balance.rs:242`) is never resolved against `resources.ron`. Sim-side `is_valid()` gates exist as defense-in-depth (e.g. `reliability.rs:321-342`, `science_cfg.is_valid()` at `research.rs:668`), but they fail at world-build/command time, not at the data gate, and they don't cover the resource id at all.

Evidence
`reliability_bench_resource: "metall"` (typo) loads clean; `bench test` commands then always fail "insufficient site inventory" (`reliability.rs:1104-1115`) — the authored resource simply never matches an inventory row. `science_softcap_k: 0.0` loads clean → every `RunExperiment` yields `room · −expm1(0) = 0` → "key saturated" on every run → science dead silently. `bubble_radius_m: -1.0` loads clean.

Impact
The file the plan calls "ALL tuning" (`§8.3`) is the *least* validated file in the set — exactly backwards from the risk profile.

Suggestion
Add `validate_balance(&BalanceDef, &ResourceFile)` covering finite/positive on rates and radii, `[0,1]`-ish fields where applicable (`recovery_efficiency`, `abandon_recovery`, `reliability_dormant_stress_scale`, `reliability_degraded_thrust_factor`), and `res.contains(reliability_bench_resource)`.

---

### [MEDIUM] Finding 5: Schema-version discipline is applied unevenly — experiments/contracts skip the validator re-check, `SiteDef.schema_version` is dead, `LocFile.schema_version` is unenforced, and only sites implement the "previous version" band
File: `validate.rs:2504-2556` (`validate_experiments` — no version check), `validate.rs:3020-3133` (`validate_contracts` — no version check), `site.rs:45-47` (per-entry `schema_version` field — checked nowhere; `validate_sites` 1288-1367 checks `file.schema_version` only), `loc.rs:16` + `load.rs` (no `load_loc` exists — tests hand-parse `en.ron` with no version gate, e.g. `tests/home_system.rs:59`), `colony.rs:34` + `load.rs:418` (v2 exact-match only), `balance.rs:55` + `load.rs:351` (v11 exact-match only)

Problem
`system.rs:5-8` states the discipline: "the loader accepts the current version (and, once a v2 exists, the previous version with explicit defaults); the validator enforces it again so a hand-rolled fixture cannot smuggle a skipped check." Four violations:

1. `validate_experiments` and `validate_contracts` never re-check `schema_version` — every other validator does (`validate_recipes:1403`, `validate_parts:2385`, `validate_techtree:2620`, `validate_colonies:1511`, `validate_sites:1294`, `check_schema_versions:855`). A hand-constructed `LoadedExperiments`/`LoadedContracts` at any version validates green.
2. `SiteDef` carries its own `schema_version` field (`site.rs:45-47` — "Must equal `SITE_SCHEMA_VERSION`") that nothing checks; a `schema_version: 99` site inside a v2 file passes. (If per-entry versioning isn't wanted, the field shouldn't exist.)
3. `LocFile.schema_version` is enforced by no loader — there is no `load_loc` at all; every consumer hand-parses `en.ron`, skipping the version gate entirely (and bypassing `DataSource`, so the embedded/wasm path has no uniform loc path).
4. `COLONY_SCHEMA_VERSION` was bumped to 2 (`colony.rs:34`) and `BALANCE_SCHEMA_VERSION` to 11 with *no* supported band — unlike `SITE_SCHEMA_SUPPORTED` (`site.rs:36`), which is the one place §3.1's "current and previous with explicit defaults" is implemented. A v1 `colonies.ron` or v10 `balance.ron` fails with a confusing RON "missing field" error (the v2+ fields aren't `#[serde(default)]`) rather than `UnsupportedSchemaVersion`.

Impact
The version machinery is decoration wherever it matters most: version checks fire on the happy path, the validator's second layer has holes, and the only file type actually honoring the migration contract is sites. Old-version data files get parse errors that misattribute the cause.

Suggestion
Re-check `schema_version` inside `validate_experiments`/`validate_contracts`; either check `SiteDef.schema_version` per entry or drop the field; add a `load_loc[_from]` that version-checks `LocFile`; decide whether colonies/balance get a `*_SCHEMA_SUPPORTED` band (with `#[serde(default)]` on additive fields) or document explicitly that their versions are hard-break — either way, stop letting the checker's output masquerade as a different error class.

---

### [MEDIUM] Finding 6: File-reference strings (`BiomeMapRef`, `ConcentrationMapRef`, `TerrainDef.heightmap`) are unvalidated paths — `..` escapes the handed directory, violating the loader's stated I/O contract
File: `system.rs:127` (`png: String`), `system.rs:130` (`legend: String`), `system.rs:117` (`heightmap`), `system.rs:140` (`ConcentrationMapRef.png`), `load.rs:328-332` (`body_dir.join(&r.legend)` / `join(&r.png)` with no sanity), `lib.rs:13` ("the loader's only I/O is reading the directory it is handed")

Problem
`body.ron` paths are joined verbatim: `png: "../../whatever.png"` or an absolute path reads outside the system directory — the exact thing `id_well_formed` (`validate.rs:788-795`) exists to prevent for IDs, applied to directory names but not to the refs that actually become `read` paths. `surface_resources` additionally gets *no* validation at all: `check_body_fields` (`validate.rs:1013-1030`) never touches it, so duplicate `ResourceId`s and dangling `png` refs pass green (resource membership can't be checked there since `validate_system` takes no `ResourceFile` — but duplicates and path hygiene could).

Evidence
`load.rs:331-332`: `let png_path = body_dir.join(&r.png); let png = src.read_bytes(&png_path)?;` — any `..` component escapes `body_dir`; an absolute `r.png` replaces the prefix entirely. `validate.rs:1157-1203` checks the PNG *bytes* (`inspect_indexed_png`) but never the path that produced them.

Impact
Violates the crate's own determinism-domain contract (`lib.rs:13`); a malformed or adversarial ref reads arbitrary files (and under `EmbeddedSource`, reads other embedded paths silently). Duplicate/unresolvable `surface_resources` rows are dead authored surface that nothing flags.

Suggestion
Validate that `png`/`legend`/`heightmap` refs are plain relative file names: non-empty, no `.`/`..`/absolute components, no separators (or at minimum no `ParentDir`/`RootDir` components); add a `surface_resources` pass (dup resource ids + path sanity + resource membership once the signature admits the table).

---

### [MEDIUM] Finding 7: Part-side `Converter.recipe` and `Experiment.kind` references are never resolved against their registries
File: `part.rs:589` (`ModuleDef::Converter.recipe`), `part.rs:567` (`ModuleDef::Experiment.kind`); `validate.rs:2041-2053` (Experiment: only `data_units`/`situations` checked), `validate.rs:2055-2061` (Converter: only `power_w` checked), `validate.rs:2356` (`validate_parts` signature — takes `LoadedParts` + `ResourceFile`, no recipes, no experiments)

Problem
Every other ID reference in the data set gets a cross-file resolution check: `tech_requirement` → merged tree, contract bodies → system, colony `Converter.recipe` → recipes (`validate.rs:1594`), drill/tank/propellant/BOM resources → `resources.ron`. The two part-side references have none: `validate_parts` isn't given the registries, and no other validator takes both sets. A typo'd `kind: "exp.material_study"` or `recipe: "recipe.smelt_orr"` passes every data gate; the failure only surfaces at run time as a `Recoverable` error ("no def loaded for kind", `research.rs:705-707`) or a converter that silently registers no recipe (`colonies.rs:545`).

Evidence
`validate.rs:2041-2053` — the `Experiment` module arm checks `data_units` positivity and `situations.is_empty()`; `kind` is never inspected. `check_module` has no access to `LoadedExperiments`/`LoadedRecipes` because `validate_parts`'s signature doesn't take them.

Impact
Dead modules and dead science data in shipped parts, discoverable only in-game. The "every part reachable"-class rules exist precisely to prevent this category.

Suggestion
Extend `validate_parts` (or add a follow-up pass) to take `&LoadedExperiments` and `&LoadedRecipes` and resolve `Experiment.kind` / `Converter.recipe`.

---

### [LOW] Finding 8: `SituationMask` spare bits are never validated — a mask with only bits 6–7 set passes `is_empty` and names zero real situations
File: `part.rs:396-453` (`SituationMask`), `validate.rs:2047` (`situations.is_empty()` only), `validate.rs:2534` (cap `situations.is_empty()` only)

Problem
`SituationMask` is a transparent `u8`; bits 6–7 are unassigned. `is_empty()` checks `self.0 == 0`, so `situations: 192` (0xC0) is non-empty, passes validation, and `situations()` yields zero real situations. On a `ModuleDef::Experiment` the module can never run (`contains()` is false for all six); on an `ExperimentCap` the entry covers nothing and the overlap check iterates an empty set — a silently dead cap. `mask ⊆ ALL` is never checked anywhere.

Suggestion
Validator rule `mask.0 & !SituationMask::ALL.0 == 0` on `ModuleDef::Experiment::situations` and `ExperimentCap::situations`.

---

### [LOW] Finding 9: Track validator's "resolves on every family part" coverage doesn't cover `Delta` composition — `base > 0` is required but `is_some()` is all that's checked
File: `validate.rs:2815-2825` (`t.stat_path.base_value(&p.def).is_none()` → error), `techtree.rs:156-158` (`Delta`), `csp-phys/src/resolve.rs:832-839` (`if delta != 0 { if !(base.is_finite() && base > 0.0) { err } }`), `part.rs:822-848` (`base_value`)

Problem
`compose_upgrade_levels` computes `(base + ΣΔ)/base` and rejects `base ≤ 0` at manufacture — but the validator's family-coverage check accepts any `base_value(...) != None`, which includes `0.0`. `ModuleDef::Control.torque_nm` may be authored `0.0` (`validate.rs:2018` allows `>= 0`), so a part in a `ControlTorqueNm`-tracked family with a zero-torque module validates green, then every `resolve_built_part` for it fails at manufacture. `Multiplier` tracks are fine (zero base × m is legal-ish); only `Delta` needs the stronger predicate.

Suggestion
When a track's levels contain ≥1 `Delta`, require `base_value(def) > 0` (finite) on every family part — the same predicate the resolver enforces.

---

### [LOW] Finding 10: `check_id` (`id_well_formed`) coverage gaps — `SiteId`, `RecipeId`, `ExperimentId`, `BiomeId`, `ExperimentCap.body`, module-side `ExperimentId`s are unchecked
File: `validate.rs:788-801` (the rule), `validate.rs:1304+` (sites — `s.id` never `check_id`ed), `validate.rs:1386+` (recipes — `d.id` never `check_id`ed), `validate.rs:2507+` (experiments — `d.id`, `cap.body` never `check_id`ed), `validate.rs:1165+` (biome legend `e.id`), `validate.rs:2004+` (`ModuleDef::Experiment.kind`)

Problem
`id_well_formed` (lowercase `[a-z0-9._-]`, not `.`/`..`) is applied to bodies, resources, parts, variants, families, tech ids, contract ids, and unlock refs — but not to site, recipe, experiment, or biome ids. Recipe/experiment ids are constrained to real file stems by the stem check (so `/` can't appear), but `My Recipe` or `UPPER.Case` are valid stems and pass — quietly breaking the project-wide ID convention and any downstream assumption that IDs are URL/path/loc-key tame. `cap.body` is also never checked for membership in the system body set (a typo'd body id is a silently dead cap — `cap_for` simply never matches).

Suggestion
Apply `check_id` uniformly to every authored ID field; consider having `validate_experiments` take the system (like `validate_contracts` does) to check `cap.body` membership.

---

### [LOW] Finding 11: `validate_recipes` stamps all errors as `InvalidColony`; `BadResourceTable` is declared but never constructed
File: `validate.rs:1379-1384` (`bad` pushes `ValidationError::InvalidColony { site: id, reason }` for every recipe error), `validate.rs:407-410` + `709` (`BadResourceTable` variant + `Display` — zero construction sites)

Problem
Recipe validation failures report through the colony error variant with the recipe id in the `site` field — diagnostics misattribute the file family, and the `BadResourceTable` variant intended for `resources.ron` structural errors is dead. CI/editor consumers of the typed list can't distinguish a recipe error from a colony error.

Suggestion
Add a recipe-specific variant (or reuse `BadResourceTable` after renaming it for the general file-malformed case); delete or wire `BadResourceTable`.

---

### [LOW] Finding 12: Duplicate-resolution direction differs between the sim's `by_id()` (first-wins) and `validate_colonies`' recipe map (last-wins)
File: `load.rs:446-452` (`LoadedRecipes::by_id` — `entry(...).or_insert`, first def wins), `validate.rs:1519-1523` (`recipe_defs` built via `.map().collect()` — `BTreeMap::collect` keeps the *last* duplicate)

Problem
On a duplicated recipe id, the sim installs the first def while `validate_colonies`' power-collision check inspects the last def's `inputs`. `LoadedExperiments::by_id`/`LoadedContracts::by_id` use the documented first-wins convention; the colony validator diverges on the one edge case where "which copy" matters.

Suggestion
Build `recipe_defs` with `entry().or_insert()` to match `by_id()`.

---

### [LOW] Finding 13: `collect_ron_files` silently drops `read_dir` entry errors; `data_set_hash` propagates them — the loaded set can diverge from the hashed set under I/O errors
File: `load.rs:688` (`.filter_map(|e| e.ok().map(|e| e.path()))`), `datahash.rs:93-97` (entry errors → `DataHashError::Io`)

Problem
A transient `read_dir` entry error (permission race, disappearing entry) is swallowed by the loader — the file silently isn't loaded — while the hasher fails loudly. The part/recipe/experiment set consumed by the sim and the set covered by the `data_set_hash` can disagree without any diagnostic. Related asymmetry: `load_parts_from` (`load.rs:711-721`) has no missing-directory early return, so `FsDataSource` errors on a missing `data/parts/` while `EmbeddedSource::ron_files` returns an empty set — the optional-table loaders (`load_recipes`, `load_experiments`, `load_contracts`, `load_techtree`) all treat missing as empty for both sources.

Suggestion
Propagate entry errors in `collect_ron_files` (match `datahash::collect`); decide whether `load_parts` should follow the empty-set convention — and if not, make the embedded source agree.

---

### [LOW] Finding 14: Colony `Drill.min_concentration` lacks the part-side `[0, 1]` bound — `> 1` validates green and silently caps the drill
File: `validate.rs:1579-1587` (colony Drill — only `v >= 0` on rate/min_concentration/power), vs `validate.rs:2109` (part-side Drill — `(0.0..=1.0).contains(min_concentration)`)

Problem
The colony `Drill` doc says `min(1, concentration / min_concentration)` scales the rate. `min_concentration: 5.0` passes the colony validator, then `yield/5` caps production at ≤20% forever — a silently degraded drill that the equivalent part-side authoring would reject. Bounds that differ between the two Drill schemas for the same-named field invite authoring errors.

Suggestion
Apply the `[0, 1]` bound colony-side (or document deliberately different semantics).

---

### [LOW] Finding 15: Biome-map validation is header-only — no check that pixel indices in use have legend entries
File: `validate.rs:1179-1202` (`inspect_indexed_png` + legend-index-vs-palette check only), `biome.rs:155-236` (`decode_indexed_png` exists and is already exercised by tests)

Problem
`check_biomes` verifies legend indices are unique and `< palette_len`, but never decodes pixels — so a map whose pixels use an index with *no legend entry* (e.g. painted index 5, legend covers 0–3) passes validation, and every pixel painted with it is an unnamed biome region. The decode machinery is in-crate and test-proven; running it at validation time costs nothing.

Suggestion
In `check_biomes`, `decode_indexed_png` the map once and verify every used index has a legend entry (it also catches out-of-palette pixel indices and corrupt IDATs the header check can't see).

---

## Minor notes (not counted above)

- `ColonyDef.stores` entries for resources with no `Store`/`Battery` capacity silently clamp to 0 at founding (`colony.rs:190-193` documents the clamp) — a validator warning would catch the dead row.
- `OceanDef.datum_radius_m` is positivity-checked but never compared to `radius_m` (`validate.rs:1027-1029`) — an ocean datum above the body's radius passes.
- `TechUnlocks` derives `Default` but its fields lack `#[serde(default)]`, so every node must spell all three empty lists (`data/techtree/home.ron` does) — verbose, not wrong; pairs with Finding 1's `deny_unknown_fields` suggestion.
- `inspect_indexed_png` takes the last `PLTE` on duplicates (`biome.rs:113-118`); PNG spec allows only one — harmless leniency.
- `datahash::collect` (`datahash.rs:104`) follows symlinked directories via `entry.path().is_dir()` — a symlink loop in `data/` recurses unboundedly; `to_string_lossy` on non-UTF-8 names makes the re-read fail honestly.
- `EmbeddedSource`/`FsDataSource` divergence on a missing `data/parts/` (Finding 13) may be intentional — parts aren't optional the way experiments are — but the two sources should agree.
- `ValidationError::SchemaVersion`'s `expected` in `load_sites_from` (`load.rs:393`) reports only the max supported version ("loader knows 2") even when v1 is also accepted — cosmetic.
- `KeplerElements`' parabolic `a := −p` convention (`system.rs:28-35`) is documented correctly and implemented consistently in `csp-sim::orbit` (`orbit.rs:691-692`, `535-538`): `r_p = |a|/2` and `n = √(μ/|a|³)` check out against `p = 2·r_p` for `e = 1`. No issue.
- `data_set_hash` and `csp_bench::replay::fixture_hash` canonicalization (`datahash.rs:132-141` vs `replay.rs:510-519`) are line-for-line identical — the pinned scheme holds.
