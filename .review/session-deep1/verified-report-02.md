# Verified Report — review-report-02 (csp-data authored-data schemas)

Method: every finding checked against source in `crates/csp-data`, `crates/csp-sim`, `crates/csp-phys`, `crates/csp-bench`, `crates/csp-view`, and the authored `data/` set. The ron-0.12.2 silent-drop claim was additionally verified **empirically** by compiling and running a probe test against the workspace's pinned `ron = "0.12.2"` (probe file created, run, and removed; see "Verification actions" at the end).

**Bottom line: 14 of 15 findings CONFIRMED; 1 finding (F4) CONFIRMED in substance but with one evidence example wrong in mechanism; 2 small factual corrections (F4's `science_softcap_k` example, F6's `EmbeddedSource` parenthetical). The three HIGH findings are all real and correctly characterized.**

---

## Finding 1 — [HIGH] Unknown RON keys silently ignored → **CONFIRMED**

- No `deny_unknown_fields` exists anywhere in `crates/csp-data` (grep: only `crates/csp-bench/src/scenario.rs:325-328` with the exact "a misspelled key is a parse error" comment, and `crates/csp-server/src/dashboard.rs:70,92,106`). The convention exists in the codebase and was never applied to data files.
- **Empirically verified** against the workspace-pinned `ron = "0.12.2"` (`Cargo.toml:95`, `Cargo.lock` resolves 0.12.2): a probe test `struct S { a: u32, b: Option<u32> }` parsed `(a: 1, bb: 5)` as `Ok(S{a:1, b:None})` and `(a: 1, zzz: 9)` as `Ok(...)` — both silent, exactly as reported.
- Concrete damage paths all verified in source:
  - `insolation_w_m2` misspelled → `None` → `body_light` `day_insolation_w_m2: env.insolation_w_m2.unwrap_or(0.0)` at `csp-sim/src/colonies.rs:411` → solar rate `day_insolation_w_m2 * area_m2 * efficiency = 0` at `colonies.rs:591`. Silent dead solar, confirmed.
  - `soi_m` misspelled → `None` → the child-apoapsis-vs-parent-SOI check at `validate.rs:1061-1070` is `if let Some(soi)`-gated and skips entirely.
  - `atmosphere`/`biome_map` misspelled → `None` → `check_atmosphere`/`check_biomes` skipped at `validate.rs:843-848`; the body silently loses its atmosphere/science surface context.
  - `deadline_days: Option<f64>` (`contract.rs:115`) misspelled → `None` → `else if let Some(tick) = deadline` at `csp-sim/src/contracts.rs:707` never schedules `ContractDeadline` → offer never expires, penalty unreachable.
  - `resource_yields` is `#[serde(default)]` (`site.rs:74-75`) → misspelled → empty → drill concentration lookup `input.concentrations.get(resource).copied().unwrap_or(0.0)` at `csp-sim/src/colonies.rs:613` → 0 output, legally.
  - `orbit`/`parent` misspelled on non-star bodies *is* caught: `OrbitMissing` (validate.rs:958-962) and `RootNotStar` (validate.rs:945-949). Report's nuance is accurate.
- Shipped `data/` is currently typo-free on these fields (verified all `body.ron` files carry `soi_m`/`insolation_w_m2`/`biome_map`/`surface_resources` correctly spelled) — the hole is latent, not live, but is the widest integrity gap as claimed. `deny_unknown_fields` composes fine with `#[serde(default)]` and the sites v1→v2 upgrade path (`load.rs:386-389` relies on the missing field defaulting to `[]`).

## Finding 2 — [HIGH] No loc validators for experiments/sites; live broken datum → **CONFIRMED**

- `lib.rs:108-113` exports `validate_loc_keys`, `validate_part_loc_keys`, `validate_techtree_loc_keys`, `validate_contract_loc_keys` — nothing for experiments or sites.
- `ExperimentDef::display_name: LocKey` (`experiment.rs:53`) and `SiteDef::display_name: LocKey` (`site.rs:51`) are checked nowhere: `validate_experiments` (validate.rs:2504-2556) and `validate_sites` (validate.rs:1288-1367) never touch `display_name`.
- **Live broken datum confirmed**: `data/experiments/exp.materials_study.ron:10` declares `display_name: "exp.materials_study.name"`; `grep -c "exp.materials_study" data/loc/en.ron` → **0** (en.ron contains no `exp.*` keys at all). Nothing failed any gate — `crates/csp-data/tests/experiments.rs` runs `validate_experiments` with no loc check.
- Report's hedge is accurate: `tests/colonies.rs:58-102` hand-checks *recipe* and *colony* display names only; shipped `sites.ron` names (`site.terrin-launch`/`terrin-downrange`/`rask-mines`) do resolve at `en.ron:84-86` by luck, unenforced.
- Additional context found: `apps/demo/data/` ships `exp.materials_study.ron` with the same key but contains **no loc file at all** — the embedded/wasm path has no loc table to resolve against regardless.

## Finding 3 — [HIGH] `ra = rp` clamp → effectively-unsatisfiable satellite offers → **CONFIRMED**

- Draw and clamp verified: `csp-sim/src/contracts.rs:243-249` — `rp` drawn from `periapsis_alt_m`, `ra` drawn from `apoapsis_alt_m`, `if ra < rp { ra = rp }`.
- Predicate verified: `contracts.rs:735-737` — `rp - radius >= *min_periapsis_alt_m && ra - radius <= *max_apoapsis_alt_m`, strict f64 comparisons, no tolerance. When clamped, `min == max == A`, and since `r_a ≥ r_p` for bound orbits (`eccentricity < 1.0` required at :729), satisfaction requires `e = 0` and `a = A + R` *bit-exact* — measure-zero via maneuver integration. Effectively unsatisfiable, confirmed.
- Authored bands confirmed: `data/contracts/contract.satellite.ron` — `periapsis_alt_m: (80_000, 250_000)`, `apoapsis_alt_m: (150_000, 1_500_000)`. Reported P(ra < rp) ≈ 2.2%: recomputed as `∫₁₅₀ₖ²⁵⁰ₖ (x−150k)/1350k · (1/170k) dx ≈ 0.0218` — **2.2% of generated satellite offers are dead on arrival**.
- Impact chain confirmed: `deadline_days: Some(45)` → `ContractDeadline` scheduled (`contracts.rs:707-708`) → `fire_contract_deadline` charges `failure_penalty_funds: 4_000` via `charge_funds` (`contracts.rs:921-934`) and increments `facts.failed`.
- Validator gap confirmed: `validate_contract_space` `range_ok` (validate.rs:2895-2922) checks `0 < lo ≤ hi` per band independently; the satellite arm never checks cross-band overlap, and no arm receives SOI data at all (`body_ids: &BTreeSet<&str>` only). A band entirely above `soi_m` names a regime the sim's own SOI-transition machinery (`csp-sim/src/orbit.rs` `next_soi_event`/`apply_soi_transition`) makes unreachable — the predicate requires `o.parent == body` (:729), which an orbit beyond SOI cannot retain.
- Doc-confirmed: `contract.rs:138-141` says the clamp exists because "a degenerate band would make an unsatisfiable contract" — the clamp manufactures exactly that band.

## Finding 4 — [MEDIUM] No `validate_balance` → **CONFIRMED** (with corrections)

- No `validate_balance` exists anywhere (`lib.rs:108-113` export list, `grep` for `fn validate_balance` → nothing). `load_balance` (load.rs:349-353) checks only RON shape + `BALANCE_SCHEMA_VERSION` (= 11, balance.rs:55). ~45 fields, zero semantic gate.
- `reliability_bench_resource: ResourceId` (balance.rs:242) is never resolved against `resources.ron` — `ReliabilityConfig::is_valid()` (csp-sim/src/reliability.rs:321-342) checks finiteness/ranges of numeric fields but not the resource id. A typo'd id → `inventory.get(&cfg.bench_resource)` → `unwrap_or(0.0)` → every bench test fails `"insufficient site inventory"` (reliability.rs:1104-1115). **Confirmed.**
- **Correction 1**: the `science_softcap_k: 0.0` example is wrong in mechanism. `ScienceConfig::is_valid()` (research.rs:261-270) requires `softcap_k > 0.0` and is checked at the head of `RunExperiment` (research.rs:668-670) — so `0.0` fails loudly as `Recoverable("science: invalid config")` on *every* run. Science dies, but loudly and at command time — never reaching the claimed `room · −expm1(0) = 0` → "key saturated" path (research.rs:729-733). The report itself noted `is_valid()` gates exist "as defense-in-depth"; this example just fails to fire silently.
- **Correction 2**: `bubble_radius_m: -1.0` does load clean, but nothing currently consumes `bubble_radius_m` in `csp-sim` (grep: only `crates/csp-data/tests/balance.rs:22` asserts `> 0.0` on the shipped file). It's a dead tunable today — latent, not an active hazard.
- Substance stands: the file the plan calls "ALL tuning" is the least-validated file; the ResourceId hole has no defense anywhere; the `is_valid` gates run at command time, not at the data gate, and coverage is per-config (economy.rs:201-204 covers `recovery_efficiency`/`abandon_recovery` ∈ [0,1]; reliability and science configs cover their own numerics).

## Finding 5 — [MEDIUM] Uneven schema-version discipline → **CONFIRMED** (all four points)

1. `validate_experiments` (validate.rs:2504-2556) — no `schema_version` check. `validate_contracts` (validate.rs:3020-3133) — no check either, **and its own docstring claims "schema_version current" (line 3004)** — the doc lies. Every other validator re-checks: `validate_recipes:1403`, `check_part:2385`, `tree_identity:2620`, `validate_colonies:1511`, `validate_sites:1294`, `check_schema_versions:855`. A hand-constructed `LoadedExperiments`/`LoadedContracts` at any version validates green.
2. `SiteDef.schema_version` (site.rs:45-47, doc "Must equal SITE_SCHEMA_VERSION") — checked nowhere; `validate_sites:1294` checks `file.schema_version` only. Shipped sites each carry `schema_version: 2` (`data/systems/home/sites.ron`).
3. `LocFile.schema_version` (loc.rs:16) — **no `load_loc` exists at all** (grep load.rs: none). All consumers hand-parse `en.ron` (`tests/colonies.rs:39`, `tests/contracts.rs:52`, `tests/home_system.rs:59`, `tests/parts.rs:102`, `tests/techtree.rs:41-42`) — version never checked, `DataSource` bypassed.
4. `COLONY_SCHEMA_VERSION = 2` (colony.rs:34) and `BALANCE_SCHEMA_VERSION = 11` (balance.rs:55) are exact-match only in the loader (`load.rs:418`, `load.rs:351`), with no `serde(default)` on the v2/v11-added fields — a v1 `colonies.ron` (v1 `Drill` lacked `min_concentration`) or v10 `balance.ron` (lacks `reliability_*`) fails at `read_ron` with a RON "missing field" error *before* `check_version` runs — misattributing the cause, exactly as reported. `SITE_SCHEMA_SUPPORTED` (`site.rs:36`, `load.rs:389-395`) is the sole implementation of the §3.1 band.

## Finding 6 — [MEDIUM] Unvalidated file-reference paths → **CONFIRMED** (with one correction)

- `load.rs:328-332` — `body_dir.join(&r.legend)` / `body_dir.join(&r.png)` verbatim into `read_string`/`read_bytes`; `..` components escape `body_dir` under `FsDataSource`, an absolute `png` replaces the prefix. Nothing validates the refs — `check_body_fields` (validate.rs:1013-1030) never touches `biome_map`'s path strings or `surface_resources` at all; `check_biomes` (1157-1203) inspects PNG *bytes*, never the path. `lib.rs:13`'s contract — "the loader's only I/O is reading the directory it is handed" — is genuinely violated.
- `surface_resources` has zero consumers and zero checks — confirmed dead authored surface (only `Vec::new()` fixtures reference it).
- `heightmap` (`TerrainDef.heightmap`, system.rs:117): unvalidated, but **currently not a live I/O path** — no loader reads it; `csp-view/src/terrain.rs:101-104` only logs a warning when `Some`. Latent issue, correctly flagged.
- **Correction**: the report's parenthetical "under `EmbeddedSource`, reads other embedded paths silently" is wrong. `EmbeddedSource::read_bytes` does `self.files.get(path)` on a `BTreeMap<PathBuf, _>` keyed by exact embedded paths — a `..`-containing `PathBuf` does not normalize to another key (`Path` equality is component-wise; `a/../b` ≠ `b`), so escapes fail `NotFound`, they do not silently cross-read. The `FsDataSource` escape stands.

## Finding 7 — [MEDIUM] Part-side `Converter.recipe` / `Experiment.kind` unresolved → **CONFIRMED**

- `ModuleDef::Converter.recipe: RecipeId` (part.rs:589) and `ModuleDef::Experiment.kind: ExperimentId` (part.rs:567).
- `check_module` — Experiment arm (validate.rs:2041-2053) checks `data_units` positivity and `situations.is_empty()` only, `kind` skipped via `..`; Converter arm (2055-2061) checks `power_w` only. `validate_parts(loaded, res)` signature (2356) takes no recipe/experiment registries.
- Asymmetry confirmed: colony-side `Converter.recipe` IS resolved (`validate.rs:1594`); contract-side `ModuleReq::Experiment` and survey refs ARE resolved against experiment ids (2953-2957, 2936-2938).
- Runtime consequences confirmed: `Experiment.kind` typo → `self.experiments.get(experiment)` → `Recoverable("experiment: no def loaded for kind")` (research.rs:705-707); `Converter.recipe` typo → `if let Some(rdef) = recipes.get(recipe)` silently registers no consumer rows (colonies.rs:545-549) and `module_productions` `.unwrap_or_default()` yields no outputs (colonies.rs:628-638) — a silently dead converter.

## Finding 8 — [LOW] `SituationMask` spare bits unvalidated → **CONFIRMED**

- Transparent `u8` mask (part.rs:400-402), bits 0-5 assigned (405-426); `is_empty` is `self.0 == 0` (430-432); `situations()` enumerates via `contains` over the six real situations (441-453).
- Validators check `is_empty()` only: `validate.rs:2047` (module), `2534` (cap). `mask & !ALL == 0` checked nowhere.
- `situations: 192` (0xC0) passes → module `contains()` false for all six → "cannot run in this situation" (research.rs:688-691); cap covers nothing real → `cap_for` never matches → "no cap authored for context" (research.rs:720-723). Dead module/dead cap, confirmed.

## Finding 9 — [LOW] `Delta` track base `> 0` not covered by family check → **CONFIRMED**

- `validate.rs:2815-2825`: coverage check is `t.stat_path.base_value(&p.def).is_none()` → error; `Some(0.0)` passes.
- `compose_upgrade_levels` (csp-phys/src/resolve.rs:832-839): `if delta != 0.0 { if !(base.is_finite() && base > 0.0) { InvalidStat } }` — manufacture-time error.
- `ModuleDef::Control.torque_nm >= 0` is legal per validator (validate.rs:2018); `base_value` returns `Some(*torque_nm)` for `ControlTorqueNm` (part.rs:844-847) — a zero-torque `Control` part in a Delta-tracked family validates green, then every `resolve_built_part` fails at manufacture. Confirmed. Multiplier-only tracks skip the base check (`delta == 0` arm) — report's "legal-ish" hedge is accurate.

## Finding 10 — [LOW] `check_id` coverage gaps → **CONFIRMED**

- `check_id`/`id_well_formed` at validate.rs:788-801. Applied to: system/star/body ids (901-914), resource ids (1259), variant ids (1966), part ids (2362), part family/tech_requirement (2391-2392), tech node/track ids (2628/2635), facility + contract_type unlocks (2713/2716), track.family (2779), contract ids (3061).
- **Not** applied to: `SiteId` (validate_sites: dup-check only, 1304-1311), `RecipeId` (validate_recipes: stem/dup only, 1386-1402), `ExperimentId` (2507-2525), `BiomeId` legend entries (1165-1177), `ModuleDef::Experiment.kind` (2041-2053, `..` pattern).
- `cap.body` additionally gets no membership check: `validate_experiments` takes only `LoadedExperiments` (2504) — a typo'd body id is a dead cap (research.rs:720-723). Report's claim that recipe/experiment ids are stem-constrained (no `/` possible) but `UPPER.Case`/`"my id"` pass is accurate — IDs are `serde(transparent)` `String` newtypes with no inherent constraints (ids.rs:18-54).

## Finding 11 — [LOW] Recipe errors stamped `InvalidColony`; `BadResourceTable` dead → **CONFIRMED**

- `validate_recipes`' `bad` closure pushes `ValidationError::InvalidColony { site: id, reason }` for every recipe error (validate.rs:1379-1384).
- `BadResourceTable` appears exactly twice: declaration (407-410) and `Display` (709-711). Zero construction sites — dead variant, confirmed.

## Finding 12 — [LOW] First-wins (`by_id`) vs last-wins (validator map) → **CONFIRMED**

- `LoadedRecipes::by_id` uses `entry().or_insert()` — first def wins, documented (load.rs:441-452). The sim installs exactly this map: `csp-view/src/lib.rs:691-697` `sim.set_recipes(load_recipes(...).by_id()...)`.
- `validate_colonies` builds `recipe_defs` via `.map().collect()` into `BTreeMap` — **last** duplicate wins (validate.rs:1519-1523), and the EC power-collision check reads `rdef.inputs` from that map (1604-1612). On a duplicated recipe id, sim and validator inspect different defs. `LoadedExperiments::by_id` (507-513) and `LoadedContracts::by_id` (571-579) share the first-wins convention — the divergence is real and localized exactly as reported.

## Finding 13 — [LOW] `collect_ron_files` swallows entry errors; `load_parts` missing-dir asymmetry → **CONFIRMED**

- `collect_ron_files` (`load.rs:688`): `.filter_map(|e| e.ok().map(|e| e.path()))` — `read_dir` entry errors silently dropped; `datahash::collect` (`datahash.rs:93-97`) propagates them as `DataHashError::Io`. Loaded set can diverge from hashed set under I/O errors — confirmed.
- `load_parts_from` (711-721) has no `!src.exists(dir)` early return → `FsDataSource` errors via `read_dir`; `EmbeddedSource::ron_files` returns `Ok(empty)` (267-276). The optional-table loaders (`load_recipes_from:467`, `load_experiments_from:531`, `load_contracts_from:595`, `load_techtree_from:663`) all early-return empty for both sources — confirmed asymmetry.

## Finding 14 — [LOW] Colony `Drill.min_concentration` missing `[0,1]` bound → **CONFIRMED**

- Colony validator: `("min_concentration", min_concentration)` checked only `v.is_finite() && *v >= 0.0` (validate.rs:1579-1587). Part-side: `(0.0..=1.0).contains(min_concentration)` (2109). Same field name, different bounds — confirmed.
- Sim semantics confirmed: `frac = (conc / min_concentration).clamp(0.0, 1.0)` (colonies.rs:614-615) — site yields are authored ≤ 1 (`validate_sites:1359`), so `min_concentration: 5.0` permanently caps output at ≤ 20%. `0` reads as "any nonzero yield → full rate" (616-620) — the unbounded side is sim-tolerated, the `>1` side is the real trap.

## Finding 15 — [LOW] Biome-map validation is header-only → **CONFIRMED**

- `check_biomes` (validate.rs:1157-1203): legend index/id dedup + `e.index < palette_len` via `inspect_indexed_png` — never decodes pixels.
- `decode_indexed_png` (biome.rs:155-236) exists in-crate, is exercised by `authored_biome_maps_decode` (312-336) over the shipped PNGs, catches out-of-palette pixel indices (line 225) and corrupt IDATs. A pixel painted with an unlegended in-palette index passes the current validator — an unnamed biome region. Confirmed.

## Minor notes — all verified

- `ColonyDef.stores` clamp documented at colony.rs:190-193 ✓
- `OceanDef.datum_radius_m` positivity-checked only, never vs `radius_m` (validate.rs:1027-1029) ✓
- `TechUnlocks` derives `Default` (techtree.rs:117), no `#[serde(default)]` fields; `data/techtree/home.ron` spells `track_levels: []` etc. explicitly ✓
- `inspect_indexed_png` last-PLTE-wins (biome.rs:113-118 — no dup check like IHDR has at 83-84) ✓
- `datahash::collect` recurses through symlinked dirs via `entry.path().is_dir()` (datahash.rs:104 — `Path::is_dir` follows symlinks); `to_string_lossy` at :98 makes non-UTF-8 names fail honestly on re-read ✓
- `load_sites_from` reports `expected` as max supported only (load.rs:393) — cosmetic ✓
- `KeplerElements` `a := −p` parabolic convention consistent: `orbit.rs:534-547` (`r_p = 0.5·p`, `n = √(μ/p³)`) — no issue ✓
- `canonicalize` identical in `datahash.rs:132-141` and `csp-bench/src/replay.rs:510-519` ✓

## Verification actions

- Read `crates/csp-data/src/{lib,ids,loc,system,site,balance,resource,recipe,biome,techtree,part,colony,contract,experiment}.rs`, `load.rs`, `validate.rs` (all relevant sections), `datahash.rs`.
- Read consumers: `csp-sim/src/{contracts,colonies,research,reliability,orbit,economy}.rs`, `csp-phys/src/resolve.rs`, `csp-view/src/{lib.rs,terrain.rs}`, `csp-view/tests/demo.rs`, `csp-bench/src/{scenario,replay}.rs`.
- Inspected authored data: `data/contracts/*.ron`, `data/experiments/*.ron`, `data/loc/en.ron`, `data/balance.ron`, `data/resources.ron`, `data/systems/home/sites.ron`, `data/systems/home/*/body.ron`, `data/techtree/home.ron`, `apps/demo/data/experiments/*.ron`.
- Empirically verified Finding 1's mechanism: created `crates/csp-data/tests/ron_probe_tmp.rs` asserting `(a:1, bb:5)` → `b:None` and `(a:1, zzz:9)` parses; `cargo test -p csp-data --test ron_probe_tmp` → **1 passed**; probe file deleted afterward. No other files changed.
