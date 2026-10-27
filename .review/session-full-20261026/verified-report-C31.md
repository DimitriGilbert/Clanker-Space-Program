# Verified Report C31 — Data schemas + loaders (csp-data)

Verification agent pass over `review-report-C31.md` against the current working tree
(project root: /home/didi/workspace/Clanker-Space-Program). Every finding was re-checked at its
cited file:line with surrounding context; PLAN quotes, git history, std documentation, and the
`data/` tree were independently enumerated. No file outside this report was modified.

**Result: 6/6 findings CONFIRMED (two carry factual corrections to supporting prose that do not
overturn them). All three WIRED claims independently CONFIRMED. The report's "no blockers" verdict
and its DEGRADED rows are well-founded.**

---

### Finding 1: §3.1's "current and previous version" loader band exists only for sites.ron — CONFIRMED

**Original summary**: PLAN.md:1016-1017 is normative ("the loader supports the current and
previous version with explicit defaults"); only sites.ron implements the band; colonies accepts
exactly v2, balance exactly v11.

**Verification**: All claims verified at source.
- PLAN.md:1016-1017 (quoted verbatim): "**Every data file's first field is `schema_version: u32`**;
  the loader supports the current and previous version with explicit defaults; the validator
  enforces presence." Normative, not aspirational phrasing.
- Band implemented only for sites: `crates/csp-data/src/site.rs:45`
  `pub const SITE_SCHEMA_SUPPORTED: [u32; 2] = [SITE_SCHEMA_VERSION - 1, SITE_SCHEMA_VERSION]`
  (= [2, 3], SITE_SCHEMA_VERSION = 3), consumed at `load.rs:503`; the v2→v3 additive field
  (`starting_stock`) carries `#[serde(default)]` (site.rs:104-115) — exactly §3.1's mechanism.
- Colonies exact-match: `load.rs:526` `read_versioned_ron(src, path, &[COLONY_SCHEMA_VERSION], …)`
  with `COLONY_SCHEMA_VERSION: u32 = 2` (colony.rs:34) — a v1 colonies.ron is refused outright.
- Balance exact-match: `load.rs:463` `&[BALANCE_SCHEMA_VERSION]` with
  `BALANCE_SCHEMA_VERSION: u32 = 11` (balance.rs:55). Version history in the doc comment runs
  2…11, all bumps single-version.
- Mitigations verified true: first commit of `data/systems/home/colonies.ron` (2ec4128) already
  declares `schema_version: 2`, and `colony.rs` was born at v2 in the same commit (its own doc
  describes the in-commit `rate_per_s`→`base_rate_per_s` restructure) — colonies v1 never shipped.
  Refusals are loud, positioned errors naming found/expected (`LoadError::UnsupportedSchemaVersion`
  Display, load.rs:162-170).

**Correction (prose, not core)**: the Suggestion's claim that "the LoadError doc (load.rs:117-127)
reads as if a general policy exists when it is sites-only" is inaccurate. That doc explicitly
states "The accepted set is per file type, not a uniform current-and-previous band" and documents
the colonies refusal and balance exact-match. The deviation is already code-documented; what is
missing is only a PLAN-adjacent §7.7 deferral note. Core deviation claim and DEGRADED status stand.

### Finding 2: schema_version policy inconsistent — one constant shared by four file types, per-entry duplication in SiteDef, stale error-enum doc — CONFIRMED

**Original summary**: system.ron, body.ron, biomes.ron, loc/en.ron all gate on
`crate::system::SCHEMA_VERSION`; SiteDef makes every site entry re-declare `schema_version`;
`LoadError::UnsupportedSchemaVersion`'s doc still says sites accepts "v1 and v2" when the band is
[2,3]; `check_version` reports only the newest accepted version.

**Verification**: All four sub-claims verified.
- Shared constant: `system.rs:17` `pub const SCHEMA_VERSION: u32 = 1;` gates all four file types
  at `load.rs:418` (system.ron), `:430` (body.ron), `:439` (biomes.ron legend), `:918`
  (loc/en.ron). Cross-module dependency is even self-documented: `LocFile.schema_version`'s doc
  says "Must equal [`crate::system::SCHEMA_VERSION`]" (loc.rs:15) and `BiomeLegend`'s says the
  same (biome.rs:44). Coupling consequence is real: a BodyDef v2 bump refuses every system.ron,
  biomes.ron and en.ron until all are rewritten.
- Per-entry duplication: `SiteDef::schema_version` (site.rs:57) sits inside `sites.ron` alongside
  `SiteFile::schema_version` — `data/systems/home/sites.ron` literally declares it 4 times
  (lines 10, 13, 52, 65). `ColonyDef` and `TechNode` confirmed to have no per-entry field (only
  ColonyFile/TechTreeDef file-level, colony.rs:209 / techtree.rs:80).
- Stale doc confirmed: load.rs:119-121 says sites accepts "v1 and v2 … v1 reads `resource_yields`
  as empty", but `SITE_SCHEMA_SUPPORTED = [2, 3]` and site.rs's own doc says "a v1 file is now two
  versions back and refused outright". Direct contradiction between the two docs.
- Band rendering: `check_version` (load.rs:339-349) sets `expected: *supported.last()` only;
  Display prints "loader knows {expected}" — the accepted band is never communicated.

**Correction (prose, not core)**: the parenthetical "(ColonyDef, TechNode, ExperimentDef have
none)" is wrong for ExperimentDef — it has `schema_version` (experiment.rs:49). However,
ExperimentDef is the *per-file root* of `data/experiments/<id>.ron` (one def per file), so its
field is the file-level contract, structurally equivalent to `SiteFile::schema_version`, not an
entry duplication. The substantive uniqueness claim — SiteDef is the only in-file *collection
entry* that re-declares a version alongside a file-level one — holds.

### Finding 3: manifest-driven system load silently ignores orphan body directories and unreferenced biome maps — CONFIRMED

**Original summary**: `load_system_from` iterates `system.bodies` only; an unlisted `<body>/`
directory or a stray `biomes.png` beside a `biome_map: None` body is never read or validated, with
no error anywhere.

**Verification**: Confirmed by tracing the full path.
- `load.rs:423` `for id in &system.bodies` — the loop is manifest-driven; nothing else in the
  loader touches the directory (`ron_files` is used only by the five registry loaders).
- `validate.rs:1131` `pub fn validate_system(sys: &LoadedSystem)` and `check_schema_versions`
  (validate.rs:1150) take only `&LoadedSystem` — no `DataSource`/directory view exists anywhere in
  the validator, and `LoadedSystem` structurally cannot carry content the loader skipped
  (load.rs:67-72).
- The production seam adds no cross-check: `load_validated_dataset` (csp-view/src/lib.rs:1649-1692)
  calls only the loaders + `validate_dataset`. Confirmed: an author adding
  `data/systems/home/duva2/body.ron` without a manifest entry gets zero diagnostics; the only
  observable effect is the data-set-hash flip (orphan files ARE hashed — they sit under `data/`
  outside the two exclusions).
- Registry loaders (parts/recipes/experiments/contracts/techtree) walk their directories via
  `src.ron_files(dir)` — confirmed the hole is system-manifest-only, as stated.
- Current tree is clean (no orphan dirs exist today), so this is a latent authoring trap, not a
  live defect — consistent with the report's framing.

### Finding 4: authored-name guard incomplete on Windows — drive-prefixed names escape the handed directory — CONFIRMED

**Original summary**: `authored_name_ok` does not reject `:`; on Windows `dir.join("C:x")` drops
the base entirely (drive-relative escape); reserved DOS device names also pass.

**Verification**: All three legs verified.
- `load.rs:382-384`: the guard rejects exactly empty/`.`/`..`/`/`/`\`/NUL. `:` passes;
  `CON`/`NUL`/`COM1` pass (plain letters). Confirmed by reading the predicate.
- The join is verbatim after the check: `join_authored` → `dir.join(name)` (load.rs:393-402), and
  `FsDataSource` reads that path directly (load.rs:221-233).
- std semantics verified against the official documentation for `Path::push`/`join` (doc.rust-lang
  .org/std/path/struct.Path.html#method.push): on Windows, "if path has a prefix but no root, self
  is ignored and path is returned". `C:x` has prefix `C:` and no root, so
  `dir.join("C:x") == Path::new("C:x")` — a drive-relative path resolved against the current
  directory of drive C, outside the handed tree. Unix is unaffected (no prefix concept; `C:x` is a
  legal filename) — exactly as the report scopes it.
- The guard's own doc (load.rs:386-392) states escape-prevention as its purpose, so the gap is
  against the code's stated contract, not an invented one.
- Context note: CI is ubuntu-only (.github/workflows/ci.yml, `runs-on: ubuntu-latest`), which
  supports the report's own "Windows-only defense-in-depth, low exploitability" framing; the MEDIUM
  label rests on the guard being the sole authored-string defense before a verbatim join.

### Finding 5: `max_tracked_vessels` ships as u64::MAX against PLAN §3.5's authored 512, validator declares it ungated — CONFIRMED

**Original summary**: PLAN.md:1225 authors `max_tracked_vessels = 512`; shipped balance.ron sets
18446744073709551615; `validate_balance`'s header lists the field among ungated counters.

**Verification**: All claims verified.
- `data/balance.ron:15`: `max_tracked_vessels: 18446744073709551615` (u64::MAX). Line 16:
  `max_debris: 64` (matches PLAN).
- PLAN.md:1225 (exact line): "  - `max_tracked_vessels = 512`, `max_debris = 64`." — under the
  §3.5 heading "Object caps and debris policy … the sim and save must not grow without bound over
  100 sim-years".
- `balance.rs:74-79`: the field's own doc admits it — "The sim-side mirror is
  `csp_sim::DEFAULT_MAX_VESSELS` (`u64::MAX` — effectively unlimited craft in flight; the shipped
  `data/balance.ron` authors the same)."
- `validate.rs:1754-1761`: the `validate_balance` doc header lists `max_tracked_vessels` among
  "Ungated counters … [that] carry no finite domain to check". Grep confirms no gate exists
  anywhere in csp-data (the only other mentions are the sim's runtime enforcement,
  csp-sim/src/lib.rs:5815/6015/7292 — which u64::MAX never triggers).
- §6.4 cut list (PLAN.md:1543-1565) verified: it sanctions advanced render, life support, Duva,
  docking, web tier — nothing that covers removing the vessel cap. The deviation is real and
  unrecorded as a decision.

### Finding 6 (DRY): five structurally identical registry loaders + wrapper structs + by_id maps — CONFIRMED

**Original summary**: `load_recipes_from`, `load_experiments_from`, `load_contracts_from`,
`load_techtree_from`, `load_parts_from` are the same 20-line function five times, each with its own
`LoadedX { def, path }` struct and near-identical `by_id()` builders.

**Verification**: Confirmed by direct read of all cited ranges. The five bodies
(load.rs:575-592, 639-661, 710-729, 782-797, 888-903) differ only in the def type, the version
constant, and the wrapper name; each is exists→empty, `ron_files`, loop `read_versioned_ron` with
a single-version slice, `strip_prefix`, push. Wrapper structs at load.rs:76-84, 533-548, 596-603,
664-672, 734-741; `by_id()` builders at 550-562, 614-626, 683-696 differ only in the key newtype.
A loader-rule change (e.g. the symlink refusal in `gather_ron_files`) indeed lands once for all —
but only because it lives in the shared walk; the per-type scaffolding is genuine duplication.
Accurate as a maintainability finding at the report's own "not urgent" urgency.

---

## WIRED claim verification

### data-set-hash coverage COMPLETE (every file under data/; two sanctioned exclusions) — CONFIRMED

Independent enumeration of `data/` (69 files, 0 symlinks):
- **Excluded, rule 1** (`test-fixtures/**`, directory match case-folded at datahash.rs:191):
  13 files (flight-launch/ 8, trivial/ 4, `.gitkeep`).
- **Excluded, rule 2** (art extensions under `art/`, datahash.rs:210-219 + ART_EXTENSIONS):
  2 files (`art/parts.csar`, `art/terrain.csar`). No other file under `art/` exists.
- **Hashed (54 files)** — includes everything the report claims: 6 `contracts/*.ron`, 5
  `eval-scenarios/*.ron`, `loc/en.ron`, `techtree/home.ron`, 3 biome PNGs
  (`systems/home/{duva,rask,terrin}/biomes.png` — `png` outside `art/` IS hashed per in_scope and
  its own test), 45 RON total, plus `data/LICENSE`, `ATTRIBUTION.md`, and 4 `.gitkeep`. No file
  falls outside "hashed or sanctioned-excluded". Injectivity framing (length prefix,
  datahash.rs:121-126) and the retired-framing collision test (datahash.rs:248-276) verified.
- Adjacent observation found during verification (not a report finding, follow-up material):
  datahash.rs:211 `rel[..4]` is a byte-index slice — a legal UTF-8 name whose 4th byte straddles
  a character boundary (e.g. `data/ab€.ron`) panics `data_set_hash`, despite the module's
  otherwise-exhaustive NonUtf8Name/CaseCollision refusal discipline.

### `deny_unknown_fields` everywhere — CONFIRMED

Systematic scan of every struct/enum deriving Deserialize across the 15 csp-data schema files:
exactly two lack the attribute — `SchemaProbe` (load.rs:334; intentional, documented at
load.rs:326-333) and `SituationMask` (part.rs:415; `#[serde(transparent)]` newtype over `u8`,
where the attribute is inapplicable — files spell it as an integer, no fields to be unknown).
Per-file counts (part.rs 22, contract.rs 10, system.rs 9, techtree.rs 7, colony.rs 4, …) match the
claim's "every authored struct and enum variant".

### load-all → validate-all → install atomicity — CONFIRMED

`load_validated_dataset` (csp-view/src/lib.rs:1649-1692): loads all 11 file kinds into
`LoadedDataset` (any loader error propagates via `?` before the next field), then runs
`csp_data::validate_dataset` over the *whole* set and returns `ViewError::Validation` carrying
every error — the struct's own doc: "loaded once and §8.2-validated before a byte reaches the
sim". Caller `from_source` (lib.rs:1736-1746) consumes the dataset only after that `?`, and test
`from_source_refuses_invalid_data` (lib.rs:5116) pins it end-to-end (a loadable-but-invalid tree
with a duplicate site id is refused pre-install).

---

## Verdict

All 6 findings CONFIRMED as real (Findings 1 and 2 with corrected supporting details that do not
change their substance); all 3 spot-checked WIRED claims CONFIRMED. The original report's
**"no blockers"** verdict, its two DEGRADED PRD rows, and its Top-3 ordering are upheld.
