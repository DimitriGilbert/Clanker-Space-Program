# Code Review — Cluster C03 (csp-data: load.rs / validate.rs / datahash.rs + tests)

Reviewed: `crates/csp-data/src/{load,validate,datahash}.rs` (all lines), `crates/csp-data/tests/{balance,colonies,contracts,experiments,home_system,parts,techtree}.rs`, plus cross-checks against `csp-sim`/`csp-bench` consumers (`contracts.rs`, `research.rs`, `replay.rs`) and shipped `data/` files.

Findings are ordered by severity. Overall the code is in good shape: deterministic containers throughout, the validator is pure, canonicalization is byte-exact, and gate tests run against real data rather than mocks.

---

### [HIGH] Finding 1: `bodies_ok`'s empty-candidate check is dead — validator green → `SimError::Fatal` at offer generation

**File:** `crates/csp-data/src/validate.rs:2885-2894`, call sites 2912, 2940, 2961, 2969, 2974, 2984

**Problem:** `bodies_ok` computes `ok = !bodies.is_empty()` and returns it, but every call site discards the return value (`bodies_ok(bodies, out);`). The closure itself only pushes an error for *unknown* bodies — an **empty** `bodies` candidate list pushes nothing. Every contract param space that carries a `bodies` list (SatellitePlacement, SurveyRun, `RegimeSpace::Orbit`, `RegimeSpace::Landed`, Rescue, ColonyResupply) therefore validates green with an empty candidate set, contradicting the validator's own spec comment at line 3011 ("candidate lists non-empty").

**Evidence:** The generator hard-fails on exactly this case: `crates/csp-sim/src/contracts.rs:241` — `bodies.get(pick(rng, bodies.len())).ok_or(SimError::Fatal("contract gen: empty body space"))` (same pattern at lines 267, 281, 292, 300, 312). `pick(rng, 0)` returns 0 → `.get(0)` on empty → `Fatal`. Sibling empty-list checks *do* push errors: `exps.is_empty()` → "survey candidates: no experiments authored" (2932), `resources.is_empty()` → "resupply candidates: no resources authored" (2985), `modules.is_empty()` (2949). Only `bodies` was wired through the discarded flag.

**Impact:** An authored template with `bodies: []` (or a template left empty by a merge/edit) passes `validate_contracts`, so the §8.2 CI gate goes green — and the first offer roll that selects that template returns `SimError::Fatal` from `draw_params`. This is precisely the class of runtime failure the data validator exists to prevent.

**Suggestion:** Push `bad("candidate body list is empty")` inside `bodies_ok` when `bodies.is_empty()` (and drop the now-pointless return value, or use it if callers genuinely need it). One line restores the intended rule.

---

### [MEDIUM] Finding 2: Three cross-reference families are never resolved by any validator

**File:** `crates/csp-data/src/validate.rs:2041-2053` (`Experiment` arm), `:2055-2062` (`Converter` arm), `:2504-2557` (`validate_experiments`)

**Problem:** The validator's stated job includes resolving cross-file references (it checks `tech_requirement` → tree, contract candidates → bodies/experiments/resources, dock-port nodes → attach nodes), but three references of the same class are unchecked:

1. `ModuleDef::Experiment { kind, .. }` — `check_module` destructures `{ data_units, situations, .. }` and never touches `kind`. A part whose module names `exp.nonexistent` validates green; the failure only surfaces at runtime (`crates/csp-sim/src/research.rs:706`: `SimError::Recoverable("experiment: no def loaded for kind")`).
2. `ModuleDef::Converter { recipe, .. }` — only `power_w` is checked; the `RecipeId` is never resolved against `data/recipes/` (`part.rs:588-595`).
3. `ExperimentCap.body` — `validate_experiments` doesn't take the system, so a cap authored for a typo'd body id is dead data that `cap_for` can never return (`experiment.rs:34,66`).

**Evidence:** `check_modules` signature is `(part, res, out)` — no experiment or recipe table is reachable. `validate_experiments(loaded)` has no `sys` parameter while `validate_contracts` resolves the same body refs via `body_ids`. The shipped data happens to be clean (`csp.science.canister.ron` → `exp.materials_study` exists), so nothing trips today.

**Impact:** Dangling refs in newly authored data ship silently and fail (or produce dead caps/unresolvable modules) only at runtime — the same gap class as `tech_requirement`, which *is* validated.

**Suggestion:** Give `validate_parts` (or a new cross-validator invoked by the gate tests) the `LoadedExperiments`/`LoadedRecipes` tables and check `kind`/`recipe`; pass `sys` (or a body-id set) to `validate_experiments` and reject caps naming unknown bodies.

---

### [MEDIUM] Finding 3: `data_set_hash` record framing is non-injective — different file sets hash identically

**File:** `crates/csp-data/src/datahash.rs:72-74` (same construction in `crates/csp-bench/src/replay.rs:460-462`)

**Problem:** Each record is `path_bytes || 0x00 || canonicalized_content` with no length prefix and no end-of-record delimiter. Since file *content* may itself contain `0x00` and arbitrary text, the boundary between one file's content and the next file's path is ambiguous across file sets. Concrete collision, verified by construction:

- Set A: `"a" → ""`, `"bc" → "d"` hashes `61 00 | 62 63 00 64`
- Set B: `"a" → "bc\x00d"` hashes `61 00 | 62 63 00 64`

Identical byte streams → identical BLAKE3. (`"bc\x00d"` is valid UTF-8, so canonicalization leaves it verbatim.) Renaming, deleting, and folding a file's tail into a sibling can all preserve the hash.

**Evidence:** `h.update(rel.as_bytes()); h.update(&[0]); h.update(&canonicalize(&bytes));` — the `0x00` terminates the path but nothing bounds the content. Canonicalized text can contain interior NUL; non-UTF-8 bytes hash raw.

**Impact:** The data-set hash feeds `BuiltPart::data_set_hash` and §7.7 drift warnings — a collision means a real data change produces no drift warning. Low probability in practice (requires an authored NUL-containing text file or a crafted binary), but the scheme is spec-pinned and the fix is cheap now, expensive after the hash is recorded in artifacts.

**Suggestion:** Length-prefix each record (`u64le(len) || rel || u64le(len) || content`) or insert a second unambiguous delimiter. Bump `DATA_HASH_VERSION` since the scheme is pinned; apply identically in `csp-bench::replay::fixture_hash` (and note `test-fixtures` exclusion carve-out stays).

---

### [LOW] Finding 4: `data_set_hash` follows symlinks — recursion can diverge or escape the tree

**File:** `crates/csp-data/src/datahash.rs:104`

**Problem:** `entry.path().is_dir()` follows symlinks. A symlinked directory creates unbounded recursion (stack exhaustion, not a `DataHashError`) if it points at an ancestor, or silently hashes files *outside* `data_root` under a data-relative path — a hash that varies with what's on the target machine. A symlinked *file* hashes its target's bytes, so the hash differs between checkouts where the symlink resolves differently (notably Windows vs. Unix checkouts of the same tree). Related edge: `entry.file_name().to_string_lossy()` (line 98) maps distinct non-UTF-8 names onto the same `rel` string — two such files hash under one identical path prefix in an order that depends on `read_dir` enumeration, i.e. nondeterministically.

**Evidence:** `collect` has no symlink/file-type check at all — contrast with the loader's walk (`load.rs:692`) which has the same issue but only affects enumeration, not a recorded hash.

**Impact:** Low probability for a controlled `data/` tree (none exist today — `find data -type l` is empty), but a single stray symlink turns a deterministic, spec-pinned hash into a machine-dependent or crashing computation.

**Suggestion:** Use `entry.file_type()` and skip or error on non-file, non-dir entries (`file_type.is_symlink()` → `DataHashError` or explicit skip); document the non-UTF-8-name assumption or reject lossy names.

---

### [LOW] Finding 5: Loader ordering does not match `data_set_hash` ordering, contradicting the documented invariant

**File:** `crates/csp-data/src/load.rs:81-82, 196-199, 432-434, 494-497, 623-626, 677-699` vs `datahash.rs:84`

**Problem:** Several doc comments claim the loader's file order is "the deterministic order `data_set_hash` hashes them in." The loader sorts `PathBuf`s component-wise; the hasher sorts `/`-joined `String`s. These orders differ whenever a file and a sibling directory share a name prefix: for `a.ron` and `a/z.ron`, `PathBuf` order yields `["a", "a.ron"]` → `a/z.ron` then `a.ron`, while string sort yields `"a.ron"` < `"a/z.ron"` (`'.' 0x2E < '/' 0x2F`). Verified empirically with a two-line `rustc` check.

**Impact:** Everything is still deterministic — no correctness bug today — but the stated invariant is false. Anything that later relies on merged order == hash order (e.g., `LoadedTechTree::merged` concatenation mirroring the hash sequence, or "first def wins" duplicate diagnostics naming the same file as the hash order) will subtly diverge, and the misleading comment invites that reliance.

**Suggestion:** Either sort `rel` strings in the loader the same way `datahash` does (join components with `/` then sort), or fix the comments to say only "deterministic ascending order."

---

### [LOW] Finding 6: `collect_ron_files` silently drops per-entry I/O errors; `load_parts` missing-dir convention diverges

**File:** `crates/csp-data/src/load.rs:683-689` and `:711-721`

**Problem (a):** `read_dir` entry errors are swallowed: `.filter_map(|e| e.ok().map(|e| e.path()))`. A permission/IO error on a single directory entry silently omits that file — a part/experiment/recipe def can vanish with no diagnostic and the load still succeeds. `datahash`'s `collect` propagates the same class of error (lines 94-97), so the two walks disagree on error handling.

**Problem (b):** `load_parts_from` is the only directory loader without an `exists(dir)` early-out (compare `load_recipes_from` 466-471, `load_experiments_from` 531-535, `load_contracts_from` 595-599, `load_techtree_from` 663-665). On `FsDataSource` a missing dir → `Io` error (reasonable if parts are required), but on `EmbeddedSource` `ron_files` returns `Ok(vec![])` → a missing parts dir silently loads as an empty part set — the exact silent-empty the convention exists to produce *only* for genuinely optional tables.

**Suggestion:** Collect `entry` results into `Result` (or at least `map_err` like datahash does). For parts, either make the error uniform (check `exists` and return `Io` explicitly for both sources) or document that parts are mandatory and make `EmbeddedSource::ron_files` error on a missing dir.

---

### [LOW] Finding 7: Per-entry `SiteDef.schema_version` is never checked by loader or validator

**File:** `crates/csp-data/src/site.rs:45-47`; checked at `load.rs:389-396` (file-level only) and `validate.rs:1294-1298` (file-level only)

**Problem:** Each site entry carries `schema_version` documented as "Must equal `SITE_SCHEMA_VERSION`", and shipped `sites.ron` authors it per entry (`data/systems/home/sites.ron:11`). `load_sites_from` validates only `SiteFile.schema_version` against `SITE_SCHEMA_SUPPORTED`; `validate_sites` iterates entries for id/body/pose/yields but never reads `s.schema_version`. A site entry declaring `schema_version: 99` loads and validates green.

**Impact:** The field is dead weight as enforced — authors get no signal when an entry-version drifts (the per-entry field exists precisely for M8-B-style evolution: v1 files parse via `#[serde(default)]`, and a future per-entry evolution has no gate).

**Suggestion:** In `validate_sites`'s per-site loop, check `s.schema_version` against `SITE_SCHEMA_SUPPORTED` (or `== SITE_SCHEMA_VERSION`, matching the field's doc).

---

### [LOW] Finding 8: Site `display_name` LocKeys are the one authored LocKey class with no resolution check

**File:** `crates/csp-data/src/site.rs:51`; consumers `validate.rs` (no `validate_sites_loc_keys` exists); test gap `tests/colonies.rs:96-107`

**Problem:** Every other LocKey surface is gated: system/body/biome names via `validate_loc_keys`, part/variant/resource names via `validate_part_loc_keys`, techtree via `validate_techtree_loc_keys`, contracts via `validate_contract_loc_keys`, colony names asserted manually in `tests/colonies.rs:100-106`. `SiteDef.display_name` (values like `"site.terrin-launch"` in `data/loc/en.ron:84-86`) is checked by neither `validate_sites` (it takes no `LocFile`) nor any test.

**Impact:** A typo'd site display-name key ships clean and renders as a raw loc key in UI at runtime — the same failure mode the other seven validators exist to catch.

**Suggestion:** Add `validate_site_loc_keys(file: &SiteFile, loc: &LocFile)` (the existing four-function pattern) and call it from the colonies/home-system gate test.

---

### [LOW] Finding 9: `validate_recipes` reports every error as `InvalidColony` with the recipe id in the `site` field

**File:** `crates/csp-data/src/validate.rs:1378-1384`

**Problem:** The `bad` closure stamps `ValidationError::InvalidColony { site: <recipe id>, reason }` for all recipe failures — stem mismatch, duplicate id, schema version, unknown/duplicate resources, zero-output recipes. `Display` renders these as `colony at site "recipe.smelt": ...`, mislabeling the file kind. Any CI consumer or reader keying on the variant name gets a colony error for a recipe problem. Additionally, `InvalidSite`/`InvalidColony` reasons are `String` while the sibling part/tech/experiment variants use stable `&'static str` reason tokens the enum doc promises ("stable reason token" on e.g. `BadExperiment`).

**Impact:** Diagnostic quality only — no logic error — but it erodes the typed-error-list contract the module advertises ("CI and the editor both consume the same list").

**Suggestion:** Add a `BadRecipe { recipe, reason: &'static str }` variant (or reuse a generic `BadFile`); move the reason strings to `&'static str` where feasible.

---

### [LOW] Finding 10: Loader joins unvalidated authored IDs/strings as path components

**File:** `crates/csp-data/src/load.rs:320-331`

**Problem:** `load_system_from` uses manifest entries (`id.as_str()`) and `biome_map` legend/png names directly as `Path::join` components *before* any validation. `id_well_formed`/`MalformedId` checks run in `validate_system`, after the files have already been read. A manifest entry `".."` or `"../other-sys"` makes the loader read `body.ron` outside the intended system directory; a `BiomeMapRef.png` of `"../../x.png"` reads outside the body dir.

**Impact:** Shipped data is trusted so this is hardening rather than a live bug, but it makes the loader's file-shape contract dependent on a validator that runs later — and `load_system` is also called on arbitrary dirs in tooling/tests.

**Suggestion:** Reject non-`id_well_formed` manifest ids (and `..`/separator-containing legend/png names) in `load_system_from` before joining — cheap `is_well_formed` check that turns the traversal into a `LoadError` instead of a silent escape.

---

### [LOW] Finding 11: `check_power_collision` hard-codes the `electric_charge` resource id

**File:** `crates/csp-data/src/validate.rs:1487`

**Problem:** The EC-collision rule compares `id.as_str() == "electric_charge"` — a magic string duplicated from `data/resources.ron`. If the resource id is renamed or a second charge-like resource is added, the rule silently no-ops while looking correct. Same function family: colony-side `Drill.min_concentration` is checked only `>= 0.0` (line 1581) while the part-side equivalent is bounded to `[0,1]` (`check_drill_module`, line 2109) and the colony doc says it "matches the part-side `ModuleDef::Drill::min_concentration`" (`colony.rs:104`).

**Suggestion:** Name the canonical EC id once (a `pub const` in `csp-data::ids` or `resource`) and use it on both sides; align the colony drill bound with the part-side `[0,1]` rule.

---

### [LOW] Finding 12: SatellitePlacement periapsis/apoapsis bands are not cross-checked

**File:** `crates/csp-data/src/validate.rs:2913-2923`; consumer `crates/csp-sim/src/contracts.rs:243-249`

**Problem:** Each altitude band is validated independently (`0 < lo <= hi`) but nothing checks that the apoapsis band can satisfy the periapsis draw. With `periapsis_alt_m: (500_000, 600_000)` and `apoapsis_alt_m: (100_000, 200_000)`, generation silently clamps `ra = rp` (`contracts.rs:245-248`), producing only degenerate circular-orbit contracts — an authoring error that validates green and degrades offer quality rather than erroring.

**Suggestion:** Require `apoapsis_alt_m.1 >= periapsis_alt_m.0` (the band intersection must allow `ap >= pe`) in `validate_contract_space`; optionally warn-or-reject when `apoapsis_alt_m.0 < periapsis_alt_m.0` since partial overlap silently narrows the distribution.

---

## Notes verified as NOT problems

- Canonicalization (`datahash.rs:132-141`) is byte-exact identical to `csp-bench`'s fixture canonicalizer (`replay.rs:510-519`) — the "identical by design" claim holds today.
- `check_tree_cycles` (validate.rs:2726-2761) terminates and covers all nodes — pushes bounded by `done`, every node is a DFS root.
- `inspect_indexed_png` needs no IDAT/CRC, so the `tiny_png` test mock is faithful to the inspector's contract as written; the coupling is documented at validate.rs:3406-3408.
- `PERIOD_REL_TOL = 1e-9` with `libm::sqrt` is deterministic and consistent with the crate's `f64::sqrt` ban; NaN/∞ inputs can't produce false `PeriodInconsistent` (they're already reported by `check_positive`/`check_finite`).
- `validate_system` error ordering is deterministic (BTreeMap/BTreeSet everywhere).
- Tests use real loaders on real `data/` — no stale mocks found; `balance.rs` re-derives the budget from the file rather than re-declaring it.
- `data/eval-scenarios` is intentionally inside the hash scope per §3.1's "everything except" list.
