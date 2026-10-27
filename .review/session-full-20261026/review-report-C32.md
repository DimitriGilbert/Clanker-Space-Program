# Review C32 — Data validator + tests (csp-data validate.rs/lib.rs + tests/)

Scope: `crates/csp-data/src/validate.rs` (6,580), `lib.rs` (121), `tests/{parts,home_system,unknown_fields,colonies,contracts,balance,experiments,techtree}.rs`.
All 121 csp-data tests pass on the current tree; every claim below was verified against code + shipped `data/` and, where behavior matters, against `csp-sim` (`colonies.rs`, `contracts.rs`, `manufacture.rs`, `csp-view/src/lib.rs`).

Quick answers to the map's focus questions:
- **Unknown-field strictness**: DENY. `#[serde(deny_unknown_fields)]` on every schema struct/enum-variant; `tests/unknown_fields.rs` covers 13 schemas + 5 misspelled-field cases + the v2-sites band. WIRED.
- **schema_version handling**: loader probes the version BEFORE the full parse (`read_versioned_ron`, load.rs:355-374) so a stale file reports its version, not a missing-field shape error; validator re-checks every namespace. Colonies is exact-match v2 (v1 refused — the v1→v2 rename has no honest mapping, documented load.rs:122-126); a future v3 file is rejected with `UnsupportedSchemaVersion` naming the file + both versions. sites.ron is the only current|previous band ([2,3]), matching §3.1. Consistent loader/validator; no silent accept anywhere.
- **Error quality**: typed `ValidationError` (100+ variants) naming file/part/site/module-index/field; production startup surfaces up to 10 (`ViewError::Validation`, csp-view/src/lib.rs:1686-1696). Good.
- **datahash**: generic walk over `data/` minus `test-fixtures/` + `art/`-extensions, so colonies/sites/contracts/experiments/recipes/techtree are covered automatically; injective v2 framing + case-collision + symlink refusals tested. WIRED.
- **Test parity for new content**: chute/dockport/lab have dedicated negative tests (validate.rs:6376-6579); colony EC-collision + drill bound tested; contracts have empty-space/inverted-band negatives. Gaps noted in findings 5/7.

---

### [HIGH] Finding 1: ColonyResupply candidate bodies are never checked for site feasibility — the shipped template offers duva, which can host no colony, and the offer can neither complete nor fail
**File**: crates/csp-data/src/validate.rs:4081-4098 (the `ColonyResupply` arm), :4127-4133 (`validate_contracts` signature has no `sites` param); data/contracts/contract.colony_resupply.ron:16.
**Problem**: The `ColonyResupply` arm only runs `check_bodies` (body ∈ system) plus resource/amount checks. A colony can only exist where a site exists (`apply_found_colony` rejects unknown sites, csp-sim/src/colonies.rs:1604; sites are authored data, not creatable at runtime), and the sim draws the offer body blindly from the authored list (csp-sim/src/contracts.rs `draw_params`). `data/systems/home/sites.ron` authors sites only on terrin and rask — duva has zero sites and zero colonies.
**Evidence**: `contract.colony_resupply.ron:16` — `bodies: ["terrin", "duva", "rask"]`. duva appears 0 times in sites.ron/colonies.ron (verified by grep).
**Impact**: Every resupply offer drawn on duva (1/3 of draws) is uncompletable forever, and `deadline_days: None` means it can never fail either — a permanent dead board entry the player can accept for the 10k advance. This is the same "uncompletable contract" class the seed review flagged; the new template reintroduces it through a validator that structurally cannot catch it (`validate_contracts` doesn't receive `sites`, even though `validate_dataset` has it in hand at :4315).
**Suggestion**: Thread `sites: &SiteFile` into `validate_contracts` (mechanical — `validate_dataset` already holds `d.sites`) and add: every `ColonyResupply` candidate body must carry ≥1 authored site. Then drop `duva` from the template (content side is C34's, but the rule belongs here).

### [HIGH] Finding 2: No recipe-graph validation — self-referential or cyclic recipes mint resources and load green
**File**: crates/csp-data/src/validate.rs:2178-2242 (`validate_recipes`).
**Problem**: `validate_recipes` checks schema version, id↔stem, uniqueness, resource resolution, finite non-negative rates, and "≥1 positive output". It does not check (a) a recipe whose `inputs` and `outputs` share a resource with net-positive yield, or (b) any cross-recipe cycle with net non-negative product (ore→metal 1:0.6 plus metal→ore 1:2).
**Evidence**: The colony flow network grants a converter its input allocation and produces its outputs at the same fraction (csp-sim/src/colonies.rs:633-691, net_rates at :1048-1070) — a self-referential or cyclic pair nets out to free units/s with no external input. Nothing in csp-data or csp-sim rejects it. The three shipped recipes (water→oxygen, ore→fuel+ox, ore→metal) are acyclic today — the gap is unexploited, not unseen.
**Impact**: One typo'd or "improved" recipe pair silently breaks the economy: levels climb linearly forever, no load error, no runtime symptom — the exact failure mode this validator exists to prevent ("a modder's typo and a wedged game"). §8.2's "cycles" row currently covers only the tech tree and body parent chains.
**Suggestion**: In `validate_recipes`, build the resource digraph (edge input→output per recipe, weight = out-rate/in-rate) and reject (a) same-resource in+out rows in one recipe, (b) any directed cycle whose product of per-edge ratios ≥ 1 (lossy refining cycles stay legal). O(R·E), trivial at this scale; add an inline negative test in the existing `mod tests`.

### [MEDIUM] Finding 3: PRD §7.7 ID-stability gate is MISSING — nothing detects a PartId that disappears
**File**: crates/csp-data/src/validate.rs (absent — no mechanism anywhere in the crate); PLAN.md:1699-1702.
**Problem**: §7.7 is normative: "**ID stability**: `PartId` strings are permanent. Renaming or deleting one requires a migration entry mapping old → new, or `Deprecated(replacement)`. **The data validator fails in CI on an ID that disappears without one.**"
**Evidence**: `grep -rn "Deprecated" crates/csp-data/ data/` → zero hits. `PartDef` has no deprecation marker; no baseline ID snapshot exists for any validator or test to diff against; `validate_parts` (:3338) only checks duplicates and id↔stem.
**Impact**: Deleting or renaming `csp.tank.s320.ron` validates green; every save/craft file referencing the old ID breaks at load with an unexplained missing-part error. The pre-1.0 policy (§7.7) permits save invalidation, but the ID-permanence rule and its CI tripwire are separate and unconditional.
**Suggestion**: Add a committed baseline (`data/part-ids.txt` or a `const` list in csp-data) listing shipped PartIds plus optional `Deprecated(replacement)` entries; `validate_dataset` fails on any baseline ID absent from the loaded set without a deprecation entry. ~40 lines + one test.

### [MEDIUM] Finding 4: Colony cross-field gaps — founding stores and drills degrade silently instead of failing validation
**File**: crates/csp-data/src/validate.rs:2517-2540 (`stores` loop), :2398-2427 (colony `Drill` arm), :2331 (`validate_colonies` already receives both `ColonyFile` and `SiteFile`).
**Problem / Evidence**: Three author mistakes produce silent zero-effect content, all checkable with inputs already in the signature:
1. `stores: [(resource, level)]` naming a resource with **no** `Store`/`Battery` capacity: `apply_found_colony` creates a 0-capacity row and clamps the level to 0 (csp-sim/src/colonies.rs:1655-1663) — the founding stock silently vanishes. Not flagged.
2. `level` above the aggregate capacity clamps silently (schema doc says clamping is by design, but the author gets no signal at load).
3. A colony `Drill` for a resource the site's `resource_yields` doesn't list produces nothing (concentration 0, colony.rs:95-98) — a dead module on barren ground, invisible at validation.
The shipped Rask outpost is coherent on all three (checked by hand: EC 3000 ≤ 5000 battery, ore 10 ≤ 100 store, drill ore ∈ rask-mines yields).
**Impact**: The colony file is the newest authoring surface; these are exactly the "runs but does nothing" states a content gate should name.
**Suggestion**: In `validate_colonies`'s per-colony pass: compute aggregate capacity per resource from `Store`/`Battery` modules; flag `stores` entries with zero capacity (InvalidColony) and entries with `level > capacity` (warning-class reason "clamps to N"); flag `Drill.resource` absent from the founding site's `resource_yields` (the site lookup is already there for the site-exists check).

### [MEDIUM] Finding 5: No "system authors a launch site" rule — deleting the pad row passes validation and degrades the game silently at runtime
**File**: crates/csp-data/src/validate.rs:2092-2100 (only the *per-body* uniqueness rule exists), :2033 (`validate_sites`).
**Problem / Evidence**: `validate_sites` checks ids, bodies, poses, per-body launch uniqueness, yields, and stock — but never that the file as a whole contains ≥1 `launch: true` site. With zero launch sites: every `ManufactureCraft` order is refused at runtime ("build orders consume a launch-site inventory", csp-sim/src/manufacture.rs:134-138) and the pad-spawn path silently falls back to lat/lon (0,0), altitude 0 (csp-view/src/lib.rs:2886-2892). Also no negative tests exist for the W2.6 `starting_stock` rows (`validate_site_stock` :2131-2153 is exercised only via green-path integration tests).
**Impact**: A one-line content deletion turns into a game that boots green, then refuses every launch with a runtime message that never names the missing pad. Sites are also how recovery works — the same deletion silently strands recovery.
**Suggestion**: File-level rule in `validate_sites`: a non-empty site set must contain at least one launch site (an intentionally site-less system authors an empty file, which the loader already reads as empty). Add negative tests for `starting_stock` (duplicate/unknown/negative) alongside it.

### [LOW] Finding 6: `ContractsCompleted { kind }` prerequisite never checks the kind has an authored template — an unsatisfiable prereq silently makes a template unofferable
**File**: crates/csp-data/src/validate.rs:4221-4225.
**Problem / Evidence**: The arm checks only `count > 0`. Completions of a kind can only come from templates of that kind, so `ContractsCompleted { kind: X, count: 1 }` with no X template in the set is a prerequisite that is never true → the carrying template never appears on the board, no error. Current data is safe by luck (all 5 kinds have templates — verified), but the validator already collects `template_ids` and could collect their kinds just as cheaply as it checks tech-node refs one arm earlier.
**Impact / Suggestion**: Same silent-dead-content class as `UnknownContractType` (:4237-4246), which IS checked — this is the missing mirror. One BTreeSet of authored kinds + membership check in the arm.

### [MEDIUM] Finding 7: DRY/complexity — 6.6k-line single file; ~250 lines of mechanically duplicated check plumbing; split is available and cheap
**File**: crates/csp-data/src/validate.rs (whole file); map notes "single unsplittable file; 10% over soft cap" — the "unsplittable" claim is wrong: nothing in AGENTS.md or PLAN prevents a module-directory split, and csp-data itself already performed exactly this split for its schema modules (biome/colony/contract/site/system/techtree.rs per the map §1).

Quantified duplication (grep-verified):
- **8 structurally identical `validate_*_loc_keys` functions** (validate.rs:1668, 2159, 2248, 2549, 3470, 3597, 3924, 4253): each is `seen: BTreeSet` + loop + `seen.insert(key) && !loc.contains(key)` push — ~150 lines. One helper — `fn check_loc_refs(refs: impl IntoIterator<Item = (String /*context*/, &LocKey)>, loc: &LocFile, out: &mut Vec<ValidationError>)` — replaces all 8 (each caller becomes a 3-line iterator over its refs).
- **4 copies of the id↔file-stem check** (:3359-3369 parts, :2193-2204 recipes, :3523-3533 experiments, :4157-4168 contracts), ~40 lines → `fn id_matches_stem(path: &Path, id: &str) -> bool`.
- **~12 near-identical schema_version gate blocks** (system/body/biomes/resources/balance/loc/sites-file/site-entry/colonies/recipes/experiments/contracts/techtree/parts), ~90 lines → `fn check_schema(file: &str, found: u32, expected: u32, out: &mut Vec<ValidationError>)`.
- **4 parallel spellings of the same numeric predicates**: `check_finite`/`check_positive` (body namespace, :1098-1120), `part_positive` (:2567), the `pos`/`nonneg`/`band` closures local to `validate_balance` (:1779-1814), plus **52 inline `is_finite() && …` range checks** scattered through the colony/module/contract arms. One family — `fn num_ok(field, v, bound: Bound, err: impl FnOnce(...) -> ValidationError, out)` — collapses ~80 lines and makes every range read identically.
- Net: ~250-280 lines (~6% of the file) plus a per-domain split removing the 6k soft-cap overage entirely: `validate/{mod.rs (Dataset+validate_dataset), error.rs (enum+Display), system.rs, parts.rs, balance.rs, sites.rs, colonies.rs, recipes.rs, experiments.rs, contracts.rs, techtree.rs, loc.rs}` — `pub use` re-exports in lib.rs:114-121 keep every external caller untouched.
- **Complexity vs §8.6**: 7 `#[allow(clippy::too_many_lines | cognitive_complexity)]` sites (:660, :846, :1770, :2329-2330, :3955, :4126). Six functions exceed the §8.6 error budget of 200 lines (`validate_balance` 237, `validate_colonies` 212; `Display::fmt` 178, `fmt_part` 147, `validate_contract_space` 145, `validate_contracts` 121 — the last four under the 200 error line but over the 100 warn line). §8.6 requires exemptions to carry a `// lint-allow: <reason>` comment **naming §8.6**; the existing reasons are honest but none cite the section (grep `lint-allow` → 0 hits repo-wide, so the convention is unimplemented — cross-cluster note for C35's lint-quality tool, not actionable here beyond the comment).
**Impact**: The typed-error enum + Display (lines 64-1058, ~1,000 lines, 23% of the file) is the right design and should be kept — the duplication worth deleting is the plumbing above, and the file split is what keeps the module navigable as content grows (the map's own priority).

### [NIT] Finding 8: `BadModule` dock-port diagnostics omit the node name
**File**: crates/csp-data/src/validate.rs:3216-3231.
**Problem**: "dock_port node names no attach node" / "two dock ports claim the same attach node" don't say WHICH node — every other check in the file names the offending id. On a ported part with several nodes the author has to diff the attach list by hand.
**Suggestion**: Fold `node.as_str()` into the reason string (the message is already `&'static str`-typed via `BadModule`; either widen that variant's reason to `String` like `InvalidSite`/`InvalidColony` already do, or append via one format site).

---

## PRD coverage table

| PLAN rule | Status | Evidence |
|---|---|---|
| §3.0 SOI > child apoapsis; parent exists + acyclic; period ↔ μ,a to 1e-9; atmo top > scale height | **WIRED** (+ extras: periapsis clearance :1422, SOI-nests-in-orbit :1436, sibling-SOI overlap :1473, SoiMissing :1329) | validate.rs:1380-1515; `PERIOD_REL_TOL = 1e-9` :59; tested :4468-4630 |
| §3.0.1 biome legend/index/palette/pixel-coverage, ≤2048×1024 | **WIRED** | validate.rs:1589-1662 (full PNG decode, not header-only); tests :4763-4837 |
| §3.1 schema_version first field, current+previous w/ defaults | **WIRED** (band only where honestly mappable — sites [2,3]; colonies v1 refused w/ reason, load.rs:122-126) | load.rs:326-374 probe-before-parse; validator re-checks every namespace |
| §3.1 datahash incl. new files, injective framing | **WIRED** | datahash.rs generic walk; DATA_HASH_VERSION=2 (lib.rs:53); tests home_system.rs:151-182 |
| §3.1 id↔filename, duplicate ids, resource cross-refs | **WIRED** | parts/recipes/experiments/contracts stem checks; UnknownResource/Recipe/ExperimentKind |
| §3.3 no cycles / every part reachable / cost monotonic in depth | **WIRED** | check_tree_cycles :3790, UnknownTechRequirement :3657, TechCostNotMonotonic :3736 |
| §3.4 reliability ranges (+ M4b Euler bound) | **WIRED** | check_reliability :3251; ConductionExceedsCapacity :3433 |
| §3.6 contract structure, candidates, prereqs | **PARTIAL** | Finding 1 (body↔site feasibility) + Finding 6 |
| §7.7 validator fails on a disappearing PartId | **MISSING** | Finding 3 |
| §8.2 "Data validators: schema_version, schema, cycles, reachability, **ID stability**, SOI/period" | **PARTIAL** | ID-stability row absent; everything else wired |
| §8.6 complexity budgets / exemption comments naming §8.6 | **PARTIAL** | Finding 7 |

Not flagged (checked, deliberately): duplicate colony sites are legal (runtime founding allows multiple colonies per site — sim-consistent); `stores.level > capacity` clamp is documented behavior (finding 4 asks for the warning, not a change); lab-seats ≥ researcher_slots is a vessel-level property, correctly a data test (tests/parts.rs:246-259) not a part-level rule; BOM-vs-pad-stock shortfalls are honest runtime refusals with named reasons.

## Top 3 issues
1. **[HIGH] ColonyResupply on duva is uncompletable and unfailable** — validator lacks (and structurally cannot express) the body↔site rule; shipped template exploits it (Finding 1).
2. **[HIGH] Recipe cycles/self-loops mint resources, load green** — no acyclicity/net-yield check over `data/recipes` (Finding 2).
3. **[MEDIUM] §7.7 ID-stability CI gate missing entirely** — deleting a part file validates green and breaks saves silently (Finding 3).
