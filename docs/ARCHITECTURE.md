# Architecture — Clanker Space Program

Canonical architecture reference for this repository. Audience: a competent Rust dev with zero
domain knowledge. Every load-bearing claim carries a file anchor (path + type/function name).
Where the code diverges from `PLAN.md` (the design doc at the repo root), both sides are stated.

**The working tree is the truth.** `PLAN.md` is the spec; `.review/session-prd1/exploration-map.md`
is a *stale* review map (the tree has since gained the OnRails/recover path, parachutes,
manufacture-costed launches, the full progression panels, and player save/load/revert — all
documented below). If this file and the code disagree, fix this file.

Related docs (being written in parallel — links are intentional):
[SIMULATION.md](SIMULATION.md) · [GAMEPLAY-GUIDE.md](GAMEPLAY-GUIDE.md) ·
[UI-AND-RENDERING.md](UI-AND-RENDERING.md) · [DATA-AND-SCRIPTING.md](DATA-AND-SCRIPTING.md) ·
[BUILD-AND-DEV.md](BUILD-AND-DEV.md) · [AGENTS.md](../AGENTS.md)

---

## 1. What the game is

**Clanker Space Program** is a KSP-style vehicle build/fly space simulation with a persistent RTS
progression layer, in a fictional hand-authored solar system at ~1/10 real scale. Rust workspace,
edition 2024, ~150k lines of Rust across 12 crates + `xtask`.

The player loop: design a rocket in the **VAB** (Vehicle Assembly Building — the editor) → place a
build order at the launch pad (funds + materials are charged; the craft is *delivered* after a
build time) → fly it with real physics inside the **physics bubble** (a 20 km sphere around the
focused craft where per-part forces, contacts, and aerodynamics are ticked at 64 Hz) → let vessels
go **on rails** (analytic Kepler propagation, no per-tick physics) when far or landed → run
experiments, transmit or recover science, buy tech-tree nodes, accept contracts → recover the
vessel for funds → repeat, with colonies, crew, reliability failures, timewarp up to ×100 000,
scripting (Rhai), an ascent autopilot, an MCP/HTTP agent interface, and a web telemetry dashboard
around it.

Five product pillars (PLAN.md header): build anything; fly it for real; a world that runs without
you; automation is first-class; modern rendering, honestly tiered.

### Glossary (first-use definitions)

| Term | Meaning |
|---|---|
| **tick** | One sim step. `TICK_HZ = 64`; the physics dt `1/64 s` is immutable forever. |
| **warp / timewarp** | Sim-rate multiplier ladder `[1, 2, 10, 100, 1 000, 10 000, 100 000]` (`csp-app/src/warp.rs → WARP_LADDER`). ×2 is "physics warp" (more ticks per second, same dt); ×10+ is "rails warp" (analytic jump, no bubble ticks). |
| **bubble** | The physics bubble: vessels within `bubble_radius_m` (20 km, `data/balance.ron`) of the focused vessel get per-tick rigid-body physics (`csp-sim/src/bubble.rs → VesselState::InBubble`). |
| **rails** | Analytic Keplerian propagation (`csp-sim/src/orbit.rs → PatchedConics`); a rails vessel's elements are epoch-anchored and evaluated on demand. |
| **surface latch** | A settled, landed vessel leaves the bubble and is stored as body-fixed `(lat, lon, alt, orientation)` — exactly reproducible, no integration drift (`VesselState::SurfaceLatched`). |
| **patched conics / SOI** | Two-body orbit segments; SOI = sphere of influence, the body's gravitational boundary. SOI transitions are solved events (`csp-sim/src/orbit.rs`, PLAN §2.6.6). |
| **weld** | The pure function that collapses a craft's part graph into one rigid body: mass, CoM, inertia, force sites, collider (`csp-phys/src/weld.rs → weld`). |
| **staging** | Jettisoning a drop set (decoupler) in flight order; the editor assigns parts to stages. |
| **golden replay** | A frozen data set + recorded command log + expected BLAKE3 state hash; CI re-runs and compares (`csp-bench/src/replay.rs`). |
| **command log** | `Vec<(CommandKey, SimCommand)>` — the complete, self-contained replay artifact (§7.6). |
| **lease** | Which producer holds a vessel's control authority: `Player | Script(ScriptId) | Agent(u32)` (`csp-sim/src/lib.rs → Lease`). |
| **craft file** | Serialized VAB working craft (`CSPCRAFT` magic + bincode; `csp-phys/src/session.rs → CRAFT_FILE_MAGIC`). |
| **CSPSAVE** | The versioned save container: `CSPSAVE\0` + JSON header + DEFLATE-over-CBOR body (`csp-sim/src/save.rs`). |

---

## 2. The four-world architecture

There are four distinct "worlds" with one direction of truth. Confusing them is the root cause of
most determinism bugs (PLAN §2.2 — implemented as specified):

```
          SimCommand (CommandKey-ordered, stamped at drain)
 UI/scripts/AI/MCP ───────────────────────────────────►  csp-sim World  (AUTHORITATIVE)
                                                          plain Rust structs, no ECS
          Arc-less: csp-view owns the sim directly             │  staged per frame
 Session state (csp-ui panels, camera,            csp-view World (bevy_ecs, PRESENTATION)
 keybinds, settings — never logged/hashed)                     │  per-pass packets
                                                               ▼
                                                    csp-render (wgpu, f32 camera-relative)
                                                    + egui pass painted by csp-app
```

1. **Authoritative sim world** — `csp-sim/src/lib.rs → World` (the type is re-exported as
   `csp_view::SimWorld`). Plain owned data in `BTreeMap` arenas keyed by monotonic `u64` ids
   (`VesselId`, `PartInstanceId`, `ColonyId`, `ContractId`, `CrewId`, `ScriptId`, ...). No ECS, no
   wgpu, no winit, no `std::time`, no `rand` facade. **The only way state changes is
   `World::step_frame(target)` driven by stamped `SimCommand`s and scheduled events.**
2. **Presentation world** — `csp-view/src/lib.rs → View` owns *both* the sim (`View.sim`) and a
   `bevy_ecs::World` (`View.world`) of mirrors and session resources. Never authoritative;
   discarded freely. `bevy_ecs` (standalone, `default-features = false`) is confined to this crate,
   so replacing it later is cheap.
3. **Render world** — `csp-render` owns wgpu device/surface and per-pass renderers. It consumes
   plain-data packets (`MapPacket`, `VabPacket`, `TerrainPacket`, `FlightPacket` — see divergence
   D3) built by `csp-view`. f32, camera-relative; **the GPU never sees f64** (PLAN §2.3).
4. **UI** — `csp-ui` panels are pure egui functions over plain-data input/output structs. The
   entire egui pass runs *inside* `csp-view`'s `ui_build` schedule (`build_ui` system); `csp-app`
   only stages `egui::RawInput` in and paints the resulting `egui::FullOutput` out
   (`csp-view/src/lib.rs → EguiPass`, `View::set_egui_input`, `View::take_ui_frame`).

**Why:** determinism (a fixed-tick, command-driven sim with hand-written state hashing cannot live
under an ECS serializer or a multi-threaded executor's ordering), savegame stability, headless
reuse (tests, replays, `csp-bench`, `csp-mcp` all drive `csp-sim` directly with no renderer), and
wasm cleanliness.

**State ownership categories** (PLAN §2.2 A/B/C, enforced by convention + the dependency lint):

- **A. Sim state** — everything in `csp_sim::World`; hashed (`state_hash`), serialized
  (`snapshot()`), mutated only by `step_frame`.
- **B. Presentation state** — bevy_ecs components/resources in `csp-view`, derived from staged sim
  reads each frame (`Clock`, `WarpMirror`, `FlightSnapshot`, `ProgramMirror`, ...).
- **C. Session state** — editor working craft (`csp_phys::CraftSession`), camera, map focus,
  keybinds, settings, dashboards. Lives in `csp-ui`/`csp-view` resources and config files. Never
  sim state, never in the command log, never hashed, never saved in the world snapshot.

---

## 3. Crate map

Workspace: `crates/*` + `xtask` (root `Cargo.toml`). Roughly 150k lines of Rust total. Dependency
edges are CI-enforced by `cargo run -p xtask -- lint-deps` (`xtask/src/lint_deps.rs`).

| Crate | Role | Key public types (anchor) | Depends on (workspace crates) | Size (LOC, src+tests) |
|---|---|---|---|---|
| **csp-core** | Determinism primitives: integer clock, f64 math, state hasher, RNG KDF, error taxonomy. Depends on nothing below. | `SimTime`, `TICK_HZ`, `DVec3`/`DQuat`, `StateHasher`, `HASH_SCHEMA_VERSION`, `StreamKey`/`StreamTag`/`derive_key`, `SimError`, `RejectKind` (`src/{time,vec3,quat,hash,rng,error}.rs`) | — | 1.9k |
| **csp-data** | RON schemas + loaders + validators + data-set hash. No sim logic. | `PartDef`/`ModuleDef`, `SystemDef`/`BodyDef`/`KeplerElements`, `TechTree`, `ContractTemplate`, `ExperimentDef`, `RecipeDef`, `SiteDef`, `ColonyDef`, `BalanceDef`, `load_system`/`load_parts`/…, `validate_dataset`, `data_set_hash`, `DATA_HASH_VERSION` (`src/{part,system,techtree,contract,experiment,recipe,site,colony,balance,load,validate,datahash}.rs`) | csp-core | 12.4k (validate.rs alone 6.6k) |
| **csp-phys** | Vessel physics: weld, contact + integration (`SimpleBackend`), aero tables, thermal, structural breakup, and the editor's craft session model. | `weld()` → `WeldedBody`, `SimpleBackend`, `PhysicsBackend`, `Environment`, `PartGraph`, `EffectivePart`/`resolve_part`, `CraftSession`, `analyze()` → `CraftAnalysis`, `build_aero_tables` (`src/{weld,simple,backend,aero,thermal,structural,resolve,session,readout,graph,merge,hull,tensor,part,profile}.rs`) | csp-core, csp-data | 21.0k |
| **csp-sim** | The authoritative headless sim. Everything gameplay: tick machine, commands, events, orbits, progression, colonies, reliability, save codec. | `World`, `SimCommand` (53 variants), `CommandKey`, `SourceClass`, `Lease`, `AdvanceReport`, `WorldSnapshot`, `step_frame`, `state_hash`, `snapshot`/`restore`, `SUBSYSTEM_ORDER`; feature modules `orbit, transfer, maneuver, bubble, joints, transfers, research, tech, economy, contracts, colonies, manufacture, life_support, reliability, scripts, events, save` (`src/lib.rs` + 17 modules) | csp-core, csp-data, csp-phys | 50.0k (`lib.rs` 24.5k: ~11.9k code + ~12.6k inline tests) |
| **csp-script** | The Rhai guest-language host + the frozen `ControlApi` control surface (kOS-equivalent). | `RhaiHost`, `ControlApi`/`IoControlApi`, `preprocess` (bare `wait` is a compile error), libm-shadowed guest math (`src/lib.rs`) | csp-sim, csp-core, csp-data, csp-phys | 2.0k (+ api_freeze test) |
| **csp-ai** | Classical automation: the ascent (gravity-turn) autopilot only. No LLM adapter yet. | `AscentAutopilot` (`src/lib.rs`) | csp-sim, csp-script, csp-core | 0.2k |
| **csp-view** | bevy_ecs presentation world, the six schedules, sim↔render bridge, VAB hosting, map/flight/terrain scenes, Program window staging. The **only** crate allowed to depend on both csp-sim and csp-render. | `View` (+ `View::new`/`new_demo`/`begin_frame`/`sim_step`/`end_frame`), `ViewMode`, `ProgramMirror`, `UiOutbox`, `EguiPass`, `launch_craft`, `recover_control`, `PLAYER_SOURCE_ID`; modules `flight, map, terrain, structures, vab, part_mesh, art, demo, tutorial` (`src/lib.rs`) | csp-core, csp-sim, csp-script, csp-ai, csp-data, csp-ui, csp-render, csp-phys | 21.0k |
| **csp-render** | wgpu renderer: device/surface lifecycle, terrain/sky/ocean, flight kitbash, map primitives, VAB pass + GPU picking. Must not depend on csp-sim. | `Gpu` (`begin_frame`/`submit`), `TerrainRenderer`, `FlightRenderer`, `MapRenderer`, `VabRenderer`, packet types, `PartLight`, `MSAA_SAMPLES = 4` (`src/{lib,terrain,flight,map,vab}.rs` + WGSL) | csp-core (math only) | 3.0k |
| **csp-ui** | Pure-egui panels + the settings/keymap model. Never sees bevy_ecs, csp-sim, wgpu, or winit. | `vab::VabSession`/`VabAction`, `program::*` (Program window), `settings::Settings`/`Keymap`/`KeyAction` (24 actions), `game::game_menu`, `navball`, `tutorial`, `flight HUD` (`src/lib.rs`) | csp-phys, csp-data, csp-core | 12.3k (vab.rs 4.9k, program.rs 2.9k) |
| **csp-app** | The binary: winit shell, input dispatch, warp controller, renderers, audio, settings IO, bug reports, wasm entry, `--serve`/`--bug-report` CLIs. **No direct csp-sim dependency** — sim types arrive via `csp-view` re-exports (`Cargo.toml` comment). | `run()`, `wasm_start()`, `App` (private), `WarpController`, `settings_io::*`, `AudioBackend`, `write_bug_report`, `logring` (`src/{lib,warp,audio,synth,settings_io,bugreport,logring,main}.rs`) | csp-core, csp-render, csp-view (+ csp-server on native; csp-ui dev-dep) | 7.9k |
| **csp-server** | Native-only axum+tokio server: Streamable-HTTP MCP, WS telemetry, static SPA hosting, auth. | `Server`/`ServerConfig`/`run_blocking`, `Mcp` (`PROTOCOL_REVISION = 2026-07-28`, `TOOL_SCHEMA_VERSION = csp.mcp.tools.v1`), `ControlBackend`, `GameHost` (attach to running game), `LocalGame` (headless), `IngressQueue`, `DashboardStore` (`src/{serve,http,mcp,backend,ingress,telemetry,dashboard}.rs`, `PROTOCOL.md`) | csp-core, csp-sim | 6.1k (+ e2e tests) |
| **csp-bench** | Native-only headless runner: golden replay record/verify, perf budget log, lockstep agent loop, declarative scenarios, economy/contract probes. Bins: `csp-replay`, `csp-perf`, `csp-eval`, `csp-mcp` (stdio MCP over `LocalGame`). | `replay::{record, verify_golden, ARTIFACT_FILES}`, `scenarios` registry, `perf::run_perf_log`, `agent`, `scenario` (`src/*`) | csp-core, csp-data, csp-sim, csp-script, csp-ai, csp-phys, csp-server | 7.9k |
| **xtask** | Repo tasks: `lint-deps`, `lint-attribution`, `lint-reach`, `bless-goldens`; bins `bake_art` (.csar art packs from CC0 sources) and `gen_biome_maps`. | `src/main.rs`, `src/{lint_deps,lint_attr,lint_reach}.rs` | csp-bench, serde_json, image, miniz_oxide | 4.2k |

Non-crate workspace members:

- **`data/`** — all shipped content, RON: 16 parts (`data/parts/*.ron` — tanks, engines, pod,
  decoupler, **parachute** `csp.chute.s125.ron`, **dock port** `csp.dockport.s125.ron`, lab, legs,
  science canister, probe core, battery, life-support tank, adapter, nosecone), the home system
  (`data/systems/home/`: Solen star + Terrin/Rask/Duva bodies, biomes PNGs, sites, one authored
  colony), `techtree/`, `contracts/` (8 templates), `eval-scenarios/`, `recipes/`, `balance.ron`,
  `resources.ron`, `loc/en.ron`, and `test-fixtures/` (frozen golden data sets). Licensed
  `LICENSE-GAME` (proprietary, source-available); engine crates are MIT OR Apache-2.0.
- **`apps/telemetry/`** — the dashboard SPA (Vite + TanStack Router/Start + Tailwind, versions
  pinned exactly in `package.json`; see divergence D5). Served by `csp --serve`.
- **`apps/demo/`** — the wasm web-demo page + reduced embedded data set (built by
  `csp-view/src/demo.rs`; ≤60 parts, no save IO).
- **`docs/perf/`** — the committed perf budget log (`csp-perf` output, the CI perf gate's baseline).

Dependency shape (who may see whom): `csp-core` ← everything; `csp-sim` never touches
bevy/wgpu/winit/egui/threads; `bevy_ecs` only in `csp-view`; `csp-render` never touches `csp-sim`;
`csp-server`/`csp-bench` never enter the wasm tree (positively asserted — `xtask/src/lint_deps.rs`,
CI wasm check excludes them); `rand` and `rayon` are banned workspace-wide (`deny.toml [bans]`).

---

## 4. Frame & data flow end-to-end

One frame, top to bottom, with the actual functions. Native entry: `csp-app/src/main.rs → main()`
→ `csp_app::run()` (wasm: `csp_app::wasm_start()`; same code below both).

### 4.1 Input → commands

```
winit WindowEvent (keyboard/mouse)
  └─ App (crates/csp-app/src/lib.rs, struct App, impl ApplicationHandler)
       ├─ settings_io::event_binding(event) → csp_ui::settings::KeyBinding → KeyAction
       │    (the rebindable keymap; rebind capture swallows the captured physical key)
       ├─ App::dispatch / dispatch_with_key (lib.rs) routes by ViewMode:
       │    · throttle verbs → App::throttle_dispatch + tick_throttle_ramp(dt_wall)
       │      — held-key RATE input is integrated against wall dt HERE (app-side),
       │        then becomes one absolute SetThrottle per frame on the player FIFO
       │        (PLAN §2.2.1: control commands must be absolute, never relative)
       │    · attitude verbs → App::attitude_press / tick_attitude → SetAttitude
       │    · Stage → View::fire_stage; RecoverControl → View::recover_control
       │    · QuickSave/QuickLoad/RevertToLaunch → App::request_session_op(SessionOp)
       └─ mouse → VAB GPU pick (encode object-id pass; readback via mpsc next frames)
```

Producer identity: every command carries `(SourceClass, source_id)`. The local player's
Ui-class channels are numbered constants: `WARP_SOURCE_ID = 0`, `csp_view::PLAYER_SOURCE_ID = 1`
(throttle + launch share one FIFO deliberately), `MANEUVER_SOURCE_ID = 2`,
`SETTINGS_SOURCE_ID = 3`, `PROGRESSION_SOURCE_ID = 4` (`csp-app/src/lib.rs` consts;
`csp-view/src/lib.rs → PLAYER_SOURCE_ID`). Scripts stamp `SourceClass::Script(ScriptId)`, the
autopilot `SourceClass::Autopilot`, external clients `SourceClass::External` with their
`ClientRegistry` index.

### 4.2 Warp decision (wall-side only)

`App::run_frame` (lib.rs) each frame:

- `WarpController::observe(last_tick_ms, dt_wall)` then `decide()` → maybe enqueue
  `SimCommand::SetWarp(level)` (auto-derate/promote per PLAN §2.6.3's inequality with 1.5×
  hysteresis over 30 frames — `csp-app/src/warp.rs`).
- `WarpController::target_tick(sim.clock().ticks(), dt_wall)` → integer target accumulated in
  u128 nanos (never float), then enqueue `SimCommand::AdvanceTo(target)` so **the replay log is
  self-contained** (the recorded `AdvanceTo`s re-derive every step target).
- `dt_wall` is clamped to `MAX_FRAME_DT = 0.1 s`. Wall clock exists *only* here and in the
  `step_frame` timing measurement — never in the sim.

Rails-warp refusal is a soft gate: every frame the app evaluates
`SimWorld::rails_warp_refusal()` (a `csp-sim` query: any InBubble vessel under thrust, in
unsettled contact, or in atmosphere) and demotes to rung 0 while it holds (`App::run_frame`;
`csp-app/src/warp.rs` module doc describes the one-frame-lag contract).

### 4.3 The frame body (§2.5 schedule order)

`View::begin_frame()` → **Input schedule**: `map::apply_map_input`, `vab::apply_vab_input`,
`flight::apply_flight_input` (schedules are built inline in the `View` constructor,
`csp-view/src/lib.rs` ~line 2066).
These consume resources the app staged (pointer events, key actions) and may enqueue commands.

`View::sim_step(target)` → **one** `SimWorld::step_frame(target)` call (see §5.2), then runs the
`sim_step` schedule (single-threaded executor). Returns the `AdvanceReport`. On
`SimError::Fatal`, `App::on_sim_fatal` halts the clock, marks the session read-only, and writes a
bug-report bundle (§8).

`View::end_frame()` — staging first, then schedules in order:

1. Stage sim reads into resources (schedules never read `sim` directly):
   `SyncInput`, `map::capture_sync_input(&sim)`, `flight::capture_sync_input(&sim, …)`,
   `stage_vab_tech()`, **`stage_program()`** → populates `ProgramMirror` (economy, contracts,
   tech, the focused vessel's science/reliability/crew/lab rows, recovery site, researchers —
   `csp-view/src/lib.rs → ProgramMirror`).
2. **Sync schedule** (single-threaded): `sync_mirrors`, `map::sync_map`, `flight::sync_flight` —
   write `Clock`, `WarpMirror`, `FrameStats`, flight snapshot mirrors.
3. **Presentation schedule**: `map::map_present`, `vab::vab_present`, `flight::flight_interp`,
   `flight::flight_camera`, `flight::flight_present`, `tutorial::tutorial_drive` —
   interpolation, camera solve, VFX cue emission (`FlightSfx`).
4. **UiBuild schedule**: `build_ui` (+ `map_pointer_input`) — the entire egui pass. Panels from
   `csp-ui` are called with plain-data mirrors; their outputs land in **`UiOutbox`**
   (`launch`, `program: Vec<SimCommand>`, `maneuver`, `craft_save/load`, `settings_apply`,
   `game_save/game_load/revert/new_game`, `warp_step`, `mode`) — consumed by the app at the *next*
   frame's Input phase (§2.5's accepted one-frame latency).
5. **RenderExtract schedule**: `map::extract_map`, `vab::extract_vab`, `flight::extract_flight`
   (plus terrain packets via `csp-view/src/terrain.rs`) → the per-pass packets
   (`csp_render::{map::MapPacket, vab::VabPacket, flight::FlightPacket, terrain::TerrainPacket}`).

### 4.4 Paint + audio

`App::paint` (lib.rs): `csp_render::Gpu::begin_frame()` (handles reconfigure/skip), then
`TerrainRenderer` → `FlightRenderer`/`VabRenderer` (per `ViewMode`) → `MapRenderer` render their
packets camera-relative f32, and `egui_wgpu::Renderer` paints `View::take_ui_frame()`'s
`FullOutput`. Audio (native): `AudioBackend::update` drains the `FlightSfx` cue queue into
synthesized one-shots and continuous voices (§7.3).

### 4.5 The launch path (a worked example of the whole flow)

`View::launch_craft` (`csp-view/src/lib.rs`) is the reference end-to-end verb:

1. Recompute the VAB analysis (`csp_ui::VabSession::recompute`, driving `csp_phys::readout::analyze`)
   — a stale cache would pair a fresh graph with another revision's resolved parts.
2. Build a `csp_sim::CraftDef` (graph + weld parts + part costs + reliability specs + part ids)
   and the aggregate bill of materials. Empty VAB session → `stock_flight_craft()` fallback.
3. Refuse pre-flight on: unresolved parts, tech-locked parts (`sim.tech().is_part_unlocked`),
   no system loaded, failed weld, occupied pad by a non-recoverable vessel
   (`launch_preflight`). A *parked, latched* previous build is auto-recovered
   (`SimCommand::RecoverVessel`) to clear the pad.
4. Capture `self.pre_launch = sim.snapshot()` — the revert-to-launch point (PLAN §3.4: revert is
   a retry, not a reroll; the pre-drawn reliability thresholds ride the restore).
5. Enqueue `SimCommand::ManufactureCraft { site, craft, build_resources, … }` — **launch costs
   funds + site inventory and delivers the craft after a build time** (`ConstructionComplete`
   event berths it surface-latched on the pad; `csp-sim/src/manufacture.rs`). The stage plan binds
   to the future vessel via `flight::StagePlan::for_launch`.

(The instant free spawn `SimCommand::SpawnPhysicalVessel` still exists: the player launch path
issues it only as the fallback when no authored launch site exists (sandbox/test worlds — the
shipped data set always authors `terrin-launch`), and the wasm demo's LAUNCH button emits it.
The elements-only `SimCommand::SpawnVessel` is the MCP/test-only variant — *that* declaration
carries the `// ui-unreachable:` marker.)

---

## 5. Determinism architecture

The product promise is **T1 replay determinism**: same binary + same platform + same save + same
command log ⇒ identical state hash (PLAN §2.7). Everything below exists to make that true.

### 5.1 The clock

- `SimTime(u64 ticks)` — private field, the only mutator is the fallible
  `SimTime::try_advance` (`csp-core/src/time.rs`). `TICK_HZ = 64`; dt is `1/64 s` **forever** —
  `csp_phys::simple::PHYSICS_DT`. Warp never changes dt (PLAN §2.6's "single most important
  rule").
- The clock lives in `csp_sim::World` and nowhere else.
- One `step_frame` call advances at most `MAX_TICKS_PER_FRAME = 69_120` ticks (18 sim-minutes)
  and returns `partial: true` beyond that — back-pressure, never a stall.

### 5.2 The single advance function

`World::step_frame(target)` (`csp-sim/src/lib.rs`) is the *only* state mutation entry. Per loop
iteration (PLAN §2.6.2, implemented verbatim):

1. Capture a `TickShadow` (one-tick rollback copy — a `Fatal` mid-tick restores it).
2. `drain_boundary` — drain the command queue in `CommandKey` order; stamp keys **at dequeue**
   (never at production); apply each via `apply_command` → `apply_*` handlers. Failed applies are
   rolled back and reported on `AdvanceReport::rejected` (typed `RejectKind`), never silently
   dropped.
3. `t_next = min(effective_target, event_heap.peek_tick(), tick_budget)`.
4. `step_bubble_range(now, t_next, …)` — integer physics ticks for every `InBubble` vessel
   (`csp-phys::SimpleBackend::step`, dt fixed), warp-independent. Includes the auto
   bubble→rails conversion at the boundary.
5. `advance_interval_subsystems(now, t_next)` — **closed-form** advancement for everything else
   (orbits propagate by re-query, colonies/research/reliability/life-support/manufacture advance
   by interval). One integrator per subsystem regardless of warp level (PLAN §2.6.1's
   partition-independence rule; see the epoch-anchored-orbits proof in `csp-sim/src/lib.rs`'s
   module doc).
6. `clock.try_advance`, then `fire_due_events` — the whole due batch at `t_next`, in
   `(tick, event_seq)` order, chunked by `MAX_EVENTS_PER_BATCH = 1024`; the frame budget is
   `advance_to_max_events_per_frame = 2000` (`data/balance.ron`, mirrored as
   `DEFAULT_MAX_EVENTS_PER_FRAME`).
7. Break on target reached / budget exhausted → `AdvanceReport { reached_tick, commands_applied,
   events_processed, fired_events, rejected, degraded, partial, effective_target, script_log }`.

Events: `BinaryHeap<ScheduledEvent>` keyed `(tick, event_seq)`; `SUBSYSTEM_ORDER` is a constant
12-slot array (`csp-sim/src/events.rs`) — reordering it is replay- and save-breaking. Two classes
carried in the enum: `Clamping` (auto-demotes warp) vs `Silent`, with per-vessel tracked opt-in.

### 5.3 Command ordering & the control lease

- `CommandKey { tick, source_class, source_id, source_seq }` — total order within a tick;
  `SourceClass` order is `Script(0) < Autopilot(1) < Ui(2) < External(3)`
  (`csp-sim/src/lib.rs → CommandKey`). Discriminants are append-only stable (they feed the log
  codec and the hash).
- The queue is per-source FIFOs under `(source_class, source_id)`; `source_seq` counters are
  serialized state — a reset would change every future stamp.
- `World::enqueue(source_class, source_id, cmd)` touches only the in-flight queue; a command
  becomes state only at the drain that stamps it.
- **All control commands are absolute** (`SetThrottle(f64)`, `SetAttitude`, `SetWarp(u32)`).
  Rate-based input is integrated app-side first (§4.1).
- **Control lease** (`Lease`): commands affecting a vessel's control from a non-leaseholder are
  rejected onto `AdvanceReport::rejected`; `SimCommand::TakeControl` transfers; `Player`
  preempts unconditionally. Autopilots are the player's own automation (never leaseholders);
  External clients must take the lease.
- External rate limits: ≤32 commands per client per drain, 64 KiB message cap, bounded ingress
  (`csp-server/src/ingress.rs → IngressQueue`) — overflow is an explicit backpressure error.

### 5.4 State hashing

- `StateHasher` (`csp-core/src/hash.rs`): BLAKE3-256 over a **hand-written** byte walk — never
  serde, never derive order. Variable-length items are length-prefixed; enums hash their stable
  `u16` discriminants; `f64`s are bit-patterns (NaN-faulting via `try_f64`).
- `World::state_hash()` / `state_hash_masked(&BTreeSet<u64>)` — the bubble-masked variant powers
  the ×1-vs-×10⁵ equivalence test (identical hashes except bubble contents).
- `HASH_SCHEMA_VERSION = 21` (currently) is hashed first and recorded next to every golden;
  bumping it requires `xtask bless-goldens --reason` in the same commit.
- What is *not* hashed: the command log (recorded history), the orbit model and all data-derived
  construction config (§7.3 class — re-installed from `data/` on load), scratch buffers.

### 5.5 RNG — KDF, never a stream

`csp-core/src/rng.rs`: `derive_key(save_seed, StreamKey) -> [u8; 32]` = BLAKE3 over
`KDF_VERSION(1) ‖ save_seed ‖ tag ‖ entity ‖ counter` (fixed LE widths); `stream()` builds a
`ChaCha8Rng` from it. `StreamTag` = `{Reliability=0, Ignition=1, Contracts=2, Reserved=3}` —
append-only (3 is a reserved never-drawn slot; a `Generation` stream was
allocated there but never shipped). The scheme **reseeds per key** (a stored counter replays the exact draw — reloading
a save and re-flying cannot reroll a failure threshold; anti-save-scum by construction).
`SeedableRng::seed_from_u64` is lint-banned. Fixed arity everywhere
(`{CONTRACTS, 0, offer_index}`).

### 5.6 What a developer must NEVER do — and the gate that catches it

| Never | Gate |
|---|---|
| Read a wall clock / `std::time` inside `csp-sim`/`csp-core`/`csp-phys`/`csp-data` | Per-crate `clippy.toml` `disallowed-types = [std::time::Instant, SystemTime, Duration]` (see divergence D4) + code review |
| Use platform `f64` transcendentals (`f64::sin`, `powf`, `sqrt`, …) in the determinism domain | Per-crate `clippy.toml` `disallowed-methods` — use the `libm` crate (workspace pins `libm = "=0.2.16"`) |
| Use `HashMap`/`HashSet` in the determinism domain (iteration order) | Same per-crate `disallowed-types` — use `BTreeMap`/`BTreeSet`/sorted `Vec` |
| `unwrap`/`expect` on fallible paths in the determinism domain | Workspace `[workspace.lints.clippy] unwrap_used = "deny", expect_used = "deny"` (root `Cargo.toml`); the four crates carry no exceptions |
| Add a `rand` facade / `getrandom` / thread-local RNG | `deny.toml [bans] deny = [{name = "rand"}, …]` + `xtask lint-deps` allowlists |
| Iterate state-affecting collections in anything but ascending-ID / `BTreeMap` order | Convention enforced by review; arenas are `BTreeMap` keyed by monotonic ids |
| Make dt or the tick rate warp-dependent | Architecture: `step_frame` is the only entry; `PHYSICS_DT` is a const |
| Feed measured time into sim state | The warp controller lives entirely in `csp-app` and emits *logged* commands (`csp-app/src/warp.rs`) |
| Add a relative-semantics `SimCommand` variant (e.g. `AdjustThrottle`) | Mandatory review item (PLAN §2.2.1); all existing verbs are absolute |
| Reorder `SimCommand` variants, `SourceClass`, `StreamTag`, `SUBSYSTEM_ORDER`, or a hash walk | Golden replays red by design; requires `HASH_SCHEMA_VERSION` bump + `bless-goldens` |
| Add a `SimCommand` with no player issuer | `cargo run -p xtask -- lint-reach` (`xtask/src/lint_reach.rs`): every variant needs a production issuer in csp-ui/csp-view/csp-app or an explicit `// ui-unreachable: <reason>` marker in the enum |
| Add a dependency edge violating §2.1 (e.g. csp-render → csp-sim, bevy outside csp-view) | `xtask lint-deps` explicit per-crate allow/deny lists; wasm purity positively asserted |
| Break the wasm build | CI: `cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown` (`.github/workflows/ci.yml`) |

### 5.7 Goldens, replay, and CI

- Artifact triple per scenario in `data/test-fixtures/<name>/`: `replay.bin` (bincode command log),
  `expected-hash.txt`, `fingerprint.json` (`csp-bench/src/replay.rs → ARTIFACT_FILES`).
- CI (`ci.yml`) runs `csp-replay verify` for **every** registered scenario; a workspace test pins
  the registry. Re-bless: `cargo run -p xtask -- bless-goldens --reason "…"` (refuses a dirty
  tree or an un-bumped `HASH_SCHEMA_VERSION`; see `xtask/src/main.rs`).
- The replay driver reads the `AdvanceTo` commands back out of the log — the log alone reproduces
  the run (no wall-clock side channel).
- Perf gate: `csp-perf --baseline docs/perf/perf-log-devbox-release.ron` fails on >10% regression
  or a budget Pass→Fail crossing.

---

## 6. Threading model, platforms, feature flags

### 6.1 Threads

- **Main thread**: winit event loop, all schedules, `step_frame`, wgpu. The sim is single-threaded
  by construction.
- **Executors**: `sim_step` and `sync` pin `SingleThreadedExecutor` explicitly (`csp-view/src/lib.rs`,
  View constructor's schedule setup, ~line 2073). The other schedules use bevy_ecs's default executor — the
  `multi_threaded` bevy_ecs feature is enabled **only on native**
  (`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` in `csp-view/Cargo.toml`), so wasm
  resolves single-threaded (PLAN §2.5).
- **Audio** (native): a writer thread + a player *subprocess* (`pw-play`/`paplay`/`aplay` over
  piped stdin — no audio FFI, no unsafe, graceful silence when absent;
  `csp-app/src/audio.rs`).
- **Server** (native, `csp --serve`): the game runs on a dedicated thread; `GameHost`
  (`csp-server/src/backend.rs`) is the `Send + Sync` seam publishing snapshots via `ArcSwap` and
  feeding commands through the bounded ingress; axum/tokio live only on the server side and never
  block the frame loop.
- **VAB GPU picking**: an offscreen object-id pass; the readback lands via `std::sync::mpsc`
  frames later, decoded against the id table snapshotted at encode time (stale-epoch drops —
  `App::run_frame`).

### 6.2 native vs wasm

One codebase; differences are `cfg`-gated:

| Concern | Native | wasm32-unknown-unknown |
|---|---|---|
| Entry | `main.rs → csp_app::run()` | `csp_app::wasm_start()` (`wasm-bindgen(start)`), `spawn_app` |
| Logging | `env_logger` + `logring` tee (500-line tail) | `console_log` |
| Time | `std::time::Instant` | `web_time::Instant` (app-side only) |
| Server/bench | `csp-server`, `csp-bench`, `xtask` build | excluded from the wasm tree; wasm purity asserted in CI |
| Audio | subprocess backend | absent (module is `cfg(not(wasm))`) |
| Data set | `data/` via `FsDataSource` (`View::new`) | embedded reduced set via `csp_data::EmbeddedSource` (`View::new_demo`, `csp-view/src/demo.rs`) |
| Saves/craft IO | `saves/*.cspsave`, `crafts/*.cspcraft` | refused (demo policy gates `UiOutbox` verbs) |
| GPU init | synchronous (pollster) | deferred browser promises; page hooks `cspGpuInitFailed` (at once) and `cspGpuReady` (on the first presented frame — the page's loading screen covers the canvas until then) |

`bevy_ecs` features: `["std"]` baseline, `+ ["multi_threaded", "async_executor"]` on native.
Rhai is pinned `=1.27.0` with `default-features = false, features = ["no_std"]` — the feature set
that preserves op budgets and libm-only guest math on *both* targets (PLAN §2.10's parity rule).

### 6.3 Settings

The model is `csp_ui::settings::Settings` (serde, RON): keymap (rebindable `KeyAction` bindings,
physical-first tokens, AZERTY-aware labels), audio, graphics quality, gameplay/difficulty (warp
cap, life-support toggle applied at new-game via `SETTINGS_SOURCE_ID`). File IO lives in
`csp-app/src/settings_io.rs`: `$config_dir/csp/settings.ron` (`config_dir()`/`settings_path()`),
collected-error loading, `backfill_missing_bindings` (new actions get defaults on old files),
`migrate_char_bindings`. Apply flow: the panel stages a copy; "Apply" sets `UiOutbox::
settings_apply`, and the app swaps the live set + persists next frame.

### 6.4 Audio

`csp-app/src/synth.rs` — a pure-function synthesized bank (fixed-seed xorshift noise; zero audio
assets → zero attribution rows). `csp-app/src/audio.rs` — the mixer + low-latency subprocess
player; `FlightSfx` (edge-detected cues from `flight_present`) drives one-shots; two continuous
voices track throttle and convective flux. Presentation-only; never feeds the sim.

### 6.5 Bug reports

`csp-app/src/bugreport.rs → write_bug_report(BugReportInput)`: save bytes (or its refusal
reason), encoded command log, `data_set_hash`, live `state_hash`, `save_seed`, GPU info, and the
`logring` 500-line tail. Triggers: the Fatal handler (`App::on_sim_fatal`), the panic hook, or
`csp --bug-report` (`main.rs`). This is what makes "reproducible down to the command log" real.

---

## 7. Error handling & logging conventions

### 7.1 Errors

Four classes, and every error maps to exactly one (`csp-core/src/error.rs → SimError`; the class
*is* the handling contract, PLAN §2.13):

| Class | Meaning | Behaviour |
|---|---|---|
| `Fatal(&'static str)` | NaN in state, corrupt arena, tick overflow | abort the tick (TickShadow rollback), halt the session read-only, dialog, bug-report bundle |
| `SaveBlocking` | playable but not persistable (config install failed) | play on; `World::snapshot()` refuses with the reason (`save_blocked`) |
| `Recoverable` | bounded subsystem failure (Kepler non-convergence, script abort, joint-cap degradation) | log/degrade, apply documented fallback, continue — surfaces on `AdvanceReport::degraded` |
| `Cosmetic` | presentation-only | log once, placeholder, continue |

Rules that hold in the tree: `step_frame` returns `Result<AdvanceReport, SimError>` and only ever
`Err`s on `Fatal`; rejected commands are rolled back completely and reported
(`AdvanceReport::rejected` + typed `RejectKind` for cross-crate branching); the data loader
collects all errors and loads the valid remainder (`csp-data/src/load.rs`); a corrupt save is
refused whole, never half-loaded; renderer device loss reconfigures once then exits cleanly.
Payloads are `&'static str` diagnostics — never hashed, never branch-determining.

### 7.2 Logging & observability

- The `log` facade at the app/server/bench boundary: `env_logger` (native), `console_log` (wasm),
  tee'd into `logring` for the bug bundle.
- **The sim never logs.** Diagnostics ride `AdvanceReport` (`fired_events`, `rejected`,
  `degraded`, `script_log`) and the notification ring (`reliability::Notification`, surfaced as
  HUD toasts by `View::drain_part_notices`). All timing instrumentation lives in the caller.
- Telemetry for dashboards/agents: snapshot reads via the `ArcSwap` publish + field-path
  subscriptions (`csp-server/src/telemetry.rs`); WS rate divisors of 64 Hz only, default 16 Hz,
  drop-to-latest backpressure (PLAN §2.12.2).

---

## 8. "Where do I change X" routing table

| I want to change… | Go to | Notes |
|---|---|---|
| A part's stats / add a part | `data/parts/*.ron`; schema `crates/csp-data/src/part.rs`; validation `crates/csp-data/src/validate.rs` | New `ModuleDef` variants need validator coverage + resolved handling in `csp-phys/src/resolve.rs` |
| Balance/tuning numbers | `data/balance.ron` (schema `csp-data/src/balance.rs`) | "ALL tuning lives here" — mirrored consts in `csp-sim/src/lib.rs` must stay in sync (a bench test pins them) |
| A sim rule (physics of a mechanic) | The feature module (`crates/csp-sim/src/{research,economy,contracts,colonies,reliability,…}.rs`) + its `apply_*` handler in `lib.rs` | New hashed fields ⇒ extend the `state_hash` walk + bump `HASH_SCHEMA_VERSION` + rebless goldens |
| Add a player verb | `SimCommand` variant (`csp-sim/src/lib.rs`) + apply handler + **a production issuer** in csp-ui/csp-view/csp-app (or `// ui-unreachable:` marker — `xtask lint-reach` fails otherwise) + `KeyAction` in `csp-ui/src/settings.rs` + `App::dispatch` in `csp-app/src/lib.rs` | Keep it absolute, never relative |
| A UI panel | New module in `crates/csp-ui/src/` (pure egui, plain-data in/out) + a mirror resource & staging in `csp-view/src/lib.rs` (`stage_*` in `end_frame`) + output field on `UiOutbox` | csp-ui must never import csp-sim/bevy/wgpu |
| The HUD / navball | `crates/csp-ui/src/{lib,navball}.rs` | |
| The map view | `crates/csp-view/src/map.rs` (scene/interaction) + `crates/csp-render/src/map.rs` (draw) | |
| Terrain / sky / ocean visuals | `crates/csp-render/src/terrain.rs` + `terrain.wgsl`; view-side packets `crates/csp-view/src/terrain.rs` | |
| The VAB editor | Session ops: `crates/csp-ui/src/vab.rs` + `csp-phys/src/session.rs` (`CraftSession`); scene/pick: `csp-view/src/vab.rs`; draw: `csp-render/src/vab.rs` | The working craft is §2.2-C session state |
| Input / keybinds | `csp-ui/src/settings.rs` (model + defaults) + `csp-app/src/{lib.rs dispatch, settings_io.rs}` | |
| Physics (integrator, contacts, aero, thermal, breakup) | `crates/csp-phys/src/{simple,aero,thermal,structural,weld}.rs` | dt is const; any hashed state change ⇒ hash walk + goldens |
| Orbit math / SOI / transfers | `crates/csp-sim/src/{orbit,transfer,maneuver}.rs` | Universal-variable solver tolerances are spec'd (PLAN §2.8) |
| Scripting host functions | `crates/csp-script/src/lib.rs` + `tests/api_freeze.rs` | `ControlApi` is **frozen** (additive-only, parity-tested) |
| The autopilot | `crates/csp-ai/src/lib.rs` | drives `ControlApi`, stamps `SourceClass::Autopilot` |
| Save format / migrations | `crates/csp-sim/src/save.rs` | Bump the save schema version + write a `migrate_vN_to_vN+1`; never half-load |
| An MCP tool | `crates/csp-server/src/mcp.rs` (+ `PROTOCOL.md`) | `csp.mcp.tools.v1` is additive-only within a major version |
| Telemetry dashboard | `apps/telemetry/src/**` + wire fields in `csp-server/src/{telemetry,http}.rs` | Versions pinned exactly; no carets |
| A golden scenario | `crates/csp-bench/src/{scenarios,scenario}.rs` + `data/test-fixtures/<name>/` + `xtask bless-goldens` | |
| Audio | `crates/csp-app/src/{synth,audio}.rs` | Procedural-first: prefer a synth patch over an asset |
| Art assets | `assets/` sources → `xtask/src/bin/bake_art.rs` → `.csar` packs under `data/art/`, decoded in `csp-view/src/art.rs` | License gate: CC-BY-NC/ShareAlike unusable (`data/ATTRIBUTION.md`, `xtask lint-attribution`) |
| CI gates | `.github/workflows/ci.yml`, `deny.toml`, `xtask/src/lint_*.rs` | |

---

## 9. PLAN-vs-code divergences (verified in the working tree)

1. **D3 — render "Frame".** PLAN §2.5 specifies a single immutable `csp_render::Frame` built by
   `csp-view`. The code instead builds **four per-pass packets** (`MapPacket`, `VabPacket`,
   `TerrainPacket`, `FlightPacket` in `crates/csp-render/src/{map,vab,terrain,flight}.rs`) drawn
   by four renderers; `csp_render::Gpu::begin_frame` returns a wgpu surface-frame wrapper, not
   the plan's Frame. The load-bearing rule survives: `csp-render` does not depend on `csp-sim`,
   and packets are immutable plain data.
2. **D1 — aero Stage-2 install path.** PLAN §2.4 has the app compute aero tables off-thread and
   install them via `SimCommand::InstallAeroTables`, with the sim *refusing to launch* a craft
   whose tables are absent. In the code `InstallAeroTables` exists but is never issued by
   app/view (MCP-only; marked `// ui-unreachable:`); the sim **lazily recomputes** tables itself
   (`World::ensure_aero`, `csp-sim/src/lib.rs`) into a per-vessel cache keyed by graph hash.
   Determinism is preserved (the recompute is the same pure function), but the "refuses to
   launch" gate does not exist.
3. **D2 — no RapierBackend.** PLAN §2.9 specifies `rapier3d-f64` for jointed islands (docking
   soft-capture). There is no `RapierBackend`; soft-capture joints are solved inside
   `SimpleBackend`'s PGS velocity pass (`csp-phys/src/simple.rs`, documented in
   `csp-sim/src/joints.rs`). `rapier3d-f64`/`parry3d-f64` remain pinned in the workspace
   `Cargo.toml` but are unused by any crate.
4. **D4 — clippy lint config location.** PLAN §2.1/§8.2 name a root `clippy.toml`; the
   disallowed-methods/types live in **per-crate** `clippy.toml` files, only in the four
   determinism crates (`csp-core`, `csp-data`, `csp-phys`, `csp-sim`). Same effect, narrower
   scope (presentation crates may use `f64::sqrt` etc.).
5. **D5 — telemetry SPA stack.** PLAN §1 settles "TanStack Start"; `apps/telemetry/package.json`
   is TanStack Router/Start on a **Vite** client build (all versions exact-pinned as the plan
   requires). No SSR is used; the recorded REVIEW2-opus #25 tradeoff stands.
6. **D6 — physics warp cap.** PLAN §2.6.2 caps physics warp at ×2 pending a ≤0.5 ms measured
   per-tick cost; the shipped `WARP_LADDER` is exactly `[1, 2, 10, …, 100_000]` — matches the
   plan; ×3/×4 remain unimplemented by design.
7. **D7 — event-budget shape.** PLAN §2.6.5's 2000-events/frame + 1024/batch atomic ceiling are
   implemented verbatim (`MAX_TICKS_PER_FRAME`, `DEFAULT_MAX_EVENTS_PER_FRAME`,
   `MAX_EVENTS_PER_BATCH`); the event-kind set has grown beyond the plan's 16 members
   (e.g. `SoiRescan`, `TransferWindow`, `SalaryDue`, `Insolvent`, colony kinds) — additive, not
   divergent.
8. **Map-vs-tree (now-closed gaps).** The stale exploration map listed colonies, crew,
   bench tests, alarms, save/load, revert, and manufacture-costed launches as implemented-but-
   unreachable. In the working tree these are player-reachable: the Program window
   (`csp-ui/src/program.rs`, 2.9k lines) covers economy/contracts/tech/science/reliability/
   crew/colonies/labs/automation; the Game menu + F5/F9/F10 wire save/load/revert
   (`csp-ui/src/game.rs`, `App::do_save/do_load`, `View::revert_to_launch`); launch is a
   costed build order (`View::launch_craft` → `ManufactureCraft`); `xtask lint-reach` now *gates*
   the "sim-complete but no UI" failure mode permanently.

Unverified / deliberately absent: N-body, CommNet, EVA, LLM adapter, wasmtime mod host, Advanced
rendering tier (bindless/indirect/Forward+/TAA), cross-platform bit-determinism (T2) — all
post-v1 per PLAN §6.4/§9; no `docs/quarterly/` cut-review ritual exists (only `docs/perf/`).

---

## 10. Reading order for a new dev

1. This file, then [BUILD-AND-DEV.md](BUILD-AND-DEV.md) to build and run.
2. `crates/csp-core/src/{time,rng,hash,error}.rs` — small, foundational, heavily documented.
3. `crates/csp-sim/src/lib.rs` module docs (§A–D partition: commands → tick machine →
   restore/hash → tests) — the doc comments are the real spec of every mechanic.
4. `crates/csp-view/src/lib.rs` — the frame contract and every mirror.
5. `crates/csp-app/src/lib.rs → App::run_frame` — the loop that drives it all.
