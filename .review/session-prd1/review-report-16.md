# PRD Review — Cluster PRD-16: view shell, session state, progression mirror, launch path

**Files reviewed (read in full or near-full):**
- `crates/csp-view/src/lib.rs` (3352 lines — View facade, six §2.5 schedules, `UiOutbox`, `ProgramMirror`/`stage_program` ~2485–3003, `launch_craft` ~2047–2230, `fire_stage` ~2257–2352, `recover_control`/`to_bubble_eligible` ~1917–1997, `from_source` install wiring ~1073–1242, `seed_starter_craft`, `launch_preflight` ~2383–2420)
- `crates/csp-view/src/tutorial.rs` (278 — full)
- `crates/csp-view/tests/*` (9 files scanned: pad_spawn, stage_plan, startup_colonies, tutorial, demo, flight_focus, flight_plume, render_gate, part_silhouettes, render_dump_extra, vab_latency)

**Cross-checked against:** `PLAN.md` (§2.5 schedules, §2.6.2 rails regime, §2.10–2.11 automation, §3.2 science, §3.5 economy/colonies/crew, §3.5.1 colony model, §3.6 contracts, §3.4 revert policy, §7 save/load); `crates/csp-sim/src/lib.rs` `SimCommand` enum (L515–1143, all 51 variants) + `apply_spawn_physical` (L5430–5500) + `apply_to_rails` (L5610–5729) + `rails_warp_refusal` (L3382–3419); `crates/csp-sim/src/{research,contracts,events,manufacture,bubble}.rs`; `crates/csp-app/src/lib.rs` (every `SimCommand::` issuance, `apply_startup_difficulty`, outbox drain); `crates/csp-ui/src/program.rs` (the only progression surface); `data/{balance.ron,contracts/*,parts/*,experiments/*}`.

## Command-reachability ledger (the review's core evidence)

51 `SimCommand` variants. A command counts **player-reachable** only if non-test `csp-view`/`csp-app` code issues it.

| Class | Commands | Path |
|---|---|---|
| Reachable, fires in gameplay (12) | `SetWarp`, `AdvanceTo`, `SetLifeSupportEnabled` (startup only), `SetThrottle`, `SetAttitude`, `SpawnPhysicalVessel`, `JettisonPart`, `ResearchTech`, `ResearchUpgradeLevel`, `AcceptContract`, `TransmitData`, `RecoverData` | warp controller (csp-app:862–879, 526–530); startup difficulty (csp-app:562–566); throttle/attitude dispatch (csp-app:1705, 1922); `launch_craft` (lib.rs:2213); `fire_stage` (lib.rs:2347); `program_command` (lib.rs:704–728) |
| Wired but **dead in practice** (5) | `SetManeuver`, `DeleteManeuver`, `ToBubble`, `TakeControl`, `RunExperiment` | map panel filters `on_rails` only (lib.rs:665–669) — no player path to rails exists (Finding 2); `recover_control` targets on-rails only (lib.rs:1917–1997); `RunExperiment` silently dropped in space situations (Finding 1) |
| **Unreachable — missing glue** (28) | `ToRails`, `RecoverVessel`, `HireCrew`, `BoardCrew`, `TransferCrew`, `TransferResource`, `ManufactureCraft`, `FoundColony`, `AbandonColony`, `AssignCrewToColony`, `EvacuateColony`, `DamageColonyModule`, `ScheduleColonyInspection`, `JoinColonyInventory`, `LeaveColonyInventory`, `TransferColonyResource`, `TransferColonyCrew`, `StartResearch`, `StopResearch`, `SetAlarm`, `WatchTransferWindow`, `SetVesselTracked`, `LoadScript`, `UnloadScript`, `SetScriptEnabled`, `EngageAutopilot`, `DisengageAutopilot`, `RunBenchTest` | no producer anywhere outside tests/bench/MCP |
| Absent-by-design, not gaps (6) | `SpawnVessel` (debug/agent verb), `Unlatch` (implicit — `SetThrottle`/`SetAttitude` unlatch latched vessels, sim doc L593), `InstallAeroTables` (worker path superseded by sim-side compute at apply, lib.rs:5517–5539), `SetAeroConfig` (no configurable modules in shipped content), `Undock` (no `DockPort` part exists), `ContractReward` (economy-harness stub) | verified |

Effective player surface ≈ **12–17 of 51** commands; the map's own "~15/40" estimate holds. Every gap above funnels through this file's three choke points: `UiOutbox` (8 fields — no colony/crew/alarm/save/script/transfer slot), `program_command` (6 of 51 variants), and `View`'s public method vocabulary (no `to_rails`, `recover_vessel`, `save_game`, `engage_autopilot`, …).

## Findings

---

### [HIGH] Finding 1: `RunExperiment` is silently dropped in space — the primary science loop is a dead button

**File:** `crates/csp-view/src/lib.rs:2603–2607` (biome derivation), `lib.rs:2540` (`science_ctx`), `lib.rs:711–719` + `lib.rs:569–572` (the drop), panel gate at `lib.rs:2640–2661` + `crates/csp-ui/src/program.rs:400–414`

**Problem:** `stage_program_vessel` computes `ctx_biome` **only for surface-side situations** (`s.is_surface_side()` — `Landed|Splashed|FlyingLow|FlyingHigh`, `csp-data/src/part.rs:395–399`). `ProgramMirror.science_ctx = ctx_biome.zip(ctx_situation)` (lib.rs:2540) is therefore `None` for `SpaceLow`/`SpaceHigh`. When the player clicks "run" on an experiment while in space, `program_command` hits `mirror.science_ctx.clone()?` and returns `None`, and the command is **filtered out of `outbox.program` before ever reaching the sim** — no enqueue, no rejected-command log, no notice. Meanwhile the panel's own `runnable` computation says the experiment *can* run (situation mask + per-(body,situation) cap both pass), so the button is visibly enabled and silently inert.

**Evidence:** The shipped instrument is built for exactly this context — `data/parts/csp.science.canister.ron` `situations: 49` = Landed|SpaceLow|SpaceHigh ("not airborne, it needs to sit still"). A vessel `InBubble` above `atm_top` stages `SpaceLow` (lib.rs:2590–2594); the experiment row shows runnable; the click produces nothing. The sim itself documents that space runs take an **empty** `BiomeId` (`csp-sim/src/research.rs:127–128`: "may be an empty id for bodies/contexts without surface biomes (space situations)"; test uses `BiomeId::from("")` at research.rs:1220), and `apply_run_experiment` never rejects an empty biome.

**Impact:** The canister cannot run while flying (mask excludes it), so the *only* working run contexts shipped are Landed-on-a-biome-mapped-body and Splashed. Orbital science — the first thing a player tries after "Reach space" — does nothing with zero feedback. This is very plausibly the "how do I do science" complaint made literal. The same drop hits any `OnRails`-focused vessel and any body lacking a biome map.

**Suggestion:** In `stage_program`, stage the context on the *situation*, not the biome: `science_ctx: ctx_situation.map(|s| (ctx_biome.clone().unwrap_or_default(), s))` (empty `BiomeId` for space, matching the sim convention), so `RunExperiment` always carries a key. Optionally surface a `flight_notice` when `program_command` drops a command so a stale mirror can never produce a dead click again.

---

### [HIGH] Finding 2: No path to `OnRails` — the entire maneuver/orbit-contract/map layer is dead, and the 20 km bubble mechanic is unimplemented dead data

**File:** `crates/csp-view/src/lib.rs` (nothing issues `ToRails`); `crates/csp-app/src/lib.rs` (same); `crates/csp-sim/src/lib.rs:5615–5729` (`apply_to_rails` reachable only via the command + `finish_burns` at 4506); `data/balance.ron:6` (`bubble_radius_m: 20_000` — "vessels go on rails at the radius and rejoin at 90%")

**Problem:** There is **no player verb and no automatic transition** that converts an `InBubble` vessel to `OnRails`. `apply_to_rails` is invoked by exactly two callers: the `ToRails` command (never issued player-side — verified by grep across `csp-app`/`csp-view`) and `finish_burns` — which only runs after a maneuver-node burn, and `SetManeuver` itself *requires* `OnRails` (lib.rs:6021). Chicken-and-egg: the set of reachable `OnRails` vessels from the launch path is **empty**. `bubble_radius_m` — the PRD's "go on rails at 20 km, rejoin at 90%" boundary — is loaded and validated but consumed by **nothing**: no setter exists, `PhysicsConfig` (`bubble.rs:397–406`) has no radius field, and no code reads it outside `csp-data`. The exploration map's "auto bubble→rails at boundary" claim is inaccurate — the sim has no such conversion; a player vessel stays `InBubble` and is physics-integrated at 64 Hz forever (warp allowed while coasting in vacuum per `rails_warp_refusal`, lib.rs:3401–3419, but residence never changes).

**Evidence:** Full-command grep: `ToRails` is issued only in `csp-sim` tests and `csp-bench/src/scenarios.rs:549`. Consequences cascade:
- The map maneuver panel filters `.filter(|(.., on_rails)| *on_rails)` (lib.rs:665–669) — its vessel list is always empty player-side; `SetManeuver`/`DeleteManeuver` are dead UI.
- `recover_control`/`ToBubble`/`TakeControl`/`FlightFocusOverride`/`lease_pending` — an elaborate, well-tested subsystem (lib.rs:1496–1509, 1561–1578, 1917–1997) — can never fire; the `RecoverControl` keypress only ever lands "NO VESSEL TO RECOVER".
- `ContractTrigger::OrbitAchieved` fires only on `SpawnVessel`/`ToRails`/SOI rebase (`contracts.rs:224–226`) — every orbit predicate is unreachable.

**Impact:** Maneuver nodes, executed burns, transfer windows, satellite/part-test-orbit contracts, recover-control — roughly the entire second half of the flight game — is unreachable even though every UI panel for it exists and is polished. Warp-integration also means a coasting vessel costs full-rate physics at any warp (documented §2.6.2 behavior, but the player can never *leave* the bubble regime).

**Suggestion:** Wire the missing promotion: either implement `bubble_radius_m`/auto-`ToRails` in the sim (the PRD's stated design — a vessel ballistic in vacuum above `atm_top` should go on rails), or emit `ToRails` from `csp-view` when the focused vessel exits the atmosphere ballistically (mirroring `to_bubble_eligible`'s pre-check pattern). Either unlocks the entire downstream chain with no new UI.

---

### [HIGH] Finding 3: The launch bypass — `launch_craft` enqueues `SpawnPhysicalVessel`, never `ManufactureCraft`; the funds gate refuses but never debits

**File:** `crates/csp-view/src/lib.rs:2210–2220` (the enqueue), `lib.rs:2396–2407` (the gate), `crates/csp-sim/src/lib.rs:5491–5500` (the apply's identical gate), `crates/csp-sim/src/manufacture.rs:111+` (the unused pipeline), `crates/csp-sim/src/lib.rs:979–998` (`ManufactureCraft` spec)

**Problem:** `launch_craft` resolves the site (lib.rs:2141–2146), welds, poses, then enqueues `SpawnPhysicalVessel` — a **free, instant** spawn. The PRD (§3.5: "funds ─► part manufacture (funds + resources + time)"; PLAN:1207–1209) routes builds through `ManufactureCraft`: debit `craft_cost_micros` funds + `build_resources` BOM from the launch-site inventory, build over `mass/rate` sim-seconds, deliver `SurfaceLatched` via `ConstructionComplete`. None of that happens: no funds debit, no inventory consumption, no build time. The *interim* model the sim's apply documents ("the build charge is paid at the pad — M8's order-and-time pipeline replaces this direct debit, but the cost exists now", lib.rs:5491–5494) isn't even honest: `apply_spawn_physical` **checks** `cost > funds` and rejects, but never spends — a gate without a debit. The view's `launch_preflight` duplicates the same check. Player-side, funds only ever *grow* (contract advances/completions, science milestone payouts); the sole player-reachable debit is contract-deadline penalties — so the affordability gate is nearly dead code on top of a fictional price.

**Evidence:** `csp_sim::economy::craft_cost_micros` computed at lib.rs:2399 and sim:5495 — compared, never subtracted. `apply_manufacture_craft` (manufacture.rs:170–232) is the only funds+inventory+time path and has zero callers outside its own tests.

**Impact:** The §3.5 economy loop is open: launches are free, `part_costs` is bookkeeping for a debit that never happens, site inventories exist only to feed a command nobody issues, and the "insufficient funds" refusal the UI advertises can essentially never trigger (1e6 starting funds vs ~25k craft costs; only deadline penalties drain). Judged against the PRD this is *missing glue*, not a scope decision — the interim comment explicitly says the pipeline replaces this path, and the pipeline shipped.

**Suggestion:** Route `launch_craft` through `ManufactureCraft { site, craft, build_resources }` (the site is already resolved; `build_resources` sums `EffectivePart::build_resources`, already computed in the analysis). `StagePlan` binding then needs the delivered `ConstructionComplete` vessel rather than a spawn-tick fingerprint — `StagePlan::vessel_for`'s `(min_id, fingerprint)` machinery already tolerates late materialization. If the instant launch is kept as the v1 scope cut, at minimum debit funds + site inventory in `apply_spawn_physical` to make the cost real — the gate is already there.

---

### [HIGH] Finding 4: `RecoverVessel` is unreachable — the verb the UI's own refusal text names does not exist

**File:** `crates/csp-view/src/lib.rs:2411–2418` (refusal text), `crates/csp-sim/src/economy.rs:859+` (`apply_recover_vessel`), `crates/csp-sim/src/contracts.rs:234,979–981` (`VesselRecovered` trigger)

**Problem:** The pad-occupancy refusal tells the player to "recover or move the parked vessel first" — but **no path issues `RecoverVessel`**: not in `program_command`, not a key, not in `UiOutbox`'s vocabulary. The only removal paths for a `SurfaceLatched` vessel are unlatch-and-fly-away (works) or destruction. Consequences:
- `contract.rescue` can never complete — its predicate requires the `VesselRecovered` trigger (`contracts.rs:980`).
- The seeded starter craft (`seed_starter_craft` → `demo::seed_pad_craft` → `launch_craft`, lib.rs:1277–1279) parks a vessel on the pad at boot; the *second* launch is refused until the player flies it away — a new player who opens the VAB first hits an occupied pad and a message naming a verb they can't perform.
- The economy's recovery edge (resource credit at `recovery_efficiency`, part-cost payout, crew return — `economy.rs:859+`) is dead, so funds recovery credit never happens.

**Impact:** §3.5's "flight recovery ─► funds" edge and the `contract.rescue` template are dead; pad occupancy becomes a confusing trap on first run.

**Suggestion:** Add a "Recover" action for `SurfaceLatched` vessels — the Program mirror already stages `input.latched` (lib.rs:2562) and the sites table is installed (lib.rs:1173–1181); a `ProgramCommand::RecoverVessel { site }` → `SimCommand::RecoverVessel` is a few lines of the existing pattern. Also soften the occupancy text until the verb exists ("move the parked vessel" is the only honest reading today).

---

### [HIGH] Finding 5: The entire colony + logistics + crew layer is founded, simmed, and invisible — 19 commands with no path

**File:** `crates/csp-view/src/lib.rs:1192–1219` (env/recipes/`found_authored_colonies` wiring), `lib.rs:2485–2543` (`stage_program` — no colony/crew section), `crates/csp-ui/src/program.rs:124–154` (`ProgramInput` has no colony/crew fields)

**Problem:** `from_source` wires the whole RTS layer correctly — colony environments, recipe table, and the authored Rask colony founded at boot via `sim.found_authored_colonies` (lib.rs:1218; `tests/startup_colonies.rs` proves it). The 5168-line colony sim then runs **invisibly**: no `ProgramMirror`/`ProgramInput` section, no map marker (`map.rs`/`structures.rs` contain zero colony references), no panel, and none of the 17 colony/logistics/crew/lab/bench commands (`FoundColony`, `AbandonColony`, `AssignCrewToColony`, `EvacuateColony`, `DamageColonyModule`, `ScheduleColonyInspection`, `JoinColonyInventory`, `LeaveColonyInventory`, `TransferColonyResource`, `TransferColonyCrew`, `TransferResource`, `TransferCrew`, `BoardCrew`, `HireCrew`, `StartResearch`, `StopResearch`, `RunBenchTest`) are reachable. `CrewSeat` exists on `csp.pod.mk1` but nobody can board; `SetLifeSupportEnabled` fires at startup (csp-app:562) into a subsystem with no crew to consume; labs can't start (and no `Lab` part exists in data anyway); `contract.colony_resupply` can never complete (needs `TransferComplete` into colony stores).

**Impact:** PRD §3.5.1/M8 and the crew model (§3.5 CrewMember) are fully implemented and fully unreachable — the "no space center gameplay" complaint's substance. Colony bookkeeping also costs event-queue budget the player can never see or manage.

**Suggestion:** A colonies/roster section in the Program window (mirror `sim.colonies()` state + the roster arena), plus vessel-side `JoinColonyInventory`/`TransferColonyResource` for latched-in-radius vessels, and hire/board rows on the roster — all expressible through the existing `program_command`/`outbox.program` pattern.

---

### [MED] Finding 6: 4 of 6 contract templates can never complete — and their deadlines make them actively hostile

**File:** `data/contracts/*` + predicate evidence at `crates/csp-sim/src/contracts.rs:935–1007` and triggers at `contracts.rs:223–238`

**Problem:** Consequence cascade of Findings 2/4/5: `contract.satellite` (needs `v.orbit()` — InBubble vessels have none) and `contract.part_test` (only `Orbit` regimes authored) are uncompletable without rails; `contract.rescue` needs `VesselRecovered`; `contract.colony_resupply` needs colony delivery. Only `contract.survey`/`contract.survey.data` (banked science) work. Worse, satellite/part_test/rescue carry `deadline_days` + `failure_penalty_funds` — the board **baits the player into guaranteed-penalty accepts** once prereqs are met (accept → can't ever complete → deadline fires → funds debited, `contracts.rs:1145–1182`).

**Suggestion:** As an interim honesty fix, filter generated offers to completable kinds; the real fix is the three missing verbs above.

---

### [MED] Finding 7: `save_seed` is never set — every new game draws identical offers and identical part failures

**File:** `crates/csp-view/src/lib.rs:1073–1241` (`from_source` — no `set_save_seed` call); `crates/csp-sim/src/lib.rs:2445` (default `save_seed: 0`), `lib.rs:5557–5562` (θ pre-draws keyed on it), `crates/csp-sim/src/contracts.rs:615–624,686` (offer generation keyed on it)

**Problem:** The setter exists, is documented ("call before the first `step_frame`"), and is used by `csp-bench` scenario/replay harnesses — but the game's startup path never calls it. Every shipped playthrough runs `save_seed = 0`: the contract offer board is identical run-to-run, and reliability θ pre-draws repeat exactly (every "Stock Hopper" Sparc draws the same failure schedule). PLAN §2.7's anti-save-scum design ("a reload re-flies the same failures") presumes a per-save seed; a *new* game was supposed to differ.

**Impact:** Zero run-to-run variety in the two player-visible random systems; a determined player learns exactly when each part fails. Determinism itself is unaffected (still fully deterministic — just always the same stream).

**Suggestion:** `sim.set_save_seed(<entropy>)` in `from_source` (native; a fixed constant for the §6.3 demo to keep its scenario reproducible), and surface the seed in the save/bug-report for repro.

---

### [MED] Finding 8: `physics.dominant` is fixed "terrin" at construction — off-Terrin vessels can never be materialized, maneuvered, or recovered

**File:** `crates/csp-view/src/lib.rs:1983` (`to_bubble_eligible` requires `orbit.parent == dominant`), `crates/csp-sim/src/lib.rs:5925–5927` (`to-bubble` same gate), `lib.rs:6021` (node start same), `crates/csp-sim/src/bubble.rs:397–406` (`PhysicsConfig.dominant`, no setter), `crates/csp-view/src/lib.rs:1099–1114` (environment built `for_body(terrin)` once)

**Problem:** The single bubble environment is anchored to Terrin forever. A vessel reaching Rask/Duva SOI on rails (already unreachable player-side, reachable agent-side) can never `ToBubble`, never host a maneuver burn (node-start gates on dominant parent too), never be recovered, never land. `recover_control`'s eligibility honestly mirrors the sim's own restriction — the gate is structural, not a view bug: nothing re-homes the dominant body. Yet contracts name `rask`/`duva` as mission targets (`contract.rescue` bodies, `contract.colony_resupply` bodies, `contract.survey.data` bodies), and Duva/Rask carry authored biome maps and a colony.

**Impact:** Interplanetary play is a one-way dead end — M5's "third body" content is decorative even for agents.

**Suggestion:** Dominant-body hand-off (re-home the environment when the focused vessel's SOI changes) is the real fix and sits deeper than glue; at minimum, document the single-body scope and gate contract bodies to `terrin` until it lands.

---

### [MED] Finding 9: Alarms, transfer-watch, and vessel tracking have no path — the encounter-safety layer is inert

**File:** `crates/csp-view/src/lib.rs` (no issuance), `crates/csp-sim/src/events.rs:449–490` (`PlayerAlarm`/`TransferWindow` always `Clamping`; `SoiTransition` `Clamping` only for tracked vessels)

**Problem:** `SetAlarm`, `WatchTransferWindow`, `SetVesselTracked` are unreachable. The KAC-equivalent (PRD §4) is absent as UI; the Lambert/porkchop planner (`csp-sim/src/transfer.rs`) can only be exercised by agents; and because nothing can set `tracked`, every vessel's `SoiTransition` stays `Silent` — **warp never demotes for an SOI transition**, so a warping player overshoots moon encounters and contract-relevant transitions silently (§2.6.5's intended `Clamping` behavior for "focused/tracked vessel SoiTransition" is unreachable).

**Suggestion:** Map-side alarm/transfer-watch panel; auto-`SetVesselTracked` the focused vessel (or issue it at launch) so SOI transitions clamp warp as designed.

---

### [MED] Finding 10: Script engine + autopilot are installed and unreachable — the "automation is first-class" pillar is invisible

**File:** `crates/csp-view/src/lib.rs:1124–1126` (`set_script_engine(RhaiHost)`, `set_autopilot_driver(AscentAutopilot)`)

**Problem:** The hosts are correctly wired (PRD §2.10/2.11) but `LoadScript`/`UnloadScript`/`SetScriptEnabled`/`EngageAutopilot`/`DisengageAutopilot` have no outbox field, no panel, no key. The ascent autopilot is bench/MCP-only. PLAN pillar 4 ("scripting API from day one") ships zero player surface — no console, no editor, no engage verb.

**Suggestion:** A minimal script console (source text + load/enable per focused vessel) and an autopilot engage control on the flight HUD — the leases/commands already exist.

---

### [MED] Finding 11: No player-facing save/load, and §3.4's revert-to-launch is absent

**File:** `crates/csp-view/src/lib.rs:98–104` (`save` re-export exists for `csp-app`'s bug-report bundle only), `csp-app/src/lib.rs:~2490` (snapshot used solely for crash bundles), PLAN:1180–1184 ("revert-to-launch **exists** … one of the top-three QoL features")

**Problem:** `WorldSnapshot`/codec/`restore` are implemented and tested, but `View` exposes no `save_game`/`load_game` plumbing and no UI/key issues either. A pre-launch snapshot for revert is never taken anywhere. §7's save/load and §3.4's revert are both dead ends at the glue layer — the machinery is all downstream.

**Suggestion:** `View::{save_game, load_game}` methods wrapping snapshot/codec, outbox/menu verbs for them, and a snapshot captured in `launch_craft` (pre-enqueue) to back a "revert" verb that restores it.

---

### [MED] Finding 12: No KSC / space-center mode exists — `ViewMode` is `Flight|Map|Vab` and facilities are inert

**File:** `crates/csp-view/src/lib.rs:84–93` (`ViewMode`), `crates/csp-ui/src/vab.rs:140–146` (`ModeSelect` — same three), `crates/csp-view/src/structures.rs` (pad/gantry/VAB are render-only)

**Problem:** The tech tree unlocks `facility.lab`/`facility.test_stand` with no effect; the space center exists only as geometry the camera flies over. This is where manufacture orders, bench tests (`RunBenchTest`), crew hire, colony founding, and the launch flow would naturally live. Confirmed **missing** (matching the exploration map); flagged here as the seam a KSC surface would occupy: a fourth `ViewMode`, a `build_ui` panel branch, and a `KscMirror` staged like `ProgramMirror`.

**Suggestion:** Even without a new mode, a "Facilities" tab in the Program window could host `ManufactureCraft` orders and `RunBenchTest` — both already resolve against the installed sites table.

---

### [LOW] Finding 13: Difficulty flags are a one-shot at startup; the settings text promises "new games" that can't be started

**File:** `crates/csp-app/src/lib.rs:486` (`apply_startup_difficulty` in `assemble`, run once), `crates/csp-ui/src/settings.rs:787` ("applies to new games")

**Problem:** `life_support_enabled`/`dormant_stress` are written before the first `step_frame` (honestly documented at lib.rs:549–559), but there is no new-game verb — editing them mid-session silently does nothing, and the only "new game" is restarting the process. `life_support_enabled: true` also enables an inert subsystem (no crew can board). Cosmetic honesty issue, not a functional bug.

---

### [LOW] Finding 14: `UiOutbox::warp_step` carries a copy-pasted doc paragraph

**File:** `crates/csp-view/src/lib.rs:305–310`

**Problem:** `warp_step`'s doc comment opens with "The VAB's LAUNCH button — `csp-app` calls `View::launch_craft` on the next Input phase." — the `launch` field's doc text, misplaced above the warp field. Cosmetic; flags that the struct grew without a doc audit.

---

## Explicitly checked and found clean

- **Install wiring order (`from_source`):** `set_parts` → `grant_free_tech_roots` → `set_economy_config` → `set_sites` → `set_contracts`/`set_contract_config` → `set_colony_env` → `set_life_support_config` → `set_recipes` → `found_authored_colonies` — the founding apply resolves all three tables it needs, in order (lib.rs:1149–1219). `set_docking_config`/`set_transfer_config`/`set_reliability_config`/`set_science_config`/`set_experiments`/`set_tech_tree`/`set_vessel_caps`/`set_script_op_budget`/`set_script_engine`/`set_autopilot_driver` all present. Verified against the complete `pub fn set_*` surface — the *only* missing setter is `set_save_seed` (Finding 7).
- **`UiOutbox` staleness:** every field is rewritten unconditionally each `build_ui` pass *or* the resource is `mem::take`n by `take_ui_outbox` (lib.rs:1770–1772) — no stale-carryover path (verified field-by-field, including `outbox.program` inside the panel-open gate).
- **`StagePlan` binding:** `(min_id, fingerprint)` lazy binding handles late mints, removed-vessel id gaps (min_id can only be *low*, fingerprint+ambiguity absorb it), and same-craft double launches (P05-F1 ambiguity retires honestly rather than firing at a random vessel — `flight.rs:696–721`, `lib.rs:2289–2304`).
- **`fire_stage` refusal completeness:** bubble/latched/rails/gone-vessel/ambiguous states each produce a distinct `FlightSfx` cue; drop-set pop waits for `InBubble` so a firing is never eaten by guaranteed rejects (lib.rs:2311–2349). Documented honest limitation: jettisoned mass leaves the sim entirely — no debris vessel (KSP-style debris spawn is unimplemented and the comment says so).
- **`launch_preflight` mirrors `apply_spawn_physical`'s refusal set faithfully** — vessel cap, funds, occupancy — and the drain re-checks everything (a refusal-only early gate, never an admission). The occupancy predicate re-materializes latched hulls at the current spin phase — correct single-berth semantics (sim lib.rs:5361–5409).
- **`sim_step`'s `ToBubble`-reject pin release** (lib.rs:1496–1509) matches on typed `RejectKind::ToBubble` + player channel, not reason text — the SOI-TOCTOU class it names is real and the release is correctly gated on `pending()`.
- **`recover_control` refusal honesty:** an ineligible *focused* vessel raises "not recoverable" rather than silently redirecting; no-eligible raises "no vessel" (lib.rs:1923–1942). `to_bubble_eligible` mirrors the apply's fallible halves (`state_at` + weld) — dead code in practice (Finding 2) but correct.
- **`program_command` context resolution:** `DataKey` row indices, biome/situation, and vessel binding all resolve against the *same* mirror frame the panel drew — a stale click drops rather than misfires (correct rule; Finding 1 is the wrong-content case it also swallows).
- **Tutorial (`tutorial.rs`):** all seven checklist steps are reachable through the shipped verb surface; `osculating_apsides` derives apsides from the *bubble* state so "Close the orbit" doesn't depend on rails (the one thing that works without Finding 2's fix); debris/`LiveVesselCount` rewind semantics correct (P12-F1/F2 covered by tests).
- **Pointer-mask discipline:** `program_panel`, settings, crash dialog, tutorial, help, top bar, and VAB editor rects all feed `ui_masks` before `vab_pointer_input` runs last (lib.rs:436–601); the debug scaffold is excluded from VAB by the `mode != Vab` gate — consistent, and the enumeration test pins it.
- **`PLAYER_SOURCE_ID` FIFO sharing** between `launch_craft` and throttle dispatch is deliberate and documented (lib.rs:72–78); source ids 0/2/3/4 are distinct producers (csp-app:72–83).
- **Save-refusal discipline on `EguiPass::drop`** clears `TexturesDelta` — the `#[must_use]` trap is handled (lib.rs:290–299).

## Test-coverage note

`crates/csp-view/tests/*` covers pad spawn, stage plans, startup colonies, tutorial, focus, render gates — all mechanics-level. **No test exercises a player-shaped end-to-end flow through the UI surface** (launch → orbit → science → contract completion); every test enqueues `SimCommand`s directly, which is exactly why the reachability gaps are invisible to the suite. A `program_command`-level test feeding a space-situation mirror would have caught Finding 1; a "click LAUNCH and assert funds decreased" test would have caught Finding 3.
