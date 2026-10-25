# Verified Report 2: csp-data loaders (csp-data / csp-view data path)
# Verified against HEAD 1e8c6da on 2026-10-25 (post-fix-agent commits).
# Concurrent-edit check: commits b644a73..1e8c6da touched csp-view render/art,
# csp-app, and one line of csp-data/datahash.rs (ART_EXTENSIONS += "csar").
# None of the four findings were touched. All four CONFIRMED.

### Finding 1: Production startup never runs the validators — CONFIRMED
**Original**: View::from_source loads data with no validate_* calls; loaders' docs claim "the validator flags it loudly"; duplicate IDs resolve first-wins via by_id() but last-wins via set_sites; data_root() prefers a CWD-relative tree.
**Verification**:
- `View::from_source` (crates/csp-view/src/lib.rs:800-940) read in full: loads system, balance, experiments, techtree, sites, contracts, recipes, colonies, parts, resources, loc and installs them into the sim. Zero `validate_*` calls anywhere in the function.
- Production startup is this path: crates/csp-app/src/lib.rs:2374 `View::new()` -> lib.rs:792 `from_source(&csp_data::FsDataSource, &data_root())`.
- Repo-wide grep for validate_* callers outside csp-data/src: only `crates/csp-data/tests/*` (home_system, parts, contracts, experiments, balance, techtree) and `crates/csp-view/tests/demo.rs:43-95`. xtask has only lint_attr/lint_deps (no validate-data task). So semantic validation runs only under `cargo test`/CI, exactly as reported.
- The misleading docs are still present verbatim: "First def wins on a duplicate id; the validator flags the duplicate loudly" at crates/csp-data/src/load.rs:533-534 (recipes), 597-598 (experiments), 665-666 (contracts).
- First-wins confirmed in all three `by_id` impls via `or_insert`: load.rs:538 (recipes), 602 (experiments), 671 (contracts).
- Last-wins for sites confirmed: csp-view/src/lib.rs:871-877 collects `sites.sites.iter().map(|s| (s.id.clone(), s.clone())).collect()` into `BTreeMap<SiteId, SiteDef>` (signature at crates/csp-sim/src/economy.rs:648); `FromIterator` for BTreeMap keeps the LAST duplicate key, so duplicate site IDs resolve opposite to by_id(). Two silent tie-breaks in the same startup function.
- data_root() confirmed CWD-first: csp-view/src/lib.rs:2116-2126 tries `PathBuf::from("data")` (relative to CWD) before the manifest-relative fallback, gated only on `balance.ron` existing. Any locally edited tree is loaded and installed unvalidated.
- Nuance: wasm `View::new` uses the compile-time-embedded demo set (lower exposure); the native app is the exposed path. Severity HIGH stands: `data/` is the documented tuning surface and duplicate/negative/NaN/dangling-ref rot loads silently.

### Finding 2: Unbounded zlib inflate in decode_indexed_png — CONFIRMED
**Original**: IDAT is inflated with no size limit before the `raw.len() != stride * h` check; a 1x1-header bomb OOMs at startup; miniz_oxide 0.9 offers decompress_to_vec_zlib_with_limit.
**Verification**:
- crates/csp-data/src/biome.rs:235-236 still reads `miniz_oxide::inflate::decompress_to_vec_zlib(&idat)` — the unbounded variant.
- The length check runs only after full inflation: biome.rs:253-257 (`unfilter_scanlines` rejects `raw.len() != stride * h` at :255). The 2048x1024 cap at biome.rs:207-209 bounds the *declared* w/h at header parse, not the inflated byte count — and the comment at biome.rs:203-206 claims oversized declarations "must not reach inflate", which the code does not enforce for the payload side.
- Startup reachability confirmed: from_source -> `terrain::config_from_bodies(&sys.bodies)` (csp-view/src/lib.rs:925) -> terrain.rs:116 `b.biomes.as_ref().and_then(biome_field)` -> terrain.rs:159 `decode_indexed_png(&loaded.png)`. The biome PNG bytes come straight from `data/systems/home/terrin/biomes.png` on disk. terrain.rs:161-164 handles decode *errors* gracefully (warn + None), but an OOM abort during inflate is not catchable — "hard crash instead of BiomeMapPngInvalid" is accurate. validate.rs:1416 also decodes (CI only).
- Fix availability confirmed: workspace pins `miniz_oxide = "=0.9.1"` (Cargo.toml:49); the vendored 0.9.1 source exports `pub fn decompress_to_vec_zlib_with_limit` (miniz_oxide-0.9.1/src/inflate/mod.rs:195). The suggested one-line fix compiles against the pinned version.

### Finding 3: gather_ron_files follows symlinks; disagrees with datahash — CONFIRMED
**Original**: Walker dispatches on path.is_dir() (follows links): ancestor loop = stack overflow, planted link = out-of-tree reads; datahash::collect refuses symlinks, so the two walkers disagree.
**Verification**:
- crates/csp-data/src/load.rs:815-838 unchanged: `if path.is_dir() { gather_ron_files(&path, out)?; }` — `Path::is_dir()` follows symlinks; no `file_type()` check, no depth cap. A `data/parts/loop -> .` link recurses forever (abort; `LoadError` cannot express it). A `data/parts/etc -> /etc` link is traversed and any `*.ron` under it is pushed and loaded.
- This is the production path: `FsDataSource::ron_files` (load.rs:222-226) -> `collect_ron_files` -> `gather_ron_files`. (EmbeddedSource is a static table, unaffected.)
- The module doc contradiction is real: load.rs:3-5 states "The loader's only I/O is reading files under the `dir` the caller hands it" — the walker breaks this; `join_authored` (load.rs:373) gates authored *names*, not filesystem links.
- The sibling walker disagrees exactly as claimed: crates/csp-data/src/datahash.rs:128-143 uses `entry.file_type()` (does not follow) and returns `DataHashError::Symlink { path }` on any symlink, with the doc comment "a symlink inside `data/` is refused rather than silently traversed". Same tree: hashing a symlinked data dir errors while loading it silently follows. (The only concurrent csp-data edit, b644a73, added "csar" to ART_EXTENSIONS and did not touch this.)

### Finding 4: colonies.ron accepts only [2]; UnsupportedSchemaVersion doc stale — CONFIRMED
**Original**: load_colonies_from passes &[COLONY_SCHEMA_VERSION]=[2] contrary to PLAN §3.1's current+previous band (which load_sites honors as [1,2]); the error doc still says "only v1 exists today".
**Verification**:
- crates/csp-data/src/load.rs:507: `read_versioned_ron(src, path, &[COLONY_SCHEMA_VERSION], ...)` with COLONY_SCHEMA_VERSION = 2 (crates/csp-data/src/colony.rs:34) — exactly [2].
- Contrast: crates/csp-data/src/site.rs:36 `SITE_SCHEMA_SUPPORTED = [SITE_SCHEMA_VERSION - 1, SITE_SCHEMA_VERSION]` used at load.rs:483 — the band exists for the identical rule, one file type over.
- PLAN.md §3.1 (lines 1010-1012): "the loader supports the current and previous version with explicit defaults; the validator enforces presence." Confirmed verbatim.
- Stale doc confirmed at load.rs:117-119: "(§3.1: current and previous are supported; only v1 exists today, so anything else is rejected)" — false with site v2, colony v2, balance at v11.
- colony.rs:29-33 documents the v1->v2 restructure (rate_per_s rename, Converter inline pairs -> RecipeId indirection), supporting the report's own caveat that a default mapping likely cannot reconstruct v1 meaning. LOW severity stands as an undocumented asymmetry + stale doc; the suggested fix (document the exception, fix the error doc) is the right shape.

---
## Summary
4 confirmed, 0 dismissed, 0 fixed-by-concurrent-edit.
