# PRD-22 Review — Data domain: schemas + loaders (csp-data)

**Scope:** `crates/csp-data/src/{lib.rs,load.rs,ids.rs,datahash.rs,part.rs,resource.rs,experiment.rs,recipe.rs,balance.rs,loc.rs}` (+ supporting reads of `colony.rs`, `contract.rs`, `validate.rs`, and downstream consumers in `csp-phys`, `csp-sim`, `csp-ui`, `csp-view`).
**PRD reference:** PLAN §3.0 (home system + schema rules), §3.1 (PartDef/ModuleDef, data-set hash), §3.2 (science), §3.5/§3.5.1 (resources, colonies), §2.4 (ConfigMask deployables), §2.9 ("20 t lander rests on gear"), §1 pillar 1 ("rockets, planes, rovers, stations"), §4 (CommNet post-v1).
**Sibling reports:** PRD-23 reviewed `validate.rs` + the shipped `data/` tree and already reports the content skeleton (4 zero-part variants, ~6 missing mechanics, the science-economy math). This report covers the **schema/loader/hash layer** — what the schema can and cannot express, and which gaps are data-only vs schema+sim work. It deliberately adds detail PRD-23 could not see from the data tree: two "supported" module variants that resolve into **nothing** on a vessel, one dead force-site discriminant, and one orphan command.

## Verdict

The schema/loader/hash machinery itself is **honest and well-built** — the §3.1 version-first probe, authored-name traversal gate, symlink/non-UTF-8/case-collision refusals, and the v2 injective hash frame are all implemented exactly as the plan specifies, with tests that prove the claimed property. The problem is entirely at the **module-variant layer**: `ModuleDef` has 10 variants of which 4 have zero authored parts (PRD-23's headline), and — a fact the data tree alone cannot reveal — **two of those four (`Drill`, `Converter`) plus `Control` resolve to no `WeldPart` field at all**: the resolver validates them, applies multipliers, then drops them on the floor. A part carrying `Drill`/`Converter` authored tomorrow would be a validated, loadable, hashable, inert mass. The schema is *ahead* of the sim in a way that actively lies: it accepts data whose promised semantics do not exist. Meanwhile the PRD mechanics the player loop needs most — decelerator (parachute), vessel power generation (solar/RTG), deployables (gear/panels/fairings), RCS, wheels — have **no `ModuleDef` variant at all** and no `WeldPart` field to resolve into. `ForceSiteKind::Rcs` is a dead discriminant; `monoprop` is a dead resource; `SetAeroConfig` is an orphan command. Roughly: adding the missing *rocket-family* parts (DockPort, Lab, probe core, LS tanks) is a data-only change; adding the missing *mechanics* is schema + phys + sim work first, then data.

---

## ModuleDef coverage matrix — schema vs resolver vs sim vs content

`ModuleDef` is defined at `crates/csp-data/src/part.rs:506–623` (10 variants). `WeldPart` (`crates/csp-phys/src/part.rs:220–303`) is what a part actually becomes in the sim: fields are `mass_kg, com, inertia, collider, force_sites, aero, thermal, structural, dock_ports, tanks, crew_seats, experiments, lab, engine` — **no `modules`, no `drill`, no `converter`, no `control`, no `power` field.**

| Variant | part.rs | Parts in `data/parts/` | Reaches `WeldPart`? | Sim consumer | Gap class |
|---|---|---|---|---|---|
| `Engine` | :509 | 2 | `engine` spec + `Thrust` site | propellant debit, thrust (lib.rs:8249+) | — |
| `Tank` | :542 | 3 | `tanks` | contents ledger, transfers, LS, transmit drain | — |
| `Control` | :549 | 1 (pod) | **NO** — resolve.rs:1310–1322 checks/multiplies, weld-spec loop `_ => {}` at resolve.rs:1933 | VAB inspector text only (csp-ui/vab.rs:2269); attitude is `SetAttitude` teleport (lib.rs:595–602, 4824–4834) | **schema lies** — torque/power modeled, never consumed |
| `Decoupler` | :559 | 1 | marker (staging = `JettisonPart`) | StagePlan→JettisonPart (csp-view/lib.rs:2233–2347) | — |
| `DockPort` | :568 | **0** | `dock_ports: Vec<DockPortSpec>` (resolve.rs:1842–1860) | joints.rs soft-capture/latch, contracts `ModuleReq::DockPort` | **data-only gap** |
| `CrewSeat` | :577 | 1 (pod) | `crew_seats` | crew bookkeeping (commands exist, UI absent) | — |
| `Experiment` | :582 | 1 (canister) | `experiments` | `RunExperiment` (research.rs) | — |
| `Drill` | :593 | **0** | **NO** — resolve.rs:1348–1369 validates, then `_ => {}` | none vessel-side; `ColonyModuleDef::Drill` (colony.rs:93+) is a **different enum** | **schema+phys+sim gap** — not data-only |
| `Converter` | :604 | **0** | **NO** — resolve.rs:1371–1376 | none vessel-side; colony converters are `ColonyModuleDef::Converter` | **schema+phys+sim gap** |
| `Lab` | :615 | **0** | `lab: Option<LabSpec>` (resolve.rs:1912–1931) | `apply_start_research` reads `p.lab` (research.rs:948) | **data-only gap** (command itself is UI-unreachable — PRD-06/15) |

**Mechanics with no `ModuleDef` variant at all** (cannot be authored as data — verified by grep over `crates/csp-data/` and `data/`): parachute/decelerator, solar panel/RTG (vessel power), generic deployable, landing gear, wheel, antenna (CommNet is post-v1 — correctly absent, PLAN:1365), RCS thruster, probe core (partially expressible as a `Control`-only part), wing/control-surface (lift *is* expressible — `AeroDef::cl_curve` is per-part — but actuation is not).

---

## Findings

### F1 — `Drill` and `Converter` part modules resolve into nothing: the schema accepts parts whose function does not exist

**Severity:** High
**Location:** `crates/csp-phys/src/part.rs:220–303` (`WeldPart` — no drill/converter/power field), `crates/csp-phys/src/resolve.rs:1348–1376` (validated then cloned), `resolve.rs:1933` (weld-spec loop `_ => {}` drop arm), `crates/csp-data/src/colony.rs:57+` (`ColonyModuleDef` — the *only* live drill/converter consumers).
**Problem:** The PRD-23 matrix credits `Drill`/`Converter` with sim support based on use-site counts, but every `ModuleDef::Drill`/`Converter` use site is a *validator or resolver bounds-check* — the resolver gate at resolve.rs:1348–1376 validates the fields and returns the module unchanged, then the weld-spec construction loop at resolve.rs:1842–1933 matches only `DockPort`/`Tank`/`CrewSeat`/`Experiment`/`Lab`/Engine-path and drops everything else via `_ => {}`. There is no vessel-side drill process: `grep` for any consumer of a resolved drill on a `Vessel`/`WeldPart`/`Craft` returns nothing outside colonies, and colonies consume the separate `ColonyModuleDef` enum populated from `colonies.ron`, never from a craft.
**Evidence:** `WeldPart` field list (part.rs:228–303) ends at `engine: Option<EngineSpec>` — a resolved craft has no slot where drill data could live. The sim's `grep "ModuleDef::Drill\|ModuleDef::Converter"` hits are all `colonies.rs` `ColonyModuleDef` substring matches or validate.rs checks. `surface_resources` is `[]` on all four bodies (PRD-23 F5) — even a working vessel drill would have nothing to mine.
**Impact:** The schema advertises §3.5.1 vessel ISRU; a content author adding `csp.drill.ore` today gets a part that loads, validates, flies — and does nothing, silently. The PRD loop "drill → converter → propellant" cannot be closed by data authoring; it needs a `WeldPart` field, a vessel-side extraction process (surface-latch + concentration + EC debit), and authored `surface_resources` maps.
**Suggestion:** Either (a) implement the vessel-side path: add `drills`/`converters` specs to `WeldPart`, a `RunDrill`/`RunConverter` process gated on `SurfaceLatched` + body `surface_resources`, and EC draw (which itself needs F3's power model); or (b) if v1 intends colony-only ISRU, *remove or doc-mark the part-side variants* — `deny_unknown_fields` means the enum arm itself is the promise. At minimum, the validator should warn that part-level `Drill`/`Converter` currently have no runtime effect.

### F2 — `Control` torque and `power_w` are resolved-and-dropped: the pod's reaction wheel is a display string

**Severity:** Medium (cosmetic-but-misleading — it does not break the loop the way F1 does, because attitude authority exists via a different mechanism; it misrepresents what the part does)
**Location:** `crates/csp-data/src/part.rs:549–554` (`Control { torque_nm, power_w }`), `crates/csp-phys/src/resolve.rs:1310–1322` (validated + `ControlTorqueNm` multiplier applied), `resolve.rs:1933` (same `_ => {}` drop), `crates/csp-sim/src/lib.rs:590–602` (`SetAttitude` — "the M2 control surface standing in for M3+/M9a's real attitude authority"), `crates/csp-ui/src/vab.rs:2269` (the only consumer: `format!("{torque_nm:.0} N·m torque")`).
**Problem:** `torque_nm` is validated, upgradeable via `StatPath::ControlTorqueNm` (part.rs:293 — a tech-tree track can spend science to multiply it), and displayed to the player — and no force/torque channel in the physics consumes it. `SimpleBackend` integrates `force, torque` from aero + thrust sites (simple.rs:1519–1584); there is no reaction-wheel torque source, no attitude-authority budget, no `power_w` debit. Attitude changes are `SetAttitude { rot, ang_vel }` — the sim writes `body.rot`/`body.ang_vel` directly (lib.rs:4824–4834), and the app issues angular-rate commands (csp-app/lib.rs:1672 "body-frame angular rate, not a torque model").
**Evidence:** `grep torque_nm` outside defs/validator/VAB-display/test fixtures: zero hits. `WeldPart` has no control/torque field (part.rs:220–303). The `ReliabilityDef` doc at part.rs:499–503 justifies `Control` via §3.4's "probe fails a reaction wheel" — but there is no wheel to fail; `FailureMode` has no wheel-specific mode, and `SensorFault` is the nearest.
**Impact:** Players read "2000 N·m torque" as capability; a `ControlTorqueNm` upgrade track sells torque that changes nothing. It also means a `Control`-only "probe core" part — the natural authoring answer to uncrewed flight — has *identical* flight capability to a part with no modules at all, since authority is not mediated by any module.
**Suggestion:** Decide v1's attitude model: if `SetAttitude`-as-authority is the v1 answer (defensible), doc-mark `Control.torque_nm` as display-only and either remove the `ControlTorqueNm` stat path or cap its tracks at 0 levels; if real authority is wanted, it needs a torque-force-site model + EC budget + SAS-style controller — schema+phys+sim, not data.

### F3 — No vessel power generation exists anywhere in schema or sim; three variants author `power_w` into the void

**Severity:** High (it gate-keeps F1's drills/converters, F2's control power, and any future powered module)
**Location:** `crates/csp-data/src/part.rs:549–554, 593–622` (`power_w` on `Control`/`Drill`/`Converter` — none on `Lab`), `data/parts/csp.battery.z1k.ron` (only EC store), `crates/csp-sim/src/research.rs:26–31` (`transmit_ec_per_data_unit` — the only routine EC debit besides life_support), `crates/csp-data/src/colony.rs:57–92` (`ColonyModuleDef::Solar`/`Generator` — colony-only).
**Problem:** (Also reported as PRD-23 F4 — restated here as the *schema* finding.) Every module `power_w` field has no consumer: there is no vessel power model — no generation, no load-shed, no allocation. EC is a tank resource that only transmits and life support drain; the 1000-unit battery is the entire energy economy. The schema has no `Solar`/`Generator`/`RTG` variant for parts; `insolation_w_m2` on every `BodyDef` serves colony solar only.
**Impact:** Even the *data-expressible* powered parts are decorative (`Control.power_w` debits nothing); the not-yet-expressible ones (drill/converter) are doubly blocked — no function AND no power. Interplanetary missions are EC-capped at launch forever.
**Suggestion:** One `ModuleDef::Generator { kind: Solar{area,eff} | Fuel{resource,rate} | Rtg{rate}, }` variant (or separate variants — `ColonyModuleDef` already establishes the pattern to mirror) plus a vessel-side generation+debit pass over `WeldPart` — schema+phys+sim work. Until then the battery is honestly the whole system, which the data does reflect.

### F4 — `ForceSiteKind::Rcs` is a dead discriminant and `monoprop` a dead resource: RCS exists at the enum layer only

**Severity:** Medium
**Location:** `crates/csp-phys/src/part.rs:82–90` (`ForceSiteKind::Rcs = 2`), `data/resources.ron:20` (`monoprop`), `data/loc/en.ron:55`.
**Problem:** The force-site *kind* exists with explicit stable discriminant (hashed by §7.1 rule 7), but no `ModuleDef` variant can produce an `Rcs` site, no command consumes one (all sim force-site consumers filter `== Thrust`), and the `monoprop` resource authored in `resources.ron` has **zero consumers** in data or code — no engine burns it, no tank ships it, no colony recipe touches it.
**Evidence:** `grep ForceSiteKind::Rcs` → definition + docs only; every sim/UI force-site match is `ForceSiteKind::Thrust` (30+ sites). `grep monoprop` → the def and its loc string only.
**Impact:** §1 pillar 1's maneuvering vocabulary is one engine axis; docking (if a port part were authored) has no fine-control tool, planes/rovers nothing. It also quietly inflates the resource registry's "ten, not thirty" promise — one of the ten is dead.
**Suggestion:** `ModuleDef::RcsThruster { thrust_n, propellant: ResourceId }` → `Rcs` sites + a translation/rotation command channel. Schema+phys+sim; or drop `monoprop`/`Rcs` for v1 honesty.

### F5 — `SetAeroConfig` is an orphan command and there is no deployable/part-state model: `PanelJam` has no subject

**Severity:** Medium
**Location:** `crates/csp-sim/src/lib.rs:663–676` (`SetAeroConfig` — "the command resolves to the nearest computed mask"), `lib.rs:4873` (apply), `crates/csp-phys/src/aero.rs:167–175` (`ConfigMask`), `crates/csp-data/src/part.rs:723` (`PanelJam`), `data/parts/csp.pod.mk1.ron:61` + `csp.science.canister.ron:64` (`PanelJam, 0.3` authored).
**Problem:** `SetAeroConfig` exists, applies, hashes — and is issued by **nobody**: zero producers in `csp-view`/`csp-app`/`csp-ui`/`csp-bench`/`csp-server` (only the enum decl, apply dispatch, and lease-key listing reference it). There is no per-part deployed state in the schema (no `Deployable` module, no `deployed: bool` on `BuiltPart`), so §2.4's ConfigMask vision — "fairing deployment, cargo-bay doors, landing gear, and solar panels per `ConfigMask`" — has mask bits over *nodes* but nothing that models *why* a node's state changed or what it means mechanically beyond aero shielding. Meanwhile two parts author a 30% `PanelJam` failure-mode weight for deployables no part can possess (PRD-23 F7 covers the validator-gap angle).
**Evidence:** `grep -rn "SetAeroConfig" crates/` → decl, dispatch, lease listing only. `JettisonPart` IS reachable (staging pops jettison decouplers, csp-view/lib.rs:2233–2347) — so a one-shot fairing *shell* is nearly data-expressible — but nothing retracts/deploys in place.
**Impact:** Deployables are half-modeled twice (aero mask with no semantics, failure mode with no subject). Gear, panels, cargo bays, and chutes all need the same missing piece: a `Deployable`/`Deploy` module + per-node deployed state + a command that flips it (which would also give `SetAeroConfig` a producer).
**Suggestion:** Add `ModuleDef::Deployable { kind: Gear|Panel|Fairing|Chute|…, deployed_mask_bit }` plus `SimCommand::SetDeployed { vessel, node, deployed }` that composes the mask update — schema+sim work that unblocks four part types at once and gives `PanelJam` a real subject.

### F6 — `validate_contracts` cannot check `ModuleReq` satisfiability: a template can mint permanently unsatisfiable offers

**Severity:** Medium (latent — the shipped `contract.part_test.ron:14` draws only `[CrewSeat, ThrustSite]`, both satisfiable)
**Location:** `crates/csp-data/src/validate.rs:4229–4235` (`validate_dataset` passes `contracts, system, experiments, techtree, resources` — no `d.parts`), `validate.rs:3960–3987` (the `PartTest` check validates `Experiment` kind IDs only), `crates/csp-data/src/contract.rs:226–239` (`ModuleReq::{Lab, DockPort, CrewSeat, Experiment, Tank, ThrustSite}`).
**Problem:** The validator checks that a `ModuleReq::Experiment(kind)` names a loaded experiment (validate.rs:3968–3972) but has no access to the part set, so it cannot notice that `ModuleReq::Lab` or `ModuleReq::DockPort` are unsatisfiable by construction — today *and forever*, until a lab/port part ships. A template authored with `modules: [Lab]` would validate green and generate offers no craft can satisfy.
**Evidence:** `craft_has_module` (csp-sim/src/contracts.rs:425–438) is the *runtime* check — the generator draws from the template's module list; nothing upstream verifies the list intersects the authored part catalog.
**Impact:** Direct PRD-loop hazard: the moment a content author adds `Lab`/`DockPort` to a contract template to make the new parts meaningful (the obvious next content edit), validation will not catch a typo or a prematurely-gated requirement. Low cost to fix — `validate_dataset` already holds `d.parts`.
**Suggestion:** Give `validate_contracts` a `&LoadedParts` parameter and check each `ModuleReq` against the union of module capabilities over the loaded part defs (the same mapping `craft_has_module` uses, computed once per dataset).

### F7 — Facility unlocks are unverifiable IDs: no facility schema exists at all

**Severity:** Low
**Location:** `crates/csp-data/src/ids.rs:114–119` (`FacilityId`), `data/techtree/home.ron` (`unlocks.facilities: [facility.lab, facility.test_stand]`), `crates/csp-view/src/structures.rs` (pad/VAB buildings are cosmetic-only).
**Problem:** (PRD-23 F8 also notes this — schema-side restatement.) `FacilityId` exists as an id type but no `FacilityDef`/facility file kind exists in csp-data — `lib.rs`'s module list has no `facility.rs`, and the load API has no `load_facilities`. §3.3's "tech nodes unlock … facilities" resolves to strings nothing defines; the validator can only check they're well-formed IDs, not that they mean anything.
**Impact:** `tech.science.1` sells "facility.lab" that has no def, no effect, and no consumer — an unlock into the void. Part of the wider "no space-center gameplay" gap (PRD-15/16 own the UI side).
**Suggestion:** Either a minimal `data/facilities/*.ron` schema (`FacilityDef { id, display_name, effects… }`) or doc-mark facilities as cosmetic unlocks until the KSC milestone.

### F8 — `Splashed` situation is classified only presentation-side at `alt ≤ 0`, and `SurfaceLatched` has no splash/latch distinction in schema

**Severity:** Low
**Location:** `crates/csp-view/src/lib.rs:2568–2592` (situation classifier — `altitude_m <= 0.0 → Splashed`), `crates/csp-data/src/part.rs:357–370` (enum), `data/experiments/exp.materials_study.ron:14` (cap mask 63 includes Splashed), `data/parts/csp.science.canister.ron:48` (module mask 49 excludes it).
**Problem:** The situation a `RunExperiment` claims is caller-supplied and only sanity-gated against residence class (`Situation::is_surface_side`, part.rs:395–401); the sole classifier lives in the view layer. The experiment def caps `terrin` at mask 63 — `Splashed` keys are *worth* data — while the one instrument's mask (49 = Landed|SpaceLow|SpaceHigh) can never run there. Consistent content (a chute-less craft shouldn't promise splashdown science), but it means the `Splashed` leg of §3.2's six-situation taxonomy has zero reachable content.
**Impact:** Minor content-honesty wrinkle — fine *given* no decelerator exists; one more datum that the recovery path is unbuilt.
**Suggestion:** None needed at schema level; note for the part-catalog work that the first wet-capable instrument should opt in deliberately.

### F9 — Stale doc links to methods that don't exist, and the §8.2 `cargo doc -D warnings` gate is absent from CI

**Severity:** Low
**Location:** `crates/csp-data/src/part.rs:39` (`[`PartCategory::required_module`]` — no such item; the category↔module check is an inline match at validate.rs:3143–3156), `crates/csp-data/src/techtree.rs:53` (`[`TechTree::merged`]` — `merged` lives on `LoadedTechTree`, load.rs:760), `crates/csp-data/src/contract.rs:7` (`[`csp_sim::contracts`]` — unresolvable, correct dep direction), `crates/csp-data/src/load.rs:9,241` (links to private items), `.github/workflows/ci.yml` (no docs job).
**Problem:** `cargo doc --no-deps -p csp-data` emits 5 warnings. PLAN §8.2 mandates "cargo doc `RUSTDOCFLAGS=-D warnings`; `missing_docs` warn on public API — per commit, CI-HOST" — the workflow runs fmt/clippy/test/wasm-check/lint-deps/lint-attribution/cargo-deny/golden-replays/perf and nothing else. The gate that would catch exactly these stale links does not run.
**Evidence:** `cargo doc --no-deps -p csp-data` output above; `grep -in doc .github/workflows/ci.yml` → only `docs/perf/` path mention.
**Impact:** Docs drift silently — `part.rs:39` currently tells a reader a `required_module` mapping method exists when the check is an inline match. Minor, but it is the §8.2 contract itself that is unenforced (a PRD-24-class gap observed from this crate).
**Suggestion:** Add the docs job (one line: `RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --no-deps`) and fix the five links — mostly re-pointing at the actual items or dropping the link syntax.

### F10 — Loader and data-set hash: verified honest (no finding, recorded for the record)

**Severity:** None — assessment
**Location:** `crates/csp-data/src/load.rs:339–402, 799–870`, `crates/csp-data/src/datahash.rs` (whole file), `crates/csp-data/src/lib.rs:35–53`, `crates/csp-data/src/ids.rs` (whole file).
**Verified against §3.1:**
- **Version-first probing** — `read_versioned_ron` (load.rs:355–374) probes `SchemaProbe` before the full parse, so a stale file reports its version rather than a masking missing-field error; post-parse re-check via `version_of`. Per-file-type supported bands are honest: sites accepts {1,2} (v1 → empty `resource_yields`), colonies refuses v1 outright (documented at load.rs:117–127 — "no honest mechanical mapping"), everything else exact-match. Tests at load.rs:933–957 pin both orderings.
- **Traversal safety** — `authored_name_ok`/`join_authored` (load.rs:382–402) reject `..`/absolute/separator-bearing authored names before any `join`; `gather_ron_files` refuses symlinks via `entry.file_type()` (non-following) and refuses to swallow failed `read_dir` entries (load.rs:835–869). The sort key (`ron_sort_key`, load.rs:818–832) is byte-wise `/`-joined relative path with a correct explanation of why `Path` ordering differs; `EmbeddedSource::ron_files` re-sorts by the same key (load.rs:310–323) — fs/embedded parity.
- **Hash frame** — `feed_file` (datahash.rs:121–126) is exactly `path || 0x00 || u64-LE len || canonicalized`; `hashed_files` sorts `/`-joined rel paths, folds `test-fixtures` case-insensitively (line 191), excludes `art/`-prefixed files only for `ART_EXTENSIONS` (in_scope, 210–219 — `biomes/terrin.png` outside `art/` IS hashed per spec), refuses symlinks/non-UTF-8 names (172–181), and refuses case-colliding paths via a full-map fold rather than adjacency (141–150 — the comment correctly identifies why byte-sort doesn't cluster them). `canonicalize` (224–234) is UTF-8→LF-trim-end, non-UTF-8 verbatim. `DATA_HASH_VERSION = 2` with the injectivity rationale (lib.rs:48–53).
- **Missing-vs-empty honesty** — optional registries (sites/colonies/recipes/experiments/contracts/techtree/parts) read a missing dir as an empty set; `loc` is required. This is honest but note it also means **a missing `data/parts/` directory silently yields a part-less game** — there is no "the catalog must be non-empty" check anywhere.
- Minor (not a finding): `LoadError::UnsupportedSchemaVersion.expected` reports only `supported.last()` (load.rs:346) — loses the band on sites.ron, cosmetically.

---

## "What does adding the missing part X require?" — the gap ledger

| Missing capability | Schema status | Work needed |
|---|---|---|
| Docking port | `DockPort` variant + `dock_ports` spec + joints sim all live | **Data only** — author the RON + loc keys (tech-gate it); validator/sim/contract paths already exercise the spec |
| Science lab | `Lab` variant + `lab` spec + research closed-form live | **Data only** for the part — but the *loop* also needs reachable `StartResearch`/`StopResearch` UI (PRD-15) and researchers that mean something (currently a stub `Vec<ResearcherAssign>`, PRD-06) |
| Probe core | `Control`-only part is legal (category `Control` accepts `Control` or `CrewSeat`) | **Data only** — though F2 means the part confers nothing mechanical; it gates nothing today |
| LS-capable tanks | `Tank{oxygen,food,water}` — all three resources exist | **Data only** — sim doc at lib.rs:2290–2296 explicitly notes the authored set provides none; `SetLifeSupportEnabled` is settings-gated opt-in (csp-app/lib.rs:559–566, defaults off at settings.rs:152) |
| More engines/tanks/experiments/variants | Fully live | **Data only** |
| Static landing legs | `Structure` part + collider — contact model needs no module | **Data only** (a leg is just a strong low part); *retractable* gear needs F5's deployable |
| One-shot fairing shell | `Structure` shell + `JettisonPart` (reachable via staging) | **Mostly data** — aero-shielding semantics are automatic via weld re-voxelization; no fairing-specific affordance needed for v1 |
| Vessel drill/converter (ISRU) | Variant exists, **resolver drops it** (F1); `surface_resources` all `[]` | **Phys + sim + data** — WeldPart field, surface-extraction process, concentration maps, and F3's power model |
| Parachute/decelerator | No variant; `PanelJam`/`ConfigMask` infrastructure half-exists | **Schema+phys+sim+data** — deployable state (F5) + drag contribution; PLAN never names "parachute" but §3.2 recovery on Terrin is unachievable without some decelerator (PRD-23 F1) |
| Solar/RTG | No part variant (`ColonyModuleDef::Solar`/`Generator` only) | **Schema+phys+sim+data** — F3's whole vessel power model |
| RCS | `ForceSiteKind::Rcs` dead discriminant + dead `monoprop` | **Schema+phys+sim+data** — module variant, force sites, translation/attitude channel |
| Wings | `AeroDef::cl_curve` produces real lift per part | **Data-only for lift**, but control surfaces/roll-pitch authority need F2's attitude model |
| Wheels/rovers | Nothing at any layer | **Schema+phys+sim+data** — drive model, surface locomotion; pillar 1's "rovers" is the furthest-out gap |
| Antenna/CommNet | Correctly absent — PLAN:1365 marks it post-v1 | None for v1 |
| Deployables (gear retract, bay doors, panels) | `ConfigMask` bits + orphan `SetAeroConfig` command | **Schema+sim** — `Deployable` module + `SetDeployed` composing the mask (F5) |

---

## Suggestions for the parent agent

1. **F1's framing change is the key contribution vs PRD-23:** `Drill`/`Converter` parts are not "schema-ready, content-missing" — they are "schema-accepted, runtime-dropped." Any content push that authors them without the WeldPart/sim work produces validated dead weight. Recommend either a validator *warning* that part-level `Drill`/`Converter` currently have no runtime effect, or a tracking item to add the vessel-side path.
2. **F2+F3 are the same disease:** schema fields (`torque_nm`, `power_w`) with no consumer are worse than absent fields — they tell the player (and the tech-tree upgrade economy) a lie. Cheapest honest fix: doc-mark them "reserved — display only" and remove/zerocap `ControlTorqueNm` tracks until an attitude/power model lands.
3. **Loader/hash need nothing.** If a finding must be found here, it's the absent "non-empty catalog" sanity check (a missing `data/parts/` loads as zero parts, green) — worth one `validate_dataset` rule.
4. **F6 is a two-line signature change** (`validate_contracts` gains `&LoadedParts`) that prevents the first contract-template edit from minting unsatisfiable `Lab`/`DockPort` offers.
5. Cross-cluster handoffs already owned elsewhere: zero-part variants' *content* side → PRD-23; `StartResearch`/crew/lab reachability → PRD-06/15/16; `PanelJam` module↔mode applicability rule → PRD-23 F7.
