# Clanker Space Program — Design & Implementation Plan

A KSP-style vehicle build/fly sim with a persistent RTS progression layer.
Working engineering document. Opinions here are decisions, not options.

Core pillars:

1. **Build anything** — rockets, planes, rovers, stations, from a data-driven part graph.
2. **Fly it for real** — welded-body physics, per-part aero/thermal, patched conics (pluggable N-body).
3. **A world that runs without you** — colonies, extraction, research advance on the sim clock under timewarp.
4. **Automation is first-class** — scripting API from day one, not a bolt-on.
5. **Modern rendering, honestly tiered** — wgpu; a native GPU-driven tier and a reduced WebGPU tier.

---

## 1. Settled decisions

| Decision | Choice |
|---|---|
| Language | Rust |
| Graphics | wgpu (Vulkan/Metal/DX12/WebGPU via one API) |
| App shell | **Hybrid**: `bevy_ecs` standalone (ECS + schedules only) + winit + egui + own renderer |
| Physics | Rapier with `f64` precision feature (pure Rust, wasm-clean) |
| Multiplayer | None in v1; sim kept deterministic-by-convention (fixed timestep, seeded RNG, no wall-clock) |
| Solar system | Fictional, hand-authored, ~1/10 real scale (Kerbal-like); system defs are data files → swappable later |
| Orbits | `OrbitModel` trait; patched conics v1, N-body as post-v1 module |
| RTS layer | Real-time continuous sim + KSP-style timewarp |
| Web | Desktop-first; `wasm32-unknown-unknown` kept compiling in CI from day one |
| AI | Classical (PEG guidance, design solver, transfer optimizer) v1 + LLM assistant later via scripting API |
| Vessel physics | Single welded rigid body + per-part analytical forces; real joints only for docking/robotics/failure |
| Precision | f64 sim world; f32 camera-relative rendering |

Non-goals for v1: multiplayer, real solar system, N-body default, native mod loading on web.

### 1.1 Consequences of the settled decisions (read before coding)

These are not relitigations. They are the bills that come due.

- **`bevy_ecs` standalone means you own the loop.** No `bevy_app`, no plugins, no `bevy_render`,
  no `bevy_asset`. You construct `World`, register `Schedule`s, and call `schedule.run(&mut world)`
  yourself from the winit event loop. Dependency rule is enforced in CI (§2.5).
- **`bevy_ecs`'s multi-threaded executor does not guarantee system *execution* order**, only the
  ordering you declare. That is incompatible with "authoritative state lives in the ECS" plus
  determinism. Therefore authoritative sim state does **not** live in the ECS (§2.2).
- **Rapier is not deterministic across versions, and not cross-platform deterministic without
  `enhanced-determinism`** (which disables some fast paths) and without `parallel`. `parallel`
  is unavailable on wasm anyway. Determinism tier is defined in §2.7 accordingly.
- **f64 transcendentals (`sin`, `cos`, `pow`, `atan2`) are not bit-identical across platforms/libms.**
  The sim must not call `std` transcendentals on any value that feeds state. `csp-sim` uses the
  `libm` crate exclusively, enforced by a clippy `disallowed_methods` lint.
- **"Bleeding-edge GPU on all vendors" and "runs on the web" are different renderers, not one
  renderer with flags.** WebGPU has no bindless binding arrays, no `multi_draw_indirect`,
  no indirect-count, no mesh shaders, no 64-bit atomics. See §5.
- **wasm32 is 32-bit, single-threaded by default, and practically capped near 2 GB heap.**
  Virtual texturing and large terrain caches are a native-tier feature. Web runs a reduced dataset.
- **Timewarp is the enemy of fixed-timestep determinism** unless warp never changes dt. It doesn't,
  here. See §2.6.

---

## 2. Architecture

### 2.1 Crate layout (cargo workspace)

```
Clanker-Space-Program/
├── PLAN.md
├── Cargo.toml                  # workspace; shared lint + dep policy
├── crates/
│   ├── csp-core/               # units, fixed-point sim clock, IDs, f64 math (libm only),
│   │                           # seeded RNG streams, error types. No deps on anything below.
│   ├── csp-data/               # part defs, system defs, tech tree (RON), loader + validator,
│   │                           # save schema + migrations. No sim logic.
│   ├── csp-sim/                # HEADLESS authoritative sim. Plain owned data, no ECS, no wgpu,
│   │                           # no winit, no rand, no std::time. Deterministic. Runs on wasm.
│   ├── csp-phys/               # collision + rigid-body integration behind `PhysicsBackend`
│   │                           # (parry3d-f64 + own integrator; Rapier impl for jointed cases)
│   ├── csp-view/               # bevy_ecs world: presentation entities, mirrors sim snapshots,
│   │                           # schedule definitions. Owns the sim<->render bridge.
│   ├── csp-render/             # wgpu renderer: camera-relative f32, reverse-Z, terrain,
│   │                           # atmosphere, native GPU-driven tier + web tier
│   ├── csp-ui/                 # egui panels, VAB UI model, input bindings, settings
│   ├── csp-app/                # winit shell, frame loop, platform glue (native + wasm entry)
│   ├── csp-script/             # scripting host: stable control API, Rhai guest everywhere,
│   │                           # wasmtime mod host (native only)
│   ├── csp-ai/                 # classical automation: guidance, design solver, trajectory
│   │                           # optimizer; LLM adapter trait
│   ├── csp-server/             # NATIVE ONLY: embedded HTTP+WS server — telemetry screens,
│   │                           # widget dashboards, MCP endpoint for external agents
│   └── csp-bench/              # NATIVE ONLY: headless scenario runner — golden replays,
│                               # LLM/eval benchmarks, economy harness. csp-sim + commands,
│                               # no renderer
└── data/
    ├── parts/
    ├── systems/home/
    └── techtree/
```

**Dependency rules (CI-enforced):**
- `csp-sim` may depend only on `csp-core`, `csp-data`, `csp-phys`, `libm`, `serde`.
- `csp-sim`, `csp-phys`, `csp-data` must not depend on `bevy_*`, `wgpu`, `winit`, `egui`, `rand`,
  `std::time`, `rayon`, or any threadpool.
- `csp-render` must not depend on `csp-sim`. It consumes `csp-render::Frame` built by `csp-view`.
- `csp-view` is the only crate allowed to depend on both `csp-sim` and `csp-render`.
- `bevy_ecs` is allowed only in `csp-view`, with `default-features = false`.

### 2.2 State ownership (the thing the old plan left unsaid)

Two worlds, one direction of truth.

```
                 commands (queued, tick-stamped)
  player/UI/script ────────────────────────────────►  csp-sim  (authoritative)
                                                          │ snapshot (read-only, per tick)
                                                          ▼
                                   csp-view (bevy_ecs World, presentation only)
                                                          │ Frame
                                                          ▼
                                                     csp-render
```

- **Authoritative state** = plain Rust structs in `csp-sim`, stored in stable index arenas
  (`slotmap`-style: `VesselId`, `PartInstanceId`, `ColonyId`, `ContractId`). Iteration order is
  always the arena's dense index order — never a `HashMap` iteration.
- **`HashMap`/`HashSet` are banned in `csp-sim` state and in any iteration that affects state.**
  Use `BTreeMap` or sorted `Vec`. CI lint.
- **No system mutates sim state except through `csp-sim`'s own ordered step function.**
  `bevy_ecs` systems may read snapshots and write presentation components. That's it.
- **Commands** are an enum (`SimCommand`) pushed into a queue with the tick they were issued at.
  UI, scripts, AI, and contract acceptance all go through it. This is what makes replay, undo,
  and the future LLM agent all fall out for free, and it is why the LLM has no privileged path.

Why not put everything in the ECS: determinism, savegame stability, headless reuse (tests,
replays, AI batch runs, a future dedicated server), and not being married to bevy_ecs's
serialization story. The cost is a mirroring layer in `csp-view`; pay it.

### 2.3 Precision, coordinates, depth

- **Sim**: `f64`, origin = system barycenter, SI units (m, kg, s, N, K, W). Angles in radians.
  Celestial positions from analytic ephemerides (Kepler elements + epoch).
- **Sim clock**: integer ticks. `TICK_HZ = 64` (sim seconds per tick = 1/64 exactly).
  `SimTime(u64 ticks)`. Never an `f64` accumulator. Epoch 0 = game start (UT seconds = ticks/64).
- **Physics bubble recentering**: the active bubble has its own `f64` origin, snapped to the active
  vessel's position whenever it exceeds 1 km from origin. Within the bubble, positions are `f64`
  relative to the bubble origin (krakensbane, done deliberately).
- **Rendering**: per frame, positions converted to `f32` relative to the camera. GPU never sees f64.
  Terrain patches carry a per-patch `f64` origin baked to `f32` locals at build time.
- **Depth**: reverse-Z, `f32` depth, infinite far plane (`[0,1]` with 1 = near). Single frustum;
  if precision cracks appear beyond ~10⁷ m, add a second far-field pass rather than log-depth
  (log-depth breaks early-Z and interpolation). Decide empirically in R1 with a measurable test.
- **Scale check**: ~1/10 real means home planet radius ≈ 600 km, SOI ≈ 8.4 Mm, system extent
  ≈ 10¹¹ m. f64 gives ~µm resolution at 10¹¹ m. Fine. f32 at 10⁷ m gives ~1 m — hence
  camera-relative is mandatory, not an optimization.

### 2.4 Vessel model

- **Weld**: on launch/scene-load, the part graph compiles to a `WeldedBody`:
  - total mass, CoM, inertia tensor (recomputed when resource mass changes; cached, updated at
    most every N ticks or on >0.5% mass delta)
  - per-part `ForceSite { local_pos, local_dir, kind }` for thrust, control surfaces, reaction wheels
  - per-part aero coefficients from a **weld-time voxelization** pass (see §2.4.1)
  - collision shape: convex hulls per part, merged into a compound shape
- **Runtime**: one rigid body; all per-part forces are analytic and summed into (force, torque).
  No per-part joints. This kills the KSP1 joint kraken at the architecture level.
- **Joints only where physically required**: docking ports, robotic hinges/pistons, decoupler
  failure cases. These create a *multi-body* island and are the only place the constraint solver
  runs. Hard cap in v1: 16 joints per bubble; exceed it and the UI refuses the design.
- **Breakup**: structural/thermal failure splits the part graph at the failed node → two
  `WeldedBody` recompiles, momentum distributed from the parent's rigid-body state. Must conserve
  linear/angular momentum to within 1e-9 relative; there's a test for it.

#### 2.4.1 Aero: reconciling "voxelized" with "analytic"

Two phases, no contradiction:

1. **Weld time (expensive, once per design change)**: voxelize the assembled hull at a fixed grid
   (default 64³ over the vessel AABB, deterministic traversal order). For each of N fixed flow
   directions (default 26: ±axes + edge/corner directions), compute per-part exposed frontal area
   and shielding factor. Store as a small per-part `AeroTable { area[N], shield[N], ref_len }`.
2. **Runtime (cheap, every tick)**: given body-frame airflow direction, interpolate the per-part
   area/shield from the table, apply analytic Cd/Cl curves vs Mach and Reynolds, sum forces at the
   part's position. Control surfaces add a deflection-dependent term.

This is FAR-like behaviour (stack fairings actually shield payloads, Cd depends on shape) without a
CFD loop in the tick. Voxelization result is cached by part-graph hash and serialized with the craft.

### 2.5 ECS usage and schedule ordering

`csp-view` owns a `World` and these schedules, run explicitly by `csp-app`:

| Schedule | When | Contents |
|---|---|---|
| `Input` | once/frame | winit events → `SimCommand`s; camera input; UI event intake |
| `SimStep` | 0..N times/frame | calls `csp_sim::World::step()` N times (N from warp, §2.6). **Single system. Not parallel.** |
| `Sync` | once/frame | sim snapshot → presentation components (spawn/despawn/update view entities) |
| `Presentation` | once/frame | interpolation, camera solve, LOD selection, particle/VFX update, audio cues |
| `UiBuild` | once/frame | egui pass; emits `SimCommand`s for *next* frame's `Input` |
| `RenderExtract` | once/frame | build `csp_render::Frame` (immutable) from presentation components |

Rules:
- Executor: `SimStep` uses the single-threaded executor always. `Presentation` and `RenderExtract`
  may use the multi-threaded executor on native, single-threaded on wasm.
- `Sync` and `Presentation` may never write to `csp-sim`. Enforced by only handing them `&SimSnapshot`.
- egui runs in `UiBuild`, after `Sync`, so UI always shows post-step state. UI-issued commands take
  effect on the *next* tick — one-tick latency, accepted, and consistent with scripting.
- Interpolation: `Presentation` interpolates between tick N-1 and N using the frame's fractional
  tick remainder. Never extrapolates. At high warp, interpolation is disabled (positions snap).
- `bevy_ecs` is used with `default-features = false`. `bevy_reflect` is off. `multi_threaded` is a
  native-only feature. If this proves painful, dropping `bevy_ecs` for a hand-rolled
  presentation store is a *cheap* change precisely because sim state isn't in it — that's the point.

**Frame loop (csp-app):**
```
winit RedrawRequested:
  dt_wall = clamp(now - last, 0, 100ms)      // wall clock ONLY here, never in sim
  ticks = warp_controller.ticks_for(dt_wall) // integer, clamped (§2.6)
  run Input
  for _ in 0..ticks { run SimStep }          // or rails advance, see §2.6
  run Sync, Presentation, UiBuild, RenderExtract
  csp_render.render(frame)
```

### 2.6 Timewarp — the real design

The old plan said "colonies tick on the sim clock either way". At ×10⁷ that is 10⁷ sim-seconds per
wall second = 640 million ticks/s. Impossible. Here is the actual model.

**Invariant: the physics dt is immutable at 1/64 s for the entire life of the project.**
Warp never changes dt. Changing dt would make trajectories depend on the player's warp choices,
which destroys replay, golden tests, and any future netcode. This is the single most important
rule in this document.

Three regimes:

| Regime | Rate | Mechanism | Allowed when |
|---|---|---|---|
| **Real time** | ×1 | 64 ticks/s | always |
| **Physics warp** | ×2, ×3, ×4 | 128/192/256 ticks/s — *more CPU work, same dt* | in bubble; drops automatically if the frame budget is exceeded |
| **Rails warp** | ×10 … ×10⁵ | no ticks; analytic jump of the sim clock | no vessel in bubble under thrust/contact/atmosphere |

Notes and consequences:
- Physics warp is honest: it is a CPU-bound speedup, capped by how fast you can step. Advertising
  ×8 physics warp is a lie unless the step is <2 ms. The warp controller measures and demotes.
- **Max rails warp is ×10⁵, not ×10⁷.** At ×10⁷ a 1-frame step is 166 sim-days; no contract
  deadline, SOI entry, or colony storage event can be resolved to acceptable precision, and
  players overshoot everything. ×10⁵ = ~1.1 sim-days per wall second. Revisit only with data.
- **Rails advance is a jump, not a loop.** `sim.advance_to(target_tick)`:
  1. Query every subsystem for its next scheduled event time (§2.6.1). Take the minimum.
  2. `t_next = min(target_tick, earliest_event)`.
  3. Closed-form advance every subsystem to `t_next` (orbits: Kepler; colonies: integrate
     production analytically; research: integrate conversion analytically; reliability: accumulate
     hazard integral).
  4. Fire the event, which may insert new events. Repeat until `target_tick`.
- **Warp is clamped by pending events**, not just by the player's key press. Approaching an SOI
  boundary, maneuver node, contract deadline, or alarm auto-demotes warp. This is a feature
  (Kerbal Alarm Clock behaviour built in), and it's also what makes the event queue mandatory.
- **No background thrust in v1.** Unloaded vessels do not burn. Warp is refused while any vessel
  has nonzero throttle. Post-v1: low-thrust-on-rails via Edelbaum/averaged elements, clearly
  marked as a separate feature with its own accuracy caveats.
- **SOI transitions are solved, not stepped**: analytic/bisection root-find on
  `|r_vessel - r_body| - r_soi` within the advance interval, to within 1 tick. Never detected by
  sampling at warp dt.

#### 2.6.1 The event queue

`csp-sim` owns a single `BinaryHeap<ScheduledEvent>` keyed by `(tick, event_seq)` — `event_seq` is a
monotonic counter that makes ordering total and therefore deterministic under ties.

Event kinds (v1): `SoiTransition`, `ManeuverNodeStart`, `ColonyStoreFull`, `ColonyStoreEmpty`,
`ResearchProjectComplete`, `ConstructionComplete`, `ContractDeadline`, `TransferWindow`,
`PlayerAlarm`, `LifeSupportDepleted`, `ReliabilityInspectionDue`.

Rule: **any subsystem that has a nonlinear or thresholded behaviour must publish its next event
time.** A subsystem that can only be computed by stepping is a bug under rails warp.

### 2.7 Determinism tier (be precise about what's promised)

Three tiers; pick deliberately per use.

| Tier | Promise | How |
|---|---|---|
| **T1 — Replay** (v1 target) | Same binary + same platform + same save + same command log ⇒ identical state hash | fixed tick, integer clock, no `HashMap` iteration, seeded RNG streams, `libm` only, no float ops dependent on thread count |
| **T2 — Cross-platform** (post-v1, needed only for netcode) | Same version, different OS/CPU ⇒ identical | additionally: `enhanced-determinism`, no `parallel`, no SIMD-width-dependent code paths, software transcendentals everywhere, pinned physics backend |
| **T3 — Cross-version** | explicitly not promised | saves store version; golden hashes regenerate on backend/version bump, with a changelog entry |

CI runs T1 golden replays on every commit (linux x86_64). A nightly job runs the same replays on
macOS-arm64 and windows-x86_64 and *reports* divergence without failing the build — this tells you
for free whether T2 is still reachable before you ever need it.

RNG: no global RNG. `Rng::for_(stream, entity_id, counter)` derives a ChaCha8 stream from
`(save_seed, stream_tag, entity_id, counter)`. Counters are part of the save. Consequence: reloading
a save and re-flying does **not** reroll an engine failure — anti-save-scum by construction, and
required for replay.

### 2.8 Orbit layer

```rust
pub struct OrbitState { pub parent: BodyId, pub elements: KeplerElements, pub epoch: SimTime }

pub trait OrbitModel {
    fn state_at(&self, s: &OrbitState, t: SimTime) -> (DVec3, DVec3);   // closed-form or integrated
    fn advance(&self, s: &mut OrbitState, to: SimTime);
    /// Must return the earliest SOI change in (now, horizon], solved to 1-tick precision.
    fn next_soi_event(&self, s: &OrbitState, now: SimTime, horizon: SimTime) -> Option<SoiEvent>;
}
```

- `PatchedConics` (v1): bodies and on-rails vessels on Keplerian rails inside their SOI.
  Universal-variable Kepler solver (handles e≈1 and hyperbolic without branching bugs).
- `NBody` (post-v1): symplectic integrator + cached trajectory spline; `next_soi_event` becomes a
  search over the spline. The trait is already shaped for it — that's why `next_soi_event` exists
  rather than a step-and-check loop.
- Transition between bubble and rails: on leaving the bubble, cartesian state → elements; on
  entering, elements → cartesian. Round-trip error budget: <1e-6 relative in position over one orbit.
  There's a test.

### 2.9 Physics backend

```rust
pub trait PhysicsBackend {
    fn step(&mut self, dt_ticks: u32, bodies: &mut [WeldedBody], joints: &[JointDef]);
    fn cast_ray(&self, ...) -> Option<Hit>;
    fn contacts(&self, body: BodyHandle) -> &[Contact];
}
```

Two implementations:
- **`SimpleBackend` (default, v1)**: own semi-implicit Euler / RK4 integrator for single welded
  bodies + `parry3d-f64` for collision queries and terrain contact. Rationale: with welded bodies
  you need *collision detection and contact resolution for landing gear*, not a general constraint
  solver. This is a few hundred lines, fully deterministic, trivially wasm-clean, and removes the
  largest third-party determinism risk in the project.
- **`RapierBackend`**: `rapier3d-f64`, used for jointed islands (docking, robotics) and as a
  cross-check oracle in tests. `parallel` off. `enhanced-determinism` on.

This inverts the old plan's risk row. The old escape hatch ("swap to Jolt") was not viable: Jolt is
C++, which breaks `wasm32-unknown-unknown` cleanliness and the pure-Rust story. Delete that idea.

### 2.10 Scripting (kOS-equivalent, first-class)

One API, one guest language, one set of semantics.

- **`ControlApi` trait** in `csp-script`: telemetry read, throttle/steer/SAS, staging, maneuver
  nodes, action groups, resource transfer, part actions, alarms, log/print. Every mutation becomes
  a `SimCommand`. Nothing bypasses the queue.
- **Guest language: Rhai, on all platforms.** Pure Rust, wasm-clean, has operation-count limits
  natively. One language means player scripts are portable between native and web, and the
  built-in autopilots are testable against the same host.
- **wasmtime is a native-only *mod* host**, not a second scripting backend. It is for Rust/other
  compiled mods that want more than the script API. Post-v1.
- **Built-in autopilots (ascent, rendezvous, landing, suicide burn) are written in Rust** against
  `ControlApi`, not in the guest language. Shipping them as guest scripts sounds elegant and means
  your core gameplay runs through an interpreter you're still designing. Rust implementations,
  exposed to scripts as callable services (`mission.ascend(target_apo, incl)`).
- **Determinism and scripts:** each script gets a fixed op budget per tick (default 20k Rhai ops,
  configurable per save). Exceeding it suspends the script until next tick and logs; it does not
  stall the sim. The budget and the suspension point are part of the deterministic state. Scripts
  may not read wall-clock, filesystem, or network.
- Scripts run in `SimStep`, before force integration, in `ScriptId` order.

### 2.11 AI

- **v1 classical**:
  - *Ascent*: PEG/gravity-turn guidance with a tuned pitch program; closed-loop terminal guidance.
  - *Transfer*: Lambert solver + porkchop scan + local refinement; outputs maneuver nodes.
  - *Design assistant*: constraint solver over part stats — given payload mass, target Δv, min TWR,
    body, propose staging. Greedy + local search over the unlocked part set. No ML.
- **Later LLM**: `LlmAdapter` trait. The LLM emits `SimCommand`s / script source through the same
  `ControlApi`. Three hard constraints, because "sandboxed by construction" was too glib:
  1. **Network I/O never happens inside a tick.** Requests are issued from `UiBuild`, responses
     arrive as commands stamped at the tick they are consumed. Otherwise latency jitter breaks replay.
  2. **Confirmation required for anything irreversible** — spending funds, launching, staging,
     deleting a craft, accepting a contract. The LLM proposes; the player commits.
  3. Opt-in, off by default, API-key supplied by the player, token budget visible.

### 2.12 External interfaces — one control surface, four clients

Everything that can *observe* or *control* the game goes through the same two doors:
telemetry comes from `csp-sim` snapshots, mutation goes through the `SimCommand` queue.
No interface has privileged access — this is what makes the agent story honest.

| Client | Transport | Direction | Lands |
|---|---|---|---|
| In-game scripts (Rhai) | `ControlApi` trait, in-process | read + commands | M9 |
| Built-in autopilots | `ControlApi` trait, in-process | read + commands | M9 |
| **MCP server** (`csp-server`) | MCP JSON-RPC over stdio or HTTP+SSE | read + commands | M9/post |
| **Telemetry web screens** (`csp-server`) | HTTP static SPA + WebSocket stream | mostly read; opt-in command channel | M9 |

Rules for `csp-server`:
- **Native-only.** The web build of the game cannot host a server — `csp-server` is
  `cfg(not(target_arch = "wasm32"))` and out of the wasm CI gate's reach (it still must
  not be a *dependency* of anything in the wasm tree).
- **Every tool/command maps to `SimCommand`s** stamped at the tick they're consumed —
  agents get the same one-tick latency and the same determinism contract as UI input.
- **Bind `127.0.0.1` by default**; a generated per-save token is required for LAN binds.
  Read-only telemetry may be unauthenticated on localhost; any mutation endpoint requires
  the token, always.

#### 2.12.1 MCP & autonomous play

An MCP server exposing `ControlApi` as tools (`telemetry.get`, `vessel.control`,
`editor.build`, `timewarp.set`, `mission.nodes`, `craft.load`, ...) plus snapshot data as
resources. Purpose: external LLMs/agents can *play* the game — scripted scenarios,
autonomous play, and eval benchmarks.

Because `csp-sim` is headless, `csp-bench` runs the whole thing with no renderer:
`scenario.yaml { initial_snapshot, command_source: agent|script|log, goal_predicate,
tick_budget }` → exit code + final state hash + score (Δv used, sim-time elapsed, funds
spent). That is an LLM eval harness by construction: deterministic replays are the
grading ground truth, and a failing run is reproducible down to the command log.

#### 2.12.2 Telemetry web screens (KSP-web-server-mod equivalent)

The game serves a small static SPA + a WebSocket feed of snapshot telemetry (altitude,
velocity, Ap/Pe, resources, stage Δv, active vessel state, colony summaries). Client-side
widgets: gauges, graphs, digital readouts, orbit-map thumbnail, resource bars.

**Dashboards are data, like workspace-welcome:** widget layouts/pages stored as JSON in
the save (`dashboards` section), a widget registry mapping `type → component + telemetry
fields`, user-defined pages with drag/resize — so a second monitor, a tablet, or a
stream overlay can each show a different layout against the same running game. The
command channel (buttons/widgets that act on the ship) is opt-in per dashboard and goes
through the same token-gated `SimCommand` path as MCP.

---

## 3. Game design — progression layer

### 3.1 Core data models

These exist because "parts have stats that improve along upgrade tracks" is not implementable as
written. Part IDs are **stable strings** (`csp.engine.lv30`), never indices — saves depend on it.

```rust
// data/parts/*.ron
struct PartDef {
    id: PartId,                  // stable string, never reused
    family: FamilyId,            // upgrade/tech grouping
    display_name: LocKey,
    mass_dry_kg: f64,
    cost_funds: f64,
    build_resources: Vec<(ResourceId, f64)>,  // what it consumes to manufacture
    attach_nodes: Vec<AttachNode>,
    scale: ScaleRule,            // None | Discrete(Vec<f64>) | Continuous{min,max,exponents}
    collider: ColliderDef,
    aero: AeroDef,               // Cd/Cl curves, ref_len; exposed area comes from voxelization
    thermal: ThermalDef,         // mass*cp, max_temp_k, emissivity, conduction to neighbours
    structural: StructuralDef,   // max_axial_n, max_shear_n, max_torque_nm
    modules: Vec<ModuleDef>,     // Engine, Tank, ReactionWheel, Experiment, Antenna, Drill, ...
    reliability: ReliabilityDef, // §3.4
    tech_requirement: TechNodeId,
}

enum ModuleDef {
    Engine { thrust_vac_n: f64, isp_vac_s: f64, isp_sl_s: f64, throttle_range: (f64,f64),
             gimbal_deg: f64, ignitions: Option<u32>, rated_burn_s: f64,
             propellants: Vec<(ResourceId, f64)>, ullage_required: bool },
    Tank { capacity: Vec<(ResourceId, f64)> },
    Experiment { kind: ExperimentId, data_units: f64, reusable: bool, situations: SituationMask },
    Drill { resource: ResourceId, base_rate_per_s: f64, min_concentration: f64, power_w: f64 },
    // ...
}
```

**Effective stats resolver (pulled forward to M3 — the old plan had this implicitly in M6 and M8,
which is why M3 looked cheap and wasn't):**

```
EffectivePart = PartDef
              ∘ tech_upgrades(family, researched_tracks)   // §3.3
              ∘ scale(user_scale)                          // TweakScale-native
              ∘ variant(user_variant)
              ∘ wear(usage_history)                        // §3.4
```

One function, one place, used by the editor readouts, the flight sim, the design AI, and contract
validation. If these ever disagree, players will find it in a day.

### 3.2 Science economy

Science is a function of **data × time × researchers**, with data as an inventory, not a currency.

- **Data**: an `Experiment` run in a `(body, biome, situation)` context yields
  `data_units` of a typed `DataRecord`. Situations: `Landed`, `Splashed`, `FlyingLow`,
  `FlyingHigh`, `SpaceLow`, `SpaceHigh` (KSP-proven granularity; don't invent more).
- **Diminishing repeats**: the old `v0 / 2^n` caps a given key's lifetime yield at `2·v0` and makes
  the 4th run worthless. Use a soft-cap on *cumulative* data per key instead:

  ```
  yield(n-th run) = cap_key * (1 - exp(-k * data_so_far_normalized)) derivative form:
  yield = cap_key * exp(-k * collected_key / cap_key) * run_units
  ```
  With `k ≈ 1.0`: run 1 ≈ 63% of cap, run 2 ≈ 86%, run 3 ≈ 95%. Players feel diminishing returns
  without "0.1 science" insults, and the cap is an authored number per `(experiment, body, situation)`
  in the tech data. Tunable per experiment; `k` is a global balance constant in one file.
- **Transmission vs recovery**: transmitting a record yields a fraction (per-experiment
  `transmit_ratio`, 0.2–1.0) and consumes `EC`; recovering the physical sample yields 100% and
  closes the key. Recovered-vs-transmitted is tracked per key so a transmitted key can be
  "completed" later by recovery for the remainder.
- **Researchers and labs**: data converts to science only in a lab, at
  `rate = Σ_researcher(skill) * lab_multiplier * min(1, data_available / throughput)` science/day.
  Researchers are hired with funds, have a skill level, and consume salary. Conversion is
  **integrated in closed form** over a rails-warp interval, and the lab publishes a
  `ResearchProjectComplete` / data-exhausted event so warp clamps correctly.
- Data can be hoarded. Hoarding is a real strategy: bank data early, build labs later.

### 3.3 Tech tree & part evolution

- A `TechNode` costs science and unlocks: new `PartId`s, new `UpgradeTrack` levels, new facilities,
  new contract types.
- **`UpgradeTrack`**: `{ family, stat_path, levels: Vec<{ science_cost, multiplier_or_delta }> }`.
  E.g. `csp.engine.lv30 / isp_vac_s` has 4 levels at +3% each. Tracks apply through the resolver
  in §3.1, so an upgrade retroactively improves parts on existing craft *the next time they are
  built*, never on craft already in flight (otherwise saves become unexplainable).
- Hard rule: **upgrades are multiplicative deltas in data, never new PartDefs**, or you get
  combinatorial part spam and a save-migration nightmare.
- Tree shape authored in `data/techtree/*.ron` with a validator: no cycles, every part reachable,
  every node's cost monotonic in depth. Validator runs in CI.

### 3.4 Reliability model

Concrete, deterministic, and anti-save-scum.

```rust
struct ReliabilityDef {
    base_mtbf_s: f64,              // at nominal regime, fully qualified
    infant_mortality: f64,         // Weibull shape < 1 contribution on first use
    qualification_curve: QualCurve,// reliability vs cumulative family test units
    regime_stress: Vec<StressRule>,// multipliers on hazard rate
    failure_modes: Vec<(FailureMode, f64)>, // weights
}

struct PartUsage {               // per part instance, serialized
    operating_s: f64, ignitions: u32, gimbal_cycles: u32,
    max_temp_k: f64, max_axial_n: f64, max_g: f64,
    hazard_integral: f64,        // Λ, accumulated
}

struct FamilyQualification {     // per family, per save, serialized
    flight_units: f64,           // successful operating seconds, weighted by regime coverage
    bench_units: f64,            // from static-fire / test stands
    observed_failures: u32,      // "failure is data too": failures add qual units
}
```

- **Hazard model**: instantaneous hazard `λ(t) = λ_base * Π(stress_i) * qual_factor(family)`.
  Per tick, accumulate `Λ += λ * dt`. Failure occurs when `Λ` crosses a per-instance threshold
  `θ = -ln(U)`, where `U = Rng::for_(RELIABILITY, part_instance_id, 0)` — drawn **once at part
  creation and stored**. Consequences: continuous, warp-safe (Λ integrates in closed form over
  rails intervals), and reloading a save cannot reroll the outcome.
- **Ignition** is a separate Bernoulli event with its own pre-drawn uniform per ignition index:
  `U_k = Rng::for_(IGNITION, part_instance_id, k)`.
- **Stress rules** (data-driven): throttle above rated, burn time beyond `rated_burn_s`,
  temperature fraction of `max_temp_k`, g-load, ullage-negative ignition, restart count beyond
  `ignitions`. Each contributes a multiplier; multipliers are displayed to the player in the editor
  and in flight. Hidden stress multipliers are bad design — the player must be able to reason.
- **Failure modes are mostly non-fatal**: degraded thrust, stuck gimbal, failure to ignite, leak,
  sensor fault, stuck decoupler, solar panel jam. Explosions are reserved for thermal/structural.
- **Qualification programs**: a test stand (facility) burns funds + resources + time to generate
  `bench_units` for a family. Test flights generate `flight_units` weighted by regime coverage, so
  flying only one profile doesn't qualify the engine for all profiles. This is what makes the
  "test part X at regime Y" contract type meaningful.
- **Player-visible number**: per-part "confidence" = `exp(-Λ_expected_for_planned_mission)`,
  shown in the editor as a mission-success estimate. The design AI optimizes against it.

### 3.5 Economy loop

```
contracts/missions ─┐
science milestones ─┼─► funds ─┬─► part manufacture (funds + resources + time)
flight recovery   ──┘          ├─► hires (researchers, crew), salaries (recurring drain)
                               └─► facilities (labs, test stands, pads, colony modules)
resources ◄── colonies/ISRU ◄── surface bases you build & supply ◄── flights
```

- **Funds**: contracts (advance + completion), science milestones, recovery value of returned parts.
  Recurring drain: salaries + facility upkeep. The drain is what makes the RTS layer a game rather
  than a screensaver; tune it so idling at warp is net-negative without being punishing.
- **Resources** (v1 set, deliberately small): `Ore`, `Metal`, `Composite`, `LiquidFuel`, `Oxidizer`,
  `Monoprop`, `ElectricCharge`, `Water`, `Food`, `Oxygen`. Ten. Not thirty.
- **Manufacture**: a craft's build order consumes resources from the launch-site inventory and
  takes sim time. Early parts are cheap in `Metal`; advanced parts need `Composite` which is
  initially import-only → creates the pull into the colony game instead of bolting it on.
- **Colonies**: modules placed on a body produce/convert resources at rates depending on local
  concentration, power, crew, and thermal. Production is **integrated analytically** over a rails
  interval (`amount = rate * dt`, piecewise-constant between events) and publishes
  `ColonyStoreFull`/`Empty` events. Logistics = you fly it, or you build a route (post-v1 automated
  routes with a flat time/cost model, no simulated cargo ships).
- **Crew/life support**: optional consumables (O₂/EC/Food) for crewed ops; toggleable per save.
  Deaths are a setting (off/respawn/permanent).

### 3.6 Contracts

- `ContractTemplate` in data: parameters, generator weights, prerequisites (tech, body visited,
  reputation), reward (funds advance/completion, science, reputation), deadline, failure penalty.
- Generation is seeded from `(save_seed, CONTRACTS, offer_index)` so the offer board is
  deterministic and not re-rollable by reload.
- v1 types: satellite placement (orbit parameters), crew transport, rescue, survey data run,
  part test at regime, base resupply, tourist flight.
- Completion is evaluated by the sim, not the UI, as a predicate over sim state checked on relevant
  events (orbit achieved, docking, recovery) — never polled every tick.

### 3.7 Assets & licensing policy

Clarified ask: *can KSP mod content seed our part/planet library, or is everything authored fresh?*

- **Mod code**: irrelevant — it's C#/Unity. Mods contribute *design lessons*, not code.
- **Mod art/configs**: check each repo's LICENSE. Most KSP part packs are **CC-BY-NC** —
  unusable if the game is ever sold, and murky even free. MIT-licensed assets are reusable
  with attribution; CC-BY-SA and GPL assets are avoided entirely (share-alike contamination
  is not worth it). Keep a `data/ATTRIBUTION.md` from the first borrowed asset.
- **Part concepts aren't copyrightable** — a stackable fuel tank, a bell nozzle, a canard are
  fine to recreate in KSP's spirit. Meshes, textures, and names are not. Own naming throughout.
- **Preferred sources** (license-clean):
  - **CC0**: Kenney (models/UI kit), Quaternius (models), Poly Haven (PBR textures/HDRI),
    ambientCG (PBR), NASA/USGS planetary maps (public domain).
  - **Procedural-first**: terrain, planet surfaces, clouds — generated content is
    license-free forever and fits the fictional system anyway.
- **Policy**: placeholder primitives through M4; first real art pass at M10, built from
  CC0 kitbash + procedural. Any third-party asset lands in `data/` with a license line in
  `ATTRIBUTION.md`; a CI check fails on files lacking attribution entries.
- KSP mod *equivalents* (§4) are reimplemented features, not lifted assets — the only thing
  ever borrowed from a mod is its ideas.

---

## 4. KSP-mod-equivalent features — scoped, with a cut list

The old table was a wishlist. Each row now has a milestone or an explicit "post-v1"/"cut".

| Mod | Built-in feature | Scope |
|---|---|---|
| Kerbal Engineer | Per-stage Δv/TWR/burn-time readouts | **v1 (M3)** |
| MechJeb | Ascent/landing/rendezvous autopilots, maneuver executor | **v1 (M9)** |
| kOS | `csp-script` Rhai API + Rust autopilot services | **v1 (M9)** |
| FAR | Weld-time voxel shielding + analytic Cd/Cl | **v1 (M4)** |
| TweakScale | Part scaling native to part defs | **v1 (M3)** |
| Kerbal Alarm Clock | Alarms + transfer-window planner (falls out of the event queue) | **v1 (M5)** |
| Scatterer | Precomputed-LUT atmospheric scattering | **v1 (R2)** |
| Life support (TAC) | Optional O₂/EC/Food consumables, toggleable | **v1 (M8), minimal** |
| Principia | `NBody` implementation of `OrbitModel` | **post-v1** |
| Parallax | Terrain displacement/scatter | **post-v1 (R3)** |
| CommNet / RemoteTech | Signal network, probe control range | **post-v1** — it gates *control*, i.e. it can break the scripting UX; do not ship it half-done |
| KIS/KAS | EVA surface assembly | **post-v1** — colony modules land as prefab units in v1 |
| Virtual texturing | Planetary surface VT | **post-v1, native tier only** (wasm heap won't take it) |
| kRPC remote | External agent control — **MCP endpoint** on `ControlApi` | **v1 (M9)** |
| KSP web-server mods | Telemetry screens + user-defined widget dashboards | **v1 (M9)** |
| Mod marketplace | — | **cut**. Not a solo-dev feature. Ship a folder and a README. |

**Rejected from the old plan: "the built-in features above are implemented *as* mods against the
public API — dogfooding."** That forces you to design a complete, stable public API before you know
what the game needs, and doubles the cost of every core feature. Instead: core features are
first-class Rust; the script API covers *control and telemetry* only; the data formats (parts,
bodies, tech) are the modding surface. Dogfooding happens where it's cheap (autopilots use
`ControlApi`), not everywhere.

---

## 5. Rendering

Two tiers, declared up front, because this is where "bleeding-edge, all vendors, web" tears.

### 5.1 Tier contract

| Capability | Native tier | Web tier (WebGPU) |
|---|---|---|
| Draw submission | bindless + `multi_draw_indirect` (+count) | per-draw bind groups, CPU-culled instanced draws |
| Material/texture access | binding arrays, partially-bound | texture atlases + fixed slot sets |
| Terrain | virtual texturing, GPU clipmaps, compute-driven LOD | CPU quadtree LOD, pre-baked tiles, reduced draw distance |
| Geometry | mesh/task shaders where available, meshlet culling | vertex pipeline only |
| Lighting | Forward+ clustered, compute culling | simple forward, few lights |
| Post | TAA, hand-rolled upscaler | FXAA/none |
| Atmosphere | precomputed LUTs + volumetric | precomputed LUTs, analytic single-scatter |

Rules:
- `csp-render` exposes a `RenderTier { Baseline, Advanced }` chosen at init from queried
  `wgpu::Features`/`Limits`. **Both tiers must produce a correct image for every scene.** A screenshot
  test per tier in CI; divergence beyond a tolerance fails the build.
- Shaders are authored once with preprocessor branches for the binding model (WESL/naga_oil),
  not duplicated by hand.
- `wgpu::hal` raw-Vulkan passthrough is **not** an escape hatch — using it forfeits Metal, DX12, and
  web. If you genuinely need a feature wgpu lacks, the honest options are: contribute it upstream,
  or make it Advanced-tier-only through a wgpu feature flag. Delete the hal idea from the risk table.
- Mesh shaders: supported in wgpu as an experimental feature (Vulkan full; Metal/DX12 via
  passthrough shaders) as of wgpu 28. Treat as Advanced-tier, opt-in, behind a settings toggle,
  and never on the critical path.

### 5.2 Phases (named R*, mapped to milestones — the old plan's render "M5" collided with the
milestone table's M5)

| Phase | Lands with | Features |
|---|---|---|
| R0 | M0–M2 | Forward HDR, reverse-Z, PBR parts, analytic sky, planet = sphere + heightmap texture, camera-relative transforms |
| R1 | M3–M4 | Depth-precision validation at 10⁷ m, shadow cascades, reentry plasma/particles, editor viewport + GPU picking |
| R2 | M5 | CDLOD/quadtree terrain with per-patch f64 origins, precomputed-LUT scattering, ocean, planet-scale LOD |
| R3 | M7+ | Advanced tier: bindless + indirect, clustered Forward+, compute LOD, TAA |
| R4 | post-v1 | Virtual texturing, meshlet/mesh-shader path, terrain scatter, ray queries where available |

### 5.3 Budgets (so "it's slow" is a failure, not a feeling)

| Scene | Native target | Web target |
|---|---|---|
| 200-part vessel in atmosphere, 1080p | 144 fps, sim step <2 ms | 60 fps |
| 1000-part station in orbit | 60 fps | 30 fps |
| Map view, 500 on-rails objects | 144 fps | 60 fps |
| Rails warp ×10⁵ with 20 colonies | advance ≤4 ms/frame | ≤8 ms |
| Weld (voxelization) of 300-part craft | <500 ms, off the sim thread | <2 s |

---

## 6. Milestones

Resequenced. The old order had three problems: M1 already did what M5 claimed; M3 (editor) secretly
depended on M6/M7 data (costs, upgrades); and M2's point-mass flight model was thrown away by M4.

| # | Milestone | Exit criteria (measurable) |
|---|---|---|
| **M0** | Scaffold & rails | Workspace builds native + wasm in CI; winit+wgpu+egui window; `csp-view` schedules run; integer sim clock; `csp-core` RNG streams; dependency-rule lint active; state-hash harness exists |
| **M1** | Orbit layer | Home planet + moon + 1 outer body on conics; map view; rails warp to ×10⁵ via `advance_to`; SOI transitions solved to 1 tick; event queue with alarms; round-trip elements↔cartesian test <1e-6 |
| **M2** | Flight core (one force model, forever) | Welded body + `SimpleBackend`; thrust, drag (analytic, pre-voxel), gravity, ground contact; prebuilt craft; launch→orbit→reentry→land on a controller; physics dt locked at 1/64; `csp-bench` headless runner replays a command log → state hash; golden replay green |
| **M3** | Editor + part/effective-stat pipeline | Part snap graph with attach nodes; staging; scaling + variants; **`EffectivePart` resolver** incl. stubbed upgrade/wear inputs; live Δv/TWR/burn-time; craft save/load with stable PartIds; GPU picking |
| **M4** | Flight fidelity | Weld-time voxelization → per-part shielding; Mach/Reynolds Cd/Cl; per-part thermal + conduction + reentry heating; structural/thermal breakup with momentum conservation test; **vertical slice complete** |
| **M5** | Interplanetary | Maneuver nodes + executor; Lambert/porkchop transfer planner; encounter prediction; transfer-window events; R2 terrain + scattering |
| **M6** | Multi-vessel & docking | Two vessels in one bubble; docking ports as the first jointed island; station assembly; resource transfer; crew transfer |
| **M7** | Progression | Science data model + soft-cap diminishing returns; labs/researchers with closed-form conversion; tech tree + upgrade tracks wired into the M3 resolver; funds; contracts; recovery |
| **M8** | RTS layer | Colony modules, drills/converters, power, closed-form production under rails warp with store events; manufacture consumes resources + time; supply missions; minimal life support |
| **M9** | Automation, interfaces & reliability | `ControlApi` frozen v1; Rhai host with op budgets; Rust autopilots (ascent/rendezvous/landing); `csp-server` — telemetry WS + widget dashboards + MCP tools on `ControlApi`; `csp-bench` scenario format + first eval suite; reliability hazard model + failure modes + test stands + part-test contracts |
| **M10** | Ship v1 | Save migration path; settings/input rebinding; audio; tutorial/first-flight flow; perf budgets met (§5.3); web tier playable at reduced scope |
| **post** | Expansion | `NBody`, CommNet, LLM assistant, wasmtime mods, EVA assembly, virtual texturing, automated logistics routes |

**Vertical slice = M2+M3+M4**: sandbox, one planet, build → launch → orbit → reentry → land, with the
real flight model. The "am I having fun" gate is M4, not M2 — a point-mass rocket is never fun and
would give you a false negative.

**Reliability is deliberately last among v1 systems.** It is a multiplier on frustration if the
flight model, editor, and economy aren't solid first, and it depends on the usage-tracking that only
exists once parts have modules and missions have length.

**Cut list if the schedule slips** (decide in this order): web tier → playable demo only; M6
docking → post-v1; life support → post-v1; third body → post-v1; R3 advanced tier → post-v1.
v1 can ship as home planet + moon + one neighbour.

---

## 7. Saves, versioning, and migration

Determinism has save consequences the old plan didn't draw out.

- **Format**: snapshot, `serde` + `bincode` (compressed), with a RON debug-dump mode. Authoritative.
- **What is serialized**: sim arenas, sim tick, event queue, RNG counters, part usage, family
  qualification, colony stores, contract board + offer index, tech state, craft files, bubble
  vessel poses/velocities.
- **What is NOT serialized**: any physics-backend internal state (solver caches, broadphase,
  contact manifolds), any derived cache (welded bodies, inertia tensors, aero voxel tables,
  trajectory splines). These are **re-derived on load**. This keeps saves independent of the
  physics backend and of `csp-phys`/Rapier versions, which is the only way T3 ever becomes possible.
- **Determinism boundary at load**: re-derivation must be exact, so re-welding a vessel from its
  part graph must produce bit-identical mass properties. There's a test: save → load → 10k ticks,
  compare state hash against uninterrupted 10k ticks. This test will fail often and each failure is
  a real bug.
- **Versioning**: `save_version: u32` + `game_version` string. Migrations are explicit
  `fn migrate_v{n}_to_v{n+1}` functions chained on load. A save that cannot be migrated is
  *refused with a message*, never silently half-loaded.
- **ID stability**: `PartId` strings are permanent. Renaming or deleting one requires a migration
  entry mapping old → new, or `Deprecated(replacement)`. Data validator in CI fails on an id that
  disappears without a migration.
- **Replays** are a separate artifact: `{ save_seed, initial snapshot, command log }`. Used for
  golden tests, bug reports, and (later) mission sharing. Not a save format.
- **Mod/data divergence**: the save records the hash of the loaded data set. Loading a save with
  changed part data warns and lists what changed. Silent stat changes on existing craft are a
  support nightmare.

---

## 8. Testing & CI

| Gate | What | Runs |
|---|---|---|
| `cargo check` native + `wasm32-unknown-unknown` | never merge a broken wasm build | every commit |
| Dependency-rule lint | `csp-sim` has no bevy/wgpu/winit/rand/std::time; no `HashMap` in state; no `std` transcendentals | every commit |
| Data validators | parts, systems, tech tree: schema, cycles, reachability, id stability | every commit |
| Golden replays (T1) | 6+ scenarios: ascent, reentry, landing, docking, 10⁵ warp jump, colony century | every commit |
| Save round-trip determinism | save/load/compare state hash | every commit |
| Physics cross-check | `SimpleBackend` vs `RapierBackend` on shared scenarios, within tolerance | nightly |
| Cross-platform divergence report | T1 replays on mac-arm64, win-x64 — report, don't fail | nightly |
| Render tier screenshots | Baseline vs Advanced image compare | nightly |
| Perf budgets (§5.3) | benchmark scenes, fail on >10% regression | nightly |

---

## 9. Risks

| Risk | Reality | Mitigation |
|---|---|---|
| Solo scope | M0–M10 is multiple years. This is the dominant risk, not a technical one. | Explicit cut list (§6); M4 is the fun gate; no system starts before its dependencies ship |
| Determinism drift | Floats, `HashMap` order, thread counts, libm, physics-backend versions | T1 tier only (§2.7); golden replays + save round-trip in CI; `SimpleBackend` default to minimise third-party exposure |
| Timewarp correctness | Any subsystem that can't publish a next-event time breaks rails warp | §2.6.1 is a hard architectural rule; add a debug assert that rails advance touched no stepping code |
| "Bleeding edge + web" | These are two renderers | Declared tier contract (§5.1); web is a reduced-scope target, and is the first thing cut |
| Rapier f64 / determinism / wasm | `parallel` unusable on wasm; no cross-version determinism; Jolt is not a viable swap | `SimpleBackend` + parry is the default path; Rapier confined to jointed islands |
| egui ceiling for VAB | egui is fine for panels, wrong for a 3D part-placement canvas | Editor viewport is `csp-render` + GPU picking; egui only for panels/lists. Evaluate in M3 with a real 100-part craft |
| wasm memory ceiling | 32-bit, ~2 GB practical, single-threaded | Web dataset is reduced (fewer bodies, smaller textures, no VT); measured in CI |
| Flight model rewrite | Avoided by building one force model in M2 | M2's model is the final one at lower fidelity, not a throwaway |
| Script API churn | Freezing `ControlApi` too early vs too late | Freeze at M9; before that it's explicitly unstable and versioned |
| Content volume | Parts, biomes, tech nodes, contracts are weeks of authoring | Authoring tools and validators from M3; generated placeholder art until M10 |
| Balance | Science/funds/reliability curves interact | All balance constants in one `data/balance.ron`; a headless "economy simulator" harness that fast-forwards 100 sim-years and plots curves |

---

## 10. Open questions (decide later, non-blocking)

- Rhai vs a kerboscript-flavoured DSL on top of Rhai for the built-in editor's ergonomics.
  (Backend is settled: Rhai. Only the surface syntax is open.)
- Part art pipeline: glTF + custom material extensions is the assumption; confirm in M3.
- Terrain source: procedural noise vs authored heightmaps vs hybrid. Hybrid is likely; decide in R2
  because it determines whether virtual texturing is ever needed.
- Whether low-thrust-on-rails (ion burns under warp) is worth the accuracy caveats. Post-v1.
- Telemetry SPA stack: hand-rolled lightweight widgets vs a real framework build step
  (a Vite/TS app in `apps/` served by `csp-server`). Leaning: real framework — dashboard
  editing UX matters, and you already own the workspace-welcome patterns.
- MCP transport: stdio (agents spawn the binary) vs HTTP+SSE only. Leaning: both — stdio
  for `csp-bench` eval harness, HTTP for connecting to a running game.
- License-audit tooling: a `data/ATTRIBUTION.md` linter in CI vs manual review cadence.
- Reputation as a third currency, or fold it into contract availability. Leaning: fold it in.
- Naming/identity: "Clanker" is a working title — needs a real one before anything public.
