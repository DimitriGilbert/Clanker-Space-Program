# PRD-23 Review — Data validation + shipped content

**Files reviewed:** `crates/csp-data/src/validate.rs` (6281 lines), `crates/csp-data/src/part.rs` (`ModuleDef`, `PartCategory`, `SituationMask`, `ReliabilityDef`), the whole `data/` tree (10 parts, 1 techtree file, 6 contracts, 1 experiment, 3 recipes, `systems/home/` 4 bodies + sites + colonies, `balance.ron`, `resources.ron`, `loc/en.ron`), `apps/demo/data/` (diffed against `data/`), `crates/csp-view/src/demo.rs`, `crates/csp-view/tests/demo.rs`, plus sim-side consumers (`csp-sim/src/research.rs`, `contracts.rs`, `economy.rs`, `lib.rs`) to verify which module mechanics actually exist.

**Verdict:** The *validator* is excellent — every §3.0/§3.1 rule is implemented, typed, and double-gated (per-kind test files plus `validate_dataset` at production startup, `csp-view/src/lib.rs:1010`). The *content* is a skeleton that cannot demonstrate the PRD loop. Of the ten `ModuleDef` variants the schema defines, four (`DockPort`, `Drill`, `Converter`, `Lab`) have **zero authored parts**, and the mechanics the PRD loop most needs — parachute/recovery hardware, solar panels, landing gear, wheels, wings, antenna, RCS — **have no `ModuleDef` variant at all**: they cannot be authored as data; they require schema + sim work first. Worse, the authored numbers don't close the science economy on their own: the total authored data cap (230 units) × the one experiment's `transmit_ratio` (0.4) ≈ 92 science, **below the 105 science the four tech nodes cost** — without data recovery (no parachute → landing unsurvivable) or a lab multiplier (no Lab part), the full tree is mathematically unreachable. The validator cannot catch any of this: it checks that authored things are *well-formed*, not that the set is *sufficient*.

---

## Module coverage matrix (the headline table)

`ModuleDef` variants defined at `crates/csp-data/src/part.rs:506–624` vs. parts authored in `data/parts/`:

| Variant | Defined at | Sim support | Parts authored | Parts |
|---|---|---|---|---|
| `Engine` | part.rs:509 | ✓ (45 use-sites) | **2** | `csp.engine.sparc`, `csp.engine.ember` |
| `Tank` | part.rs:542 | ✓ (22) | **3** | `csp.tank.s160`, `csp.tank.s320`, `csp.battery.z1k` (Tank of `electric_charge`) |
| `Control` | part.rs:549 | ✓ (13) | **1** | `csp.pod.mk1` (with `CrewSeat`) |
| `Decoupler` | part.rs:559 | ✓ (10) | **1** | `csp.decoupler.s125` |
| `CrewSeat` | part.rs:577 | ✓ (11) | **1** | `csp.pod.mk1` |
| `Experiment` | part.rs:582 | ✓ (14) | **1** | `csp.science.canister` |
| `DockPort` | part.rs:568 | ✓ (6; balance has 8 `dock_*` constants, M6-B/C/D) | **0** | — |
| `Drill` | part.rs:593 | ✓ (6) | **0** | — |
| `Converter` | part.rs:604 | ✓ (7) | **0** | — |
| `Lab` | part.rs:615 | ✓ (`LabSpec`, `apply_start_research`, `research.rs:945–953`) | **0** | — |

Variants **absent from the enum entirely** (no authoring path exists): `Parachute`/`Deployable`, `SolarPanel`/`Generator`, `LandingGear`, `Wheel`, `Wing`/`ControlSurface`, `Antenna`, `RCS`. Verified by `grep -niE "parachute|solar|antenna|gear|wheel|wing|rcs"` over `crates/csp-data/src/` and `data/` — the only hits are `ColonyModuleDef::Solar` (colony.rs:64, a *colony* module, not a part module) and `FailureMode::PanelJam` (part.rs:722), a failure mode for deployables no part can possess.

The 10 authored parts: `csp.pod.mk1` (CrewSeat+Control), `csp.engine.sparc` (tech.start), `csp.engine.ember` (rocketry.2), `csp.tank.s160`/`s320`, `csp.battery.z1k`, `csp.decoupler.s125`, `csp.nosecone.s125`, `csp.structure.adapter`, `csp.science.canister`. (Exploration map said 11; verified count is **10** `.ron` defs.)

---

## Findings

### F1 — [HIGH] No recovery path exists: there is no `Parachute` variant in the schema, no landing gear, no legs — §3.2's recover-vs-transmit loop is dead on arrival

**Where:** `crates/csp-data/src/part.rs:506–624` (enum ends at `Lab` — no decelerator/deployable variant exists); `data/parts/` (nothing approximates one); `crates/csp-sim/src/research.rs:843–897` (`apply_recover_data` requires `VesselState::SurfaceLatched`); `crates/csp-sim/src/lib.rs:4262` (`apply_breakup` on hard surface contact); `data/experiments/exp.materials_study.ron` (`transmit_ratio: 0.4`).

**Problem:** §3.2 makes recovery the premium science path: transmitting banks `units · transmit_ratio` (0.4) and recovery banks 100% / the `1 − ratio` remainder and *closes the key*. `RecoverData` requires the vessel be `SurfaceLatched` (`research.rs:855`). There is no way to get a vessel from orbit to a surface latch intact on Terrin: the schema cannot express a parachute, drogue, airbrake, heatshield-with-ablator, or landing leg. The only decelerators in the game are the two engines — a pod re-entering at orbital velocity has ~1.5 km/s of atmosphere to shed before touchdown and a pod `max_temp_k` of 1200 K. Rask (no atmosphere, 1.6 m/s²) allows powered descent in principle, but a Terrin return — the mandatory crew-return leg — has no survivable authored path.

**Evidence:** enum list above; `ModuleDef` has no drag-chute/deployable variant; the authored `situations` mask on the canister (49 = Landed|SpaceLow|SpaceHigh, `csp.science.canister.ron:48`) even excludes `Splashed` — the content authors knew splashing down isn't survivable.

**Impact:** The 60% remainder of every transmitted science key, the `Splashed` situation, crew return to Terrin, the `contract.rescue` template (`data/contracts/contract.rescue.ron` — "recover a vessel sitting on the surface", gated to `tech.rocketry.3`), and §3.5's recovery economy (`recovery_efficiency` in `balance.ron:58`) all depend on a survivable landing that no part can deliver.

**Suggestion:** Add a `ModuleDef::Parachute { deploy_alt_m / deploy_speed_mps, drag_area_deployed_m2, … }` variant (or a generic `Deployable` with drag/strength curves — it also gives `PanelJam` a real subject, see F7) and author one small chute. This is a schema + sim + content change, not a data-only fix; the sim side needs a deploy event and drag contribution. Landing legs can ride the same variant later; wheels/wings need their own.

---

### F2 — [HIGH] No `Lab`-module part: the entire §3.2 lab pipeline (data → science conversion with researchers) is unreachable, and combined with F1 the science economy cannot close

**Where:** `data/parts/` (zero `Lab(` occurrences); `crates/csp-sim/src/research.rs:926–953` (`apply_start_research` rejects with `"research: node carries no lab"` — unreachable); `crates/csp-sim/src/research.rs:959` (`researcher_slots` check — dead code in practice); `data/balance.ron:45–48` (`science_softcap_k`, `research_data_epsilon`, `lab_science_per_data`, `transmit_ec_per_data_unit` all authored); `data/techtree/home.ron:33` (`facility.lab` unlock at `tech.science.1`).

**Problem:** Every piece of lab machinery shipped except the lab: the closed-form two-regime processor, the epsilon-exhaustion events, the balance constants, even a `facility.lab` tech unlock — but no `data/parts/*.ron` carries `ModuleDef::Lab`. `StartResearch` is a command that cannot ever succeed in authored content.

**Evidence — the economy math doesn't close without it.** Total authored data cap = 30 (terrin) + 60 (rask) + 90 (duva) + 50 (solen) = **230 data units** (`exp.materials_study.ron:14–17`; caps converge asymptotically under the §3.2 soft-cap). Transmit-only yield = 230 × 0.4 = **≤92 science**. Tech nodes cost 0 + 15 + 30 + 60 = **105 science** (`home.ron`); upgrade-track levels add ~209 more. So: transmission alone cannot finish the node tree even with every key ground to exhaustion; the remaining ~13+ science *must* come from recovery (F1 — no parachute) or lab processing (this finding — no lab). The loop `science → research` is open at both ends.

**Impact:** §3.2's headline mechanic ("labs: the closed form is exponential", "bank data early, build labs later") is unplayable; `researcher_slots`/researcher assignment, `ResearchProjectComplete`/`ResearchDataExhausted`, and the hoarding strategy are all dead surface. `tech.rocketry.3` — the node that gates `contract.part_test`, `contract.rescue`, `contract.colony_resupply` — is only purchasable if the player grinds every body+situation cap *and* recovers samples that can't survive re-entry.

**Suggestion:** Author a `csp.lab.mk1` part (`Lab { throughput_data_per_s, multiplier: >1.0, researcher_slots: 1..2 }`, `CrewSeat` for the researchers, `tech_requirement: "tech.science.1"` so the `facility.lab` unlock has a visible payload) — the schema and sim already fully support it; this is a pure data authoring gap. Separately, consider whether `facility.lab` should itself process data — but that is a sim/design question (see F8 on facility unlocks being unverifiable).

---

### F3 — [HIGH] Docking is fully built in sim and schema — and zero `DockPort` parts exist

**Where:** `data/parts/` (zero `DockPort(` occurrences); `crates/csp-data/src/part.rs:568–574` (`DockPort { size, node }`); `data/balance.ron:24–41` (eight authored dock/transfer constants: `dock_capture_range_m`, `dock_capture_angle_deg`, `dock_soft_pull_per_s`, `dock_latch_*`, `transfer_radius_m`, `transfer_rate_units_per_s`, `crew_transfer_ticks`); `crates/csp-data/src/validate.rs:3130–3150` (validator checks the port face names an `attach_nodes` entry and that two ports don't claim one node — neither rule can ever fire).

**Problem:** The M6-B/C/D chain — soft capture, latch merge, post-dock resource/crew transfer — is implemented and tuned, the schema models port size classes and face nodes, the validator is ready, and no port exists to fly. Docking is also the *enabler* for everything downstream the user complains about: stations, crew transfer between vessels, the `Rescue` contract's natural completion path (dock/transfer rather than land-and-luck), and colony resupply rendezvous.

**Impact:** No docking → no stations, no multi-vessel assembly, no rescue-by-rendezvous; `transfer_radius_m`/`crew_transfer_ticks` and `TransferCrew`/`BoardCrew` plumbing (`csp-sim/src/lib.rs:780–793`) exercise only the trivial pad-boarding case.

**Suggestion:** Author `csp.dockport.s125` (one `DockPort { size: <class>, node: "top" }` + `Control`-free Utility category; the validator's face-node check will immediately exercise). Also needs at least a probe-core or second-pod path to give the *target* something to dock with — two pods docked is a valid first content pair.

---

### F4 — [HIGH] No vessel power generation: no `Solar`/`Generator` `ModuleDef` variant exists — every craft is a 5.5-hour battery

**Where:** `crates/csp-data/src/part.rs:506–624` (no power-producing variant); `data/parts/csp.battery.z1k.ron` (the only EC source: `Tank(capacity: [("electric_charge", 1000.0)])`, 40 kg); `data/parts/csp.pod.mk1.ron:45` (`Control(power_w: 50.0)` — the only always-on draw); `data/balance.ron:75–77` (life support `life_support_ec_per_crew_s: 0.05` adds more).

**Problem:** `ColonyModuleDef::Solar`/`Generator` exist for colonies (`colony.rs:57–64` — the authored Rask colony runs on them), but the *part* schema has no generator variant at all. A vessel's total energy budget is the 1000-EC battery; the pod's Control module draws 50 W continuously while providing authority → full depletion in ~5.6 hours even before transmission EC (`transmit_ec_per_data_unit: 1.0`), life support, or any powered module. Terrin orbit period is ~1.5 h; a Rask round trip is days.

**Impact:** Even if Drill/Converter/Lab parts were authored tomorrow they could not run — there is no power to run them. Interplanetary flight (Duva, 587 W/m² insolation authored and unused, `duva/body.ron:43`), long science campaigns, and station-keeping are all EC-capped at launch. The `insolation_w_m2` field every body carries exists for colony solar only.

**Suggestion:** Add `ModuleDef::SolarPanel { area_m2, efficiency, deployable }` (shares the deployable concept with F1's parachute — one `Deployable` mechanism serves both) and author a starter panel at `tech.science.1`. Until then the authored `insolation_w_m2` per-body data has exactly one consumer (colonies).

---

### F5 — [MEDIUM] Vessel ISRU is dead twice over: zero `Drill`/`Converter` parts **and** `surface_resources: []` on every body

**Where:** `data/parts/` (no Drill/Converter modules); `data/systems/home/{terrin,rask,duva,solen}/body.ron` — all four declare `surface_resources: []` (e.g. `duva/body.ron:42`); `data/systems/home/sites.ron` (`resource_yields` on 3 sites — the *colony* drill path only); `data/recipes/*.ron` (3 recipes resolve only via colony `Converter`, `colonies.ron:26–29`).

**Problem:** The §3.0 schema's `surface_resources: Vec<(ResourceId, ConcentrationMapRef)>` — the per-body concentration maps a vessel-borne `Drill` module would mine — is empty on all four bodies, and no `*.png` concentration map exists anywhere in `data/` (verified: the only PNGs are `biomes.png`). So the part `ModuleDef::Drill`/`Converter` variants have neither parts nor the world data they would consume. The colony mining loop works (authored `rask-mines` site at 0.9 ore yield + Solar/Drill/Converter/Store colony modules), but a *vessel* can never extract anything anywhere.

**Impact:** No mobile ISRU, no surface-sample resource gameplay; `recipe.isru_propellant`/`recipe.electrolysis` exist only for the one authored colony.

**Suggestion:** Author concentration maps for at least `ore`/`water` on Rask and `ore`/`water` on Terrin (the biome-map toolchain `gen_biome_maps` already establishes the pipeline), plus one drill part and one converter part gated mid-tree. Also confirms the validator correctly has no rule here — empty `surface_resources` is legal; sufficiency is not its job.

---

### F6 — [MEDIUM] Tech tree gates almost nothing: 4 nodes, 2 of which unlock zero parts; the deepest node unlocks no hardware at all

**Where:** `data/techtree/home.ron:15–94`; per-part `tech_requirement` fields (`data/parts/*.ron`).

**Problem:** `tech.start` covers 8 parts; `tech.rocketry.2` gates exactly 2 (`csp.engine.ember`, `csp.tank.s320`); `tech.science.1` and `tech.rocketry.3` gate **zero `PartId`s** — they unlock only track levels, facilities, and contract types. The 60-science terminal node sells "Deep propulsion tuning" (loc `tech.rocketry.3.desc`, `en.ron:67`) but delivers no new parts; conversely `tech.science.1` sells "Instrument packages and the lab" (`en.ron:63`) and unlocks neither a new instrument nor the (nonexistent) lab part — only the `facility.lab` ID. A player grinding the tree finds the science branch is a dead promise and the rocketry branch ends in refinements of the same two engines.

**Impact:** The "research" leg of the loop has no *content payoff* to buy: there is no second experiment to unlock (only `exp.materials_study` exists, already unlocked), no lab part, no port, no chute, no drill. Even a perfectly-working economy purchases only +3% Isp tweaks. Combined with F2's math, the tree is both unfinishable and barely worth finishing.

**Suggestion:** This is a content-volume problem (PLAN §9 lists it as a known risk) — the tree shape itself validates fine. When parts from F1–F5 are authored, give each a real node; consider a `tech.recovery`/`tech.flight` branch so the tree communicates progression structure rather than being a flat gate on two parts.

---

### F7 — [MEDIUM] Validator gap: no module↔failure-mode applicability check — `PanelJam` is authored on parts where nothing can jam (nothing can, anywhere)

**Where:** `data/parts/csp.pod.mk1.ron:61` (`(PanelJam, 0.3)`), `data/parts/csp.science.canister.ron:64` (`(PanelJam, 0.3)`); `crates/csp-data/src/part.rs:722` (`PanelJam` = "Deployable (panel/antenna) does not deploy"); `crates/csp-data/src/validate.rs:3153–3248` (`check_reliability` — validates weights > 0, knot ordering, ranges; never checks the mode is meaningful for the part's modules).

**Problem:** No `ModuleDef` variant models a deployable, so `PanelJam` is a failure mode with no subject — 30% of failure draws on the pod and canister resolve to an event that cannot describe anything on the craft. Similarly there is no check that e.g. `IgnitionFailure`/`StuckGimbal`/`DegradedThrust` only ride parts with an `Engine` module (currently fine — only engines author them — but nothing enforces it as the part set grows), or `StuckDecoupler` only on `Decoupler` parts, or `Leak` only on `Tank` carriers (battery leaks are odd but defensible).

**Impact:** Dormant-hazard rolls (nonzero `dormant_stress` on both parts) can fire `PanelJam` on a pod with no panels — a confusing failure notification at best, wasted hazard budget at worst. The §3.4 promise "hidden stress multipliers are bad design" cuts the other way here: a *displayed* failure that references hardware the part doesn't have is worse than none.

**Suggestion:** Add a validator rule mapping each `FailureMode` to the module classes that make it meaningful (`PanelJam` → deployable-bearing modules once they exist; `IgnitionFailure`/`StuckGimbal`/`DegradedThrust` → `Engine`; `StuckDecoupler` → `Decoupler`; `Leak` → `Tank`; `Explosion`/`SensorFault` → universal). Until a deployable variant lands, drop `PanelJam` from both files (reweight to `SensorFault` 1.0 on the canister, or add `Explosion`-free alternates on the pod).

---

### F8 — [LOW] `unlocks.facilities` are unverifiable strings — no facility registry exists for the validator to check against

**Where:** `crates/csp-data/src/validate.rs:3658–3663` (`for f_id in &n.unlocks.facilities { check_id(f_id) }` — well-formedness only); `data/techtree/home.ron` (`facility.launchpad`, `facility.vab`, `facility.lab`, `facility.test_stand`); `crates/csp-data/src/ids.rs:119` (`FacilityId` is a bare ID type). Contrast: `unlocks.contract_types` IS cross-checked (`UnknownContractType`, `validate.rs:4129–4140`) and `track_levels` grants ARE checked (`UnknownUpgradeTrack`, level-cap bounds).

**Problem:** Contract-type unlocks and track grants resolve against authored registries; facility unlocks resolve against nothing — there is no `data/facilities/` or equivalent. A typo'd `facility.laab` validates green, and `facility.lab`/`facility.test_stand` unlocks have no authored data describing what the facility is or does (the sim's `FacilityId` use is a crew-location marker, `economy.rs:273`, and the test-stand is referenced by `reliability.rs:1122` but no facility def carries its economics).

**Impact:** Low today (4 consistent IDs), but the asymmetry means one whole unlock channel is editorial-only. It's also what makes F2's `facility.lab` unlock unverifiable.

**Suggestion:** Either author a `data/facilities/*.ron` registry (name, loc key, function) so the validator can `UnknownFacility` it, or drop `facilities` from `unlocks` until the facility layer exists.

---

### F9 — [MEDIUM] The part set cannot provision life support, RCS, or uncrewed-light missions: dead resource + missing variants make three §3.5 systems unstaffable

**Where:** `data/resources.ron:20` (`monoprop` — **nothing consumes it**: verified zero references outside its own registry entry and loc string); `data/parts/` (no tank authors `oxygen`, `food`, or `water` capacity; no `Control`-only part exists as a probe core); `data/balance.ron:75–77` (`life_support_oxygen_per_crew_s: 0.02`, `food 0.005`, `ec 0.05` — M8-C consumption is tuned); `crates/csp-sim/src/lib.rs:788–793` (`BoardCrew`).

**Problem:** Three sufficiency gaps in one: (a) `monoprop` is a registered resource with zero producers/consumers/carriers — an RCS module doesn't exist to burn it; (b) life support consumes `oxygen`/`food`/`electric_charge` per crew-second, and while `Tank` is generic enough to hold them, no authored tank does — so enabling LS on a crewed flight has no provision path in content; (c) the only `Control` module is bolted to the 800 kg two-seat pod, so the minimum controllable stage is the pod itself — no lightweight probe core means every satellite/relay is effectively crew-rated mass.

**Impact:** `contract.satellite` (an uncrewed-satellite-shaped contract) must fly an 800 kg crew pod; crewed missions beyond ~1 orbit either disable LS or starve crew; docking-without-ports (F3) has no RCS backup even conceptually.

**Suggestion:** Author a `csp.probe.s0625` (Control-only, small torque, solar-dependent — pairs with F4), an `csp.tank.life.s` (O₂+food+water), and either an RCS block or remove `monoprop` from `resources.ron` until a consumer exists (validator won't flag it — dead resources are legal, again a sufficiency-not-wellformedness gap).

---

### F10 — [LOW] `apps/demo/data/` drift is guarded for *file-set* and *validity*, but not for *content equality* of the 17 shared files

**Where:** `apps/demo/data/` vs `data/` (diffed); `crates/csp-view/src/demo.rs:221–246` (`demo_files_match_disk` — asserts the `FILES` manifest equals the on-disk set only); `crates/csp-view/tests/demo.rs:24–105` (`demo_dataset_loads_and_validates` — full semantic gate over the embedded set); `crates/csp-view/tests/demo.rs:106–116` (`demo_system_is_terrin_only` — pins the §6.3 subset).

**Problem:** The demo is a deliberately-reduced Terrin-only slice — `system.ron` drops rask/duva, `sites.ron` drops `rask-mines`, the experiment's rask/duva caps are removed, the tree's rask-reaching contract unlocks are emptied, three contracts and `colonies.ron` are absent (the loader tolerates the missing file, `load.rs:519–525`), and `en.ron` is a documented subset. This is **intentional** (§6.3, every delta is commented) — not drift. The risk: the 17 files that *are* shared verbatim (all 10 parts, `balance.ron`, `resources.ron`, all 3 recipes, `solen/body.ron`, `terrin/body.ron`, `terrin/biomes.*`) are byte-identical copies with **no test asserting they stay identical**. A balance tweak to `data/parts/csp.engine.sparc.ron` silently leaves the wasm demo flying a different engine. (Verified today: `diff -rq` shows zero differences on all shared files.)

**Impact:** Latent divergence between what the demo demonstrates and what the game ships — exactly the failure class `demo_files_match_disk` was built to catch, one layer deeper (contents, not file names).

**Suggestion:** Add a test asserting `apps/demo/data/<p>` equals `data/<p>` for every path present in both trees (or generate the demo subset from `data/` at build time so there is one source of truth). ~15 lines in `crates/csp-view/tests/demo.rs`.

---

## Validator coverage assessment (vs. PLAN §3.0/§3.1 rules)

`validate_dataset` (`validate.rs:4216–4250`) runs all per-kind validators plus cross-file `check_surface_resources`; it is invoked at production startup (`csp-view/src/lib.rs:1010`) and mirrored by per-kind test files (`crates/csp-data/tests/{parts,techtree,contracts,experiments,home_system,balance,colonies,unknown_fields}.rs`).

| PLAN rule | Covered | Where |
|---|---|---|
| §3.0 SOI > every child apoapsis | ✓ | `SoiTooSmall`, `check_orbit_rules` (~1410) |
| §3.0 parent exists + acyclic | ✓ | `ParentNotFound`, `ParentCycle`, `DisconnectedFromStar`, `check_graph`/`check_connectivity` (1226–1314) |
| §3.0 period consistent w/ parent μ, a to 1e-9 | ✓ | `PeriodInconsistent` (~1452) |
| §3.0 atmosphere top > scale height | ✓ | `AtmosphereTopTooLow` (~1544) |
| §3.0.1 biome legend/map integrity | ✓ | `BiomeLegendDuplicate`, `BiomeIndexExceedsPalette`, `BiomeIndexNotInLegend`, `BiomeMapPngInvalid`, `BiomeMapTooLarge` (1592–1660) |
| §3.1 `schema_version` first field | ✓ | `SchemaVersion` in every kind validator |
| §3.1 stable ids / id↔filename | ✓ | `MalformedId`, `PartIdFileMismatch`, contract stem check |
| §3.1 engine rules (isp_sl ≤ isp_vac, propellant fractions sum 1, throttle band, ignitions ≠ 0) | ✓ | `check_engine_module` (2965–3078) |
| §3.1 tank/exp/drill/conv/lab module rules | ✓ | `check_module` (2853–2952), `check_drill_module` |
| §3.1 dockport face node + unique face | ✓ | `check_modules` (3130–3150) |
| §3.3 no cycles / every part reachable / cost monotonic | ✓ | `check_tree_cycles`, `UnknownTechRequirement`, `TechCostNotMonotonic` (3553–3680) |
| §3.3 track resolves on every family part; delta needs positive base | ✓ | `check_tree_tracks` (3695–3820) |
| §3.6 contract refs (bodies, experiments, tech prereqs, resource refs) + tree `contract_types` resolve | ✓ | `validate_contracts` (4038–4140) |
| Loc resolution for every user-facing key | ✓ | `validate_*_loc_keys` family + `validate_loc` |
| `PanelJam`/failure-mode ↔ module applicability | ✗ | F7 |
| `unlocks.facilities` resolve | ✗ | F8 (no registry exists) |
| Content *sufficiency* (does a part exist for each reachable mechanic; does the economy close) | ✗ (out of scope, correctly) | this report |

Assessment: coverage of the *stated* rules is essentially complete. The two real gaps are the missing failure-mode applicability rule (F7) and the facility registry (F8); the rest of the pain is content volume, which no validator rule can express.

---

## PRD-loop sufficiency summary (the answer to "is the content enough?")

| Loop stage | Required content | Present? |
|---|---|---|
| Science: run experiment | Experiment part + experiment def | ✓ minimal — 1 experiment, 1 single-use instrument |
| Science: transmit | EC source | ~ — battery only; no generation (F4) |
| Science: recover | survivable landing | ✗ — no parachute/legs variant exists (F1) |
| Research: lab processing | Lab part + crewed researchers | ✗ — no Lab part (F2) |
| Research: pay nodes | economy closes | ✗ — transmit-only ceiling ~92 < 105 node cost (F2) |
| Colony: founding/supply | authored colony + resupply | ~ — 1 authored colony + `contract.colony_resupply`; FoundColony command exists but vessel-side Drill/Converter/concentration maps absent (F5) |
| Crew: boarding/transfer | seats, transfers | ~ — `BoardCrew`/`TransferCrew` exist; crew can't get home from orbit (F1); LS unprovisionable (F9) |
| Docking/stations | port parts | ✗ — zero `DockPort` parts (F3) |
| Planes/rovers | wings, wheels, gear | ✗ — no variants exist at all |
| Contracts | 6 templates | ✓ all reference valid bodies/experiments — but `rescue` + `colony_resupply` completion paths lean on F1/F3/F5 content that doesn't exist |

**Minimum content to make the PRD loop playable (in dependency order):** (1) parachute/deployable variant + `csp.chute.s125` [F1]; (2) `csp.lab.mk1` [F2]; (3) `csp.dockport.s125` + probe core [F3/F9]; (4) solar panel variant + part [F4]; (5) concentration maps + drill/converter parts [F5]; (6) life-support tank + RCS or drop `monoprop` [F9]; (7) a second experiment and real part-unlock payload on the two empty tech nodes [F6]. Items 1–4 each need a schema/sim touch, not pure data authoring.

## Suggestions for the parent agent

- The four zero-part variants (`DockPort`, `Drill`, `Converter`, `Lab`) are pure-data gaps: parts can be authored today with no code change. The missing variants (`Parachute`, `Solar`, `Gear`, `Wheel`, `Wing`, `Antenna`, `RCS`) are schema gaps — flag to whoever owns `ModuleDef` evolution since they also gate F7's fix.
- Cross-cluster: F2's "science economy can't close" interacts with PRD-06/07 (science/space-center gameplay); F1's no-recovery interacts with PRD-15/16 (recovery UI affordances mean nothing without survivable landing).
- The exploration map's "parts×11" is stale — verified 10.
