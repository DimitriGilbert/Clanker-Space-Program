# Review C31 — Data schemas + loaders (csp-data: part/load/ids/datahash/resource/experiment/recipe/balance/loc/biome/colony/contract/site/system/techtree)

Scope: current working tree. All 15 assigned files read in full; PRD §3.0–§3.6, §6.4, §7.7 checked
against them; `cargo test -p csp-data --lib` passes (81 tests). Parallel findings respected (C32
owns validate.rs; C15 owns the zeroed `BuiltPart.data_set_hash`; C33 owns content values).

**Verdict: no blockers.** The schema layer is unusually disciplined: `deny_unknown_fields` on every
authored struct and enum variant, version probe before parse (a stale file reports its version, not
a missing-field serde error), injective hash framing with a test proving the retired framing
collided, symlinks/non-UTF-8/case-collisions refused in both walker and hash, and an atomic
production seam (`load_validated_dataset`, csp-view/src/lib.rs:1649-1689) that validates the whole
set before a byte reaches the sim. The findings below are deviations and gaps, not breakage.

### [SEVERITY: MEDIUM] Finding 1: §3.1's "current and previous version" loader band exists only for sites.ron
**File**: crates/csp-data/src/load.rs:526 (colonies), load.rs:463 (balance) vs site.rs:45
**Problem**: PLAN.md:1016-1017 is normative: "the loader supports the current and previous version
with explicit defaults". Only sites.ron implements the band (`SITE_SCHEMA_SUPPORTED = [2,3]` with
`#[serde(default)]` fields); `load_colonies_from` accepts exactly v2 (a v1 colonies.ron is refused
outright), and `load_balance_from` accepts exactly v11 — ten version bumps, never once keeping the
previous.
**Evidence**: load.rs:526 `read_versioned_ron(src, path, &[COLONY_SCHEMA_VERSION], …)`;
load.rs:463 `&[BALANCE_SCHEMA_VERSION]`. Mitigations verified: colonies v1 never shipped (first
commit of `data/systems/home/colonies.ron`, 2ec4128, was already v2; the v1→v2 restructure landed
inside one commit), and refusals are loud, positioned errors naming found/expected versions — not
the "bare serde error" §3.1 wrote the rule to prevent.
**Impact**: Pre-1.0 single-author, cost today is ~zero; cost at the first community/partner data
files or the first split of a data PR across a version bump is exactly what §3.1 predicted. PRD
status: DEGRADED (implemented 1 of the 2 file types that have a previous version).
**Suggestion**: Either implement the band for colonies (v1 reads `base_rate_per_s` defaults,
Converter inline pairs → refuse-with-pointer-to-migration is also defensible but should be a
recorded decision), or add a `PRE_1_0` note to the module docs stating §3.1's band clause is
deferred under §7.7's pre-1.0 policy — right now the LoadError doc (load.rs:117-127) reads as if a
general policy exists when it is sites-only.

### [SEVERITY: MEDIUM] Finding 2: schema_version policy is inconsistent across file types — one constant shared by four types, duplicated per-entry in another, stale doc on the error enum
**File**: crates/csp-data/src/system.rs:17, loc.rs:16, biome.rs:42, site.rs:57, load.rs:117-127
**Problem**: Four *distinct* file types share one version constant: system.ron, body.ron,
biomes.ron and loc/en.ron all gate on `crate::system::SCHEMA_VERSION` (load.rs:418, :430, :439,
:918; loc.rs:16 and biome.rs:42 even document the cross-module dependency). Meanwhile SiteDef
(site.rs:57) makes every site *entry* re-declare `schema_version` inside the file — a duplication
no other in-file collection has (ColonyDef, TechNode, ExperimentDef have none). And the
`LoadError::UnsupportedSchemaVersion` doc (load.rs:119-123) still says sites accepts "v1 and v2"
when `SITE_SCHEMA_SUPPORTED` is `[2, 3]` since W2.6's v3.
**Evidence**: A future additive change to `BodyDef` (say v2) must bump the shared constant, which
refuses every system.ron, biomes.ron and en.ron at load until all four are rewritten — three of
which did not change shape. The check_version error message (load.rs:162-170,
`expected: *supported.last()`) also reports only the newest accepted version, so a sites author on
v1 is told "loader knows 3" when 2 is what they should migrate to.
**Impact**: Coupled version bumps across unrelated files; authors copying a site entry get a
second version field to forget (validator catches it, but it is pure boilerplate); the stale doc
misinforms the next person who touches version policy.
**Suggestion**: Give each file type its own constant (`SYSTEM_/BODY_/BIOME_/LOC_SCHEMA_VERSION`),
all currently 1 — mechanical, no data churn. Drop `SiteDef::schema_version` (a vN file contains
vN defs by construction; the file-level check is the contract). Re-word the LoadError doc and have
`check_version` render the whole band (`loader knows 2..3`).

### [SEVERITY: MEDIUM] Finding 3: manifest-driven system load silently ignores orphan body directories and unreferenced biome maps
**File**: crates/csp-data/src/load.rs:422-448
**Problem**: `load_system_from` iterates `system.bodies` only. A body directory whose
`body.ron` exists but was never added to the manifest — or a `biomes.png`/`biomes.ron` present
while `body.ron`'s `biome_map` is `None` — is never read, never parsed, never validated. There is
no error anywhere: the validator only ever sees `LoadedSystem`, which cannot contain content the
loader skipped.
**Evidence**: load.rs:423 `for id in &system.bodies`; validate.rs's `check_schema_versions`/
`validate_system` take `&LoadedSystem` (validate.rs:1131, 1150) — no filesystem view. The only
observable effect is a data-set-hash flip.
**Impact**: The exact "wrong file produces silent wrong behavior" case the review was asked to
enumerate: an author adds `data/systems/home/duva2/` with a full body.ron, launches, and the body
does not exist — no diagnostic. (Registry loaders — parts/contracts/recipes/experiments/techtree —
walk their directories, so only the system manifest has this hole.)
**Suggestion**: After the body loop, cross-check `src.ron_files(dir)` (which the loader already
has) against the manifest: any `<name>/body.ron` whose `<name>` is not in `system.bodies` emits a
validation error ("body directory not listed in system.ron"). Same one-liner for a `biomes.png`
beside a `biome_map: None` body if cheap.

### [SEVERITY: MEDIUM] Finding 4: authored-name guard is incomplete on Windows — drive-prefixed names escape the handed directory
**File**: crates/csp-data/src/load.rs:382-384
**Problem**: `authored_name_ok` rejects empty/`.`/`..`/`/`/`\`/NUL. It does not reject `:`. On
Windows, a name like `C:x` parses as a drive-relative path (`path.prefix().is_some()`), and
`PathBuf::join` *replaces* the base when the joined path carries a prefix — so
`dir.join("C:x") == Path::new("C:x")`, reading outside the data tree the caller handed the loader.
Reserved DOS device basenames (`CON`, `NUL`, `COM1`) also pass.
**Evidence**: std `PathBuf::_push` truncates self when `path.is_absolute() || path.prefix().is_some()`;
Unix is unaffected (no prefix concept there, `C:x` is a legal filename). Exploitability is low —
the names come from first-party RON the CI validator also reads — but the guard's own doc
(load.rs:386-392) states its purpose as preventing exactly this escape, and it is the only thing
between an authored string and `FsDataSource`'s verbatim join.
**Impact**: Windows-only defense-in-depth gap in the path-traversal guard; a hostile/corrupted
data file could read an arbitrary drive-relative path (bytes then feed legend/PNG parsing).
**Suggestion**: Reject `:` outright (no legitimate authored segment uses it on any platform), and
either reject the classic reserved device names or, simplest, restrict authored names to a
conservative charset (e.g. `[A-Za-z0-9._-]`).

### [SEVERITY: MEDIUM] Finding 5: `max_tracked_vessels` ships as u64::MAX against PLAN §3.5's authored 512, and the validator declares the field ungated
**File**: crates/csp-data/src/balance.rs:74-79; data/balance.ron:15; crates/csp-data/src/validate.rs:1754-1761
**Problem**: PLAN.md:1225 authors the object cap explicitly: "`max_tracked_vessels = 512`,
`max_debris = 64`". The shipped balance.ron sets `18446744073709551615` (u64::MAX) — balance.rs's
own doc admits it ("effectively unlimited craft in flight; the shipped data/balance.ron authors
the same"), and validate_balance's header lists the field among "ungated counters [that] carry no
finite domain to check".
**Evidence**: `grep max_tracked_vessels data/balance.ron` → 18446744073709551615. §6.4's cut list
does not sanction removing the vessel cap (it sanctions advanced render, life support, Duva,
docking-UI, web polish). §3.5's save-growth guarantee (≤32 MB after 100 sim-years) leans on this
cap together with the debris policy.
**Impact**: PRD deviation that silently disables the tracked-vessel ceiling; 100-sim-year
save-size protection now rests solely on the economy harness assertion. Content value is C33/C34
territory, but the "no finite domain" validator stance and the self-documenting schema comment
live in this cluster.
**Suggestion**: Either author 512 per the PRD (one-line data change) or record the deviation the
way the RON debug-dump note in §7.7 does — a PLAN-adjacent decision note. If it stays ungated,
the validator should at least bound it below u64::MAX so "forgot to author a cap" and
"deliberately unlimited" stop looking identical.

### [SEVERITY: MEDIUM] Finding 6 (DRY): five structurally identical registry loaders + wrapper structs + by_id maps, hand-rolled
**File**: crates/csp-data/src/load.rs:575-592, 639-661, 710-729, 782-797, 888-903 (+ structs at 533-548, 596-603, 664-672, 734-741, 76-84; by_id at 550-562, 614-626, 683-696)
**Problem**: `load_recipes_from`, `load_experiments_from`, `load_contracts_from`,
`load_techtree_from`, `load_parts_from` are the same 20-line function five times
(exists→empty, `ron_files`, loop `read_versioned_ron` with a per-type constant, `strip_prefix`,
push), each with its own `LoadedX { def, path }` pair-struct and three near-identical `by_id()`
builders. The next registry (facilities? eval content?) will be copy number six, and a loader-rule
change (like the symlink refusal or sort-key fix that already had to land in `collect_ron_files`
once for everyone) must be re-verified in five places.
**Evidence**: The five bodies differ only in the def type, the version constant, and the wrapper
name; `by_id` differs only in the key newtype.
**Suggestion**: One generic core — `fn load_registry<T: DeserializeOwned + VersionedRon>(src, dir)
-> Result<LoadedRegistry<T>, LoadError>` with `trait VersionedRon { const SCHEMA_VERSIONS: &[u32];
fn version(&self) -> u32; }` and a single `LoadedRegistry<T> { defs: Vec<LoadedDef<T>> }` with a
generic `by_id<K: From<&T>>` (or keep the per-type `by_id` as three-line typedefs). Collapses
~230 lines to ~60 and makes the next file type a 5-line impl. Not urgent; do it when a sixth
registry lands.

## PRD coverage table

| PRD item | Status | Evidence |
|---|---|---|
| §3.1 PartDef schema + module set (incl. Control/Decoupler/Parachute plan-implied additions) | WIRED | part.rs:506-652, documented rationale at 498-503 |
| §3.1 `schema_version` first field + presence + probe-before-parse ordering | WIRED | load.rs:334-374, tests load.rs:934-957; validator re-checks every type (validate.rs:1150-1180 etc.) |
| §3.1 loader supports current AND previous version | **DEGRADED** | sites only (site.rs:45); colonies/balance exact-match — Finding 1 |
| §3.1 stable string IDs, never indices | WIRED | ids.rs newtypes, `serde(transparent)`, Ord for BTreeMap; no stringly-typed leaks found (attach-node/DockPort `node` and terrain refs are legitimately local strings; refs path-gated) |
| §3.1 data-set hash definition (framing, exclusions, canonicalization, symlink/non-UTF-8 refusal) | WIRED | datahash.rs:103-234; exclusions exactly `test-fixtures/` + art-ext-under-`art/`; **coverage of new file types complete** — contracts/, eval-scenarios/, loc/, techtree/ all hashed by the walk (datahash.rs:195-218); injectivity + case-collision tests |
| §3.0 SystemDef/BodyDef/Kepler schema incl. `surface_resources`, `biome_map` | WIRED | system.rs (ConcentrationMapRef schema-only, honestly documented at site.rs:79-83 as the per-site stand-in) |
| §3.0.1 authored biome maps (index PNG + legend, decoupled from terrain, hashed) | WIRED | biome.rs; hard 2048×1024 cap + bounded inflate before allocation (biome.rs:207-240) |
| §3.3 techtree schema (tracks, level caps, freeze-at-manufacture composition) | WIRED | techtree.rs:19-58, 119-134 |
| §3.4 ReliabilityDef verbatim | WIRED | part.rs:776-799 |
| §3.5 object caps authored in balance.ron | **DEGRADED** | max_debris 64 ✓; max_tracked_vessels u64::MAX vs PRD 512 — Finding 5 |
| §3.5.1 colony schema (modules, recipes indirection, stores) | WIRED | colony.rs |
| §3.6 contract schema (kinds, param space, fact prereqs, rewards) | WIRED | contract.rs; crew-transport/tourist deferral documented at 34-37 (sim-predicate honesty, not sanctioned-cut-list material but a fair scoping) |
| §7.7 datahash side of drift warning | WIRED here | datahash + DATA_HASH_VERSION=2 in save/fingerprint path (csp-app lib.rs:655+); per-part freeze dead per C15, save-level warning live per V-C13 — not re-derived |
| Loader error quality (file+line in every error, version named before shape) | WIRED | LoadError Display load.rs:157-181 (stale doc aside — Finding 2); atomic whole-set validation, no partial installs (csp-view lib.rs:1649-1689) |
| Unknown-field strictness at loader layer | WIRED | `#[serde(deny_unknown_fields)]` on every authored struct and enum variant across all 15 files ( sole exception: the intentional SchemaProbe) |

Follow-ups (out of cluster): csp-app/src/lib.rs:655,3039 swallow `data_set_hash(...)` errors with
`.ok()` — a failed hash walk (stray symlink in data/) silently disables §7.7 drift detection
instead of surfacing; `FsDataSource::exists`/`datahash::collect` follow a symlink handed *as* the
root (only symlinks within the tree are refused) — caller-controlled, nit.

## Top 3 issues
1. Finding 1 — §3.1's current+previous loader band is sites-only (colonies v1, balance v10 refused);
   decide band-or-documented-deferral before the first external data files.
2. Finding 3 — orphan body directories / unreferenced biome maps load silently (manifest-driven
   loop never cross-checks the directory); the one true "silent wrong behavior" for content authors.
3. Finding 5 — `max_tracked_vessels` ships as u64::MAX against PLAN §3.5's explicit 512, with the
   validator declaring the field ungated.
