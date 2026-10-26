# Exploration Map — session-prd1 (PRD-informed review, ~22 clusters)

## 1. Project overview

**Clanker Space Program** — a KSP-style build/fly sim with an RTS progression layer, per `PLAN.md` (R3, 1972 lines, the authoritative spec). Rust workspace, edition 2024, `bevy_ecs`-standalone + wgpu 30 + egui 0.37; deterministic headless sim (64 Hz integer ticks, `libm`-only, BLAKE3 state hash, ChaCha8 KDF RNG, golden replays).

Crate layout (all exist — no `(new)` stubs remain):

 Crate | Role |
---|---|
 `csp-core` | SimTime, DVec3/DQuat, StateHasher, RNG KDF, error classes (~1.7k LOC) |
 `csp-data` | RON schemas, loaders, validators, data-set hash (~12.5k incl. 6.3k validate.rs) |
 `csp-phys` | Weld, EffectivePart resolution, SimpleBackend (semi-implicit Euler + PGS contact), aero/thermal/structural, CraftSession (~19k) |
 `csp-sim` | Authoritative world — `lib.rs` is **22,489 lines** (~10.9k code + ~11.6k inline tests) + 16 feature modules (~18k) |
 `csp-script` | Rhai host + `ControlApi` (frozen, M9c) (1.6k) |
 `csp-ai` | Ascent autopilot only (222 lines) |
 `csp-view` | `bevy_ecs` presentation world, schedules, render extract, session state, ProgramMirror (~11k) |
 `csp-render` | wgpu pipelines + WGSL (~3.9k) |
 `csp-ui` | Pure-egui panels: VAB session, HUD, navball, program window, settings, tutorial (~8.1k) |
 `csp-app` | winit shell, input dispatch, warp controller, audio, settings IO, bugreport (~6.5k) |
 `csp-server` | Native-only axum HTTP/WS + MCP (~5.9k + 1.3k e2e) |
 `csp-bench` | Replay/perf/eval/scenario + 4 bins (~7.8k) |
 `xtask` | lint-deps, lint-attribution, bake_art, gen_biome_maps (~2.9k) |
 `apps/telemetry` | TanStack/Vite dashboard SPA (~1.1k TS) |
 `apps/demo` | wasm web-demo data set + index.html |
 `data/` | Shipping RON content + `test-fixtures/` goldens |
 `docs/perf/` | perf log only — no `docs/quarterly/` (§6.5 ritual never exercised) |

Prior review sessions exist under `.review/` (session-deep1, session-play1, session-ruthless-20261025); this map supersedes their clusters. **This session's question is different: does the shipped product match the PRD?**

## 2. The PRD feature list (extracted from PLAN.md) vs. observed state

Legend: **[WIRED]** implemented + player-reachable · **[HIDDEN]** implemented in sim/data but no player path · **[PARTIAL]** stub/reduced vs spec · **[MISSING]** absent.

 PRD feature (PLAN §) | Status | Evidence |
---|---|---|
 Deterministic core: 64 Hz tick, CommandKey ordering, control lease, event heap Clamping/Silent, interval-closed-form subsystems, warp ladder, surface latch, state hash, RNG KDF (§2.2–2.7) | **[WIRED]** | `csp-sim/src/lib.rs` — full machinery present and exercised by golden replays |
 Orbit layer: patched conics, universal solver, SOI solving, maneuver nodes + executor, transfer planner + window events (§2.8, M5) | **[WIRED]** except `WatchTransferWindow` has no UI (map has maneuver panel only) | `orbit.rs`, `transfer.rs`, `maneuver.rs` |
 Weld Stage-1 + Stage-2 aero tables, thermal, structural breakup (§2.4, M2/M4) | **[WIRED]** — but `InstallAeroTables` (worker path) is never issued; sim computes tables itself (fallback per `lib.rs:5529` etc.) | `csp-phys/{weld,aero,thermal,structural,simple}.rs` |
 Joints/docking: soft-capture → merge → undock (§2.4, M6) | **[PARTIAL]** — sim machinery complete (SimpleBackend PGS island, not Rapier — documented deviation in `joints.rs:14–18`); **unusable**: no `DockPort` part exists in `data/parts/` | `joints.rs`, `csp-phys/merge.rs` |
 Science: experiments → data inventory → transmit/recover → labs → science (§3.2, M7) | **[PARTIAL]** — run/transmit/recover wired via Program window; recovery requires `SurfaceLatched` and there is no parachute/gear → effectively transmit-only on Terrin; `StartResearch`/`StopResearch`/labs have **no UI and no Lab part**; `researchers` is a stub `Vec<ResearcherAssign>`, not crew | `research.rs`, `csp-ui/program.rs`, `data/parts/csp.science.canister.ron` (only instrument; one-shot) |
 Tech tree + upgrade tracks frozen at build (§3.3) | **[WIRED]** — Program window buys nodes/tracks; but content is 4 nodes gating only 2 parts (tank.s320, engine.ember) | `tech.rs`, `data/techtree/home.ron` |
 Economy: funds, salaries, recovery credit, insolvency (§3.5) | **[PARTIAL]** — ledger exists; `HireCrew`/`RecoverVessel`/`ContractReward` unreachable; launches cost nothing (see below); `SalaryDue` exists but nobody can be hired | `economy.rs` |
 Contracts: seeded offer board, fact prereqs, event predicates, deadlines (§3.6) | **[WIRED]** — board + accept via Program window; 6 templates in `data/contracts/` | `contracts.rs`, `csp-ui/program.rs` |
 Crew: hire/board/transfer/roster, seats (§3.5, M6-D) | **[HIDDEN]** — `CrewMember` arena + commands exist; zero UI; pod has `CrewSeat` but no one can board | `economy.rs` roster, `transfers.rs` |
 Colonies: flow network, power day/night, drills/converters, states, inspections (§3.5.1, M8) | **[HIDDEN]** — 5,168-line implementation + one authored Rask colony in `data/systems/home/colonies.ron`; **no colony UI anywhere** — player cannot see state, found, supply, or crew one | `colonies.rs` |
 Manufacture: build orders consume site inventory + time (§3.5, M8) | **[PARTIAL]** — sim complete; **launch path bypasses it**: `launch_craft` → `SpawnPhysicalVessel` (free, instant) at `csp-view/src/lib.rs:2210–2220` | `manufacture.rs` |
 Logistics: `ResourceTransfer` process, proximity transfer, colony inventory join (§3.5, M6-D/M8) | **[HIDDEN]** — process implemented, no UI | `transfers.rs` |
 Life support (§3.5, M8) | **[PARTIAL]** — implemented; `SetLifeSupportEnabled` issued at startup by app (`csp-app/src/lib.rs:565`); inert because no crew can board | `life_support.rs` |
 Reliability: hazard Λ, pre-drawn θ, failures, dormant hazard, qualification, bench test, mission confidence, revert (§3.4, M9b) | **[PARTIAL]** — failures fire and display in Program window; `RunBenchTest` unreachable; **no revert-to-launch anywhere** | `reliability.rs`, `csp-ui/program.rs` |
 Space center / facilities | **[MISSING as gameplay]** — tech unlocks `facility.lab`/`facility.test_stand` that have no UI or effect; `structures.rs` renders pad buildings cosmetically only | `csp-view/src/structures.rs` |
 Editor/VAB: snap graph, staging, scale+variants, Δv readout, craft files, GPU picking (M3) | **[WIRED]** — rich session model; **user-reported bugs**: can't move the rocket, removing a part "glues the stack to the ground"; uniform-only scaling complaint from play1 | `csp-ui/vab.rs` (4418), `csp-view/vab.rs`, `csp-phys/session.rs` |
 Flight controls + HUD + navball (M2/M10) | **[WIRED]** — throttle axis, attitude verbs, staging, navball, full metric readouts (TWR/Δv/mass-flow/burn time now present at `csp-ui/lib.rs:530–545`) | `csp-app`, `csp-ui` |
 Map view + alarms (M1/M5, KAC-equivalent §4) | **[WIRED map]** / **[MISSING alarms UI]** — `SetAlarm` exists but is unreachable | `csp-view/map.rs` |
 Scripting: Rhai `on_tick`, wakes, persist, budgets (§2.10, M9a) | **[HIDDEN]** — `RhaiHost` installed (`csp-view/lib.rs:1125`); no script editor/console → unreachable except MCP/bench | `csp-script`, `csp-sim/scripts.rs` |
 Autopilots: ascent + landing/rendezvous/transfer optimizer + design assistant (§2.11, M9a) | **[PARTIAL]** — ascent-only `AscentAutopilot`, installed (`csp-view/lib.rs:1126`) but `EngageAutopilot` unreachable via UI; no transfer/design assistant, no `LlmAdapter` | `csp-ai/lib.rs` (222 lines) |
 Multi-vessel in one bubble | **[PARTIAL]** — mechanics exist (bubble = 20 km around active; auto bubble→rails at boundary); practically reachable only via multiple launches; no rendezvous aids | `lib.rs` residence machinery |
 Server/MCP/telemetry (§2.12, M9c/d) | **[WIRED for agents]** — MCP tools incl. generic `sim.command` (mcp.rs:312) that exposes the *entire* unreachable command surface to agents; telemetry SPA exists | `csp-server`, `apps/telemetry` |
 Save/load game (§7) | **[HIDDEN]** — codec + snapshot/restore fully implemented and tested; player-facing save/load **absent** (bug-report bundles + MCP `session.save`/`session.load` only) | `csp-sim/save.rs`, `csp-app/lib.rs:2490+` |
 Replays/bench/eval (§7.6, §8.5) | **[WIRED]** | `csp-bench` |
 Rendering tiers (§5) | **[PARTIAL]** — Baseline tier only; Advanced tier (bindless/indirect/Forward+/TAA) not started — acceptable, it is §6.4 cut-list item 1 | `csp-render` |
 Terrain/atmosphere/ocean (R2) | **[WIRED]** — quadtree, heightfield, LUT scattering, ocean, biome tint | `csp-render/terrain*`, `csp-view/terrain.rs` |
 Presentation floor (§3.7/M4c) | **[WIRED]** — kitbash silhouettes, plume, plasma sheath, dust, sky, SFX cue channel + real audio backend | `csp-view/flight.rs`, `csp-app/audio.rs` |
 Audio (M10) | **[WIRED]** — synth bank + subprocess backend | `csp-app/{audio,synth}.rs` |
 Settings + rebindable keys (M10) | **[WIRED]** | `csp-ui/settings.rs`, `csp-app/settings_io.rs` |
 Tutorial (M10) | **[WIRED]** — first-flight checklist | `csp-ui/tutorial.rs`, `csp-view/tutorial.rs` |
 "Build planes/rovers/stations" (pillar 1) | **[MISSING]** — no wings/wheels/gear/ports; catalog is 11 rocket-stack parts | `data/parts/` |
 Third body / interplanetary (M5) | **[PARTIAL]** — Duva/Rask authored; `duva_reachable_in_sim` test exists; no player-visible transfer tools beyond maneuver nodes | `data/systems/home/` |
 Web demo (§6.3) | **[WIRED]** — reduced embedded data set, 60-part cap | `csp-view/demo.rs`, `apps/demo/` |
 Migrations (§7.7) | **[MISSING]** — correctly deferred (pre-1.0 policy: may invalidate) | — |
 N-body, CommNet, EVA, VT, LLM | **[MISSING]** — correctly post-v1 | — |

**Bottom line for reviewers:** the user's "half the game is missing" is accurate *at the player-facing layer*. The sim implements ~all of PLAN §3, but the shipped game exposes: VAB → free launch → fly/orbit → program window (science transmit, tech, contract accept) → map. Everything else — recovery, crew, colonies, logistics, labs, manufacture cost, scripting, autopilot, alarms, saves, revert, docking — is either unreachable or content-gated.

## 3. Known user complaints → where they land

- *"How do I do science"* → clusters **PRD-06** (sim) + **PRD-15** (Program window): the flow exists but recovery is effectively impossible (no parachute → no `SurfaceLatched` science recovery on Terrin; transmit-only), labs unreachable.
- *"VAB can't move the rocket, removing a part glues the stack to the ground"* → cluster **PRD-11** (`csp-ui/vab.rs` Grab/Remove/Drop state machine + `csp-phys/session.rs` attach graph) and **PRD-16** (view-side pointer routing).
- *"No space center gameplay"* → confirmed **MISSING**: no KSC mode exists (`ViewMode` has Vab/Map/Flight only, `csp-view/lib.rs:85`); clusters **PRD-16/PRD-15** cover where it would live.
- *"Half the game missing"* → the reachability table above is the evidence; clusters **PRD-06/07/08/18/19** hold the implemented-but-unreachable code.

## 4. Clusters

Line counts are working-tree counts where verified (marked `✓`) or carried from session-ruthless (marked `≈`).

### PRD-01 — Determinism primitives (csp-core) — 8 files
- `crates/csp-core/src/{lib ✓25, error ≈55, hash ≈781, quat ≈232, rng ≈319, terrain ≈?, time ≈93, vec3 ≈197}.rs`
- **What:** SimTime, math types, §7.1 StateHasher visitor, ChaCha8/BLAKE3 KDF.
- **Completeness:** WIRED. **Focus:** does the hash visitor cover every *new* progression field (crew roster, colony stores, contract board, script persist, joint refusal set)? KDF arity at every draw site.

### PRD-02 — csp-sim/lib.rs §A+B: command contract + World shell — 1 file, ~3.7k lines
- `crates/csp-sim/src/lib.rs` **L1–3655**: module decls, `Vessel`/`Lease`/`SourceClass`, `CommandKey`, **`SimCommand` (L515–1143 — the whole game-verb surface)**, `AdvanceReport`, `WorldSnapshot`, `WorldConfig`, `TickShadow`/`VesselsShadow`/`EventsShadow`, `World` struct + constructors + `set_*` config installers + `enqueue`/`telemetry`/`snapshot`/`restore` entry.
- **Completeness:** WIRED. **Focus:** the SimCommand doc comments double as the PRD's feature ledger — cross-check each variant's "M-tag" against reachability (§3 above); §7.2 snapshot field coverage; save-refusal rules (§7.4) in `snapshot()`/`check_serializable`.

### PRD-03 — csp-sim/lib.rs §C: the tick machine — 1 file, ~5.2k lines
- `crates/csp-sim/src/lib.rs` **L3656–8868**: `step_bubble_range` (fuel plan, contracts notify, debris, joints lifecycle hooks, breakup consequences), `apply_command` dispatch + **all `apply_*` handlers** (spawn/rails/bubble/maneuver/aero/jettison/undock/lease/transfers/science/economy/contract/colony/manufacture/script applies), `drain_boundary`/`advance_interval_subsystems`/`advance_orbits`, `fire_due_events`/`fire_event`/`apply_soi_transition`, `step_frame`, `state_hash`, `WorldTickIo`.
- **Completeness:** WIRED sim-side; many applies reachable only via MCP. **Focus:** which `advance_interval_subsystems` arms are real vs no-op-by-design (research is event-driven — documented honest); rollback discipline on rejected applies (the "queued silently" bug class); event-publish coverage per subsystem (§2.6.5 mandate).

### PRD-04 — csp-sim/lib.rs §D: restore/breakup/hash walk + the test suite — 1 file, ~13.6k lines
- `crates/csp-sim/src/lib.rs` **L8869–22489**: `restore_*` per-arena restores, `transition_latched`, `apply_breakup`, `hash_vessel`, then `mod tests` (**~11.6k lines**).
- **Completeness:** WIRED. **Focus:** snapshot↔hash↔restore three-way coverage (the §7.5 round-trip hazard); whether tests exercise *end-to-end player-shaped* flows (science loop, colony supply loop) or only API surfaces; missing coverage = missing gameplay confidence.

### PRD-05 — Orbit layer & transfers — 5 files
- `crates/csp-sim/src/{orbit ✓2536, transfer ✓2556, transfers ✓1687, maneuver ≈362, bubble ≈325}.rs`
- **What:** `PatchedConics` + universal solver + SOI scan/Brent; Lambert/porkchop transfer planner + `WatchTransferWindow`; maneuver node executor; resource/crew transfer *processes*; surface-pose math.
- **Completeness:** WIRED orbits/maneuver; **[HIDDEN]** transfer-watch UI absent, resource/crew transfer unreachable; **Focus:** does the transfer planner produce usable output for a player (or is it MCP-only)? SurfacePose ↔ rendered-terrain convention parity.

### PRD-06 — Progression I: science, tech, economy, contracts — 4 files
- `crates/csp-sim/src/{research ✓1300, tech ≈729, economy ✓1407, contracts ✓2133}.rs`
- **What:** §3.2 data ledger + two-regime lab closed form; tech nodes + upgrade tracks (frozen at build); funds ledger/salaries/hire/recovery/insolvency; seeded offer board + prereqs + event predicates.
- **Completeness:** sim-complete; reachable subset = tech buy, track buy, contract accept, run/transmit/recover. **[HIDDEN]** `StartResearch`/`StopResearch`, `HireCrew`, `BoardCrew`, `RecoverVessel`, `ContractReward`; `ResearcherAssign` is a non-crew stub (`lib.rs:844–846`). **Focus:** this is "how do I do science" — verify the *whole* loop end-to-end: is any reachable path capable of producing a `SurfaceLatched` science vessel on Terrin? Does the offer board produce contracts a 3-part rocket can satisfy? Does anything charge funds for a launch?

### PRD-07 — RTS layer: colonies, manufacture, life support — 3 files
- `crates/csp-sim/src/{colonies ✓5168, manufacture ≈591, life_support ≈754}.rs`
- **What:** §3.5.1 flow network (allocation pass, TerminatorCrossing, states, inspections); build orders + site inventories + `ConstructionComplete`; optional LS consumption.
- **Completeness:** **[HIDDEN]** — authored Rask colony founded at boot (`csp-view/lib.rs:1218`) but no panel shows it; `ManufactureCraft` exists yet launch uses `SpawnPhysicalVessel` (free/instant — `csp-view/lib.rs:2210`). **Focus:** the colony sim runs invisibly — what breaks when it does (event budget, dormant bookkeeping)? Is the launch-bypass intentional scope or missing glue? LS is toggled ON at startup (`csp-app/lib.rs:565`) but no crew can board — dead code or trap?

### PRD-08 — Reliability, joints/docking, events, scripts host, save codec — 5 files
- `crates/csp-sim/src/{reliability ✓1834, joints ✓1248, events ≈719, scripts ≈737, save ✓754}.rs`
- **What:** hazard Λ/θ + qualification + failure modes; soft-capture lifecycle + latch merge/undock split (PGS island, not Rapier — documented deviation); event kinds/registry; `ScriptEngine`/`AutopilotDriver` traits + wake conditions; CSPSAVE codec.
- **Completeness:** sim-complete; **[PARTIAL]** `RunBenchTest` unreachable, docking needs a `DockPort` part that doesn't exist, save has no player path. **Focus:** dormant-failure notification path (does anything surface it to the player?), joint cap degradation honesty, save-refusal coverage vs §7.4 list.

### PRD-09 — csp-phys I: integration + contact + part resolution — 6 files
- `crates/csp-phys/src/{simple ✓4197, resolve ✓3611, part ≈440, backend ≈199, tensor ≈324, lib ≈145}.rs`
- **What:** `SimpleBackend` (semi-implicit Euler, PGS contact, settle latch), `EffectivePart = resolve(BuiltPart) ∘ wear`, `WeldPart`, `PhysicsBackend` trait.
- **Completeness:** WIRED; rapier3d-f64/parry3d-f64 are pinned in workspace `Cargo.toml:60–65` but likely unused — verify whether a `RapierBackend` exists at all (PLAN wants it for jointed islands; `joints.rs` documents the PGS deviation). **Focus:** 20 t-lander criterion, contact stability, resolver single-source-of-truth vs editor/flight/contract consumers.

### PRD-10 — csp-phys II: weld, aero, thermal, structural, session — 10 files
- `crates/csp-phys/src/{weld ≈1577, aero ≈2033, merge ≈865, thermal ≈831, structural ≈544, readout ≈706, graph ≈355, hull ≈329, profile ≈402, session ✓3204}.rs` + `crates/csp-phys/tests/{resolve_parts ≈267, session ≈244}.rs`
- **What:** Stage-1 weld, 66-direction voxel aero tables + ConfigMask, conduction/reentry heat, breakup thresholds, Δv/TWR readouts, MissionProfile, and **`CraftSession`** — the VAB's attach-graph model (attach edges, validation, staged ops).
- **Completeness:** WIRED. **Focus:** `CraftSession` is where the VAB bugs live (remove-part reparenting, ground-offset/"glued" stack, move/lift semantics); aero table determinism + `Cm_α` claim; breakup conservation tolerances.

### PRD-11 — VAB/editor UX — 5 files
- `crates/csp-ui/src/vab.rs` (✓4418 — `VabSession`, `VabAction` vocabulary: CatalogPick/Carried/Grab/Drop/Remove/Reroot/SetStage/Rescale, palette, inspector, stage column, craft files)
- `crates/csp-view/src/vab.rs` (✓1319 — VabEvent routing, pick→select, extract packet)
- `crates/csp-render/src/{vab.rs ≈428, vab.wgsl ≈88}` (kitbash pass + GPU object-id pick)
- `crates/csp-view/tests/vab_latency.rs` (≈165 — §5.4 16 ms kill criterion)
- **Completeness:** WIRED but user-reported broken. **Focus (HIGHEST):** the two complaints — (a) can't move the rocket: `Carried::Moving`/`Grab` path, root-move refusal ("root defines the build frame" — is there *any* way to reposition a placed part or the whole stack?), (b) removing a part "glues the stack to the ground": `Remove` reparenting + any post-remove ground-snap/offset that anchors the craft; attach-mode toggles (Axial/Side/End/Auto), symmetry, radial attach (play1 complaint "no radial/side attach" — check if since fixed), scaling (uniform-only? per-axis? tank length?), Δv readout honesty.

### PRD-12 — Renderer (wgpu + WGSL) — 8 files
- `crates/csp-render/src/{lib ≈349, terrain.rs ≈1553, terrain.wgsl ≈854, map.rs ≈159, map.wgsl ≈48, flight.rs ≈233, flight.wgsl ≈86}.rs` (+ `vab.*` counted in PRD-11)
- **What:** Baseline-tier pipelines: terrain quadtree + heightfield + transmittance-LUT sky + ocean + biome tint; map primitives; flight kitbash pass; reverse-Z.
- **Completeness:** **[PARTIAL]** — Baseline only; no Advanced tier, no shadows, no TAA (cut-list OK, but the "graphics garbage" complaint maps here). **Focus:** what's honestly missing vs PRD §5.1 Baseline column (CPU-culled instanced draws? fixed slot sets?), depth-compare conventions, shader correctness.

### PRD-13 — Scene presentation & extract — 6 files
- `crates/csp-view/src/{flight ✓3092, terrain ✓787, part_mesh ≈946, art ≈396, structures ✓867, demo ≈252}.rs`
- **What:** flight extract (kitbash parts, plume, plasma sheath, dust, sun dir, chase camera), terrain cache/packets, per-part mesh classifier, .csar art decode, **space-center structures** (pad/gantry/VAB building — *visual only*), web-demo embedding.
- **Completeness:** WIRED. **Focus:** terrain-vs-datum mismatch (rendered ground can sit above the sim's contact datum — the "buried pad" class); the structures' ENU/spin math vs sim `surface_pose_*`; whether the visible space center *implies* gameplay that doesn't exist.

### PRD-14 — Map view — 1 file
- `crates/csp-view/src/map.rs` (✓2472 — camera/focus/zoom-to-cursor, click-to-focus, orbit lines, apsides, node previews, pick lists, starfield)
- **Completeness:** WIRED; missing: transfer-window/alarm UI surface. **Focus:** focus-anchor lifecycle vs vessel leaving snapshot; `sample_orbit`/`state_at` presentation-tick rule (§2.5); encounter/maneuver preview completeness for M5.

### PRD-15 — egui panels: HUD, navball, program window, settings, tutorial — 5 files
- `crates/csp-ui/src/{lib.rs ✓1090 (flight HUD + notice overlay + debug + help), navball.rs ≈542, program.rs ✓505, settings.rs ✓1024, tutorial.rs ≈541}.rs`
- **What:** all player-facing chrome outside the VAB. `program.rs` is the *only* progression surface (funds/science/contracts/tech/experiments/reliability).
- **Completeness:** **[PARTIAL]** — program window emits only 6 `ProgramCommand`s; nothing for colonies/crew/labs/hire/alarms/saves/autopilot/scripts. **Focus:** the science UX the user couldn't find — discoverability of "run/transmit/recover"; what's *absent* by design vs oversight (no lab section, no crew section, no colony section, no save/load buttons, no alarm/transfer-watch UI); settings→difficulty honesty.

### PRD-16 — View shell, session state, progression mirror, launch path — 4 files
- `crates/csp-view/src/{lib.rs ✓3352, tutorial.rs ≈278, vab.rs (counted PRD-11)}.rs` + `crates/csp-view/tests/*` (9 files ≈2k — treat as one group)
- **What:** `View` façade, six §2.5 schedules, `UiOutbox`/`ProgramMirror` staging (`stage_program` L2485+), `launch_craft` (L2160+), `fire_stage`/StagePlan, `recover_control`/`ToBubble`, `found_authored_colonies`, `set_*` installer wiring, `seed_starter_craft`.
- **Completeness:** WIRED but thin — the glue layer where *most* unreachable commands would get their UI. **Focus (HIGHEST):** the launch bypass (`SpawnPhysicalVessel` vs `ManufactureCraft` — funds/resources/time never charged); `program_command` coverage vs `SimCommand` (the 17/40 reachability); what `stage_program` *doesn't* mirror (colonies, crew, labs, alarms); whether a KSC/program-expanding PRD feature would land here.

### PRD-17 — App shell: input, warp, audio, settings, bugreport — 8 files
- `crates/csp-app/src/{lib.rs ✓3684, audio.rs ✓839, warp.rs ✓639, settings_io.rs ≈708, synth.rs ≈383, bugreport.rs ≈237, logring.rs ≈77, main.rs ≈18}.rs`
- **What:** winit loop, key dispatch (20 `KeyAction`s), throttle/attitude held-axis integration, warp controller (§2.6.3), audio backend, settings/craft-file IO, bug-report bundle, wasm entry.
- **Completeness:** WIRED. **Focus:** missing verbs — no save/load/revert/pause/SAS/action-group/EVA keys exist (`KeyAction` enum `csp-ui/settings.rs:190–237` is the whole vocabulary); warp-controller demotion correctness; the one-frame-latency contract for every dispatched command.

### PRD-18 — Scripting + AI/automation — 3 files
- `crates/csp-script/src/lib.rs` (✓1570 — RhaiHost, `ControlApi` frozen surface: telemetry/steer/throttle/stage/nodes/transfer/alarm/waits) + `crates/csp-script/tests/api_freeze.rs` (≈169) + `crates/csp-ai/src/lib.rs` (✓222 — ascent autopilot only)
- **Completeness:** **[HIDDEN]** — engine installed but `LoadScript`/`EngageAutopilot` have no UI; §2.11 scope shortfall: no transfer optimizer, no design assistant/`MissionProfile` generator, no `LlmAdapter`. **Focus:** ControlApi vs §2.10's promised list (maneuver nodes ✓, action groups ✗, alarms ✓, resource transfer ✓, part actions ~); wake-condition coverage; whether the autopilot can even be engaged by a player (it cannot — MCP/bench only).

### PRD-19 — csp-server: HTTP/WS/MCP — 10 files
- `crates/csp-server/src/{mcp ≈1124, http ≈1059, backend ≈1029, dashboard ≈472, serve ≈289, ingress ≈273, telemetry ≈144, lib ≈45}.rs` + `crates/csp-server/{tests/e2e.rs ≈1293, PROTOCOL.md ≈93}`
- **What:** axum/tokio server: WS telemetry (subscription model), static SPA, Streamable-HTTP MCP (tools_v1, session-class marking), token auth, Origin allowlist, bounded ingress.
- **Completeness:** WIRED (native). Notably the **`sim.command` generic tool (mcp.rs:312) reaches every hidden command** — the sim is fully playable by agents, not by players. **Focus:** §2.12.x security contract; rate-limit/lease enforcement; tool surface vs §2.12.1's promised list (`editor.*`, `camera.*`, `craft.load` absent?).

### PRD-20 — Telemetry SPA — ~15 files
- `apps/telemetry/src/**` (routes, ws.ts, dashboard.ts, widgets: orbit-map/gauge/graph/table/registry, card.tsx, styles, vite/package configs ≈1.1k)
- **What:** TanStack dashboard widgets over the WS protocol.
- **Completeness:** WIRED for its (small) scope. **Focus:** subscription field paths vs server wire protocol; whether it surfaces program data (colonies/contracts/crew) the game UI lacks — could it be an accidental better UI?

### PRD-21 — csp-bench: replay/perf/eval/scenarios — 15 files
- `crates/csp-bench/src/{replay ≈1628, perf ≈1112, scenarios ≈1020, scenario ≈720, agent ≈607, economy ≈478, contracts ≈308, eval ≈237, lib ≈37, build.rs ≈206, bin/{csp-mcp ≈149, csp-eval ≈133, csp-replay ≈112, csp-perf ≈102}}.rs` + `tests/eval_suite.rs` (≈523)
- **What:** golden replay verify/record, perf budgets, scenario runner + lockstep agent loop, economy-harness bands, csp-mcp stdio bin.
- **Completeness:** WIRED. **Focus:** do scenarios cover the *player-shaped* loop (science loop, colony resupply, contract lifecycle) or only mechanics? The eval suite is where "the whole game" is most honestly testable — what's asserted vs §8.5.

### PRD-22 — Data domain: schemas + loaders — 10 files
- `crates/csp-data/src/{lib ≈120, load ≈1030, ids ≈144, datahash ≈229, part ≈874, resource ≈79, experiment ≈74, recipe ≈58, balance ≈249, loc ≈28}.rs`
- **What:** RON schemas (`PartDef`/`ModuleDef` incl. Lab/Drill/Converter/DockPort variants that have **no data instances**), loader/collected-errors, data-set hash.
- **Completeness:** WIRED; schema supports modules that content never ships. **Focus:** which `ModuleDef` variants are exercised by any `data/parts/*.ron` (Experiment/Engine/Tank/CrewSeat only) vs dead-schema.

### PRD-23 — Data validation + shipped content — ~9 files
- `crates/csp-data/src/validate.rs` (✓6281) + `crates/csp-data/tests/*` (8 files ≈1.3k) + `data/` tree as content (parts×11, techtree×1, contracts×6, experiments×1, recipes×3, systems/home×(solen/terrin/rask/duva + sites + colonies), balance.ron, resources.ron, loc/en.ron)
- **Completeness:** **[PARTIAL content]** — the content is a *skeleton*: 11 parts, 1 experiment, 4-node tech tree, 1 authored colony, 4 bodies. **Focus:** validator coverage vs §3.0/§3.1 rules; whether content is sufficient to *demonstrate* the PRD loop (it isn't — no parachute means no recovery path exists in content even if UI shipped tomorrow); `apps/demo/data` drift vs `data/`.

### PRD-24 — Tooling, CI, workspace config — ~7 files
- `xtask/src/{main ≈393, lint_deps ≈942, lint_attr ≈319, bin/{bake_art ≈900, gen_biome_maps ≈312, parts/mod.rs}}`, `Cargo.toml` (workspace pins/lints), `deny.toml`, `rust-toolchain.toml`, `.github/workflows/ci.yml`, `data/ATTRIBUTION.md`, `docs/perf/perf-log-devbox-release.ron`
- **Completeness:** WIRED. **Focus:** §8.2 gate coverage vs what CI actually runs; bless-goldens; whether lint-deps' allow/deny list matches current crate set (csp-ui exists now — was it added?).

## 5. Cluster count & dispatch guidance

**Total: 24 clusters** (within max-15-files each; `csp-sim/src/lib.rs` split into 4 line-range sub-clusters PRD-02/03/04 plus feature modules).

Suggested order (highest PRD-gap signal first):
1. **PRD-16** — the view-shell glue where reachability dies (launch bypass, program mirror coverage).
2. **PRD-11** — VAB, the user's #2 complaint.
3. **PRD-06 + PRD-07** — the "how do I do science" and "no space center gameplay" heartland: verify which sim systems have *any* completion path.
4. **PRD-15** — the panel layer where missing surfaces would live.
5. **PRD-03/02** — command contract vs apply honesty.
6. **PRD-18** — automation promised "first-class" but unreachable.
7. **PRD-23** — content sufficiency (no parachute = no science recovery, period).
8. Then mechanics clusters (PRD-05, 08, 09, 10, 12–14), interfaces (PRD-19/20/21), foundations (PRD-01/04/22/24).

## 6. Actions the parent must handle

- **Write this file** — I have no write/exec tools (Explore mode read-only): create `.review/session-prd1/exploration-map.md` with the content above.
- Line counts marked `≈` were carried from session-ruthless (2026-10-25) and may have drifted ±10%; `✓` counts are verified current.
- I did not run `cargo`, `git log`, or `wc` — recency/diff attribution of specific bugs is unverified; the completeness/reachability claims above are all verified by direct reads/greps today.