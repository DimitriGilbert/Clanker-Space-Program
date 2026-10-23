# Verification Report — Cluster C03 (csp-data: load.rs / validate.rs / datahash.rs)

Source report: `.review/session-deep1/review-report-03.md`
Verified against: `crates/csp-data/src/{load,validate,datahash,site,experiment,part,colony}.rs`, `crates/csp-sim/src/{contracts,research}.rs`, `crates/csp-bench/src/replay.rs`, `crates/csp-data/tests/*`, and the shipped `data/` tree. Empirical checks run where noted (rustc sort-order probe, `find data -type l`, prefix-collision scan of `data/`).

**Verdict summary: all 12 findings CONFIRMED.** No finding is dismissed. Line numbers in the original report match the current source exactly.

---

### Finding 1 [HIGH]: `bodies_ok` return value discarded — empty `bodies` validates green → `SimError::Fatal` — **CONFIRMED**

Directly answers the prompt's question: **yes, `bodies_ok` is really discarded, and yes, an empty `bodies: []` validates then Fatals.**

Evidence:
- `validate.rs:2885-2894` — the closure computes `let mut ok = !bodies.is_empty();` and returns `ok`, but the only error it pushes is `"candidate body is not in the system"` for *unknown* bodies. An empty slice iterates zero times and pushes nothing.
- All six call sites discard the return (`bodies_ok(bodies, out);` as a statement): `validate.rs:2912` (SatellitePlacement), `2940` (SurveyRun), `2961` (`RegimeSpace::Orbit`), `2969` (`RegimeSpace::Landed`), `2974` (Rescue), `2984` (ColonyResupply). Verified by reading the full match block (2903-2996); no other caller exists.
- `bool` is not `#[must_use]`, so this compiles silently.
- The validator's own spec comment at `validate.rs:3009-3011` states "candidate lists non-empty" — the check exists in prose but not in code.
- Sibling empty-list checks *do* push errors, confirming this is an anomaly: `exps.is_empty()` → `"survey candidates: no experiments authored"` (2932-2934), `modules.is_empty()` (2949-2951), `resources.is_empty()` (2985-2987).
- Consumer side confirmed at `crates/csp-sim/src/contracts.rs:239-241`: `bodies.get(pick(rng, bodies.len())).ok_or(SimError::Fatal("contract gen: empty body space"))`. `pick` (223-225) is `(next01(rng) * len as f64) as usize` → `pick(rng, 0) == 0` → `.get(0)` on an empty slice is `None` → `Fatal`. Identical guard at 261-267, 272-280, 289-291, 298-300, 310-316.
- Net effect: a template authored with `bodies: []` passes `validate_contracts` (nothing else gates body-list emptiness) and the first offer roll selecting it returns `SimError::Fatal` from `draw_params`. The report's severity and one-line fix are accurate.

### Finding 2 [MEDIUM]: Three cross-reference families never resolved — **CONFIRMED**

All three sub-claims verified:

1. **`ModuleDef::Experiment::kind` unchecked.** `check_module` at `validate.rs:2041-2053` destructures `ModuleDef::Experiment { data_units, situations, .. }` — `kind` falls into `..`. Runtime failure confirmed at `csp-sim/src/research.rs:705-706`: `self.experiments.get(experiment)` → `SimError::Recoverable("experiment: no def loaded for kind")`. The shipped data is clean (`data/parts/csp.science.canister.ron:43-48` references `exp.materials_study`, which exists at `data/experiments/exp.materials_study.ron`).
2. **`ModuleDef::Converter::recipe` unchecked.** `part.rs:586-595` defines `Converter { recipe: RecipeId, priority, power_w }`; `check_module` at `validate.rs:2055-2062` destructures `{ power_w, .. }` and checks only `power_w`. `check_modules`/`validate_parts` signatures (`validate.rs:2199`, `2356`) take `(part/loaded, res, out)` — no `LoadedRecipes` table is reachable, so the check could not be added inside the current signature. Contrast: the colony-side converter *does* resolve `recipe` against `recipe_ids` (`validate.rs:1589-1600`, via `validate_colonies`'s `recipes` parameter).
3. **`ExperimentCap.body` unchecked.** `validate_experiments(loaded)` at `validate.rs:2504` has no `sys`/body-set parameter; `cap.body` is read only as a `covered` key (2547). `cap_for` (`experiment.rs:63-69`) matches `c.body == *body` exactly, so a typo'd body id is dead data — confirmed. Shipped caps (`data/experiments/exp.materials_study.ron:14-17`: terrin/rask/duva/solen) all exist in `data/systems/home/system.ron:11` `bodies: ["solen","terrin","rask","duva"]`.

### Finding 3 [MEDIUM]: `data_set_hash` framing is non-injective — **CONFIRMED**

Directly answers the prompt's question: **yes, the framing is really non-injective.**

Evidence:
- `datahash.rs:72-74`: `h.update(rel.as_bytes()); h.update(&[0]); h.update(&canonicalize(&bytes));` — `0x00` terminates the path; nothing bounds the content. Same construction at `csp-bench/src/replay.rs:460-462`.
- The claimed collision replays exactly: Set A `{"a"→"", "bc"→"d"}` (sorted order `a`, `bc`) hashes `61 00 | 62 63 00 64`; Set B `{"a"→"bc\x00d"}` hashes `61 00 | 62 63 00 64` — identical byte streams → identical BLAKE3.
- `canonicalize` (`datahash.rs:132-141`) passes `bc\x00d` through verbatim: NUL is valid UTF-8, `.lines()` yields it whole, `trim_end` does not remove `\x00`. Non-UTF-8 content hashes raw, so binary content can carry the same ambiguity.
- `DATA_HASH_VERSION` exists and is pinned (`csp-data/src/lib.rs:47`, `= 1`), so the report's "cheap now, expensive after artifacts" framing is correct. The hash feeds `BuiltPart::data_set_hash` — confirmed consumers at `csp-sim/src/tech.rs:114,316-322`.

### Finding 4 [LOW]: `data_set_hash` follows symlinks; lossy non-UTF-8 names — **CONFIRMED**

- `datahash.rs:104` `entry.path().is_dir()` — `Path::is_dir` calls `fs::metadata`, which follows symlinks. A symlink to an ancestor gives unbounded recursion (each level pushes a `collect` frame → stack exhaustion, not a `DataHashError`); a symlink to an outside dir hashes external files under data-relative `rel` strings; a symlinked file hashes its target's bytes via `std::fs::read` at line 68.
- `datahash.rs:98` `entry.file_name().to_string_lossy()` — two distinct non-UTF-8 names (e.g. `\xff.ron`, `\xfe.ron`) collapse to the same `rel`; `out.sort()` (line 84) then orders the identical-key records in `read_dir` enumeration order → nondeterministic record order → nondeterministic hash.
- `collect` has no `file_type()` check anywhere (lines 88-115 read in full). Contrast with the error-handling asymmetry the report notes: `collect` propagates entry errors (94-97) while `collect_ron_files` swallows them.
- `find data -type l` in the repo is empty — verified; no symlinks today, so this is latent, matching the report's LOW rating.

### Finding 5 [LOW]: Loader ordering ≠ `data_set_hash` ordering — **CONFIRMED** (empirically)

- `collect_ron_files` (`load.rs:682-699`) sorts `Vec<PathBuf>` per level then recurses inline — equivalent to a global component-wise `PathBuf` sort. `hashed_files` (`datahash.rs:81-86`) sorts `/`-joined `String`s.
- Verified empirically with a rustc probe: `["a.ron", "a"].sort()` as `PathBuf` → `["a", "a.ron"]` (dir contents emit first → `a/z.ron` before `a.ron`); `["a/z.ron", "a.ron"].sort()` as `String` → `["a.ron", "a/z.ron"]` (`'.' 0x2E < '/' 0x2F`). The orders genuinely differ.
- The doc comments claiming the order *is* "the same order `data_set_hash` hashes them in" exist as cited: `load.rs:81-82` (LoadedParts), `196-199` (FsDataSource::ron_files), `432-434` (LoadedRecipes), `494-497` (LoadedExperiments), `558-560` (LoadedContracts — also present though not cited), `623-626` (LoadedTechTree, which additionally claims `merged` concatenates "in ascending-path file order"), `679-681` (collect_ron_files). The invariant is false; both orders are individually deterministic.
- No `X/` vs `X.ron` prefix collision exists in today's `data/` tree (verified by comparing `find data -type d` stems against `find data -name '*.ron'` stems — empty intersection), so there is no live divergence — consistent with the report's LOW.

### Finding 6 [LOW]: `collect_ron_files` drops entry errors; `load_parts` missing-dir divergence — **CONFIRMED**

(a) `load.rs:688`: `.filter_map(|e| e.ok().map(|e| e.path()))` — a per-entry `Err` (permission/IO) is silently dropped; the affected file vanishes from the load with no diagnostic, and `ron_files` still returns `Ok`. `datahash::collect` propagates the same error class (94-97) — the asymmetry is real.

(b) `load_parts_from` (`load.rs:711-721`) is confirmed as the only directory loader without an `exists(dir)` early-out — `load_recipes_from:467-471`, `load_experiments_from:531-535`, `load_contracts_from:595-599`, `load_techtree_from:663-665` all have it. On `FsDataSource` a missing dir → `read_dir` error → `LoadError::Io`. On `EmbeddedSource`, `ron_files` (267-276) returns `Ok(vec![])` for a missing dir (BTreeMap prefix filter matches nothing) → silently empty `LoadedParts`. Confirmed divergence.

### Finding 7 [LOW]: Per-entry `SiteDef.schema_version` never checked — **CONFIRMED**

- `site.rs:45-47` — `SiteDef.schema_version` documented "Must equal `SITE_SCHEMA_VERSION`"; shipped `data/systems/home/sites.ron:11` authors `schema_version: 2` per entry (verified).
- `load_sites_from` (`load.rs:386-395`) checks only `sites.schema_version` — the `SiteFile` field — against `SITE_SCHEMA_SUPPORTED`.
- `validate_sites` (`validate.rs:1304-1366`) iterates `s.id`, `s.body`, `s.latitude_rad`, `s.longitude_rad`, `s.altitude_m`, `s.launch`, `s.resource_yields` — `s.schema_version` is never read. A `schema_version: 99` entry loads and validates green. Confirmed.

### Finding 8 [LOW]: Site `display_name` LocKeys have no resolution check — **CONFIRMED**

- `site.rs:51` — `display_name: LocKey`; shipped keys `site.terrin-launch`, `site.terrin-downrange`, `site.rask-mines` exist in `data/loc/en.ron:84-86` (verified).
- `validate_loc_keys` (`validate.rs:1209-1230`) takes `(sys, loc)` only — it checks `system.epoch_note`, body `display_name`s, and biome legend names; `SiteFile` is not a parameter so sites cannot be covered.
- `validate_part_loc_keys` (2470), `validate_techtree_loc_keys` (2839), `validate_contract_loc_keys` (3139) all exist and are exercised by gate tests (`tests/parts.rs:99-104`, `tests/contracts.rs:46`, `tests/home_system.rs:60`). Colony names are asserted manually at `tests/colonies.rs:96-107` (verified — the loop checks `c.display_name` only). No `validate_site_loc_keys` exists and no test iterates `SiteDef::display_name` — grep over `crates/csp-data/{src,tests}` confirms. A typo'd site key ships clean and renders as a raw loc key. Confirmed.

### Finding 9 [LOW]: `validate_recipes` stamps `InvalidColony` with the recipe id in `site` — **CONFIRMED**

- `validate.rs:1379-1384` — the `bad` closure pushes `ValidationError::InvalidColony { site: id.to_string(), reason }` for every recipe failure (stem mismatch 1394, duplicate 1401, schema 1404, unknown/duplicate/negative resources 1409-1429, zero-output 1433).
- `Display` renders `InvalidColony` as `colony at site {site:?}: {reason}` (`validate.rs:770-772`) — a recipe error would print `colony at site "recipe.smelt": ...`. Confirmed.
- Reason-type asymmetry confirmed: `InvalidSite`/`InvalidColony` carry `reason: String` (enum lines ~508-525) while `BadModule`/`BadExperiment`/`BadContract`/`BadTechNode` carry `reason: &'static str` documented as "stable reason token" (e.g. 412-418). Confirmed — diagnostic quality issue only.

### Finding 10 [LOW]: Loader joins unvalidated authored strings as path components — **CONFIRMED**

- `load.rs:320` `dir.join(id.as_str())`, `328` `body_dir.join(&r.legend)`, `331` `body_dir.join(&r.png)` — all inside `load_system_from`, before any validation. `id_well_formed`/`check_id`/`MalformedId` live in `validate.rs:788-800` and run only inside `validate_system` — after the reads. Confirmed.
- A manifest id `".."` reads `<sys>/../body.ron` → outside the system dir; a `BiomeMapRef` png of `"../../x.png"` escapes the body dir. Path::join with `..` components does traverse (no normalization). Trusted-data context acknowledged; the report's "hardening, not live bug" framing is accurate.

### Finding 11 [LOW]: `check_power_collision` hard-codes `"electric_charge"`; colony drill bound diverges — **CONFIRMED**

- `validate.rs:1487`: `id.as_str() == "electric_charge"` — magic string; the canonical id lives only in `data/resources.ron:21`. All other occurrences of the string in `validate.rs` are inside `#[cfg(test)]` fixtures (3552, 4461, 4470, 4481) — no `pub const` exists.
- Bound divergence confirmed: colony-side `M::Drill::min_concentration` is checked only `is_finite() && >= 0.0` via the loop at `validate.rs:1579-1587`; part-side `check_drill_module` enforces `(0.0..=1.0).contains(min_concentration)` at `validate.rs:2109`. The colony field's doc at `colony.rs:102-104` says it "matches the part-side `ModuleDef::Drill::min_concentration`" — verified verbatim. Confirmed.

### Finding 12 [LOW]: SatellitePlacement peri/apo bands not cross-checked — **CONFIRMED**

- `validate.rs:2913-2922` — `periapsis_alt_m` and `apoapsis_alt_m` each pass through `range_ok` independently (`0 < lo <= hi`); no check relates the two bands.
- `contracts.rs:243-249` — draws `rp` then `ra`, and clamps `ra = rp` when `ra < rp`. With `periapsis_alt_m: (500_000, 600_000)`, `apoapsis_alt_m: (100_000, 200_000)` every draw clamps → `min_periapsis_alt_m == max_apoapsis_alt_m` → only degenerate circular-orbit offers, silently. Validates green today. Confirmed.

---

## "Notes verified as NOT problems" — re-verified, all hold

- `canonicalize` in `datahash.rs:132-141` and `replay.rs:510-519` are byte-for-byte identical logic (same `from_utf8` → `lines` → `trim_end` → `join("\n")`; raw verbatim otherwise). Holds.
- `check_tree_cycles` (`validate.rs:2726-2761`) read in full: `done` bounds pushes, every node is a DFS root, `visiting`/`stack` discipline is correct — terminates and is total. Holds.
- `tiny_png` mock: `validate.rs:3406-3408` documents "the inspector never checks CRCs" — the coupling is acknowledged in-source. Holds.
- BTreeMap/BTreeSet used throughout `validate_system`/`validate_contracts` — deterministic error order. Holds.
- Tests (`tests/parts.rs`, `colonies.rs`, `home_system.rs`, `contracts.rs`) load real `data/` via the real loaders — no stale mocks observed in the files examined. Holds.
- `data/eval-scenarios` exists and is not under `test-fixtures/` or `art/` → inside hash scope, consistent with the note. `data/art` does not exist today; `data/test-fixtures` does and is excluded by `datahash.rs:106`. Holds.
- `PERIOD_REL_TOL`/`libm` claim not exhaustively re-derived but consistent with everything observed; no counter-evidence found.

## Additional observation (not in the original report — informational only)

`ValidationError`'s `Display` impl ends with `_ => Ok(())` (`validate.rs:781` area). Every currently-declared variant does have an explicit arm (cross-checked the enum ~64-526 against the arm list ~530-776), so nothing renders empty today, but a future variant added without a Display arm would format as an empty string rather than a compile error or fallback. Not a defect in the reviewed change; worth a `debug_assert`/`unreachable!` or removing the wildcard if the enum is meant to stay exhaustive.

## Conclusion

12/12 findings **CONFIRMED** with direct source evidence; all cited line numbers are accurate. The two prompt questions resolve as: (1) `bodies_ok`'s return is discarded at all six call sites — an empty `bodies` list produces zero validation errors and then `SimError::Fatal("contract gen: empty body space")` at first offer draw; (2) the `path || 0x00 || content` framing is genuinely non-injective — the report's two-set collision produces identical BLAKE3 input streams by construction.
