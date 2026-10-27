# Data & Scripting — Clanker Space Program

The canonical reference for the **data layer** (`data/` + `crates/csp-data/`) and the
**automation surface** (`crates/csp-script/` Rhai host, `crates/csp-ai/` autopilot,
`crates/csp-server/` HTTP/WS/MCP, `apps/telemetry/` SPA). Audience: a competent Rust
dev with zero domain knowledge. Normative sources: `PLAN.md` (the PRD; sections cited
as §N), and **the working tree** — where they diverge, both are stated (§8).

Sibling docs: [ARCHITECTURE.md](ARCHITECTURE.md) · [SIMULATION.md](SIMULATION.md) ·
[GAMEPLAY-GUIDE.md](GAMEPLAY-GUIDE.md) · [UI-AND-RENDERING.md](UI-AND-RENDERING.md) ·
[BUILD-AND-DEV.md](BUILD-AND-DEV.md) · [AGENTS.md](../AGENTS.md). Commands assume repo
root; `cargo xtask …` is NOT an alias — always `cargo run -p xtask -- …`.

---

## 1. The data layer

### 1.1 Directory layout — every file type under `data/`

| Path | Type | Schema (crate symbol) | Loader |
|---|---|---|---|
| `data/parts/<id>.ron` | Part definitions — **16 files** (e.g. `csp.chute.s125.ron`, `csp.engine.sparc.ron`) | `PartDef` (`csp-data/src/part.rs`), `PART_SCHEMA_VERSION = 1` | `load_parts` |
| `data/resources.ron` | The 10-resource registry (id → kg/unit) | `ResourceFile` (`resource.rs`), v1 | `load_resources` |
| `data/recipes/<id>.ron` | Colony converter recipes — 3 files | `RecipeDef` (`recipe.rs`), v1 | `load_recipes` |
| `data/experiments/<id>.ron` | Experiment kinds (caps, transmit ratio) — 1 file (`exp.materials_study`) | `ExperimentDef` (`experiment.rs`), v1 | `load_experiments` |
| `data/systems/<sys>/system.ron` | System manifest: `id`, `star`, `bodies[]` | `SystemDef` (`system.rs`), `SCHEMA_VERSION = 1` | `load_system` |
| `data/systems/<sys>/<body>/body.ron` | One celestial body per manifest entry (solen/terrin/rask/duva) | `BodyDef` (`system.rs`), v1 | `load_system` |
| `data/systems/<sys>/<body>/biomes.ron` | Biome legend (palette index → biome id) | `BiomeLegend` (`biome.rs`), v1 | `load_system` |
| `data/systems/<sys>/<body>/biomes.png` | Palette-indexed equirect biome map (64×32) — indexed by `BodyDef::biome_map` | header-inspected (`biome.rs::inspect_indexed_png`) | `load_system` |
| `data/systems/<sys>/sites.ron` | Surface sites: pads, recovery sites, mineable yields, **starting stockpile** | `SiteFile`/`SiteDef` (`site.rs`), `SITE_SCHEMA_VERSION = 3` | `load_sites` |
| `data/systems/<sys>/colonies.ron` | Authored starting colonies (one Rask mining outpost) | `ColonyFile`/`ColonyDef` (`colony.rs`), `COLONY_SCHEMA_VERSION = 2` | `load_colonies` |
| `data/contracts/<id>.ron` | Contract templates — **6 files** | `ContractTemplate` (`contract.rs`), v1 | `load_contracts` |
| `data/techtree/<name>.ron` | Tech-tree fragments (`home.ron`: **4 nodes + 5 upgrade tracks**) | `TechTreeDef`/`TechNode`/`UpgradeTrack` (`techtree.rs`), v1 | `load_techtree` |
| `data/balance.ron` | ~45 global tunables (physics caps, science, economy, reliability, `script_op_budget`) | `BalanceDef` (`balance.rs`), `BALANCE_SCHEMA_VERSION = 11` | `load_balance` |
| `data/loc/en.ron` | English string table every `LocKey` resolves against | `LocFile` (`loc.rs`), v1 | `load_loc` |
| `data/eval-scenarios/*.ron` | 5 `csp-eval` scenario files (§5.8) | `csp_bench::scenario` types | csp-eval bin |
| `data/test-fixtures/<scenario>/` | Frozen golden-replay data sets + artifacts (§2) | — | csp-bench |
| `data/art/*.csar` | Baked art packs (`parts.csar`, `terrain.csar`); excluded from the data-set hash | `.csar` format (`xtask/src/bin/bake_art.rs`) | bake-time output |
| `data/ATTRIBUTION.md`, `data/LICENSE` | Asset license manifest (`path \| source \| license \| url` lines; `xtask lint-attribution`) | — | — |

Registries (`parts/`, `contracts/`, `recipes/`, `experiments/`, `techtree/`) are
directory walks: `<id>.ron` files, subdirectories allowed as organization only; a
**missing directory loads as an empty set** (`load.rs`, the `load_experiments`
convention). `sites.ron`/`colonies.ron` are optional per system. `resources.ron`,
`balance.ron`, `loc/en.ron` are required (missing = `LoadError::Io`).

### 1.2 File-shape conventions (all of `data/`)

- **`schema_version: u32` is the first field of every file.** The loader probes it
  *before* the full parse (`load.rs::read_versioned_ron` + `SchemaProbe`) so a stale
  file reports its version, not a confusing missing-field error. Version acceptance
  is per file type: most take exactly the current version; `sites.ron` accepts the
  current **and previous** (v2 lacks `starting_stock` → reads empty; `SITE_SCHEMA_SUPPORTED = [2, 3]`);
  `colonies.ron` accepts **only v2** (its v1→v2 restructure has no honest mapping —
  refused outright). Pre-1.0 policy: a schema bump may invalidate data files with no
  migration written (PLAN §7.7).
- **Stable string IDs everywhere** (`csp.engine.sparc`) — never indices; saves and
  science keys depend on them (PLAN §3.1). For registry files, the **id must equal
  the file stem** (validator rule).
- **Every struct is `#[serde(deny_unknown_fields)]`** — an unknown key is a load
  error, not a silent skip.
- **No path traversal**: authored names that become path components (`system.bodies`
  entries, biome refs) must be tame single-segment names; symlinks anywhere in a
  scanned tree are refused (`load.rs::join_authored`, `LoadError::UnsafeName`/`Symlink`).
- **`LocKey` indirection**: display names are keys into `data/loc/en.ron`, and the
  validator checks every key resolves (`validate_*_loc_keys` family).
- Determinism-domain rules in `csp-data`: no `HashMap`/`HashSet`, no `std::time`,
  libm-only floats (crate `clippy.toml` lints).

### 1.3 Schema walkthroughs

#### PartDef — `data/parts/<id>.ron` (`csp-data/src/part.rs::PartDef`)

Field order: identity → economics → shape → scaling → physics → functionality →
reliability → gating. A complete real example: `data/parts/csp.chute.s125.ron` (78
lines, includes a `Parachute` module); an engine example: `csp.engine.sparc.ron`.

- `schema_version`, `id`, `family` (upgrade/qualification family, e.g.
  `csp.family.recovery`), `display_name` (LocKey), `category` (`PartCategory`:
  Engine/FuelTank/Structure/Control/Payload/Science/Utility — drives the editor
  browser **and** a validator rule: an `Engine` part must carry an `Engine` module, etc.).
- `mass_dry_kg`, `cost_funds` — at scale 1, stock variant.
- `size_m: (x, y, z)` — bounding box at scale 1, **+z is the stack axis** (thrust points +z).
- `build_resources: [(ResourceId, f64)]` — the manufacture BOM, spent from a site's
  `starting_stock` by `ManufactureCraft`.
- `attach_nodes: [{id, position_m, direction, diameter_m}]` — the editor snap graph;
  mates need opposing unit-normals and equal size class.
- `scale: ScaleRule` — `None | Discrete([factors]) | Continuous {min, max, exponents}`;
  exponents are per-stat-family `s^e` (mass/cost/thrust/capacity/structural, bounded [0,4]).
  Discrete-scaled parts may not carry `Tank`/`Engine` modules (a size menu cannot scale their stats).
- `collider: ColliderDef` — `Box{half_extents_m}` | `Cylinder{radius_m, half_height_m}` | `Convex{points}`.
- `aero: {cd_curve, cl_curve, ref_len_m}` — piecewise-linear in Mach, clamped outside.
- `thermal: {heat_capacity_j_per_k, max_temp_k, emissivity, conduction_w_per_k}`
  (validator: conduction ≤ capacity — thermal-Euler stability).
- `structural: {max_axial_n, max_shear_n, max_torque_nm}`.
- `variants: [{id, display_name, cost_multiplier, stat_multipliers: [(StatPath, f64)]}]`
  — multiplier sets over the base def, never new defs. Convention: every part ships a
  unit `"stock"` variant.
- `modules: [ModuleDef]` — see table below.
- `reliability: ReliabilityDef` — `{base_mtbf_s, infant_mortality, qualification_curve
  (QualKnots), regime_stress ([{kind: StressKind, threshold, multiplier}]),
  dormant_stress, failure_modes: [(FailureMode, weight)]}`. Stress kinds: OverThrottle,
  BurnBeyondRated, Overtemperature, ExcessG, UllageNegativeIgnition, ExcessRestarts.
  Failure modes: DegradedThrust, StuckGimbal, IgnitionFailure, Leak, SensorFault,
  StuckDecoupler, PanelJam, Explosion (the only fatal one).
- `tech_requirement: TechNodeId` — the node that unlocks this part (the *only*
  part↔tree edge; nodes do not re-list parts).

`ModuleDef` variants (`part.rs::ModuleDef`) — the functional payloads:

| Module | Key fields |
|---|---|
| `Engine` | `thrust_vac_n, isp_vac_s, isp_sl_s, throttle_range, gimbal_deg, ignitions: Option<u32>, rated_burn_s, propellants: [(resource, mass fraction)] (sum 1.0), ullage_required` |
| `Tank` | `capacity: [(resource, units)]` — fuel tank, cargo bay, and battery (`electric_charge` has mass 0) |
| `Control` | `torque_nm, power_w` — reaction wheel/SAS |
| `Decoupler` | marker |
| `DockPort` | `size: PortSize (Small/Medium/Large), node: <attach_nodes id of the port face>` |
| `CrewSeat` | `slots` |
| `Experiment` | `kind: ExperimentId, data_units, reusable, situations: SituationMask` (bitmask over Landed/Splashed/FlyingLow/FlyingHigh/SpaceLow/SpaceHigh) |
| `Drill` | `resource, base_rate_per_s, min_concentration, power_w` |
| `Converter` | `recipe: RecipeId, priority, power_w` |
| `Lab` | `throughput_data_per_s, multiplier, researcher_slots` |
| `Parachute` | `deploy_alt_m, deploy_speed_mps, packed_drag_area_m2, deployed_drag_area_m2, deploy_time_s` |

#### resources / recipes / experiments

- `resources.ron` (`resource.rs`): 10 entries; the only physical datum is
  `mass_per_unit_kg` (0 legal — `electric_charge`). `ELECTRIC_CHARGE = "electric_charge"`
  is pinned in code (`resource.rs`) because several rules name it literally.
- `recipes/<id>.ron` (`recipe.rs`): `inputs`/`outputs` as `(resource, units/s)` rate
  pairs — the colony flow network's stoichiometric rows; `outputs` must have ≥1
  positive entry.
- `experiments/<id>.ron` (`experiment.rs`): `transmit_ratio` (0.2–1.0) plus `caps`:
  per `(body, SituationMask)` asymptotic data ceilings; the per-biome soft-cap ledger
  keys on the full `(experiment, body, biome, situation)`.

#### System / Body / biomes — `data/systems/home/`

`system.ron` (`SystemDef`): `id, star, bodies: [solen, terrin, rask, duva], epoch_note`.
`<body>/body.ron` (`BodyDef`): `parent, radius_m, mu_m3_per_s2, soi_m`
(None = ∞ for the star), `rotation_period_s` (None = tidally locked — Rask),
`tilt_rad, orbit: Option<KeplerElements>` (incl. a redundant authored `period_s` the
validator checks against `2π√(a³/μ_parent)` to 1e-9 — `PERIOD_REL_TOL`), `atmosphere:
Option<AtmosphereDef>` (exponential `p0·exp(−h/H)` to a hard cut at `top_m`, plus a
piecewise-linear temperature table), `ocean: Option<{datum_radius_m}>`, `terrain:
{noise_seed, heightmap}`, `biome_map: Option<{png, legend}>`, `surface_resources`
(empty in v1 — schema-only), `insolation_w_m2`.

`biomes.ron` (`BiomeLegend`): `entries: [{index: u8, id: BiomeId, name: LocKey}]`.
The paired PNG must be an 8-bit-palette equirect ≤2048×1024 (validator), every painted
index legended. **Biomes are deliberately decoupled from terrain noise** (PLAN §3.0.1):
science keys must survive terrain tuning. The PNGs are generated — see §2.3.

#### sites.ron — `SiteDef` (`site.rs`, v3)

Per site: `id, display_name, body, latitude_rad, longitude_rad, altitude_m, launch: bool`
(at most one launch site per body; launch sites get recovery efficiency 1.0),
`resource_yields: [(resource, fraction)]` (v2 — what colony `Drill`s extract; a
flat per-site stand-in for the never-authored PNG concentration maps), and
`starting_stock: [(resource, units)]` (v3 — the seeded site inventory that
`ManufactureCraft` BOMs and `RunBenchTest` stands spend, and that recovery re-credits).
Real file: `data/systems/home/sites.ron` — `terrin-launch` carries
`starting_stock: [("metal", 2000.0), ("composite", 400.0)]`.

#### colonies.ron — `ColonyDef` (`colony.rs`, v2)

`{site: SiteId, display_name, modules: [ColonyModuleDef], stores: [{resource, level}]}`
(founding levels clamped to module-declared capacity). Founding order = `ModuleIndex` =
the flow-network allocation tie-break. Modules: `Solar{area_m2, efficiency}`,
`Generator{fuel, fuel_rate_per_s, rate_per_s, priority}` (`fuel_rate_per_s: 0` = RTG),
`Drill{resource, base_rate_per_s, min_concentration, power_ec_per_s}`,
`Converter{recipe, priority, power_ec_per_s}`, `Store{resource, capacity}`,
`Battery{capacity}`, `Radiator{area_m2, emissivity}`,
`LifeSupport{consumes_per_crew, power_ec_per_s}`. Real file:
`data/systems/home/colonies.ron` — the Rask mining outpost (Solar 40 m²/0.20, Battery
5000, ore Drill, `recipe.smelt_ore` Converter, ore/metal stores).

#### contracts — `ContractTemplate` (`contract.rs`, v1)

`{id, display_name, description, kind, generate: {weight, space}, prerequisites,
rewards: {advance_funds, completion_funds, failure_penalty_funds}, deadline_days}`.
`ContractKind` (5): `SatellitePlacement, SurveyRun, PartTest, Rescue, ColonyResupply` —
each with a matching `ContractParamSpace` variant describing the deterministic draw
space (weighted template pick, then per-kind params from the seeded generator).
`ContractPrereq` is fact-based (no reputation currency): `TechResearched`,
`ContractsCompleted{kind, count}`, `BodyVisited`, `CrewLostAtMost`. Six templates live
in `data/contracts/`. PLAN §3.6 names seven v1 types; **crew transport and tourist
flight are deferred** (documented in the `contract.rs` module doc — their predicates
have no honest sim-state reading yet).

#### techtree — `TechTreeDef` (`techtree.rs`, v1)

Fragments: `nodes: [TechNode]` + `tracks: [UpgradeTrack]`, merged in file order
(`LoadedTechTree::merged`). `TechNode = {id, display_name, description, science_cost,
requires: [TechNodeId], unlocks: {track_levels: [(track, cap)], facilities:
[FacilityId], contract_types: [ContractTypeId]}}`. Parts unlock via the
`PartDef::tech_requirement` edge (not re-declared on nodes). `UpgradeTrack =
{id, family, stat_path: StatPath, levels: [{science_cost, multiplier_or_delta:
Multiplier(f) | Delta(f)}]}` — node grants raise the purchasable **cap**; each level
still costs its own science. `data/techtree/home.ron` ships 4 nodes (`tech.start`
0 → `tech.science.1` 15 → `tech.rocketry.2` 30 → `tech.rocketry.3` 60) and 5 tracks.
(Note: the review map's "9-node techtree" counts nodes+tracks; the file has 4 nodes.)

#### balance.ron — `BalanceDef` (`balance.rs`, v11)

~45 named tunables, each a mirror of a sim-side default: bubble radius 20 km,
`advance_to_max_events_per_frame` 2000, object caps, docking capture thresholds,
transfer radius/rate, science soft-cap `k` and `lab_science_per_data`,
`transmit_ec_per_data_unit`, `starting_funds_micros`, salary cadence, hire cost,
recovery efficiency 0.9, `contract_offer_slots`, `manufacture_rate_kg_per_s`, life
support rates, **`script_op_budget: 20_000`**, and the reliability block (dormant
scale, ignition hazard, bench costs). ALL tuning lives here (PLAN §8.3).

### 1.4 Loading pipeline — loaders → validator → game wiring

```
csp_data::load_* (load.rs)          — file-shape only: presence, RON syntax,
  │                                   schema_version band, tame names, no symlinks
  ▼
validate::Dataset (11 file kinds — the loader's product;
  │              csp_data::validate::Dataset, validate.rs)
  ▼
csp_data::validate::validate_dataset — ALL semantic rules, one typed error list
  │                                    (Vec<ValidationError>, never asserts)
  ▼
View::from_source (csp-view/src/lib.rs) — installs into the sim World:
  PatchedConics::from_body_defs, event budget, sites, colonies, contracts,
  tech tree, script engine (RhaiHost), autopilot driver (AscentAutopilot),
  script op budget, VAB catalog, biome/loc/terrain resources
```

- Two `DataSource` impls (`load.rs`): `FsDataSource` (native) and `EmbeddedSource`
  (compile-time table — the wasm web demo's reduced set rides the same loader and checks).
- The game's entry point: `View::new()` → `load_validated_dataset`
  (`csp-view/src/lib.rs`) — **any validation error refuses startup** with the whole
  error list (`ViewError::Validation`), never a silent default. `data/` resolves via
  `csp_view::data_root()` (CWD first, then repo-relative from the crate manifest).
- Registry load order is deterministic: ascending `/`-joined relative path, byte-wise
  (`load.rs::ron_sort_key`) — the same order the data-set hash uses.

### 1.5 Validation rules (crates/csp-data/src/validate.rs, ~6.6k lines)

`validate_dataset` (validate.rs) runs every sweep: system, resources, parts (with
resources/recipes/experiments), sites, experiments, techtree (with parts), contracts
(with system/experiments/techtree/resources), recipes, balance, colonies, loc + the
per-kind loc-key sweeps + surface-resource refs. Highlights:

- **System** (`validate_system`): manifest ↔ defs agree exactly; parent exists,
  acyclic, terminates at `star`; parent SOI exceeds every child apoapsis; orbital
  period consistent to 1e-9; atmosphere top > scale height; temperature table ≥2
  strictly-increasing knots; biome legend indices unique and inside the PNG palette;
  PNG decodes as real 8-bit palette ≤2048×1024 with every painted index legended;
  sibling SOI overlap checked.
- **Parts** (`validate_parts`): id↔stem; positive masses/sizes; variants non-empty,
  unique, positive multipliers, unique stat paths; scale rules sane; attach nodes
  unique-id unit-normal; category↔module consistency; resource refs resolve against
  `resources.ron`; engine `isp_sl ≤ isp_vac`, propellant fractions sum to 1; parachute
  deployed > packed area; reliability ranges; thermal conduction ≤ capacity.
- **Sites** (`validate_sites`): body ∈ manifest, one launch site per body, stock
  resources resolve. **Colonies** (`validate_colonies`): site exists, recipe refs
  resolve, rates positive, one-EC-draw check (`check_power_collision`).
- **Techtree** (`validate_techtree`): no cycles, prereqs resolve, cost monotonic in
  depth, every part's `tech_requirement` names a node ("every part reachable"),
  track `(family, stat_path)` unique, every stat path resolves on every family part,
  node `contract_types` reference real templates.
- **Contracts** (`validate_contracts`): weights > 0, param space matches `kind`, draw
  ranges non-inverted, prereq refs resolvable, rewards finite.

The same sweep runs in CI over `data/` via the csp-data test suite
(`crates/csp-data/tests/`: parts, home_system, unknown_fields, colonies, contracts,
balance, experiments, techtree) and at every game startup.

### 1.6 The data-set hash (`datahash.rs::data_set_hash`)

BLAKE3-256 over the concatenation, ascending path order, of
`path_bytes || 0x00 || u64-LE len || canonicalized_bytes` for every file under the
`data/` root **except** everything under `data/test-fixtures/` and art-extension files
(`png jpg jpeg tga webp ktx2 ogg wav glb csar` — `ART_EXTENSIONS`) *only when under
`data/art/`* (biome PNGs elsewhere **are** hashed). Canonicalization: UTF-8 text →
LF line endings, trailing whitespace stripped; non-UTF-8 verbatim. The walk refuses
symlinks, non-UTF-8 names, and ASCII-case-colliding paths. Scheme version:
`DATA_HASH_VERSION = 2` (`csp-data/src/lib.rs`) — v2 added the length prefix after
the v1 framing was shown to be non-injective. The hash is:

- recorded in the save header (`csp-sim/src/save.rs`, `data_set_hash`) —
  PLAN §7.7's data-set drift warning keys on it;
- frozen into every `BuiltPart` at manufacture (`csp-sim/src/tech.rs`) — an in-flight
  craft keeps the numbers it was built with even if data changes;
- mirrored over a golden's fixture dir as `fixture_hash` by csp-bench (§2.2).

### 1.7 Authoring recipes — how to add content end-to-end

All of these are pure data edits + validator pass; no code changes. Run
`cargo test -p csp-data` (or just launch the game) to validate.

**Add a part** (template: `data/parts/csp.chute.s125.ron` — smallest complete file):
1. Copy the template to `data/parts/csp.<family>.<name>.ron`; the file stem **is** the id.
2. Set `family` (reuse an existing one like `csp.family.engine.launch` to inherit its
   upgrade tracks, or coin one — then tracks are optional), `display_name` LocKey,
   `category` (must match the modules you carry — the validator enforces it).
3. Fill physics/economics; add modules from the §1.3 table; every resource id you
   reference (build_resources, tank contents, propellants, drill output) must exist in
   `data/resources.ron`.
4. Add the `display_name` (and any variant names) to `data/loc/en.ron` — key
   convention `part.<id>.name`, `part.variant.<vid>`.
5. Point `tech_requirement` at an existing node (usually `tech.start` for a starter
   part) or add a node (below).
6. Costs: `build_resources` must be payable from the pad's `starting_stock` if you
   expect it manufacturable at game start.
7. Validate: `cargo test -p csp-data`. Note the **wasm demo parity tree**
   (`apps/demo/data/parts` — also 16 parts) is a separate reduced set; keeping them
   in sync is manual.

**Add a colony** (template: `data/systems/home/colonies.ron`): append a `ColonyDef`
to `colonies`; `site` must exist in `sites.ron`; converter `recipe` must exist under
`data/recipes/`; founding `stores` clamp to module capacities. EC math sanity: solar
output depends on the site's latitude and the body's insolation (Rask is tidally
locked — permanently daylit sites never see `PowerLost`).

**Add a site**: append to `sites.ron` — one `launch: true` per body max;
`resource_yields` feed drills; `starting_stock` seeds the site inventory.

**Add a contract type** (template: `data/contracts/contract.colony_resupply.ron`):
a template is data, but a **new `ContractKind` needs code** (the completion predicate
lives in `csp-sim/src/contracts.rs`). A new template over an existing kind is data
only: pick the kind, author `generate.space` (must match the kind), prereqs, rewards;
unlock it by naming its id in a tech node's `unlocks.contract_types`.

**Add a tech node / upgrade track** (`data/techtree/home.ron` or a new fragment):
append a `TechNode` (`requires` acyclic, cost monotonic in depth — validator) and
optionally `UpgradeTrack`s (`family` + `stat_path` must resolve on every part of that
family); grant track caps from nodes via `unlocks.track_levels`; add loc keys for
name/description. Parts then point at the node via `tech_requirement`.

**Add a resource / recipe / experiment**: append to `resources.ron` / drop a file in
`recipes/`/`experiments/` (id = stem). New experiment caps are per
`(body, situation)`; a run in an uncovered context is rejected by the sim.

---

## 2. What breaks when data changes — and what to regenerate

Data changes never silently break saves or replays, but they do move hashes. The
blast radius:

| You changed… | What moves | Action |
|---|---|---|
| Any hashed file under `data/` (all of it except `data/art/*` art extensions and `data/test-fixtures/`) | `data_set_hash` → new saves record the new hash; loading an old save **warns on drift** (PLAN §7.7); in-flight craft are safe (BuiltPart froze their numbers) | None required; expect the drift warning on old saves |
| Shipping data (parts/bodies/balance) | **Golden replays do NOT red** — they load frozen copies under `data/test-fixtures/<scenario>/` (PLAN §8.3) | None |
| Fixture data or sim serialization | Golden replays red; bincode artifact may fail to decode | `cargo run -p xtask -- bless-goldens --reason "<behavioural change>"` |
| Biome map rules (the generator's authored region rules) | `data/systems/home/*/biomes.png` must stay byte-identical to the generator | Run `cargo run -p xtask --bin gen_biome_maps --check` manually — **not CI-gated** (no workflow or test invokes it); regenerate without `--check` |
| Art sources (`assets/src/`) | `data/art/*.csar` packs | `cargo run -p xtask --bin bake_art` |
| A schema field | Bump the file-type's `SCHEMA_VERSION` const in the same commit; decide the acceptance band (exact vs current+previous) per §1.2 |

### 2.1 bless-goldens (`xtask/src/main.rs::bless_goldens`)

Regenerates every golden (walks the scenario registry, records into
`data/test-fixtures/<scenario>/`: `replay.bin`, `expected-hash.txt`,
`fingerprint.json` — `csp-bench/src/replay.rs::ARTIFACT_FILES`). Refusals, by design
(PLAN §8.4): **dirty tree** (commit/stash first); **orphan goldens** whose recipe no
longer exists; **partial artifact sets** (all-or-nothing per dir); and **hash change
without a `csp_core::HASH_SCHEMA_VERSION` bump** unless `--reason "<text>"` documents
the behavioural change (echoed into the summary for the commit message).

### 2.2 Golden artifacts & the fingerprint (`csp-bench/src/replay.rs`)

Artifact shape (§7.6): `{fingerprint, save_seed, data_fixture_hash, initial snapshot,
final_tick, Vec<(CommandKey, SimCommand)>}` in bincode (fixed `standard`
configuration) — **not a save format**; replays are version-scoped by T3 with no
migration promise. The `fingerprint.json` sidecar names everything that can
legitimately move a golden hash: rustc version, target triple, parry/rapier/libm/
rand_chacha/blake3/**rhai** versions, `HASH_SCHEMA_VERSION`, `KDF_VERSION`,
`DATA_HASH_VERSION`, and the fixture's own data hash. Verify one golden:
`cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/flight-launch`
(exit 1 = hash mismatch, 2 = harness error).

**Known issue (working tree)**: the `flight-launch` golden currently fails at the
bincode **decode** step against the uncommitted W2.6 serialized-type churn — passes
at HEAD; noted in `.review/session-prd1/STATUS.md`. A `bless-goldens --reason` run is
the documented path once the churn lands.

### 2.3 gen_biome_maps (`xtask/src/bin/gen_biome_maps.rs`)

The biome PNGs are **outputs of this program** — the region rules in its source are
the authored content. 64×32 equirect palette PNGs, deterministic dependency-free
encoder. `--check` regenerates in memory and byte-compares against the committed
files — run it manually after touching the rules; **nothing in CI invokes it**.
To change biome geography: edit the pixel rules in the bin, run it
without `--check`, and remember biome ids are load-bearing science keys.

### 2.4 bake_art (`xtask/src/bin/bake_art.rs`)

Bakes `assets/src/` into `data/art/*.csar` (Clanker Space Art Resource: deflated
image/mesh entries; format documented in the bin header). Part models are
**procedurally authored** in `xtask/src/bin/parts/mod.rs` (lathe/sweep/prism
aerospace geometry — the Kenney kit was retired). `.csar` files are excluded from the
data-set hash, so art bakes never move it.

---

## 3. Scripting — the Rhai host (`csp-script`) and its sim side (`csp-sim/src/scripts.rs`)

One API, one guest language, one set of semantics (PLAN §2.10). Scripts are
**tick-entry-point programs, not coroutines**: the host calls `fn on_tick(ctx)` once
per tick; there is no resumable VM.

### 3.1 Lifecycle and tick model

- **Load**: `SimCommand::LoadScript {vessel, source}` (`csp-sim/src/lib.rs:1106`).
  The apply allocates the next `ScriptId` and calls `ScriptEngine::prepare`
  (`csp-script` `RhaiHost::prepare`) — the §2.10 gate: bare `wait(...)`/`until(...)`
  are compile errors (`preprocess`), the script must define `fn on_tick(ctx)` with
  exactly one parameter, and a compile fault rejects the command `Recoverable`
  (never a silent dead script).
- **Tick**: inside `step_bubble_tick`, **before force integration**, in ascending
  `ScriptId` order; engaged autopilots run after scripts. Effects are atomic: a
  faulted call drains **no** effects.
- **Latency**: every host function enqueues a `SimCommand` stamped
  `SourceClass::Script` that drains at the next tick's boundary — scripts pay the
  same one-tick latency as UI/MCP (nothing bypasses the queue).
- **Off-bubble**: a script bound to an `OnRails`/`SurfaceLatched` vessel runs *only*
  when its `ScriptWake` event fires (§3.4). In-bubble, wakes are vacuous — the bubble
  ticks the script every tick anyway (a firing wake is consumed silently to avoid
  double-ticking).
- **Disable**: 5 consecutive faulting calls (`STRIKE_DISABLE_LIMIT`, scripts.rs)
  disable the script **visibly** (`ScriptState::disabled_reason` shows in the UI).
  `UnloadScript` drops it; `SetScriptEnabled` parks/resumes without losing `persist`.
- A world with no engine installed (`set_script_engine` never called) still stores
  scripts; they simply never tick (engine presence is construction config, §7.3).

### 3.2 The `ctx` map (Rhai)

`on_tick(ctx)` receives a map with `tick`, `script` (its own id), `persist` (the live
map — writes land in the save), and `vessel` — the bound vessel's telemetry:
`altitude, apoapsis, periapsis, time_to_apoapsis, time_to_periapsis, speed,
vertical_speed, horizontal_speed, throttle, mass, residence ("rails"|"bubble"|
"latched"|"missing"), found, pos {x,y,z}, vel {x,y,z}, rot {w,x,y,z}, mu, body,
resources (map id→units), parts (array of `#{node, part}` — the handles
`stage`/`jettison`/`deploy` take), node/node_tick (maneuver, when scheduled)`.
Missing values surface as `NaN`. (Built in `csp-script/src/lib.rs::telemetry_to_map`
over `csp_sim::VesselTelemetry`.)

### 3.3 The ControlApi surface — FROZEN at M9c

The Rust trait (`csp-script/src/lib.rs::ControlApi`) is the one surface shared by the
Rhai host, the Rust autopilot (`csp-ai` drives `IoControlApi`), and — conceptually —
the MCP tools. **Frozen**: `crates/csp-script/tests/api_freeze.rs` pins the method
set by reading the trait source; drift is a test failure and requires a
`csp.mcp.tools` major-version bump.

Rust trait methods (17): `telemetry`, `telemetry_of(vessel)`, `set_throttle(f)`,
`steer_dir(dir)`, `hold_attitude()`, `jettison(node)`, `add_node(tick, dv)`,
`delete_node()`, `transfer_resource(to, resource, amount)`, `add_alarm(tick, label)`,
`request_wake(condition)`, `cancel_wake()`, `log(line)`, `deploy(node)`,
`engage_autopilot(apo_m, pe_m)`, `disengage_autopilot()`, `vessels()`.

Guest (Rhai) names — `CONTROL_API_SURFACE` (25 entries), each verified callable:

| Guest call | Effect (`SimCommand` issued) |
|---|---|
| `set_throttle(f)` | `SetThrottle` (clamped [0,1] at apply) |
| `steer(x, y, z)` | `SetAttitude` — craft +Z (thrust axis) onto the world dir; zero vector = hold |
| `steer_quat(w, x, y, z)` | `SetAttitude` with an explicit quaternion (scalar-first) |
| `hold_attitude()` | `SetAttitude` at the tick's snapshot orientation (the SAS hold) |
| `stage(node)` / `jettison(node)` | `JettisonPart` — the staging primitive (both names, same verb) |
| `add_node(tick, pro, norm, rad)` | `SetManeuver` |
| `delete_node()` | `DeleteManeuver` |
| `transfer(to, "resource", amount)` | `TransferResource` (same-bubble vessels) |
| `add_alarm(tick, "label")` | `SetAlarm` (absolute-tick `PlayerAlarm` event) |
| `log(s)` / `print(s)` | line into the report's `script_log` → player console |
| `deploy(node)` | `DeployPart` — parachute/decelerator verb |
| `engage_autopilot(apo_m, pe_m)` | `EngageAutopilot` (`AutopilotProgram::Ascent`) |
| `disengage_autopilot()` | `DisengageAutopilot` |
| `telemetry_of(id)` | read — any vessel's telemetry map (`#{found:false}` when unknown) |
| `vessels()` | read — every live vessel's telemetry array |
| `wait_ticks(n)`, `wait_until_tick(t)`, `wait_apoapsis()`, `wait_periapsis()`, `wait_soi()`, `wait_node()`, `wait_altitude(m, ascending)` | wake registration (§3.4) |
| `cancel_wait()` | drop the pending wake |

Guest math is deterministic by construction: pinned Rhai features (no `unchecked`,
no `only_i32`, no `f32_float`, `sync` off, `no_std`) plus **explicit libm overrides**
for every transcendental (`register_libm_math`); `BasicTimePackage` is never
registered — scripts cannot read the wall clock even if they try.

### 3.4 Wake conditions (survive rails warp)

`WakeCondition` (csp-sim/src/scripts.rs): `AtTick`, `InTicks`, `AtApoapsis`,
`AtPeriapsis`, `AtAltitude {above_m, ascending}`, `AtNextSoiTransition`,
`AtManeuverNode`. Conditions must reduce to a **closed-form time**
(`resolve_apsis_ticks` / `resolve_altitude_ticks` solve the conic analytically —
ellipse and hyperbola branches); anything unsolvable (open conic past its apsis,
target outside `[r_p, r_a)`, circular conic for altitude crossings, no queued SOI
event) is **refused at registration**. A registered wake publishes a `ScriptWake`
event (class `Clamping` — rails warp demotes at it) and persists as the
`ScriptState::wake_tick` ↔ pending-event pair.

### 3.5 Budgets, strikes, poll detection

- Op budget: `set_max_operations(script_op_budget)` — default 20 000, from
  `data/balance.ron`, forwarded to the engine via `World::set_script_op_budget`.
  Memory ceilings too (`MAX_SCRIPT_STRING_SIZE` 1 MiB, arrays/maps 10 000).
- Exceeding the budget **aborts the call** (classified `ScriptFault::BudgetExceeded`
  — never misread from a thrown string) and increments the strike counter; a clean
  tick resets it; 5 consecutive strikes disable the script.
- **Poll detection**: a tick is *heavy* if the call aborted **or** completed having
  consumed ≥9/10 of the budget (`POLL_OPS_NUM/DEN`). 10 consecutive heavy ticks
  (`POLL_DEMOTE_TICKS`) set `poll_demoted` — `World::rails_warp_refusal` then names
  the script and warp is demoted to ×1. (PLAN's literal "budget-exhausting for 10
  consecutive ticks" is implemented as near-exhaust-completions-count-too, since
  aborted-only would have the strike limiter kill the script first — documented in
  the scripts.rs module doc.)
- A script holding nonzero throttle blocks rails warp independently (§2.6.2 rule).

### 3.6 Persistence

Cross-tick state is the script's own `ctx.persist` map — **primitives only**
(`PersistValue`: Bool / Int(i64) / Float(f64) / Text; non-finite floats and rich
values refuse loudly at write-back). `persist` serializes into the save and is part
of the state hash (`hash_scripts`). Everything else about a script — source verbatim,
enabled, strikes, wake_tick — is sim state too; the compiled AST is scratch
(§7.3) and recompiles from source after a restore.

### 3.7 Worked examples

Four stock scripts ship in the UI's console (`csp-ui/src/program.rs::STOCK_SCRIPTS`);
two canonical ones:

```rhai
// telemetry logger — heartbeat every ~10 s
fn on_tick(ctx) {
    if ctx.tick % 640 == 0 {
        let t = ctx.vessel;
        print(`t${ctx.tick} alt ${t.altitude}m spd ${t.speed}m/s Apo ${t.apoapsis}m`);
    }
}

// circularize at apoapsis — arms the wake; on rails at the apsis it
// schedules the prograde burn: vc = sqrt(mu/r) − v_horizontal
fn on_tick(ctx) {
    let t = ctx.vessel;
    if t.residence == "rails" && "armed" in ctx.persist && t.time_to_apoapsis < 30.0 {
        let r = sqrt(t.pos.x*t.pos.x + t.pos.y*t.pos.y + t.pos.z*t.pos.z);
        let dv = sqrt(t.mu / r) - t.horizontal_speed;
        if dv > 1.0 { add_node(ctx.tick + 2, dv, 0.0, 0.0); }
        ctx.persist["armed"] = 0;
    } else {
        wait_apoapsis();
        ctx.persist["armed"] = 1;
    }
}
```

A full guided-ascent script (pitch program computed by hand from `pos`/`vel`) is
`data/eval-scenarios/script-ascent.ron` — it is also an eval scenario: goals over
telemetry field paths, tick/event budgets, `stop_when` predicate.

(Note: the *circularize* listing above is abridged for space — the shipped
`STOCK_SCRIPTS` entry also prints the scheduled Δv or an "already circular"
line; the logic shown is complete.)

### 3.8 How you actually reach scripting today

- **In-game UI [WIRED]**: the Program window's **AUTOMATION section**
  (`csp-ui/src/program.rs::automation_section`) — a stock-script button row, a
  paste-in source buffer ("load source" compiles + binds to the focused vessel),
  per-script pause/resume/unload with strike + wake status lines, and the script-log
  tail (log lines also surface as notices in `csp-app`). `ProgramCommand::LoadScript`
  maps to `SimCommand::LoadScript` in `csp-view/src/lib.rs::program_command`.
- **Via MCP**: `sim.command {"LoadScript": {"vessel": N, "source": "fn on_tick(ctx){…}"}}`
  (§5.5). The `--serve` and `csp-mcp` hosts install the Rhai engine.
- **Via scenarios/bench**: eval scenarios embed script source (`data/eval-scenarios/script-ascent.ron`).

---

## 4. Autopilots (`csp-ai`)

`crates/csp-ai/src/lib.rs` is **222 lines, one program**: the launch-to-orbit
**ascent autopilot** (`AscentAutopilot`), a proportional gravity-turn controller:

- **Interface**: implements `csp_sim::AutopilotDriver` (`program()`, `run_tick`).
  The world drives it once per tick after scripts, through the same `TickIo`
  (commands stamp `SourceClass::Autopilot`), via `csp_script::IoControlApi` — the
  autopilot has no privileged state access. All run state lives in
  `AutopilotState::persist` (phase machine), so a save mid-ascent resumes it.
- **Phases**: 0 liftoff (vertical to 1.5 km) → 1 gravity turn (pitch
  vertical→horizontal over 1.5–60 km, full throttle until target apoapsis) →
  2 coast to apoapsis (12 s lead) → 3 circularize (prograde, throttled against the
  remaining periapsis gap, overshoot guard) → 4 done (throttle 0 until disengaged).
- **Engage**: `SimCommand::EngageAutopilot {vessel, program: Ascent {target_apoapsis_m,
  target_periapsis_m}}` — replaces any engaged program on the vessel. Three issuers:
  the UI button (below), a script (`engage_autopilot(apo, pe)`), MCP `sim.command`.
  Engagement is **bubble-only** (a latched/rails vessel has no control ticks).
- **UI [WIRED]**: AUTOMATION section's "engage ascent" button with a target-km field
  (`program.rs`; enabled only when the focused vessel's residence is `"bubble"`),
  plus a "disengage" button while engaged.

**What does NOT exist** (PLAN §2.11 scoped more; `csp-ai` did not grow):
landing, rendezvous, and maneuver-executor autopilots; the transfer optimizer
(Lambert/porkchop machinery *does* exist in `csp-sim/src/transfer.rs` and is
player-reachable as the map's **transfer-window planner** — but it is a planner, not
an autopilot); the design assistant; `MissionProfile` confidence; `LlmAdapter`
(post-v1). Named-tool MCP autopilot verbs are absent — use `sim.command`.

---

## 5. Server & MCP automation (`csp-server`)

Native-only (wasm ban asserted in CI). axum + tokio live only here. Protocol pins:
`crates/csp-server/PROTOCOL.md`.

### 5.1 Process model — three hosts, one backend seam

| Host | What it is | Where |
|---|---|---|
| `LocalGame` | In-process world + bounded ingress; the reference headless host (tests, `csp-mcp` bin) | `backend.rs:633` |
| `GameHost` | Dedicated **game thread** owns the `World` (it is `!Send`); publishes snapshots into an `ArcSwap`; commands land on a channel; `inspect()` for diagnostics | `backend.rs:826` |
| **`csp --serve`** | `csp-app` CLI flag → `serve_cli(&ServeFlags)` (`csp-app/src/lib.rs`): spawns `GameHost::spawn_full` with a **fresh** `View::new()` world (full data load; Rhai engine + ascent autopilot installed; starter craft **seeded** via `seed_starter_craft` like the windowed app), shares the game's CWD-relative `saves/` dir, serves the packaged telemetry SPA. Flags: `--port <u16>` (default 47900), `--bind-lan` (wildcard bind + printed warning; token still required). Unknown flags refuse loudly | `csp-app/src/lib.rs`, `csp-server/src/serve.rs::run_blocking` |
| `csp-mcp` | The **stdio** MCP host (PLAN §2.12.1): newline-delimited JSON-RPC over its own headless sim. Flags: `--empty` (bare `World::new()` — **the default**) \| `--fixture <dir>` (the flight-launch world, engines re-installed on `session.load`), `--save-dir`, `--dashboard-dir`, `--client <name>` | `csp-bench/src/bin/csp-mcp.rs` |

Honest note vs PLAN §2.12 ("attach to a running game"): **the windowed app hosts no
server**. HTTP attach means attaching to a `--serve` instance — a separate headless
game that can `session.load` any save from the shared saves dir. (`backend.rs`'s
doc comment sketches an app-frame-loop attach as an M9d shape; no such wiring exists.)

### 5.2 HTTP endpoints (`http.rs::router`)

Every endpoint sits behind the auth guard (§5.3).

| Route | Method | Purpose |
|---|---|---|
| `/` | GET | Telemetry SPA shell (token-free placeholder when no `spa_dir`) |
| `/assets/{*path}` | GET | SPA static assets (token-free) |
| `/api/telemetry` | GET | Current snapshot, verbatim JSON (debug read) |
| `/api/dashboards` | GET | Sidecar dashboard name list |
| `/api/dashboards/{name}` | GET / PUT | Read / validate-and-store one dashboard document |
| `/mcp` | **POST** | Streamable-HTTP MCP (one JSON-RPC message per POST); **GET/DELETE → 405** (revision 2026-07-28 removed them) |
| `/ws` | GET (upgrade) | §2.12.2 telemetry WebSocket (§5.7) |

### 5.3 Auth & security model

- **Token**: 32-byte CSPRNG hex, rotated per process start, written to
  `$XDG_RUNTIME_DIR/csp/<pid>.token` (mode 0600, atomically, under a 0700 dir; file
  contains `<token>\n<port>\n`; removed on shutdown; pre-existing/symlinked paths
  refused). Clients send `Authorization: Bearer …`; `?token=` is valid **only** on
  the `/ws` upgrade (browser upgrades cannot set headers). Default port **47900**.
- **Origin/Host allowlist on every endpoint** (DNS-rebinding protection), derived
  from the bind: loopback bind accepts localhost authorities; a specific non-loopback
  bind accepts its own address; wildcard binds accept IP literals. 403 otherwise.
- **Bounds**: 64 KiB max message (HTTP body + WS frame), bounded ingress queue,
  **≤32 commands per client per drain**, ≤16 live `/ws` sessions (503 over), WS ping
  every 30 s / dead-reader close at 90 s.
- Default bind `127.0.0.1` only. `--bind-lan` (§2.12.3) opts into a wildcard
  (`0.0.0.0`) bind, prints a warning, and keeps the bearer-token requirement —
  the allowlist machinery (`http.rs::BindAllowlist::for_bind`, covered by
  `http.rs` unit tests and the `lan_bind_allowlist_*` e2e) then accepts
  IP-literal authorities. `--port <u16>` overrides the default port.

### 5.4 MCP transport & lifecycle

Protocol revision **2026-07-28** (also accepts 2025-11-25; never 2024-11-05
HTTP+SSE). Pinned by `mcp::PROTOCOL_REVISION` / `SUPPORTED_REVISIONS` and asserted by
test. Sessionless: no `Mcp-Session-Id`; client identity is the `X-CSP-Client` header
(or `?client=`), allocated in connection order (the `source_id` its commands stamp
under — a diagnostic tag, not a capability). **Lifecycle gate**: a client's first
request must be `initialize` (`ping` excepted) — `-32600` otherwise. Responses are
JSON, or SSE (`text/event-stream`) when the client's `Accept` includes it.

### 5.5 Tool surface — `csp.mcp.tools.v1` (`mcp.rs::tools_v1`)

**Sim-class** (map to `SimCommand`s stamped `source_class = External`, one-tick
latency, §2.2.1 control lease; or telemetry reads):

| Tool | Args → effect |
|---|---|
| `telemetry.get` | `{vessel?}` → one vessel's `VesselTelemetry`, or the whole table |
| `vessel.list` | roster: id, residence, control lease, altitude, speed |
| `vessel.control` | `{vessel, throttle?, steer_dir [3]?, attitude {rot[4], ang_vel[3]}?, hold_attitude?}` — requires the caller's lease. `rot` is a quaternion in scalar-first order **`[w, x, y, z]`** — telemetry emits `rot` as an `{x,y,z,w}` object, so a round-trip must re-map components |
| `vessel.take_control` | acquire the lease (`Lease::Agent`); vacant vessels only |
| `vessel.release_control` | release back to Player (holder-only) |
| `mission.nodes` | `{vessel, set {tick, dv_*}? , delete?}` — read/schedule/delete a maneuver node |
| `timewarp.set` | `{level}` — store a warp ladder rung |
| `timewarp.advance_to` | `{tick}` — logged advance target under §2.6.5 budgets |
| `sim.command` | `{command}` or `{commands: [...]}` — **raw `SimCommand` passthrough**, the escape hatch for every variant the named surface doesn't cover (`serde_json` externally-tagged enum: `{"SetWarp": 4}`, `{"LoadScript": {...}}`, `{"RunExperiment": {...}}`, …) |

**Session-class** (declared `"class": "session"`, not replay-relevant):
`session.list_saves`, `session.save {name}` (writes `<name>.cspsave`),
`session.load {name}` (restores, replacing the running world),
`session.snapshot` (tick + state-hash), `dashboard.list`, `dashboard.get {name}`,
`dashboard.save {dashboard}` (the §2.12.2 sidecar store under
`$XDG_CONFIG_HOME/csp/dashboards/*.json` — never in the save/log/hash).

PLAN §2.12.1 sketched richer names (`craft.load`, `editor.build`, `mission.ascend`,
…); **those named tools do not exist** — the four verb families above plus
`sim.command` are the surface.

### 5.6 The driving model

The headless world advances **only when a submit arrives**: each tool call queues
into the bounded ingress, then `drain_and_step` (`backend.rs`) drains the queue and
steps at least one boundary (clock+1) — or chases an `AdvanceTo` target up to 64
frames per call, re-issuing on `partial` (the report tells you: `rejected`, `partial`,
`reached`, `target`, fired events). So an agent's loop is: issue commands (each
steps a tick) → `timewarp.advance_to` to fast-forward → read `telemetry.get` /
`session.snapshot` to observe. Rejections come back per-channel with reasons;
fired events (alarms, failures) are included in every report.

### 5.7 WS telemetry protocol (`GET /ws`)

Envelope `{proto: 1, kind, seq, tick, payload}`. Client declares `fields` — dotted
paths into the snapshot (`vessels.0.altitude_m`, `focus.throttle`,
`program.funds_micros`, `program.colonies`, `program.sites`, `program.parts`, …;
source: `telemetry.rs::TelemetrySnapshot`,
which nests `VesselTelemetry` per vessel plus a `focus.*` alias and a
`ProgramSnapshot` with funds/science/contracts/crew/colonies/scripts rows
**plus the authoring surface** — `program.sites` rows (site id, launch flag,
live stockpile) and `program.parts` catalog summaries (id, mass, cost, tech
gate, BOM) so `ManufactureCraft` is authorable over the protocol). There is
**no "send everything"**; ≤64 paths; `rate_hz` ∈ {64,32,16,8,4,2,1} (default 16);
unresolvable paths reported in `unknown_fields`. Backpressure is drop-to-latest —
the server re-reads the `ArcSwap` per send and never queues. `{kind: "command"}`
frames are the opt-in command channel (`command_ack` frames carry their own seq).
PLAN §2.12.2's "MessagePack for arrays" is **deferred** — the stream is JSON-only
until a widget needs bulk arrays (recorded in `http.rs`'s deferral note; the
`ws_subscription_stays_within_byte_budget` e2e is the tripwire).

### 5.8 Worked example — drive a game over MCP: connect → launch → science → save

```bash
cargo run -p csp-app --bin csp -- --serve        # headless world on 127.0.0.1:47900
TOKEN=$(sed -n 1p "$XDG_RUNTIME_DIR/csp/$PID_OF_CSP.token")
```

(Or stdio: `cargo run -p csp-bench --bin csp-mcp -- --fixture data/test-fixtures/flight-launch`.)

1. **Connect** — handshake (per client name):
   `POST /mcp` `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"agent","version":"0"}}}`
   then `tools/list` if you want the descriptors.
2. **Launch** — the seeded `--serve` world opens with the starter craft
   latched on the pad (vessel ids come from `vessel.list`; site ids and the
   part BOM live under `program.sites`/`program.parts` in the telemetry
   snapshot). To build another vessel, either load a save
   (`session.load {"name":"quicksave"}`) or build one through raw commands, e.g.
   `sim.command` with `{"ManufactureCraft": {...}}` (the costed launch path — build
   order against the pad's stockpile, craft delivered `SurfaceLatched` at
   `ConstructionComplete`), or `{"SpawnVessel": {"parent":"terrin","elements":{...}}}`
   for an on-rails test article, then `vessel.take_control` + `vessel.control
   {"throttle":1.0}`. (`EngageAutopilot` via `sim.command` will fly the ascent for
   you once the vessel is in the bubble.)
3. **Run science** — `sim.command {"RunExperiment": {"vessel":N, "node":P,
   "experiment":"exp.materials_study", "biome":B, "situation":"SpaceLow"}}`
   (fields per `SimCommand::RunExperiment`, `csp-sim/src/lib.rs:836`; the biome
   context is caller-derived — read the vessel's residence/altitude against the body
   table), then `{"TransmitData": {"vessel":N, "key":{"experiment":…, "body":…,
   "biome":…, "situation":…}}}` to bank `units·transmit_ratio`
   (`DataKey` = `{experiment, body, biome, situation}`, `csp-sim/src/research.rs:122`),
   or `{"RecoverData": {"vessel":N, "key":…}}` for 100% once surface-latched.
4. **Advance & observe** — `timewarp.advance_to {"tick": T}` (the report carries
   `reached`/`partial`), `telemetry.get`, `session.snapshot` for the state hash.
5. **Save** — `session.save {"name":"agent-run"}` → `<saves>/agent-run.cspsave`,
   loadable from the windowed app's Game menu (shared saves dir).

The suite `crates/csp-server/tests/e2e.rs` boots a real server over a socket and
covers: token lifecycle/symlink refusal, Origin/Host allowlists (loopback binds;
the **wildcard/LAN-bind allowlist shape is covered by
`lan_bind_allowlist_accepts_ip_authorities`** binding `0.0.0.0:0` directly),
`mcp_round_trip_and_external_ordering` (External
stamping + client-index assignment — the §2.2.1 proof), bounded-ingress
backpressure, lease enforcement over the wire, local-vs-server hash parity, WS
subscription/command/cap/envelope rules and byte budget, dashboards API+tools,
token-free-but-allowlisted SPA serving, `backend_calls_leave_workers_free`, and
`session_save_and_load_over_the_wire` (the step-5 save/load round-trip). Steps
not covered at the wire level: the science verbs (`RunExperiment`/`TransmitData`/
`RecoverData`) and `ManufactureCraft` — those are exercised in `csp-sim`'s unit
tests and the `csp-bench` scenarios, reachable over the wire only through
`sim.command`'s raw passthrough.

---

## 6. Telemetry web screens (`apps/telemetry`)

TanStack/Vite SPA (~1.1k TS; versions pinned exactly in its `package.json`; build
output `dist/` is what `--serve` ships as `spa_dir` — `packaged_spa_dir()` resolves
`apps/telemetry/dist/client` in dev checkouts, `share/csp/telemetry` when packaged).

- **What it shows**: the flight dashboard from `dashboards/default.json` — gauges
  (altitude/speed/throttle), an orbit table (apsides, time-to-apsis, residence),
  altitude/vertical-speed trace graphs, an orbit map, and a world table; widget
  kinds registered in `src/lib/widgets/registry.tsx` (gauge/graph/orbit-map/table).
  Widget `fields` are
  the §5.7 subscription paths, so `program.*` rows (funds/contracts/crew/colonies)
  are subscribable the same way.
- **Dashboards are sidecar config** (`$XDG_CONFIG_HOME/csp/dashboards/*.json`,
  `schema_version` 1): edited via the SPA, stored through `/api/dashboards` PUT or
  the `dashboard.save` MCP tool — never in the save, the command log, or the state hash.
- **Feeding it**: open `http://127.0.0.1:47900/` — the shell is token-free; the SPA
  asks for the bearer at a first-boot gate (paste it, or arrive with
  `?csp_token=…` which it stashes in `sessionStorage` and strips from the URL —
  `src/lib/ws.ts::cspToken`). It then opens `/ws?token=…&client=telemetry-spa` and
  subscribes at the dashboard's `rate_hz`. A server restart rotates the token — the
  gate re-asks.
- **Building**: `npm install && npm run build` in `apps/telemetry` (Node needed for
  this crate only; see [BUILD-AND-DEV.md](BUILD-AND-DEV.md)).

---

## 7. Honest status — the automation-first pillar

Legend: **[WIRED]** player-reachable in the shipped UI · **[MCP-only]** reachable by
an agent over MCP/stdio but with no in-game UI · **[ABSENT]** not in the tree.

| Capability | Status | Anchor |
|---|---|---|
| Rhai script console (paste-in editor + stock library + pause/unload + status + log tail) | **[WIRED]** | `csp-ui/src/program.rs::automation_section` (AUTOMATION section); `STOCK_SCRIPTS` |
| Script load/unload/enable as sim verbs | **[WIRED]** | `SimCommand::{LoadScript,UnloadScript,SetScriptEnabled}` (`csp-sim/src/lib.rs:1106–1131`) |
| Ascent autopilot, engage/disengage from UI | **[WIRED]** (bubble-only gate) | `program.rs` "engage ascent"; `csp-ai/src/lib.rs::AscentAutopilot` |
| Autopilot from scripts | **[WIRED]** | guest `engage_autopilot`/`disengage_autopilot` |
| Maneuver nodes / alarms from automation | **[WIRED]** | guest `add_node`/`add_alarm`; alarm book `csp-view/src/map.rs:769+` |
| Transfer-window planner + `WatchTransferWindow` | **[WIRED]** (planner, not an autopilot) | map planning panel `csp-view/src/map.rs` (`AlarmEntry`, staged transfer plan) |
| MCP over Streamable HTTP | **[WIRED]** (headless `--serve` world, not the windowed session) | `csp-server/src/{http,mcp,serve}.rs`; `serve_cli` |
| MCP over stdio (own headless sim) | **[WIRED]** | `csp-bench/src/bin/csp-mcp.rs` |
| Generic `sim.command` escape hatch | **[WIRED]** | `mcp.rs::t_sim_command` |
| Save/load over MCP (CSPSAVE codec shared with the UI) | **[WIRED]** | `session.*` tools; `csp-sim/src/save.rs` |
| Telemetry dashboards (SPA + sidecar store) | **[WIRED]** | `apps/telemetry`; `dashboard.rs` |
| Landing / rendezvous / maneuver-executor autopilots | **[ABSENT]** | `csp-ai` is ascent-only |
| Design assistant / `MissionProfile` / LLM adapter | **[ABSENT]** (post-v1 per PLAN) | — |
| `craft.load` / `editor.build` named MCP tools | **[ABSENT]** (use `sim.command`) | `mcp.rs::tools_v1` |
| Attach MCP to the *windowed* app's live session | **[ABSENT]** | windowed app hosts no server |
| `--bind-lan` / `--port` flags | **[WIRED]** | `parse_serve_flags` + `serve_cli` (`csp-app/src/lib.rs`) |
| Command reachability guarantee (every `SimCommand` has a production issuer or an explicit opt-out) | **[WIRED]** — CI gate | `xtask/src/lint_reach.rs`; 9 remaining `// ui-unreachable:` opt-outs in `csp-sim/src/lib.rs` (L552, 564, 631, 644, 674, 692, 778, 939, 997) |

---

## 8. Divergences from PLAN.md (both sides stated)

1. **MCP tool surface** — PLAN §2.12.1 lists `craft.load`, `editor.build`, …; the
   tree ships 9 sim-class + 7 session-class tools with `sim.command` as the raw
   passthrough for everything else (`mcp.rs::tools_v1`).
2. **"Attach to a running game"** — PLAN §2.12 makes HTTP-MCP the attach path to a
   running game; today only `csp --serve` hosts, with a **fresh headless world**
   (seeded with the starter craft; shares the game's CWD-relative `saves/` dir).
   The windowed app runs no server — frame-loop attach is unimplemented (v1
   deviation, recorded per §6.5).
3. **AI scope** — PLAN §2.11 scopes v1 ascent *and* transfer autopilots plus a
   design assistant; only the ascent autopilot exists (`csp-ai`, 222 lines). The
   transfer *planner* exists as sim machinery + map UI, not as an autopilot.
4. **Script-callable autopilot services** — PLAN §2.10 names
   `mission.ascend(target_apo, incl)`; the shipped guest verb is
   `engage_autopilot(apo_m, pe_m)` — same capability, different shape (`CONTROL_API_SURFACE`).
5. **Poll detection** — PLAN's "budget-exhausting for 10 consecutive ticks" is
   implemented as 10 consecutive *heavy* ticks (aborted **or** ≥90% budget consumed),
   because aborts alone would hit the 5-strike disable first (scripts.rs module doc).
6. **Contract kinds** — PLAN §3.6 names seven v1 types; 5 kinds ship; crew transport
   and tourist flight are explicitly deferred (`contract.rs` module doc).
7. **Wire encoding** — PLAN §2.12.2's "MessagePack for arrays" is deferred; the WS
   stream is JSON-only with `proto` as the bump mechanism (`http.rs` deferral note).
8. **Quaternion wire order** — `vessel.control`'s `attitude.rot` takes
   scalar-first `[w, x, y, z]` while `DQuat`'s serde and telemetry emit
   `{x, y, z, w}` objects; the orders are documented in the tool descriptor
   and pinned by `attitude_rot_is_scalar_first_on_the_wire` (a component
   permutation passes the sim's norm-only ingest gate silently).

For the authoritative testing/regeneration workflow see [BUILD-AND-DEV.md](BUILD-AND-DEV.md);
for how the sim consumes these verbs, [SIMULATION.md](SIMULATION.md); for the panels
named here, [UI-AND-RENDERING.md](UI-AND-RENDERING.md).
