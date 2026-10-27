# Verified report C32 — data validator + tests

Verifier: independent re-check of every finding against source, shipped `data/`, csp-sim/csp-view behavior, PLAN.md, and the C09 verified report. `cargo test -p csp-data`: **121/121 pass** (81 unit + 40 integration), matching the report's header. File sizes match exactly (validate.rs 6,580 lines; lib.rs 121).

---

### Finding 1: ColonyResupply candidate bodies never checked for site feasibility (duva) — CONFIRMED

**Original**: ColonyResupply arm only runs `check_bodies` + resource/amount checks; duva has no sites so colonies cannot exist there; `deadline_days: None` makes the offer unfailable; `validate_contracts` has no `sites` param so the rule is inexpressible.

**Verification**: every link checked.
- `data/contracts/contract.colony_resupply.ron`: `bodies: ["terrin", "duva", "rask"]` (line 16), `deadline_days: None` (line 29), `advance_funds: 10_000.0`. All as claimed.
- `data/systems/home/sites.ron`: exactly three sites — terrin-launch, terrin-downrange (body terrin), rask-mines (body rask). Repo-wide grep for `duva`: body.ron, biomes.ron, loc, and the two contract templates only — zero sites, zero colonies. duva IS in `system.ron` bodies, so `check_bodies` (membership-only) passes it.
- FoundColony requires an authored site: csp-sim/src/colonies.rs:1593-1595 rejects unknown sites; `SimCommand` has no site-creation variant (grep: no FoundSite/AddSite/CreateSite) — sites are authored data. (Trivial citation drift: the unknown-site rejection is at :1593-1594, not :1604 — :1604 is the adjacent colony_env check; substance unaffected.)
- The sim draws the offer body blindly: csp-sim/src/contracts.rs:377-391 (`draw_params` ColonyResupply arm) picks `bodies[pick]` with no colony-existence gating.
- `deadline_days: None` arms no deadline: contracts.rs:873-910 arms `ContractDeadline` only `Some(days)`; accept still pays the 10k advance. Permanent, completable-never, fail-never — as claimed.
- Validator structurally cannot express the rule: `validate_contracts` signature (validate.rs:4127-4133) takes loaded/sys/experiments/techtree/resources — no `sites`; `validate_dataset` holds `d.sites` (Dataset field :4284, used at :4315 for `validate_sites`) but does not pass it to `validate_contracts` (:4318-4324). The ColonyResupply arm (:4081-4098) runs only `check_bodies` + empty-resources + resource-resolution + amount-range.
- Cross-check vs C09 verified Finding 5 ("resupply 8/9 uncompletable, duds permanently occupy a slot — CONFIRMED"): consistent, not contradictory. C09 established the same facts (duva siteless, deadline None, no cancel verb, MAX_ACTIVE_CONTRACTS slot burn) and went further — 8 of 9 body×resource draws are uncompletable because no possible colony has the needed store. C32's duva claim is a strict subset, and C32's contribution (the validator-side gate that would have caught it at load) is real: the rule is currently inexpressible in `validate_contracts`.

CONFIRMED as HIGH. Fix (thread `&SiteFile`, require ≥1 site per candidate body, drop duva) is mechanical and correctly scoped to csp-data.

---

### Finding 2: No recipe-graph validation — cyclic/self-referential recipes load green and mint — CONFIRMED

**Original**: `validate_recipes` never checks self-referential rows or cross-recipe cycles with net non-negative yield; the colony flow network would run them at fraction 1.0 forever.

**Verification**:
- `validate_recipes` (validate.rs:2178-2242) checks exactly: id, id↔stem, duplicate ids, schema_version, per-row resource resolution, finite non-negative rates, ≥1 positive output. No cross-recipe pass, no graph, no ratio math. Confirmed by reading the full function.
- Cycle checks exist elsewhere only for body parent chains (validate.rs:1274+, `ParentCycle`) and the tech tree (`check_tree_cycles` :3790). Nothing anywhere in csp-data or csp-sim inspects recipe graphs (grep `cycle|acyclic`).
- Sim mechanism verified: converter outputs come straight from the recipe def (colonies.rs:685-695 within the cited 633-691 region) and `net_rates` = produced − consumed (colonies.rs:1048-1067). C09's verified Finding 1 **empirically reproduced** the mint through the real public API (`a 1.0 → b 0.5` + `b 0.5 → a 1.2`, founding stock 10 → 130 units in 600 s, fraction [1.0, 1.0, 1.0], halted only by store capacity). The runtime hole is proven, not hypothetical.
- C09 also verified the three shipped recipes (electrolysis water→oxygen, isru_propellant ore→LF+ox, smelt_ore ore→metal) are acyclic — read all three files; no output is any recipe's input. C32's framing is exactly right: the data-side acyclicity/net-yield gate is what prevents the C09 defect from recurring via future authored or modded content, and it is absent. Complementary to C09's sim-side SCC fix, not duplicate.
- PLAN §8.2's Data validators row literally lists "cycles" as a validator duty — the implementation covers only tree/body cycles.

CONFIRMED as HIGH.

---

### Finding 3: PRD §7.7 ID-stability gate missing — CONFIRMED

**Original**: PLAN §7.7 requires the validator to fail in CI on a disappearing PartId; no mechanism exists.

**Verification**:
- PLAN.md:1699-1701 quoted accurately and in full: "**ID stability**: `PartId` strings are permanent. Renaming or deleting one requires a migration entry mapping old → new, or `Deprecated(replacement)`. **The data validator fails in CI on an ID that disappears without one.**" Normative language, unconditional.
- Grep for `Deprecated` / `part-ids` / `part_ids` snapshot / `baseline` across crates/, xtask/, data/: nothing — only unrelated `CraftDef::part_ids` sim plumbing. No baseline ID list exists in data/ (directory listing checked).
- `validate_parts` (validate.rs:3338-3372): duplicates + id↔stem + per-def checks only — nothing can detect a removed id without a baseline.
- xtask bins are lint_attr, lint_deps, lint_reach, bake_art, gen_biome_maps, parts — no ID-stability/removed-id check.
- Corroborating: PLAN §8.2's validators row also lists "ID stability". And the §8.6 exemption convention ("`// lint-allow: <reason>` comment naming this section") is likewise unimplemented — grep `lint-allow` repo-wide: 0 hits, matching the report's cross-cluster note.

CONFIRMED as MEDIUM. Deleting a part file validates green today.

---

### Finding 4: Colony cross-field gaps (stores without capacity, silent clamps, drill on barren ground) — CONFIRMED

**Original**: three author mistakes produce silent zero-effect content with inputs already in `validate_colonies`' signature.

**Verification**:
- `validate_colonies` (validate.rs:2331) receives both `ColonyFile` and `SiteFile` — inputs in hand, as claimed.
- stores loop (:2517-2540): duplicate-resource, known-resource, finite/non-negative level. No capacity cross-check of any kind.
- Sim behavior verified: apply_found_colony aggregates capacity only from Store/Battery modules, `or_insert`ing capacity 0.0 for a store-named resource with none (colonies.rs:1662-1668), then `anchor_level = s.level.min(store.capacity)` — founding stock on a store-less resource silently becomes 0; over-capacity levels clamp silently. The schema doc "clamped at apply to the aggregate capacities" (csp-data/src/colony.rs:195-196) confirms clamping is by design — the finding correctly asks for a load-time signal, not a behavior change.
- Drill arm (:2398-2427): checks resource against `resources.ron` and min_concentration ∈ [0,1], never against the founding site's `resource_yields` (site_ids set at :2345 is used only for site existence). Barren-ground drill produces nothing: concentration `unwrap_or(0.0)` (colonies.rs:670), documented at csp-data/src/colony.rs:93-98 (the report's colony.rs:95-98 citation is accurate).
- Shipped Rask outpost hand-checked coherent: stores EC 3000 ≤ battery 5000, ore 10 ≤ store 100, drill resource "ore" ∈ rask-mines yields (ore 0.9).

CONFIRMED as MEDIUM.

---

### Finding 5: No file-level "≥1 launch site" rule; no starting_stock negative tests — CONFIRMED

**Original**: deleting the pad row passes validation; runtime refuses every build order and the pad spawn silently falls back to (0,0)/alt 0.

**Verification**:
- `validate_sites` (validate.rs:2033-2125): per-entry rules plus the per-body uniqueness rule at :2092-2100 ("second launch site on … — one launch site per body"). No file-level existence rule for any launch site — grep confirms nothing like "at least one launch" exists.
- csp-sim/src/manufacture.rs:134-138: `if !def.launch { return Err(Recoverable("manufacture: build orders consume a launch-site inventory")) }` — verbatim, and the message never names a missing pad.
- csp-view/src/lib.rs:2880-2896: `.find(|s| s.body == dominant && s.launch)` then `map_or((0.0, 0.0, 0.0), …)` — silent (0,0)/altitude-0 fallback exactly as cited.
- starting_stock negative tests: `validate_site_stock` (:2131-2153) exists with duplicate/unknown/negative rules, but grep across tests/ finds only the v2→v3 default-band test (unknown_fields.rs:179-191) and unrelated part "stock variant" checks — no negative validation tests.

CONFIRMED as MEDIUM.

---

### Finding 6: `ContractsCompleted { kind }` never checks the kind is authored — CONFIRMED

**Original**: the arm checks only `count > 0`; an unauthored kind makes the prereq unsatisfiable and the template unofferable, silently.

**Verification**:
- validate.rs:4221-4225: the arm matches `ContractsCompleted { count, .. }` and tests only `*count == 0` — `kind` ignored (the `..` in the pattern). The mirror check for unlocks (UnknownContractType class, :4237-4246) does exist, so the asymmetry claim is right.
- Shipped data safe by luck, as claimed: 5 templates cover all 5 kinds (ColonyResupply/PartTest/Rescue/SatellitePlacement/SurveyRun); the three `ContractsCompleted` prereqs reference SatellitePlacement and SurveyRun, both authored.
- `template_ids` is already collected (:4145-4149); collecting kinds is the same cost.

CONFIRMED as LOW.

---

### Finding 7: DRY/complexity quantification + split feasibility — CONFIRMED

**Original**: 8 identical loc_keys fns, 4 stem checks, ~12 schema_version blocks, 6 over-budget fns behind allows, split available via `pub use`.

**Verification** (all grep/measured):
- The 8 `validate_*_loc_keys` fns exist at exactly the cited lines — 1668, 2159, 2248, 2549, 3470, 3597, 3924, 4253 — and are structurally identical (`seen: BTreeSet` + insert + `!loc.contains(key)` push). Read three in full; the pattern holds.
- 4 id↔stem checks at 2195 (recipes), 3361 (parts), 3525 (experiments), 4159 (contracts) — matches :3359-3369/:2193-2204/:3523-3533/:4157-4168.
- Schema-version gates: 21 `SchemaVersion {` construction sites total (enum def + Display + validation pushes) — "~12 near-identical gate blocks" is an accurate count of the actual gates (system, body-via-system, biomes, resources, balance, sites-file, site-entry, colonies, recipes, experiments, contracts, techtree, parts).
- `is_finite()` occurrences: 73 file-wide; the report's "52 inline range checks" is a consistent subset.
- Function extents measured by brace matching: `validate_balance` 238 lines (report: 237), `validate_colonies` 213 (212), `Display::fmt` 178 (178), `fmt_part` 148 (147), `validate_contract_space` 146 (145), `validate_contracts` 122 (121). All within 1. PLAN §8.6 verified: `too_many_lines` ≤ 100 warn / ≤ 200 error; two fns (validate_balance 238, validate_colonies 213) exceed the error budget behind `#[allow]`s.
- The 7 `#[allow]` sites at exactly :660, :846, :1770, :2329-2330, :3955, :4126; none carries a `lint-allow` comment naming §8.6 (0 hits repo-wide), which §8.6's own text requires ("exemptions require a `// lint-allow: <reason>` comment naming this section").
- Split feasibility: lib.rs:114-121 re-exports every public item from `validate::`; grep for `validate::` path usage in csp-sim, csp-view, csp-server, apps: zero — all external callers go through the flat re-exports. A `validate/` module directory keeps every caller untouched. The map's "unsplittable" claim is indeed wrong.

CONFIRMED as MEDIUM (maintainability), quantification accurate.

---

### Finding 8: dock-port diagnostics omit the node name — CONFIRMED

**Original**: the two BadModule reasons are static strings that don't say which node.

**Verification**: validate.rs:3216-3231 — "dock_port node names no attach node" (:3222) and "two dock ports claim the same attach node" (:3227), neither including `node`. `BadModule::reason` is `&'static str` (variant at :443-447) while `InvalidSite`/`InvalidColony`/`InvalidRecipe` already carry `reason: String` — the suggested fix is feasible.

CONFIRMED as NIT.

---

## PRD coverage table — spot-verified, all rows accurate

- §3.0 geometry: `PERIOD_REL_TOL = 1e-9` at :59; `SoiMissing` variant :184, fired :1329; sibling-SOI overlap fn :1460-1473 called from the per-body pass at :1445; periapsis-clearance and SOI-nesting checks live in the same :1380-1515 pass. All present.
- §3.0.1 biomes: full-decode checks at :1589-1662 region.
- §3.1 version handling: `read_versioned_ron` (load.rs:355-374) probes `SchemaProbe` BEFORE the full parse — probe-before-parse confirmed verbatim. Colonies v1 refusal documented at load.rs:118-128 (report cited 122-126). `SITE_SCHEMA_SUPPORTED = [v-1, v]` = [2,3] (site.rs:45). (Side observation, out of scope: the load.rs comment says "v1 and v2" — stale wording now that SITE_SCHEMA_VERSION is 3; the code band is [2,3] as the report says.)
- Unknown fields DENY: `deny_unknown_fields` present in all 12 schema modules (84 markers); unknown_fields.rs has 5 `misspelled(` cases and the v2-sites band test. DENY confirmed.
- §3.3/§3.4: `check_tree_cycles` :3790, `UnknownTechRequirement` :587/:3657, `TechCostNotMonotonic` :549/:3736, `ConductionExceedsCapacity` :480/:3433 — all present.
- §3.6 PARTIAL / §7.7 MISSING / §8.2 PARTIAL / §8.6 PARTIAL rows are justified by confirmed Findings 1, 6, 3, 7 respectively.
- Error surfacing: csp-view `ViewError::Validation` Display takes the first 10 errors (csp-view/src/lib.rs:1694-1696), as cited.

## Verdict

8/8 findings confirmed; 0 dismissed. No false positives found. Severity ratings are appropriate (the two HIGHs are corroborated by independently verified C09 findings). Line citations are accurate to within 1-2 lines throughout; the only drift (colonies.rs:1593 vs :1604, load.rs:122-126 vs 118-128) does not affect any claim.
