# Clanker Space Program — Design & Implementation Plan

A KSP-style vehicle build/fly sim with a persistent RTS progression layer.
Working engineering document. Opinions here are decisions, not options.
Normative language is deliberate: **must**, **must never**, **refuses**.

Revision: R3 (after REVIEW.md, REVIEW2-opus.md, REVIEW2-normal.md).
Every upstream-status claim carries a dated stamp. Stale stamp = re-verify before relying on it.

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
| Physics | Own `SimpleBackend` default; `rapier3d-f64` (`enhanced-determinism`) confined to jointed islands |
| Multiplayer | None in v1; sim deterministic by construction (fixed integer tick, derived RNG, no wall-clock) |
| Solar system | Fictional, hand-authored, ~1/10 real scale; system defs are data files → swappable later |
| Orbits | `OrbitModel` trait; patched conics v1, N-body as post-v1 module |
| RTS layer | Real-time continuous sim + KSP-style timewarp |
| Web | Desktop-first; `wasm32-unknown-unknown` kept compiling in CI from day one |
| AI | Classical (PEG guidance, design solver, transfer optimizer) v1 + LLM assistant later via scripting API |
| Vessel physics | Single welded rigid body + per-part analytical forces; real joints only for transients |
| Precision | f64 sim world; f32 camera-relative rendering |
| Sim clock | Integer ticks, `TICK_HZ = 64`, dt immutable forever |
| Background thrust | **None in v1.** Confirmed explicitly by the owner. |
| Telemetry SPA | **TanStack Start** + Tailwind + shadcn + custom CSS, in `apps/telemetry/` |
| MCP transport | **Both**: stdio + Streamable HTTP |
| Attribution | `data/ATTRIBUTION.md`, enforced by CI lint |

Non-goals for v1: multiplayer, real solar system, N-body default, native mod loading on web,
cross-platform bit-determinism (T2), mod marketplace, EVA construction.

**Recorded tradeoff (do not relitigate):** REVIEW2-opus #25 recommends a plain Vite + React SPA
instead of TanStack Start, because `csp-server` serves static assets plus a WebSocket and gets no
SSR or server-function value from Start, while Start costs a ~daily-patch-cadence Node toolchain in
a Rust project. The owner keeps TanStack Start for consistency with existing projects and
familiarity; the cost is accepted, versions are pinned exactly (§2.12.2), and the dashboard is
schedule slack (§6.0) so its toolchain can never block the critical path.

### 1.1 Consequences of the settled decisions (read before coding)

These are not relitigations. They are the bills that come due.

- **`bevy_ecs` standalone means you own the loop.** No `bevy_app`, no plugins, no `bevy_render`,
  no `bevy_asset`. You construct `World`, register `Schedule`s, and call `schedule.run(&mut world)`
  yourself from the winit event loop. Dependency rule is enforced in CI (§8.2).
  *(Verified: upstream supports standalone use; `bevy_reflect` is an optional default that can be
  disabled — as of `bevy_ecs` 0.20, 2026-07. MSRV 1.95, recorded in `rust-toolchain.toml`.)*
- **`bevy_ecs`'s multi-threaded executor does not guarantee system *execution* order**, only the
  ordering you declare. That is incompatible with "authoritative state lives in the ECS" plus
  determinism. Therefore authoritative sim state does **not** live in the ECS (§2.2).
- **Rapier/parry are not deterministic across versions, and not cross-platform deterministic
  without `enhanced-determinism`** (which disables fast paths) and without `parallel` (unusable on
  wasm anyway). Determinism tier is defined in §2.7 accordingly.
- **f64 transcendentals (`sin`, `cos`, `pow`, `atan2`) are not bit-identical across
  platforms/libms.** `csp-sim`/`csp-core`/`csp-phys`/`csp-data` must use the `libm` crate
  exclusively, enforced by `clippy.toml` `disallowed-methods` (§8.2). This lint cannot reach
  *inside* parry/nalgebra — see §2.7's T1/T2 caveat.
- **"Bleeding-edge GPU on all vendors" and "runs on the web" are different renderers, not one
  renderer with flags.** WebGPU has no bindless binding arrays, no `multi_draw_indirect`,
  no indirect-count, no mesh shaders, no 64-bit atomics. See §5.
- **wasm32 is 32-bit, single-threaded, no shared-memory atomics in our build, and practically
  capped near 2 GB heap.** Nothing in the sim or the weld pipeline may require a worker thread;
  expensive work is *incremental across frames* on wasm, never "off thread" (§2.4, §5.3).
  Virtual texturing and large terrain caches are native-tier only.
- **Timewarp is the enemy of fixed-timestep determinism** unless warp never changes dt. It doesn't,
  here. See §2.6. And warp level is itself a logged command, never a measured quantity (§2.6.3).
- **The web tier is paid for up front and continuously** (wasm-clean dependency selection,
  no-threadpool constraints, Baseline renderer, Rhai feature parity, wasm CI gate). Cutting it late
  refunds only polish. The cut list (§6.4) reflects that honestly: web polish is *last* to be cut,
  not first.

### 1.2 Project frame: capacity, effort, licensing

- **Capacity (recorded so the risk has an instrument):** 10 h/week. One **effort unit** (EU) in §6
  ≈ 70 h. Full plan = 51 EU ≈ 3 570 h ≈ 6.9 years at capacity. Minimum shippable v1 (§6.4) = 33 EU
  ≈ 2 310 h ≈ 4.4 years. **Conclusion: the cut list is mandatory, not contingency.** A quarterly
  review (§6.5) executes the next cut-list item whenever the previous quarter missed its milestone.
- **Commercial intent (decided, because §3.7 and the asset rules depend on it):** the engine crates
  (`crates/*`) are `MIT OR Apache-2.0`. `data/`, `apps/telemetry/`, and the shipped game binary are
  **proprietary, source-available** under `LICENSE-GAME`. `license` is set per-crate, never
  workspace-wide. Consequence: **CC-BY-NC and share-alike (CC-BY-SA, GPL) assets are unusable.**
  This is now a hard rule, not a conditional one.
- **Localization (decided):** v1 is English-only. `LocKey` is indirection into a single
  `data/loc/en.ron`. Translation, pluralization, RTL, and non-Latin font atlases are post-v1.
  This removes an invisible quarter of work from M10.

---

## 2. Architecture

### 2.1 Crate layout (cargo workspace)

`(new)` = does not exist yet in `crates/`; the dependency lint must still list it so the layout is
enforced the day it appears.

```
Clanker-Space-Program/
├── PLAN.md
├── Cargo.toml                  # [workspace.dependencies] + [workspace.lints] — see §8.2
├── clippy.toml                 # disallowed-methods / disallowed-types — see §8.2
├── rust-toolchain.toml         # pinned toolchain; MSRV 1.95
├── deny.toml                   # cargo-deny bans + license audit
├── crates/
│   ├── csp-core/               # units, SimTime, IDs, f64 math (libm only), RNG KDF,
│   │                           # StateHasher, error types. Depends on nothing below.
│   ├── csp-data/      (new)    # part/system/tech/balance defs (RON), loader + validator,
│   │                           # frozen save schemas + migrations. No sim logic.
│   ├── csp-sim/                # HEADLESS authoritative sim. Plain owned data, no ECS, no wgpu,
│   │                           # no winit, no rand facade, no std::time. Deterministic. wasm-clean.
│   ├── csp-phys/      (new)    # weld (pure fn), collision + rigid-body integration behind
│   │                           # `PhysicsBackend` (parry3d-f64 + own integrator; rapier for joints)
│   ├── csp-view/               # bevy_ecs world: presentation entities, mirrors sim snapshots,
│   │                           # schedule definitions. Owns the sim<->render bridge.
│   ├── csp-render/             # wgpu renderer: camera-relative f32, reverse-Z, terrain,
│   │                           # atmosphere, native GPU-driven tier + web tier
│   ├── csp-ui/        (new)    # egui panels, VAB session model, input bindings, settings
│   ├── csp-app/                # winit shell, frame loop, warp controller, platform glue
│   ├── csp-script/    (new)    # scripting host: ControlApi, Rhai guest everywhere,
│   │                           # wasmtime mod host (native only, post-v1)
│   ├── csp-ai/        (new)    # classical automation: guidance, design solver, transfer
│   │                           # optimizer; LlmAdapter trait
│   ├── csp-server/             # NATIVE ONLY: axum+tokio HTTP/WS server — telemetry screens,
│   │                           # widget dashboards, Streamable-HTTP MCP endpoint
│   └── csp-bench/              # NATIVE ONLY: headless runner. Bins: `csp-bench`, `csp-mcp`
│                               # (stdio MCP over its own headless sim), `csp-replay`
├── xtask/                      # dependency lint, attribution lint, golden blessing, perf log
├── data/
│   ├── balance.ron
│   ├── parts/
│   ├── systems/home/{system.ron, terrin/{body.ron,biomes.png,biomes.ron}, rask/…, duva/…}
│   ├── techtree/
│   ├── contracts/
│   ├── loc/en.ron
│   ├── test-fixtures/          # FROZEN data sets for golden replays (§8.3)
│   └── ATTRIBUTION.md
└── apps/
    └── telemetry/  (not yet created) # TanStack Start SPA (Tailwind + shadcn + custom CSS),
                                # bootstrapped with TanStack CLI; built assets served by csp-server
```

**Dependency rules (CI-enforced by `xtask lint-deps`, which fails loudly on tooling error):**
- `csp-sim` may depend only on `csp-core`, `csp-data`, `csp-phys`, `libm`, `serde`, `blake3`,
  `rand_chacha`, `rand_core`.
- The RNG rule is precise: **no `rand` facade, no `getrandom`, no thread-local or global RNG, no
  `SeedableRng::seed_from_u64`.** `rand_chacha`/`rand_core` *are* allowed, used only through
  `csp_core::rng` (§2.7).
- `csp-sim`, `csp-phys`, `csp-data`, `csp-core` must not depend on `bevy_*`, `wgpu`, `winit`,
  `egui`, `std::time`, `rayon`, `tokio`, or any threadpool.
- `csp-render` must not depend on `csp-sim`. It consumes `csp_render::Frame` built by `csp-view`.
- `csp-view` is the only crate allowed to depend on both `csp-sim` and `csp-render`.
- `bevy_ecs` is allowed only in `csp-view`, `default-features = false`.
- `csp-server` and `csp-bench` must never appear in the dependency tree of `csp-app` for
  `wasm32-unknown-unknown`. Positively asserted, not just excluded (§8.2).

### 2.2 State ownership

Three categories, one direction of truth. Confusing them is the root cause of most determinism bugs.

```
          SimCommand (CommandKey-ordered, §2.2.1)
 UI/scripts/AI/MCP ───────────────────────────────►  csp-sim  (AUTHORITATIVE)
          SessionCommand                                 │ Arc<SimSnapshot> (read-only, per frame)
 UI/MCP ──────────────► session state (csp-ui)            ▼
          (never logged, never hashed)       csp-view (bevy_ecs World, PRESENTATION only)
                                                          │ Frame
                                                          ▼
                                                     csp-render
```

**A. Authoritative sim state** — plain Rust structs in `csp-sim`, in stable index arenas keyed by
`VesselId`, `PartInstanceId`, `ColonyId`, `ContractId`, `CrewId`, `ScriptId`.
- Every ID is a **monotonic `u64` that is never reused**; the next-id counters are serialized (§7.2).
  This is required by the reliability model: `θ` is drawn from `(RELIABILITY, part_instance_id, 0)`,
  so a reused id would hand a new part a dead part's failure threshold.
- **Iteration that affects state must be in ascending ID order**, never in arena dense/slot order
  (dense order is a function of removal history, not of current state) and never over a hash map.
- **`HashMap`/`HashSet`/`std::time` are banned** in `csp-sim`/`csp-core`/`csp-phys`/`csp-data`.
  Use `BTreeMap` or sorted `Vec`. CI lint (§8.2).
- **No code mutates sim state except `csp_sim::World::step_frame()`**, driven by commands and
  scheduled events. CI test: a debug build asserts that every state write is reached from a command
  handler or an event handler.
- `active_vessel: Option<VesselId>` is **sim state** (it gates control authority), changed only by
  command.

**B. Presentation state** — `bevy_ecs` components in `csp-view`. Derived from snapshots every
frame. Never read by the sim. Discarded freely.

**C. Session state** — editor working craft, camera, selection, map focus, dashboards, settings,
keybinds. Lives in `csp-ui` / config files. **Not sim state, not in the command log, not in the
state hash, not in the save.** It has its own `SessionCommand` channel, used by egui and by
session-class MCP tools alike.
This is what makes `editor.build` honest: the VAB working craft is session state, and committing it
(`SimCommand::ManufactureCraft { craft_hash, … }`) is the sim mutation. Replay determinism is a
property of the `SimCommand` log; session traffic is irrelevant to it.

Why not put everything in the ECS: determinism, savegame stability, headless reuse (tests, replays,
AI batch runs), and not being married to bevy_ecs's serialization story. The cost is a mirroring
layer in `csp-view`; pay it. Corollary: dropping `bevy_ecs` later is cheap — that's the point.

#### 2.2.1 Command ordering — total, and independent of wall clock

Replay requires a **total order** over commands within a tick. Four producers (Rhai scripts, Rust
autopilots, UI, external MCP/WS) cannot be ordered by arrival time, because arrival time is wall
clock and wall clock is banned from the sim.

```rust
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct CommandKey {
    pub tick: u64,
    pub source_class: SourceClass, // Script=0 < Autopilot=1 < Ui=2 < External=3
    pub source_id: u32,            // ScriptId / AutopilotId / 0 for UI / client index
    pub source_seq: u64,           // per-source monotonic, serialized
}
```

Rules:
- **Commands are stamped at the tick they are *dequeued*, never at the tick they were produced.**
  Stale or future stamps are impossible by construction; there is no "command for an already-executed
  tick" case to handle.
- The sim drains the queue in `CommandKey` order at the start of each tick boundary (and at the
  start of each `advance_to` interval).
- The **command log** (`Vec<(CommandKey, SimCommand)>`) is the replay artifact (§7.6).
- `source_seq` counters are serialized. Reordering `SourceClass` is a replay-breaking change and
  requires a `HASH_SCHEMA_VERSION` bump.

**All control commands must be absolute, never relative.** `SetThrottle(f64)`,
`SetPitchInput(f64)`, `SetWarp(level)` — never `AdjustThrottle(+Δ)`. Rationale: at 144 fps ~55% of
frames produce zero ticks and at 20 fps a frame is 3.2 ticks, so a per-frame delta command makes
control rate depend on frame rate. Rate-based input (held key, egui drag) is integrated in
`csp-app` against `ticks_consumed` **before** becoming a command. Reviewing a new `SimCommand`
variant for relative semantics is a mandatory review checklist item.

**Control lease.** Each vessel has exactly one `control_lease: Lease` where
`Lease = Player | Script(ScriptId) | Agent(ClientId)`. Commands affecting a vessel's control from a
non-leaseholder are **rejected** with an error delivered on the originating channel; they are not
silently dropped and not logged as state changes. `SimCommand::TakeControl` transfers the lease;
`Player` preempts unconditionally. Stated plainly: **determinism is a property of the command log,
not of a live multi-client session.** Without the lease, two clients setting throttle in one tick
produces a last-writer-wins race that users will report as a determinism bug.

**External rate limits.** Each external client gets ≤32 commands per tick and a 64 KiB max message
size, with a bounded ingress queue. Overflow returns an explicit backpressure error to the client.
The drop/defer decision is a **queue rule**, never a timing accident, so it is reproducible.

### 2.3 Precision, coordinates, depth

- **Sim**: `f64`, origin = system barycenter, SI units (m, kg, s, N, K, W, Pa). Angles in radians.
  Celestial positions from analytic ephemerides (Kepler elements + epoch).
- **Sim clock**: integer ticks. `TICK_HZ = 64` (sim seconds per tick = 1/64 exactly).
  `SimTime(u64 ticks)`. Never an `f64` accumulator. Epoch 0 = game start (UT seconds = ticks/64).
  The clock lives in `csp_sim::World` and nowhere else.
- **Physics bubble**: radius **20 km** (`balance.ron: bubble_radius_m`), with 10% hysteresis
  (vessels go on rails at 20 km, rejoin at 18 km). A vessel is in the bubble iff it is within the
  radius of `active_vessel` or is surface-latched within it (§2.6.4).
- **Bubble recentering**: the bubble origin snaps to a **1 km integer lattice**:
  `origin_km = (pos_m / 1000).floor() as i64`, recentre when the active vessel's lattice cell
  changes. Consequences: the shift is exactly representable in f64, the transform is exact for the
  mantissa range in play, and the threshold test is an integer comparison — not a
  float-compare-sensitive event. A float distance threshold would make recentering a
  state-dependent rounding event, which it must never be.
- **Rendering**: per frame, positions converted to `f32` relative to the camera. GPU never sees f64.
  Terrain patches carry a per-patch `f64` origin baked to `f32` locals at build time.
- **Depth**: reverse-Z, `f32` depth, infinite far plane (`[0,1]`, 1 = near). Single frustum; if
  precision cracks appear beyond ~10⁷ m, add a second far-field pass rather than log-depth
  (log-depth breaks early-Z and interpolation). Decided empirically in R1 by the measurable test in
  §5.4.
- **Scale check**: ~1/10 real means home radius 600 km, home SOI 84 Mm, system extent ≈ 2×10¹⁰ m.
  f64 gives sub-µm resolution at 10¹¹ m. f32 at 10⁷ m gives ~1 m — camera-relative is mandatory,
  not an optimization. Full body parameters: §3.0.

### 2.4 Vessel model

- **Weld** is a **pure function** with no threads, no hash maps, and no I/O:
  `csp_phys::weld(part_graph, &[EffectivePart]) -> WeldedBody`. It is split into two stages with
  different budgets and different authority:
  - **Stage 1 — mass properties & force sites (cheap, synchronous, exact).** Total mass, CoM,
    inertia tensor, per-part `ForceSite { local_pos, local_dir, kind }`, merged convex-hull
    compound collider. Must be bit-identical for a given `(graph, effective_parts)`; a property
    test asserts invariance under part-ordering permutation. **Runs on the sim thread, always.**
  - **Stage 2 — aero tables (expensive, deferred).** §2.4.1.
- **Who owns the weld.** `csp-app` may compute Stage 2 on a worker thread (native) or incrementally
  across frames (wasm). Its result enters the sim only as
  `SimCommand::InstallAeroTables { graph_hash, config_mask, tables }`, stamped at the tick it is
  dequeued. The sim **refuses to launch** a craft whose Stage-2 tables for `ConfigMask::default()`
  are not installed, and refuses to enter atmosphere-bearing flight without them. Debug builds
  byte-compare a worker-computed table against a sim-thread recompute. This is how "<500 ms off the
  sim thread" and "deterministic data path" coexist.
- **Inertia cache has no time trigger.** Mass properties are recomputed when the **quantized**
  resource mass changes, where each tank's contents are quantized to 1/1024 of capacity. There is
  **no** "every N ticks" trigger. Rationale: a time-or-threshold trigger makes the cache a function
  of history, so a reloaded save derives a fresh exact tensor while the live run holds a stale one,
  and §7.5's save→load→N-ticks hash test diverges *by design*. **Every derived cache in this project
  must be a pure function of currently-serialized state.** No exceptions.
- **Runtime**: one rigid body per vessel; all per-part forces are analytic and summed into
  (force, torque). No per-part joints. This kills the KSP1 joint kraken at the architecture level.
- **Joints are transients only.** Live joints exist for: docking soft-capture (≤ a few seconds),
  robotic hinges/pistons, and decoupler-failure cases. Hard cap **16 live joints per bubble**.
  - Editor-time: a design exceeding the cap is refused.
  - Runtime: joint creation beyond the cap **degrades to an immediate clean separation (no joint)**
    and logs a `Recoverable` error. The behaviour is defined, not undefined.
- **Docking rigidizes** (both reviews agree; this is the resolution):
  1. Approach: the two bodies are a jointed island solved by `RapierBackend` for the soft-capture
     window (≤ `balance.ron: capture_window_ticks`, default 192 = 3 s).
  2. Latch: the two part graphs **merge**; Stage-1 weld runs synchronously on the merged graph,
     producing **one** `WeldedBody` with a recorded `DockSeam { port_a, port_b, latch_tick }`.
     The island collapses; the constraint solver stops running.
  3. Undock: the graph splits at the seam via the breakup path below.
  The 16-joint cap therefore constrains *transients*, not station size. A 1000-part station is one
  rigid body with ~30 recorded seams. M6's exit criteria include the merge test.
- **Breakup**: structural/thermal failure splits the part graph at the failed node → two Stage-1
  welds, momentum distributed from the parent's rigid-body state. Child bodies **inherit the
  parent's aero tables** until a Stage-2 recompute lands (documented approximation; a reentry
  breakup cascade must not stall the tick).
  Conservation test, with the denominator defined so near-zero cases are testable:
  - linear: `‖Δp‖ / max(‖p_before‖, m_total · 1 m/s) < 1e-9`
  - angular: `‖ΔL‖ / max(‖L_before‖, I_max · 1e-3 rad/s) < 1e-9`

#### 2.4.1 Aero: reconciling "voxelized" with "analytic"

Two phases, no contradiction. The *structure* survived both reviews; the *parameters* did not.

1. **Weld time (Stage 2, expensive, once per design × configuration)**:
   - **Isotropic voxels of fixed edge length** `target_voxel_m` (default 0.15 m), **not** 64³ over
     the AABB. 64³ over a 2 m × 2 m × 40 m rocket gives 0.03 × 0.03 × 0.63 m voxels — anisotropic
     in exactly the axis that determines frontal area. Total voxel count is capped at
     `voxel_budget` (default 4 × 10⁶); on exceeding it, edge length doubles until it fits and a
     warning is logged and shown in the editor.
   - **Voxelize once in body frame; sweep rays in rotated frames.** Never re-voxelize per direction
     (26–66 re-voxelizations is where the budget dies). The traversal is a deterministic 3D-DDA
     sweep over the voxel grid in a fixed axis order.
   - **Direction set: a geodesic sphere of 66 directions** (icosahedron, 1 subdivision), not 26 cube
     face/edge/corner directions. 26 directions is 45° resolution, and pitch/yaw **stability
     derivatives** (`Cm_α`) are obtained by differencing this table — 45° quantization makes SAS and
     the autopilot oscillate. Interpolation is **barycentric over the triangulated direction hull**,
     which is well-defined for a geodesic set and is not well-defined for 26 scattered points.
   - Output per part: `AeroTable { area[66], shield[66], ref_len }`.
   - **Per configuration.** Fairing deployment, cargo-bay doors, landing gear, and solar panels
     change shielding at runtime without changing the part graph. Stage 2 therefore produces a table
     **per `ConfigMask`** (bitset over deployable/jettisonable parts), computed lazily on first use,
     capped at 8 masks per craft (editor warns beyond). Unknown mask → nearest computed mask by
     Hamming distance, with a logged warning. One table would leave a jettisoned fairing's payload
     permanently shielded.
   - **Skippable.** Aero only matters in atmosphere. A craft assembled in orbit around an
     atmosphere-free body skips Stage 2 entirely — that is also the performance answer for stations.
2. **Runtime (cheap, every tick)**: given body-frame airflow direction, barycentric-interpolate
   per-part area/shield, apply analytic Cd/Cl curves vs Mach and Reynolds, sum forces at the part's
   position. Control surfaces add a deflection-dependent term.

**Aero tables are never serialized in the save.** They are re-derived on load (§7.3). An optional,
**deletable, non-authoritative** side cache may live in
`$cache_dir/csp/aero/<part_graph_hash>-<config_mask>-<voxelizer_version>.bin`. It never enters the
state hash and the game is correct with the directory deleted. §2.4.1 previously said "serialized
with the craft"; that sentence is deleted. Craft files store the `part_graph_hash` only.

### 2.5 ECS usage and schedule ordering

`csp-view` owns a `World` and these schedules, run explicitly by `csp-app`, **in this order**:

| # | Schedule | When | Contents |
|---|---|---|---|
| 1 | `Input` | once/frame | winit events + external ingress → `SimCommand`s/`SessionCommand`s; camera input |
| 2 | `SimStep` | once/frame | **a single system** calling `csp_sim::World::step_frame(target_tick)` (§2.6.2) |
| 3 | `Sync` | once/frame | `Arc<SimSnapshot>` → presentation components; mirrors the clock into a presentation-only `Clock` |
| 4 | `Presentation` | once/frame | interpolation, camera solve, LOD selection, VFX, audio cues |
| 5 | `UiBuild` | once/frame | **the entire egui pass**; emits commands for *next* frame's `Input` |
| 6 | `RenderExtract` | once/frame | build `csp_render::Frame` (immutable) from presentation components |

Rules:
- `Input` runs **before** `SimStep`. Always. (The scaffold currently inverts this — Appendix A.)
- egui runs **inside `UiBuild`**, never inline in the renderer, and never by reading
  `view.world` directly from `csp-app`. (Appendix A.)
- Executor: `SimStep` and `Sync` use the single-threaded executor always. `Presentation` and
  `RenderExtract` may use the multi-threaded executor on native. Required features, declared now so
  the divergence is explicit rather than discovered in R3:
  - native: `bevy_ecs` features `["std", "multi_threaded", "async_executor"]`
  - wasm: `["std"]`
  If the parallel executor is never actually enabled, §2.5's own escape hatch ("dropping bevy_ecs is
  cheap") becomes the obvious call — that is a feature of the design, not a surprise.
- `Sync` and `Presentation` may never write to `csp-sim`. Enforced by only handing them
  `&SimSnapshot`.
- UI-issued commands take effect on the *next* tick — one-tick latency, accepted, identical for
  scripts and agents.
- **Interpolation (reworded; "disabled at high warp" was vacuous):**
  - ×1 and physics warp ×2–×4: `Presentation` interpolates bubble bodies between ticks N−1 and N
    using the frame's fractional tick remainder. Never extrapolates.
  - Rails warp: there are no ticks to interpolate between, and a frame can cover ~18 sim-minutes.
    Map-view presentation therefore evaluates `OrbitModel::state_at(presentation_tick)` directly,
    where `presentation_tick` is a **separate, non-authoritative** f64 value that may lie between
    the pre- and post-advance clock. `state_at` must be cheap and pure (it is, for conics).
    Snapping an orbit map by 18 sim-minutes per frame is not acceptable, and warp is the exact
    situation in which the player is looking at the map.

**Frame loop (csp-app):**
```
winit RedrawRequested:
  dt_wall = clamp(now - last, 0, 100ms)        // wall clock ONLY here, never in sim
  warp_controller.observe(last_step_ms, dt_wall)
  if let Some(level) = warp_controller.decide() { enqueue SimCommand::SetWarp(level) }
  target_tick = warp_controller.target_tick(sim_snapshot.tick, dt_wall)   // integer
  run Input                                    // drains external ingress, stamps CommandKeys
  run SimStep                                  // step_frame(target_tick) — one call, always
  run Sync, Presentation, UiBuild, RenderExtract
  csp_render.render(frame)
```

### 2.6 Timewarp — the real design

**Invariant: the physics dt is immutable at 1/64 s for the entire life of the project.**
Warp never changes dt. Changing dt would make trajectories depend on the player's warp choices,
which destroys replay, golden tests, and any future netcode. **This is the single most important
rule in this document.**

#### 2.6.1 Corollary: subsystems advance by *interval*, never by tick

The previous revision integrated colonies, research, and reliability per tick at ×1 and in closed
form under rails. Two integrators over the same interval do not agree, so warp choice changed
outcomes — violating the headline rule the section is built on.

**There is exactly one integrator per subsystem, and it is the interval-based closed-form one.**
- The **tick loop drives only the physics bubble**: forces, contacts, control, scripts.
- **Everything else** — orbits on rails, colony flow network, research conversion, reliability
  hazard, life support, manufacturing, construction — advances by closed-form integration over the
  interval `[t_now, t_next]`, at ×1 exactly as under rails warp.
- **Subsystem advancement is location-independent.** A lab, drill, or life-support module advances
  by interval whether its vessel is in the bubble at 256 ticks/s or on rails. The bubble's tick loop
  never touches them. One sentence; it kills a whole class of bugs.
- **Test (mandatory, per-commit):** run 10 sim-days from the same save at ×1 and at ×10⁵; the state
  hashes must be **identical** except for bubble contents (the bubble is excluded by a documented
  mask). This test is the enforcement mechanism for this entire section.

#### 2.6.2 The single advance function

```
World::step_frame(target_tick):
  loop {
    drain command queue in CommandKey order; apply
    t_next = min(target_tick, event_heap.peek_tick(), bubble_tick_budget_limit)
    if bubble non-empty { for t in now..t_next { step_bubble_tick() } }     // integer ticks
    advance_interval_subsystems(now, t_next)                               // closed form
    clock = t_next
    fire all events at t_next (may insert new events)
    if clock == target_tick || budget_exhausted { break }
  }
  return AdvanceReport { reached_tick, events_processed, partial: bool }
```

Same code path at ×1 and at ×10⁵. Under rails the bubble is empty, so the loop is a jump.

Three regimes:

| Regime | Rate | Mechanism | Allowed when |
|---|---|---|---|
| **Real time** | ×1 | 64 ticks/s | always |
| **Physics warp** | ×2 | 128 ticks/s — *more CPU work, same dt* | bubble occupied; demoted by the controller |
| **Rails warp** | ×10 … ×10⁵ | no bubble ticks; analytic jump | no bubble-resident vessel under thrust, unsettled contact, or in atmosphere |

- **Physics warp is capped at ×2 in v1.** Arithmetic: §5.3 budgets ≤0.5 ms per tick for a 200-part
  vessel on REF-A. ×2 = 128 ticks/s = 64 ms of sim CPU per wall second. At 144 fps (6.9 ms frames)
  that is 0.9 ticks/frame ≈ 0.45 ms — comfortable. ×4 = 256 ticks/s = 1.8 ticks/frame ≈ 0.9 ms,
  which is satisfiable only if the per-tick budget holds, so **×3/×4 are unlocked only when the
  measured per-tick cost on the reference machine is ≤0.5 ms for the current scene**. Advertising
  a fixed ×4 would be the same dishonesty the plan already killed for ×8.
- **Max rails warp is ×10⁵.** At ×10⁷ and 60 fps a single frame covers ~1.9 sim-days (≈116 sim-days
  per wall second) — no contract deadline, SOI entry, or store event resolves usefully and players
  overshoot everything. ×10⁵ ≈ 1.1 sim-days per wall second, ~18 sim-minutes per frame at 60 fps.
  Revisit only with data.
- **No background thrust in v1** (owner-confirmed). Rails warp is refused while any vessel has
  nonzero throttle. **Throttle is forced to zero when a vessel leaves the bubble**, so decoupled
  debris and dead scripts cannot wedge warp forever; and the warp-refusal UI names the offending
  vessel and offers "zero its throttle". Post-v1: low-thrust-on-rails via Edelbaum/averaged
  elements, clearly marked as a separate feature with its own accuracy caveats.

#### 2.6.3 Warp level is a command, never a measurement

The warp controller lives **entirely in `csp-app`**, observes wall-clock frame cost, and emits
`SimCommand::SetWarp(level)` and `SimCommand::AdvanceTo(target_tick)`. The sim **never measures
anything**. The replay harness replays the logged commands and never re-derives them.

Without this rule, measured frame time feeds sim state and T1 replay is broken for any session that
ever demoted — on a slower machine the same log produces a different world.

Demotion rule, numerically: warp level is the largest `k` in the allowed ladder such that
`measured_step_ms * TICK_HZ * k < budget_fraction * frame_budget_ms`, with `budget_fraction`
defaulting to 0.5, and **hysteresis of 1.5×** (promote only when the inequality holds with 1.5×
margin for 30 consecutive frames) to prevent oscillation.

#### 2.6.4 Surface rails — landed vessels must not block warp

Rails warp forbidden on "contact" means you cannot warp on the pad, cannot warp while a rover sits
at a colony, and cannot warp through a surface construction timer — i.e. the RTS layer is
unreachable from where the player spends most of their time. That is unacceptable, and it does not
require breaking the no-thrust rule.

**Surface rails:** a vessel whose `‖v‖ < settle_v` (default 0.05 m/s) and `‖ω‖ < settle_w` for
`settle_ticks` (default 128 = 2 s) while in contact is **latched to a body-fixed frame**:
- It leaves the bubble, its contacts are released, and the physics backend holds no state for it.
- Its pose is stored as body-fixed `(lat, lon, alt, orientation)` and is exactly reproduced on
  unlatch. No integration occurs; no drift is possible.
- It **does not block rails warp** and **does not require the no-thrust rule to bend**: a latched
  vessel's throttle is zero by the §2.6.2 rule, and any throttle command unlatches it first.
- Unlatch triggers: a control command, a `SimCommand::Unlatch`, the vessel becoming the active
  vessel with a crewed/controlled input, or a nearby impact event.
- **This is also what makes saving possible while landed** (§7.4): a latched vessel has no live
  contacts, so the contact-state exclusion does not forbid the most common save situation.

#### 2.6.5 The event queue, event classes, and back-pressure

`csp-sim` owns one `BinaryHeap<ScheduledEvent>` keyed by `(tick, event_seq)`; `event_seq` is a
serialized monotonic counter making ordering total under ties.

Rule: **any subsystem with nonlinear or thresholded behaviour must publish its next event time.**
A subsystem that can only be computed by stepping is a bug under rails warp.

**Subsystem query/advance order is a constant array in `csp-sim`**, because `event_seq` is assigned
at insertion and therefore depends on it:
`[Orbits, Vessels, Power, Colonies, LifeSupport, Research, Manufacturing, Construction,
Reliability, Contracts, Scripts, Alarms]`.
Reordering this array is a **replay-breaking and save-breaking change** and requires a
`HASH_SCHEMA_VERSION` bump.

**Two event classes, carried in the enum so the policy is not sprinkled across subsystems:**

| Class | Behaviour | Members |
|---|---|---|
| `Clamping` | auto-demotes warp so the player cannot overshoot | focused/tracked vessel `SoiTransition`, `ManeuverNodeStart`, `PlayerAlarm`, `ContractDeadline`, `ResearchProjectComplete`, `LifeSupportDepleted`, `ScriptWake`, `Insolvent`, `ConstructionComplete` |
| `Silent` | processed inside `advance_to`, never touches warp | any other vessel's `SoiTransition`, `ColonyStoreFull`/`Empty`, `ReliabilityInspectionDue`, `PartFailure`, `TerminatorCrossing`, `DebrisDecay` |

Per-object opt-in ("track this vessel") promotes that object's events `Silent → Clamping`.
Without this split, 20 colonies and 500 on-rails objects leave warp perpetually clamped — the
feature becomes unusable at exactly the scale it exists for.

**Back-pressure (the real warp limiter).** At ×10⁵ a frame covers ~18 sim-minutes, so any subsystem
with a sub-18-minute characteristic period generates ≥1 event per frame per object. The budget is
therefore stated in **events, not milliseconds**:
- `advance_to` processes at most **2000 events per frame** (`balance.ron`), checked **between
  same-tick batches** — the loop stops only at a tick boundary. A single tick's due batch is
  **atomic against the budget**: "fire ALL events at `t_next`" is verbatim spec, and a half-fired
  tick left in the heap is exactly the corrupt mid-state back-pressure exists to avoid.
- The atomic unit is itself bounded: one tick's batch fires in chunks of at most **1024 events
  per loop iteration**. A larger same-tick batch *defers its tail* — the remaining events stay in
  the heap at the clock and later iterations flush them zero-advance (each still fires at its
  due tick, in `(tick, seq)` order). While a deferred tail is pending, the tick's command drain
  is postponed: a tick value still hosts exactly one drain, at the departing iteration once the
  batch clears. A ceiling-exceeding batch does NOT by itself produce `partial` — a batch in
  (1024, 2000] defers its tail without tripping the frame budget; `partial` and the warp demotion
  come only from exceeding the *frame* budget (which a pathological pile-up reaches quickly, since
  deferred batches keep counting events against it). The ceiling bounds the atomic work per
  iteration, it never drops or re-ticks events.
- On exceeding the budget it **returns early at the tick reached** with `partial: true`. Warp
  becomes self-limiting instead of frame-stalling.
- Periodic subsystems must publish **coarse** events (next store-full, not next drill cycle).
  Publishing a per-cycle event is a code-review rejection.
- A nightly golden scenario runs at **pathological event density** (20 colonies × 6 converters ×
  small buffers + 400 debris objects), not just "colony century".

#### 2.6.6 SOI transition solving

`|r_vessel − r_body| − r_soi` is not monotonic, and a grazing pass can enter and exit within one
interval, so bisection on an unbracketed interval misses encounters. "Solved to 1 tick" is a
precision claim and needs a measurement definition.

Specification:
1. **Coarse scan** at stride `h` chosen so relative motion over `h` is `< 0.1 · r_soi`:
   `h = 0.1 · r_soi / v_rel_max` (ceil to ticks, min 1 tick).
2. **Sign-change bracketing** on the scan samples.
3. **Brent's method** on each bracket, to `|Δt| ≤ 1 tick`, iteration cap 64.
4. **The real guarantee, stated as such:** encounters whose entire SOI residence is shorter than
   `h` may be missed. `h` is reported in the debug overlay. Increasing `v_rel_max` fidelity reduces
   `h`; the guarantee is not "never misses", it is "never misses a residence longer than `h`".
5. Test: a deliberate grazing hyperbolic pass with residence just above and just below `h`,
   asserting detection and non-detection respectively.

### 2.7 Determinism tier (be precise about what's promised)

| Tier | Promise | How |
|---|---|---|
| **T1 — Replay** (v1 target) | Same binary + same platform + same save + same command log ⇒ identical state hash | fixed tick, integer clock, no hash-map iteration, ascending-ID iteration, own RNG KDF, `libm` only in our code, no thread-count-dependent float ops, warp as logged commands |
| **T2 — Cross-platform** (post-v1, only needed for netcode) | Same version, different OS/CPU ⇒ identical | additionally: `enhanced-determinism` on parry/rapier, no `parallel`, no SIMD-width-dependent paths, **and a `libm`-only collision path** — see caveat |
| **T3 — Cross-version** | **explicitly not promised** | saves store versions; golden hashes regenerate on backend/version bump with a changelog entry |

**T1/T2 caveat, stated so the nightly report is interpretable:** the `disallowed_methods` lint
covers *our* crates only. `parry3d-f64` and `rapier3d-f64` call platform float math internally.
T1 is unaffected (same binary, same platform). **T2 additionally requires a `libm`-only fork or a
float-math-free collision path; until that exists, the nightly cross-platform divergence report is
expected to show drift in contact scenarios and is informational for non-contact scenarios only.**
Writing this down is the difference between a useful report and a report everyone ignores.

Third-party determinism feature set, pinned now (exact `=` versions in `[workspace.dependencies]`):
`parry3d-f64` and `rapier3d-f64` with `default-features = false`, `enhanced-determinism` **on**,
`parallel` **off**, `simd8`/`simd-nightly` **off** (mutually exclusive with enhanced-determinism).
Note `enhanced-determinism` pulls `indexmap` + `simba/libm_force` — upstream agrees the *default*
build has iteration-order and transcendental hazards.
Opportunity, with a condition: `parry3d_f64::transformation::voxelization` can serve §2.4.1, but its
output may only feed sim state after auditing it for hash-map/`HashablePartialEq` use.

CI runs T1 golden replays on every commit (linux x86_64). A nightly job runs the same replays on
macOS-arm64 and windows-x86_64 and **reports** divergence without failing the build. Do not let
anyone turn this into a gate; it is the cheapest high-value instrument in the plan.

**RNG — own the key derivation.**
```rust
pub struct StreamKey { pub tag: StreamTag, pub entity: u64, pub counter: u64 }
pub fn rng(save_seed: u64, k: StreamKey) -> ChaCha8Rng;   // ChaCha8Rng::from_seed(kdf(..))
```
- `kdf` builds the full **32-byte** ChaCha key itself: `BLAKE3(KDF_VERSION_LE || save_seed_LE ||
  tag_LE || entity_LE || counter_LE)` truncated to 32 bytes. `KDF_VERSION: u8` is recorded in the
  save header.
- **`SeedableRng::seed_from_u64` must never be used.** Its output is a `rand_core` implementation
  detail; a dependency bump would silently change every pre-drawn reliability threshold in every
  existing save. The scaffold's splitmix-fold-into-`seed_from_u64` also throws away 192 of 256 seed
  bits. Both are fixed in Appendix A.
- A unit test asserts hard-coded expected outputs for a fixed key set, so a dependency bump fails
  loudly instead of silently rewriting history.
- **Semantics, stated because the reliability model depends on it:** this scheme *reseeds per key*;
  it does not advance a stream. "Counters are part of the save" means **we store an index**, not a
  stream position. So `U_k = rng(seed, {IGNITION, part_instance_id, k})` is reproducible from the
  index alone, and the save stores `next_counter` per `(tag, entity)` only where a monotonic index
  is needed (e.g. contract offer index).
- Fixed arity everywhere: contract generation is `{tag: CONTRACTS, entity: 0, counter: offer_index}`.
  The 3-tuple spelling used previously is deleted.
- Consequence: reloading a save and re-flying does **not** reroll an engine failure — anti-save-scum
  by construction, and required for replay.

### 2.8 Orbit layer

```rust
pub struct OrbitState { pub parent: BodyId, pub elements: KeplerElements, pub epoch: SimTime }

pub trait OrbitModel {
    fn state_at(&self, s: &OrbitState, t: SimTime) -> (DVec3, DVec3);   // cheap, pure
    fn advance(&self, s: &mut OrbitState, to: SimTime);
    /// Must return the earliest SOI change in (now, horizon], per §2.6.6.
    fn next_soi_event(&self, s: &OrbitState, now: SimTime, horizon: SimTime) -> Option<SoiEvent>;
}
```

- `PatchedConics` (v1): bodies and on-rails vessels on Keplerian rails inside their SOI.
- **Universal-variable solver, specified concretely.** "Handles e≈1 and hyperbolic without branching
  bugs" overstates it: universal variables remove *branching*, not *conditioning*. The spec is:
  1. For elliptic orbits, reduce `Δt` modulo the period **before** solving. Under rails warp `Δt`
     can be enormous.
  2. Stumpff functions `C(z)`, `S(z)` by closed form for `|z| ≥ ε` (ε = 1e-6) and by **series
     expansion** for `|z| < ε`, to avoid catastrophic cancellation near `z ≈ 0`.
  3. **Laguerre–Conway iteration with n = 5** plus bracketing, not bare Newton (Newton diverges for
     large `Δt` and near-parabolic orbits).
  4. Iteration cap 64. On non-convergence: return `SimError::Recoverable`, log the elements, and
     fall back to the previous valid state for that tick. Never silently return garbage.
- **Round-trip budget, restated to be defined for all conic types:** cartesian → elements →
  cartesian over **one period (elliptic) or Δt = 10⁷ s (parabolic/hyperbolic)** must have relative
  position error `< 1e-6` and relative velocity error `< 1e-6`. ("Over one orbit" is undefined for
  open orbits.)
- Property test: `e ∈ {0, 0.5, 0.9, 0.999, 1.0, 1.001, 3.0}` × `Δt` over 12 decades, asserting the
  round-trip budget.
- `NBody` (post-v1): symplectic integrator + cached trajectory spline; `next_soi_event` becomes a
  search over the spline. The trait is already shaped for it — that is why `next_soi_event` exists
  rather than a step-and-check loop.

### 2.9 Physics backend

```rust
pub trait PhysicsBackend {
    fn step(&mut self, bodies: &mut [WeldedBody], joints: &[JointDef]);   // dt is always 1/64
    fn cast_ray(&self, ray: &Ray, max_toi: f64) -> Option<Hit>;
    fn contacts(&self, body: BodyHandle) -> &[Contact];
}
```

- **`SimpleBackend` (default, v1).** Integrator is **pinned: semi-implicit (symplectic) Euler** for
  the body state. RK4 is explicitly rejected: it costs 4× the force loop and is wrong anyway with
  discontinuous contact forces. The previous "semi-implicit Euler / RK4" wording named two
  integrators and chose neither.
- **Contact must be unconditionally stable at 1/64 s**, because the immutable-dt rule makes contact
  substepping illegal. A penalty spring cannot hold a 20 t lander on gear at 64 Hz. Specification:
  **projected Gauss–Seidel impulse solve** over the contact manifold (8 iterations, fixed order by
  ascending `(body_id, contact_index)`), Baumgarte position stabilization (β = 0.2), restitution 0,
  Coulomb friction with a fixed-order two-direction friction basis, and the **settle latch** of
  §2.6.4 as the terminal state.
- **M2 exit criterion for this:** a 20 t lander rests on gear for 10 000 ticks with `‖v‖ < 1e-6`,
  no visible jitter, and a state hash that becomes constant once latched.
- **`RapierBackend`**: `rapier3d-f64`, used for jointed islands (soft-capture, robotics) and as a
  test oracle. `parallel` off, `enhanced-determinism` on.
- **Backend hand-off semantics (previously unspecified; it is a state discontinuity):**
  - `Simple → Rapier` (dock approach, robotics activation): bodies are handed over with pose,
    linear and angular velocity only. Warm-start impulses start at zero. The transition tick is
    recorded in the event log and is a known, documented discontinuity.
  - `Rapier → Simple` (latch/undock/robotics idle): the merged or split `WeldedBody` takes the
    island's aggregate momentum, conserved to the §2.4 tolerances.
  - `Bubble → surface rails` and back: §2.6.4.
  - Because these are discontinuities, **saving is refused while a Rapier island is live** (§7.4).
- **Oracle cross-check, correctly scoped** (the backends cover nearly disjoint cases, so the old
  "within tolerance" gate was meaningless): the nightly oracle test is restricted to **≤2 s,
  contact-free, analytically checkable** scenarios — free fall, drag terminal velocity, circular
  orbit, torque-free precession — with stated numeric tolerances (1e-6 relative on position,
  1e-5 on angular state). It validates the integrator, **not** contact, and the plan no longer
  implies otherwise.

The old escape hatch ("swap to Jolt") was never viable: Jolt is C++, which breaks
`wasm32-unknown-unknown` cleanliness and the pure-Rust premise. Deleted.

### 2.10 Scripting (kOS-equivalent, first-class)

One API, one guest language, one set of semantics.

- **`ControlApi` trait** in `csp-script`: telemetry read, throttle/steer/SAS, staging, maneuver
  nodes, action groups, resource transfer, part actions, alarms, log/print. Every mutation becomes a
  `SimCommand`. Nothing bypasses the queue.
- **Guest language: Rhai, on all platforms.** Pure Rust, wasm-clean.
- **Scripts are tick-entry-point programs, not coroutines.** *Verified against the Rhai API
  (rhai.rs/book, 2026-07): the budget mechanism is `Engine::set_max_operations` /
  `Engine::on_progress`; exceeding the limit **terminates the script with an error**, and
  `on_progress` returning `Some(token)` terminates immediately. There is no continuation or
  resumable-VM API. Upstream's own `pause_and_resume` example requires a dedicated thread plus an
  MPSC channel — banned in the sim.* Therefore the previous claim ("exceeding the budget suspends
  the script until next tick and the suspension point is part of deterministic state") has no
  implementation and is deleted. The model is:
  - The host calls `fn on_tick(ctx)` **once per tick**, with `set_max_operations(budget)`
    (default 20 000, configurable per save).
  - Exceeding the budget **aborts that call**, logs, and increments a per-script strike counter.
    `strikes >= 5` (consecutive) disables the script, visibly, with the reason in the UI.
  - Cross-tick continuation is the script's job via an explicit `persist` map (a Rhai map of
    primitives), **serialized into the save** and included in the state hash.
  - Op budget, strike counter, and `persist` are all deterministic state. The *suspension point* is
    not, because it does not exist.
  - `wait`/`until` are **host services** that register a wake event and return (below). A bare
    blocking `wait` inside a tick is a **compile-time error** in the script preprocessor.
- **Wake events, so scripts survive rails warp.** Under rails there are no ticks, so a naive design
  freezes every script for up to 1.1 sim-days per wall second — including scripts whose whole job is
  "wait until Ap, then burn". That would make "automation is first-class" false.
  - A script registers a wake condition; the host publishes `ScriptWake { script_id, tick }` as a
    **`Clamping`** event. Rails warp clamps on it.
  - Conditions that reduce to a closed-form time (absolute tick, time-to-Ap/Pe, SOI entry, node
    start, altitude crossing on rails) are accepted. Conditions that do not are **refused at
    registration** with an error, not silently approximated.
  - A script with no wake condition and no throttle does not block warp.
  - A script that **polls** (budget-exhausting for 10 consecutive ticks) is detected and demotes
    warp to ×1 with a visible warning naming the script.
  - A script holding nonzero throttle already blocks rails warp under §2.6.2.
- **Rhai feature parity is mandatory, and it costs performance.** Rhai's own wasm guidance
  recommends `unchecked` (which **disables `max_operations` and progress tracking entirely**), plus
  `only_i32` and `f32_float`. Any of those makes the web build a different language with different
  numerics and no budget enforcement, while this plan sells script portability. **Pinned feature set
  for both targets: not `unchecked`, not `only_i32`, not `f32_float`, `sync` off.** The ~30% wasm
  slowdown is accepted. CI diffs the resolved Rhai feature set across targets
  (`cargo tree -f "{p} {f}"`); divergence fails the build (§8.2).
- **wasmtime is a native-only *mod* host**, post-v1, not a second scripting backend.
- **Built-in autopilots are written in Rust** against `ControlApi`, exposed to scripts as callable
  services (`mission.ascend(target_apo, incl)`). Shipping core gameplay through an interpreter you
  are still designing is not elegant, it is fragile.
- Scripts run inside `step_bubble_tick()`, before force integration, in ascending `ScriptId` order.
  Scripts may not read wall clock, filesystem, or network.

### 2.11 AI

- **v1 classical**:
  - *Ascent*: PEG/gravity-turn guidance with a tuned pitch program; closed-loop terminal guidance.
  - *Transfer*: Lambert solver + porkchop scan + local refinement; outputs maneuver nodes.
  - *Design assistant*: constraint solver over part stats — given payload mass, target Δv, min TWR,
    body, propose staging. Greedy + local search over the unlocked part set. No ML.
  - *Mission profile*: the design assistant emits the `MissionProfile` that §3.4's confidence number
    requires (§3.4).
- **Later LLM**: `LlmAdapter` trait. The LLM emits `SimCommand`s / script source through the same
  `ControlApi`. Three hard constraints:
  1. **Network I/O never happens inside a tick.** Requests are issued from `UiBuild`; responses
     arrive as commands stamped at the tick they are dequeued.
  2. **Confirmation required for anything irreversible** — spending funds, launching, staging,
     deleting a craft, accepting a contract, abandoning a colony. The LLM proposes; the player
     commits.
  3. Opt-in, off by default, API key supplied by the player, token budget visible.

### 2.12 External interfaces — one control surface, four clients

Telemetry comes from `csp-sim` snapshots; sim mutation goes through the `SimCommand` queue; session
mutation goes through the `SessionCommand` channel (§2.2 category C). No interface has privileged
access.

| Client | Transport | Direction | Lands |
|---|---|---|---|
| In-game scripts (Rhai) | `ControlApi`, in-process | read + sim commands | M9a |
| Built-in autopilots | `ControlApi`, in-process | read + sim commands | M9a |
| **MCP — stdio** (`csp-mcp` bin) | MCP JSON-RPC over stdio, own headless sim | read + sim + session | M9c |
| **MCP — Streamable HTTP** (`csp-server`) | single POST endpoint, per-request SSE upgrade; attach to a running game | read + sim + session | M9c |
| **Telemetry web screens** (`csp-server`) | HTTP static SPA + WebSocket stream | mostly read; opt-in command channel | M9d |

**MCP transport, corrected.** *Verified 2026-07: Streamable HTTP replaced HTTP+SSE in protocol
revision 2025-03-26; revision 2026-07-28 removed the GET stream endpoint and protocol-level
sessions.* The previous "HTTP+SSE" choice would have built a deprecated transport modern clients do
not speak. Normative:
- **Target protocol revision: `2026-07-28`.** The server must also accept `2025-11-25`. It must
  **never** implement the 2024-11-05 HTTP+SSE transport.
- The targeted revision is recorded in `csp-server/PROTOCOL.md` and asserted by a test; bumping it
  is a deliberate commit.
- Spec-mandated security, which a token alone does not provide: **validate the `Origin` header on
  every request** (403 on invalid) and **bind only `127.0.0.1`** when local. This is DNS-rebinding
  protection and it is not optional.

Rules for `csp-server`:
- **Native-only.** `cfg(not(target_arch = "wasm32"))`, and it must never be a *dependency* of
  anything in the wasm tree (positively asserted in CI, §8.2). Consequence, now stated plainly:
  **the web build has no telemetry dashboards and no MCP.**
- **Every sim tool/command maps to `SimCommand`s** stamped at dequeue, with
  `source_class = External` — agents get the same one-tick latency and the same determinism contract
  as UI input, plus the §2.2.1 rate limit and control lease.
- **Session-class MCP tools are declared as such** in the tool schema (`"class": "session"`) and are
  **not replay-relevant**: `editor.*`, `camera.*`, `dashboard.*`, `map.focus`.

#### 2.12.1 MCP & autonomous play

Tools over `ControlApi` (`telemetry.get`, `vessel.control`, `timewarp.set`, `mission.nodes`,
`craft.load`, `editor.build`, …) plus snapshot data as resources.

- Tool schema is versioned independently: `csp.mcp.tools.v1`. **Compatibility promise: additive-only
  within a major version; a tool never silently changes semantics.**
- **Two hosts, decided:** attach-to-a-running-game is the **HTTP** path only. The **stdio** path is
  the separate `csp-mcp` bin in `csp-bench`, which owns its own headless sim. An eval harness
  spawns `csp-mcp`; it never spawns the windowed app.
- `csp-bench` runs the whole thing with no renderer:
  `scenario.ron { initial_snapshot, data_fixture, command_source: agent|script|log, goal_predicate,
  tick_budget, event_budget }` → exit code + final state hash + score (Δv used, sim-time elapsed,
  funds spent).
- **Agent loop is lockstep, and the reproducibility claim is scoped honestly.** The sim does not
  advance until every registered agent has returned for the current tick; a per-agent tick-count
  timeout counts as a no-op. Statement: **logged runs are byte-reproducible; agent runs are
  reproducible only as logs** — the sim is deterministic, the agent is not, because an LLM's
  commands depend on what it read and when. A free-running agent loop would change every score.

#### 2.12.2 Telemetry web screens & dashboards

**SPA stack (settled):** TanStack Start, bootstrapped with the TanStack CLI, Tailwind + shadcn +
a custom CSS layer, in `apps/telemetry/`. Versions pinned **exactly** in `package.json` (no carets)
because of the near-daily patch cadence across the package set; upgrades are deliberate commits.
Tradeoff recorded in §1.

**Dashboards are non-authoritative sidecar config, not save data.** They live in
`$config_dir/csp/dashboards/*.json`, versioned independently (`schema_version`), **never in the
save, never in the command log, never in the state hash**, mutated via `SessionCommand`. The
previous "stored as JSON in the save" wording is deleted: a tablet resizing a widget must not mutate
the authoritative snapshot or change replay ground truth.

**Wire protocol (specified, because 64 Hz × a 1000-part vessel is megabytes per second):**
- Envelope: `{ proto: 1, kind, seq, tick, payload }`. `proto` is bumped on any breaking change.
- **Subscription model:** the client declares the telemetry field paths its widgets need; the server
  sends only those. There is no "send me everything" request. Per-part detail is capped and opt-in.
- **Rate: divisors of 64 Hz only** (64/32/16/8/4/2/1), default **16 Hz**, never tied to the tick
  rate. (60 Hz against a 64 Hz tick duplicates and drops samples forever.)
- Encoding: JSON for scalars and control, **MessagePack for arrays** (per-part vectors).
- **Backpressure: drop to latest, never queue.** The server reads an `ArcSwap<SimSnapshot>` and must
  never block the frame loop; a slow client degrades its own rate and nothing else.
- Budget: **≤256 KB/s at 16 Hz for a 1000-part vessel** (§5.3 row).

#### 2.12.3 `csp-server` runtime, concurrency, and auth

- `axum` + `tokio` live **only** in `csp-server`. Nothing in the determinism domain sees them.
- **Snapshot publication:** `SimStep` builds an immutable `Arc<SimSnapshot>` once per frame and
  publishes it into an `ArcSwap`. Readers (WS, HTTP, MCP) load the `Arc` and never block the frame
  loop. There is exactly one publish point.
- **Ingress:** a bounded MPSC channel drained in `Input`, ordered by `CommandKey` with
  `source_class = External` and `source_id = client_index` (assigned in connection order, recorded
  in the log).
- **Auth and discovery:**
  - Default bind `127.0.0.1` on a fixed default port (`--port` to override); a lockfile in
    `$XDG_RUNTIME_DIR/csp/` handles multiple instances and discovery.
  - **`Origin`/`Host` allowlist on *all* endpoints, including read-only.** "Read-only telemetry is
    unauthenticated on localhost" was wrong: any web page the player visits could `fetch` their game
    state and enumerate ports.
  - Token at `$XDG_RUNTIME_DIR/csp/<pid>.token`, mode 0600, rotated per process start, injected into
    the served HTML — **never stored in the save**. Documented for MCP clients to read.
  - LAN binds require an explicit `--bind-lan` flag, print a warning, and require the token on
    every endpoint. No TLS in v1; that is stated as a limitation, not hidden.

### 2.13 Error-handling policy

`csp-sim` runs inside a server and a bench harness. **It must never panic in release builds.**

Four classes, and every error maps to exactly one:

| Class | Meaning | Behaviour |
|---|---|---|
| `Fatal` | invariant violated beyond recovery (NaN in state, corrupt arena) | abort the tick, surface a dialog, mark the session read-only, write a bug-report bundle (§2.14) |
| `SaveBlocking` | the world is playable but not persistable (data-set validation failures, uninstalled weld) | play continues, saving is refused with the reason listed |
| `Recoverable` | a bounded subsystem failed (Kepler non-convergence, script abort, joint-cap degradation) | log, apply the documented fallback, continue |
| `Cosmetic` | presentation-only (missing mesh, shader warm-up) | log once, placeholder, continue |

Normative rules:
- `csp_sim::World::step_frame` returns `Result<AdvanceReport, SimError>`. Debug asserts are allowed;
  `unwrap`/`expect` on fallible paths in the determinism domain is a CI lint failure.
- **A `Fatal` mid-tick rolls back to the pre-tick snapshot.** This requires a one-tick shadow copy of
  authoritative state. **That cost is accepted and stated**: ~1 structural clone per tick, budgeted
  inside §5.3's per-tick number.
- **Loader errors are collected, not fail-fast**: report *all* invalid part/system/tech files, load
  the valid remainder, and mark the session `SaveBlocking`. Failing on the first bad file makes
  modding miserable.
- A script panicking inside a host function is caught at the host boundary and becomes a script
  strike (`Recoverable`), never a sim abort.
- A corrupt or unmigratable save is **refused with a message**, never half-loaded.
- Renderer device loss: reconfigure once; on second loss, exit cleanly after writing the bundle.
- `csp-server` errors never reach the sim. A server panic kills the server task only.

### 2.14 Logging, observability, bug reports

§2.1 forbids `std::time` and host side effects in the sim — which would otherwise make the one
budgeted subsystem the one you cannot instrument. Resolution:

- **`tracing`** is used at the `csp-app` / `csp-server` / `csp-bench` boundary. Levels:
  `error` (Fatal/SaveBlocking), `warn` (Recoverable), `info` (milestone events), `debug`, `trace`.
- **`csp-sim` emits structured `SimEvent`s into a ring buffer it owns** (fixed capacity, part of no
  hash, drained each frame by `csp-app`). It never formats strings into a global logger and never
  reads a clock.
- **All timing instrumentation lives in the caller.** `csp-app` measures `step_frame()`; the sim
  never measures itself (§2.6.3).
- **Bug-report bundle** (one command, `--bug-report`, and the Fatal handler): save + command log +
  data-set hash + determinism fingerprint (§8.4) + last 500 log lines + system info. This is also
  what makes §2.12.1's "reproducible down to the command log" true in practice.

---

## 3. Game design — progression layer

### 3.0 Home system reference sheet

M1 and every balance number downstream depend on these. They are **hand-authored fiction**, fixed
now, and an M1 deliverable with a measurable exit criterion. (Note: §2.3's previous "SOI ≈ 8.4 Mm"
was inconsistent with a moon orbiting at 12 Mm; corrected below.)

Derived conventions: **sim-day = 21 600 s** (home rotation), **sim-year = 426 sim-days** (home
orbital period). `G = 6.67430e-11` but μ is authored directly; mass is derived.

| | **Solen** (star) | **Terrin** (home) | **Rask** (moon) | **Duva** (neighbour) |
|---|---|---|---|---|
| Parent | — | Solen | Terrin | Solen |
| Equatorial radius (m) | 2.616e8 | 6.000e5 | 2.000e5 | 3.200e5 |
| μ (m³/s²) | 1.1723e18 | 3.5316e12 | 6.5138e10 | 3.0137e11 |
| Surface g (m/s²) | — | 9.810 | 1.628 | 2.943 |
| SOI radius (m) | ∞ | 8.400e7 | 2.430e6 | 4.790e7 |
| Rotation period (s) | 432 000 | 21 600 | tidally locked | 65 518 |
| Axial tilt | 0° | 0° | 0° | 0° |
| Semi-major axis (m) | — | 1.360e10 | 1.200e7 | 2.070e10 |
| Eccentricity | — | 0.000 | 0.000 | 0.051 |
| Inclination | — | 0.000° | 0.000° | 0.060° |
| Orbital period (s) | — | 9.203e6 | 1.390e5 | 1.730e7 |
| Atmosphere | — | yes | none | thin |
| Sea-level pressure (Pa) | — | 101 325 | — | 6 755 |
| Scale height (m) | — | 5 600 | — | 3 000 |
| Atmosphere top (m) | — | 70 000 | — | 50 000 |
| Ocean | — | yes (datum r = R) | no | no |
| Insolation at orbit (W/m²) | 1361 @ 1.36e10 m | 1361 | 1361 | 587 |

Derived checks that become M1 tests:
- Terrin surface escape velocity = `sqrt(2μ/R)` = **3 431 m/s** (±0.1%).
- Terrin 75 km circular orbit velocity = `sqrt(μ/(R+h))` = **2 287 m/s** (±1%). *This is the
  canonical "home LEO = 2.29 km/s" number all Δv budgets and engine stats are tuned against.*
- Rask orbital period = **1.390e5 s** = 6.44 sim-days (±0.1%).
- Axial tilt 0° in v1 is a deliberate simplification: it makes insolation a function of latitude and
  local hour only, which is what §3.5.1's two-state day/night model needs.

**Atmosphere model (v1, closed-form so it can be used inside the tick):**
`p(h) = p0 · exp(-h/H)` up to `h_top`, then zero (hard cut, documented discontinuity at `h_top`).
Temperature from an authored piecewise-linear table per body. Density from the ideal gas law with an
authored molar mass. Speed of sound from `sqrt(γRT/M)`.

**`SystemDef` / `BodyDef` schema** (`data/systems/<id>/`), RON, with `schema_version` as the first
field, validated in CI:
```rust
struct SystemDef { schema_version: u32, id: SystemId, star: BodyId, bodies: Vec<BodyId>,
                   epoch_note: LocKey }
struct BodyDef {
    schema_version: u32, id: BodyId, display_name: LocKey, parent: Option<BodyId>,
    radius_m: f64, mu: f64, soi_m: Option<f64>, rotation_period_s: Option<f64>, tilt_rad: f64,
    orbit: Option<KeplerElements>, atmosphere: Option<AtmosphereDef>, ocean: Option<OceanDef>,
    terrain: TerrainDef,                 // noise seed + authored heightmap refs
    biome_map: Option<BiomeMapRef>,      // §3.0.1
    surface_resources: Vec<(ResourceId, ConcentrationMapRef)>,
}
```
Validator rules: SOI must exceed every child's apoapsis; every body's parent must exist and be
acyclic; orbital period must be consistent with parent μ and `a` to 1e-9; atmosphere top must exceed
scale height.

#### 3.0.1 Biomes

Science keys on `(body, biome, situation)`, so biomes must be **stable against terrain tuning**.
If biomes derived from the noise function, any terrain tweak would silently invalidate every saved
science key and every biome-referencing contract.

- Biomes are **authored, versioned, low-resolution index maps**: equirectangular, ≤2048×1024,
  palette-indexed PNG at `data/systems/<sys>/<body>/biomes.png`, plus a RON legend
  (`biomes.ron`: `schema_version`, `Vec<{ index: u8, id: BiomeId, name: LocKey }>`).
- **Explicitly decoupled from terrain noise.** Terrain may change freely; biomes may not, except
  through a deliberate data-set change that triggers §7.7's drift warning.
- The data-set hash covers them.

### 3.1 Core data models

Part IDs are **stable strings** (`csp.engine.lv30`), never indices — saves depend on it.
**Every data file's first field is `schema_version: u32`**; the loader supports the current and
previous version with explicit defaults; the validator enforces presence. Without this, the first
required field you add breaks every community part file with a bare serde error.

```rust
// data/parts/*.ron
struct PartDef {
    schema_version: u32,
    id: PartId, family: FamilyId, display_name: LocKey,
    mass_dry_kg: f64, cost_funds: f64,
    build_resources: Vec<(ResourceId, f64)>,
    attach_nodes: Vec<AttachNode>,
    scale: ScaleRule,            // None | Discrete(Vec<f64>) | Continuous{min,max,exponents}
    collider: ColliderDef,
    aero: AeroDef,               // Cd/Cl curves, ref_len; exposed area from voxelization
    thermal: ThermalDef,         // mass*cp, max_temp_k, emissivity, conduction to neighbours
    structural: StructuralDef,   // max_axial_n, max_shear_n, max_torque_nm
    modules: Vec<ModuleDef>,
    reliability: ReliabilityDef, // §3.4
    tech_requirement: TechNodeId,
}

enum ModuleDef {
    Engine { thrust_vac_n: f64, isp_vac_s: f64, isp_sl_s: f64, throttle_range: (f64,f64),
             gimbal_deg: f64, ignitions: Option<u32>, rated_burn_s: f64,
             propellants: Vec<(ResourceId, f64)>, ullage_required: bool },
    Tank { capacity: Vec<(ResourceId, f64)> },   // a Cargo bay is just a Tank
    Experiment { kind: ExperimentId, data_units: f64, reusable: bool, situations: SituationMask },
    Drill { resource: ResourceId, base_rate_per_s: f64, min_concentration: f64, power_w: f64 },
    Converter { recipe: RecipeId, priority: u8, power_w: f64 },
    DockPort { size: PortSize },
    CrewSeat { slots: u8 },
    Lab { throughput_data_per_s: f64, multiplier: f64, researcher_slots: u8 },
    // ...
}
```

**`EffectivePart` is two functions, not one** — and saying so now avoids exactly the M3→M7 rework
that M3's resequencing was meant to prevent. `upgrades`/`scale`/`variant` are **frozen at
manufacture**; `wear` is live:

```rust
struct BuiltPart {                    // SERIALIZED per part instance (§7.2)
    part_id: PartId,
    scale: f64,
    variant: VariantId,
    upgrade_levels_at_build: BTreeMap<FamilyId, TrackLevels>,
    data_set_hash: Hash32,            // what the numbers meant when it was built
}

EffectivePart = resolve(BuiltPart) ∘ wear(PartUsage)
```
This also fixes the otherwise-unimplementable §3.3 promise: because re-derivation on load uses the
**frozen** `BuiltPart`, not the live tech state, an in-flight craft cannot silently gain upgrades
across a reload. One resolver, one place, used by editor readouts, the flight sim, the design AI,
and contract validation. If these ever disagree, players find it in a day.

**Data-set hash, defined precisely** (it is load-bearing for §7.7's drift warning and for
`BuiltPart`): BLAKE3 over the concatenation, in ascending path order, of
`path_bytes || 0x00 || canonicalized_file_bytes` for every file under `data/` **excluding**
`data/test-fixtures/` and any file matching the art extensions list (`*.png` is **included** for
biome maps and concentration maps, and excluded for textures under `data/art/`). Canonicalization =
LF line endings, trailing whitespace stripped. Algorithm pinned; changing it bumps
`DATA_HASH_VERSION`.

### 3.2 Science economy

Science is a function of **data × time × researchers**, with data as an inventory, not a currency.

- **Data**: an `Experiment` run in a `(body, biome, situation)` context yields `data_units` of a
  typed `DataRecord`. Situations: `Landed`, `Splashed`, `FlyingLow`, `FlyingHigh`, `SpaceLow`,
  `SpaceHigh` (KSP-proven granularity; do not invent more).
- **Diminishing repeats** (soft cap on *cumulative* data per key):
  `yield = cap_key · exp(-k · collected_key / cap_key) · run_units`, `k ≈ 1.0` global.
  Run 1 ≈ 63% of cap, run 2 ≈ 86%, run 3 ≈ 95%. `cap_key` is authored per
  `(experiment, body, situation)`; `k` is one constant in `balance.ron`.
- **Transmission vs recovery**: transmitting yields a per-experiment `transmit_ratio` (0.2–1.0) and
  consumes `ElectricCharge`; recovering the physical sample yields 100% and closes the key.
  Recovered-vs-transmitted is tracked per key, so a transmitted key can later be completed by
  recovery for the remainder.
- **Labs: the closed form is exponential, and exhaustion needs an epsilon.**
  `rate(t) = R · min(1, D(t)/T)` science/s where `R = Σ_researcher(skill) · lab_multiplier` and
  `T = throughput_data_per_s`.
  - Abundant regime (`D ≥ T`): `rate = R`, linear; `D(t) = D0 − R·t/η`. Event: `D` reaches `T`.
  - Scarce regime (`D < T`): `rate = R·D/T`, so `D(t) = D0·exp(−R t/(ηT))` — **exponential decay,
    not constant**, and `D` never reaches zero. The previous "integrated in closed form" wording hid
    this.
  - **Exhaustion is defined as `D < data_epsilon` (1e-6 data units)**, giving a finite event time
    `t = (ηT/R)·ln(D0/ε)`. Without the epsilon the event never fires and warp never clamps.
  - All rates are per **second**, like the rest of the sim. (`science/day` is deleted.)
  - Publishes `ResearchProjectComplete` and `ResearchDataExhausted` (both `Clamping`).
- Data can be hoarded; hoarding is a real strategy. Bank data early, build labs later.

### 3.3 Tech tree & part evolution

- A `TechNode` costs science and unlocks new `PartId`s, `UpgradeTrack` levels, facilities, and
  contract types.
- **`UpgradeTrack`**: `{ family, stat_path, levels: Vec<{ science_cost, multiplier_or_delta }> }`.
  E.g. `csp.engine.lv30 / isp_vac_s`, 4 levels at +3%.
- **Upgrades are multiplicative deltas in data, never new `PartDef`s** — otherwise combinatorial
  part spam plus a save-migration nightmare.
- Upgrades apply **at manufacture**, frozen into `BuiltPart.upgrade_levels_at_build` (§3.1), and
  therefore never retroactively to craft already in flight, **including across a reload**.
- Tree shape authored in `data/techtree/*.ron` with a validator: no cycles, every part reachable,
  every node's cost monotonic in depth. Runs in CI.

### 3.4 Reliability model

Concrete, deterministic, anti-save-scum.

```rust
struct ReliabilityDef {
    base_mtbf_s: f64, infant_mortality: f64,
    qualification_curve: QualCurve,
    regime_stress: Vec<StressRule>,
    dormant_stress: f64,                     // default 0.02 — nonzero, small
    failure_modes: Vec<(FailureMode, f64)>,
}
struct PartUsage {                           // per instance, serialized
    operating_s: f64, dormant_s: f64, ignitions: u32, gimbal_cycles: u32,
    max_temp_k: f64, max_axial_n: f64, max_g: f64, hazard_integral: f64,  // Λ
}
struct FamilyQualification {                 // per family, per save, serialized
    flight_units: f64, bench_units: f64, observed_failures: u32,
}
```

- **Hazard**: `λ(t) = λ_base · Π(stress_i) · qual_factor(family)`. `Λ` accumulates; failure occurs
  when `Λ` crosses a per-instance threshold `θ = −ln(U)` with
  `U = rng(seed, {RELIABILITY, part_instance_id, 0})`, **drawn once at part creation and stored**.
  `Λ` integrates in closed form over intervals (§2.6.1), so this is warp-safe and reload cannot
  reroll the outcome.
- **Ignition** is a separate Bernoulli draw per ignition index:
  `U_k = rng(seed, {IGNITION, part_instance_id, k})`.
- **Failure is a published event.** `PartFailure { vessel, part_instance, mode }` is a **`Silent`**
  event; its time is solved in closed form from `Λ(t) = θ` for piecewise-constant `λ`. Failure is a
  thresholded nonlinearity, so it *must* publish a next-event time (§2.6.5) — the previous event
  list contained `ReliabilityInspectionDue` but no failure event, which was a hole in the rails
  model, not an oversight in the UI.
- **Dormant hazard is real but small.** Non-operating parts accrue `λ_base · dormant_stress`
  (thermal cycling, storage). So a probe in orbit for 10 sim-years *can* fail a reaction wheel while
  unobserved. Constraints: **dormant failures are restricted to non-fatal modes**, are *recorded*
  when they fire, surfaced in a notification log, and *applied* on next load/observation. Setting
  `dormant_stress = 0` in `balance.ron` disables the mechanic entirely.
- **Stress rules** (data-driven): throttle above rated, burn beyond `rated_burn_s`, temperature
  fraction of `max_temp_k`, g-load, ullage-negative ignition, restarts beyond `ignitions`. Each
  multiplier is **displayed to the player** in editor and flight. Hidden stress multipliers are bad
  design.
- **Failure modes are mostly non-fatal**: degraded thrust, stuck gimbal, failure to ignite, leak,
  sensor fault, stuck decoupler, panel jam. Explosions are reserved for thermal/structural.
- **Qualification programs**: a test-stand facility burns funds + resources + time for
  `bench_units`; test flights generate `flight_units` weighted by regime coverage, so flying one
  profile does not qualify the engine for all profiles. This is what makes "test part X at regime Y"
  contracts meaningful. Failures add qualification data.
- **Mission confidence** needs an object to be confident *about*, so one is defined:
  ```rust
  struct MissionProfile { burn_segments: Vec<(duration_s, throttle, Regime)> }
  ```
  Produced by the design assistant (§2.11), editable in the editor. Confidence is the pure function
  `exp(−Λ_expected(MissionProfile))`. **Stub in M3, real in M9b.**
- **Revert policy (stated, because the pre-drawn design makes it load-bearing).** Revert-to-launch
  **exists** and restores the pre-launch snapshot — it is one of the top-three QoL features of the
  genre. It **keeps the pre-drawn thresholds**, so the engine still fails at the same `Λ`. The UI
  says so explicitly at the revert prompt, so players do not report it as a bug. Revert is not a
  reroll; it is a retry of the flying.

### 3.5 Economy loop

```
contracts/missions ─┐
science milestones ─┼─► funds ─┬─► part manufacture (funds + resources + time)
flight recovery   ──┘          ├─► hires (crew), salaries (recurring drain)
                               └─► facilities (labs, test stands, pads, colony modules)
resources ◄── colonies/ISRU ◄── surface bases you build & supply ◄── flights
```

- **Funds**: contracts (advance + completion), science milestones, recovery value. Recurring drain:
  salaries + facility upkeep. **Measurable target** (replacing "net-negative without being
  punishing"): *a fully-staffed base with no active missions loses **2–5% of liquid funds per
  sim-month** at default difficulty.* The economy harness (§8.3) asserts the band and fails outside
  it. A sim-month is 30 sim-days.
- **Bankruptcy is a defined state**, because warping to a transfer window can drain you: funds
  **clamp at zero**; unpaid salaries trigger `StaffAttrition` and `FacilityDormant` events; an
  `Insolvent` state is a **`Clamping`** event that auto-demotes warp to ×1 and notifies. Negative
  funds never exist.
- **Resources** (v1 set, deliberately small): `Ore`, `Metal`, `Composite`, `LiquidFuel`, `Oxidizer`,
  `Monoprop`, `ElectricCharge`, `Water`, `Food`, `Oxygen`. Ten. Not thirty.
- **Manufacture**: a build order consumes resources from a **launch-site inventory** (each launch
  site has its own) and takes sim time, publishing `ConstructionComplete`. Early parts are cheap in
  `Metal`; advanced parts need `Composite`, which is **import-only** at first → creates the pull
  into the colony game instead of bolting it on.
- **Logistics, with actual mechanics** (previously "you fly it", which is not a mechanism):
  - **Recovery** credits a vessel's resource contents to the recovery site's inventory at
    `recovery_efficiency` (default 0.9, 1.0 at the launch site).
  - A `Cargo` bay is just a `Tank` with `ResourceId` capacity.
  - **`ResourceTransfer` is a sim process, not an instant action**: precondition = docked **or**
    both vessels surface-latched within `transfer_radius_m` (default 100 m) of each other; rate =
    `min(source_outflow, dest_inflow)` from the connected ports; publishes `TransferComplete`
    (so it survives warp). Instantaneous transfer would be invisible to the event queue.
  - **A surface-latched vessel within a colony's radius may join the colony inventory**, opt-in via
    `SimCommand::JoinColonyInventory`. Never automatic — automatic joining makes resource
    bookkeeping unexplainable.
  - Post-v1: automated routes with a flat time/cost model; no simulated cargo ships.
- **Object caps and debris policy** (the sim and save must not grow without bound over 100
  sim-years of launches). In `balance.ron`:
  - `max_tracked_vessels = 512`, `max_debris = 64`.
  - Debris with periapsis below `h_top` decays on a scheduled `DebrisDecay` event; suborbital debris
    is removed on impact; debris leaving the outermost SOI is deleted; `SimCommand::TerminateVessel`
    exists.
  - On hitting `max_debris`, **oldest-debris eviction** with a notification. Deterministic, because
    "oldest" is by creation tick and ID.
  - The economy harness asserts **save size after 100 sim-years ≤ 32 MB**.
- **Crew model (named individuals; four systems depend on it).**
  ```rust
  struct CrewMember {                       // arena keyed by CrewId, serialized
      name: String, kind: CrewKind,         // Pilot | Engineer | Scientist | Tourist
      skill: u8,                            // 0..=5; no XP in v1
      location: CrewLocation,               // Facility(id) | Vessel(id) | Colony(id) | Unassigned
      salary_per_s: f64, status: CrewStatus, // Active | Lost | Retired
  }
  ```
  No XP in v1 (it is a post-v1 tuning knob). Assignment transitions: hire → `Facility`; board →
  `Vessel` (requires a free `CrewSeat`); M6 transfer moves between docked vessels; lab staffing
  assigns `Scientist`s to a `Lab` module. Tourists are `CrewKind::Tourist` with a contract-defined
  itinerary. Deaths are a per-save setting (`off | respawn | permanent`).
- **Life support**: optional `Oxygen`/`ElectricCharge`/`Food` consumption for crewed ops, toggleable
  per save, advanced by interval like every other subsystem. Publishes `LifeSupportDepleted`
  (`Clamping`).

#### 3.5.1 Colony model — a piecewise-linear flow network

"Production is integrated analytically" is only true of a model the plan previously did not have:
insolation varies continuously, radiator equilibrium is T⁴, converter chains couple rates to levels,
and two converters sharing a scarce feedstock need an allocation rule. This is the load-bearing math
of the whole RTS layer, so it is specified.

The colony is a **flow network recomputed only at events**, with these rules:

1. **Power source is a two-state step, not a continuous function.** Insolation is `Day` or `Night`,
   with `TerminatorCrossing` events (`Silent`) at the local sunrise/sunset times, computable in
   closed form from body rotation and orbital phase (tilt is 0°, §3.0). Day insolation uses an
   authored per-latitude average rather than an instantaneous cosine. Batteries are linear
   reservoirs, which is fine.
2. **Thermal is solved algebraically to steady state per interval, never integrated.** Radiator
   equilibrium `T_eq` from `Q_in = εσA T⁴` is solved once at the start of each interval and held
   constant over it. T⁴ has no closed-form integral; holding steady state over an
   event-bounded interval does.
3. **Converters run at a rate fixed for the interval**, determined by a deterministic allocation
   pass: iterate resources in ascending `ResourceId`, then consumers in `(priority, ColonyId,
   ModuleIndex)` order, granting each its requested rate until the resource's available rate is
   exhausted, then pro-rating the remainder. Tie-breaking by `BTreeMap` order would be an accident;
   this is a design.
4. **Next event** = `min` over all resources of `level / (−net_rate)` (emptying) and
   `(cap − level) / net_rate` (filling), plus the next `TerminatorCrossing` and the next scheduled
   inspection. Levels are therefore exactly linear over each interval, and §3.5's
   `amount = rate · dt` is literally true.
5. **Colony states are explicit**, with transition events:
   ```rust
   enum ColonyState { Nominal, Degraded { reason: DegradeReason }, Dormant, Abandoned }
   ```
   - `PowerLost` → `Degraded`; converters stop, batteries drain to zero, life support switches to
     reserves.
   - `LifeSupportDepleted` with crew aboard → crew `Lost` or evacuated per the deaths setting;
     colony → `Dormant`.
   - `Dormant` **consumes and produces nothing and schedules no events**, so it costs nothing under
     warp. This is the mechanism that keeps 20 half-dead colonies from drowning the event budget.
   - `SimCommand::AbandonColony` → `Abandoned`, recovering a fraction of build resources
     (`abandon_recovery`, default 0.25).
   - A crashing vessel impacting within a colony radius damages modules (a `Degraded` reason). There
     are no hostile entities in this design, so colony *defense* is correctly a non-requirement —
     colony *failure* is not.

### 3.6 Contracts

- `ContractTemplate` in data: parameters, generator weights, prerequisites, rewards, deadline,
  failure penalty.
- **Reputation is resolved, not open** (both reviews, and the owner brief, converge): there is **no
  reputation currency**. Prerequisites use concrete serialized facts — contracts completed by type,
  bodies visited, crew lost, failures in the last N sim-days, tech researched. A single derived
  scalar, `standing`, is computed from that history **for display and offer-board weighting only**.
  It is never spent, never a reward line, and never stored (it is derived). The §10 open question is
  deleted.
- Generation is seeded from `{tag: CONTRACTS, entity: 0, counter: offer_index}` so the offer board is
  deterministic and not re-rollable by reload.
- v1 types: satellite placement (orbit parameters), crew transport, rescue, survey data run, part
  test at regime, base resupply, tourist flight.
- Completion is evaluated **by the sim**, as a predicate over sim state checked **on relevant
  events** (orbit achieved, docking, recovery, transfer complete) — never polled every tick.

### 3.7 Assets, licensing, and audio policy

Given §1.2's commercial decision, these rules are now unconditional.

- **Mod code**: irrelevant — it's C#/Unity. Mods contribute *design lessons*, not code.
- **CC-BY-NC, CC-BY-SA, and GPL assets are unusable**, full stop (NC because the game is
  source-available-but-proprietary; share-alike because contamination is not worth it). MIT/CC0/
  public-domain assets are reusable with attribution.
- **Part concepts aren't copyrightable** — a stackable tank, a bell nozzle, a canard are fine to
  recreate. Meshes, textures, and names are not. Own naming throughout.
- **Preferred sources**: CC0 — Kenney, Quaternius, Poly Haven, ambientCG, NASA/USGS maps.
  Procedural-first for terrain, surfaces, and clouds: generated content is license-free forever and
  fits a fictional system anyway.
- **Audio** (previously absent): CC0/public-domain sources only (freesound CC0 filter, CC0 Sonniss
  packs), or synthesized. Same `ATTRIBUTION.md` file, same CI check.
- **Policy**: placeholder primitives through M3; **M4c ships the presentation floor** (below); first
  real art pass at M10, built from CC0 kitbash + procedural.
- **`ATTRIBUTION.md` format (so the CI check is implementable)**: one pipe-delimited line per asset,
  `path | source | license | url`. `xtask lint-attribution` fails if any file under `data/` or
  `assets/` with a non-authored extension lacks a matching line, or if any line's license is not on
  the allowlist.
- KSP mod *equivalents* (§4) are reimplemented features, not lifted assets.

**Presentation floor for the M4 fun gate.** The gate was moved to M4 precisely to avoid a false
negative; judging it on grey primitives with no audio reintroduces one. M4c's exit criteria
therefore include: silhouette-correct kitbashed parts for the stock ascent craft, engine plume VFX,
reentry plasma, ground-contact dust, and basic SFX (engine, staging, impact, UI). **Stated
explicitly: art *fidelity* is not judged at the gate — only legibility and feel.**

> **M4c as-shipped note (SFX row).** "Basic SFX" ships as the *cue channel*, not an audio device:
> `csp_view::flight`'s `FlightSfx` edge-detects the named transitions — engine ignition/cut,
> staging/jettison, breakup, contact/impact — and flashes them in the flight HUD, and the same
> transition stream is the seam a real backend consumes. Actual sound output is deferred to M10's
> own exit row: no audio stack exists in the tree, and adding one mid-milestone would pull a new
> dependency through this section's license audit plus the wasm CI matrix for zero gate-visible
> fidelity. Reentry plasma (convective-flux-driven hull sheath) and ground-contact dust (the §2.9
> geometric predicate wired to a surface puff) ship in full.

---

## 4. KSP-mod-equivalent features — scoped, with a cut list

| Mod | Built-in feature | Scope |
|---|---|---|
| Kerbal Engineer | Per-stage Δv/TWR/burn-time readouts | **v1 (M3)** |
| MechJeb | Ascent/landing/rendezvous autopilots, maneuver executor | **v1 (M9a)** |
| kOS | `csp-script` Rhai API + Rust autopilot services | **v1 (M9a)** |
| FAR | Weld-time voxel shielding + analytic Cd/Cl | **v1 (M4a)** |
| TweakScale | Part scaling native to part defs | **v1 (M3)** |
| Kerbal Alarm Clock | Alarms + transfer-window planner (falls out of the event queue) | **v1 (M5)** |
| Scatterer | Precomputed-LUT atmospheric scattering | **v1 (R2)** |
| Life support (TAC) | Optional O₂/EC/Food consumables, toggleable | **v1 (M8), minimal** |
| kRPC remote | External agent control — MCP on `ControlApi` | **v1 (M9c)** |
| KSP web-server mods | Telemetry screens + user-defined widget dashboards | **v1 (M9d)** |
| Principia | `NBody` implementation of `OrbitModel` | **post-v1** |
| Parallax | Terrain displacement/scatter | **post-v1 (R4)** |
| CommNet / RemoteTech | Signal network, probe control range | **post-v1** — it gates *control*, so it can break the scripting UX; never ship it half-done |
| KIS/KAS | EVA surface assembly | **post-v1** — colony modules land as prefab units in v1 |
| Virtual texturing | Planetary surface VT | **post-v1, native tier only** (wasm heap won't take it) |
| Mod marketplace | — | **cut.** Not a solo-dev feature. Ship a folder and a README. |

**Rejected, in writing, so it does not come back: "the built-in features above are implemented *as*
mods against the public API — dogfooding."** That forces a complete, stable public API before you
know what the game needs, and roughly doubles the cost of every core feature for a solo dev. Core
features are first-class Rust; the script API covers *control and telemetry* only; the data formats
(parts, bodies, tech, contracts, biomes) are the modding surface. Dogfooding happens where it is
cheap (autopilots use `ControlApi`), not everywhere.

---

## 5. Rendering

Two tiers, declared up front, because this is where "bleeding-edge, all vendors, web" tears.
**Every upstream claim below carries a dated stamp.**

### 5.1 Tier contract

| Capability | Native tier (Advanced) | Web tier (Baseline, WebGPU) |
|---|---|---|
| Draw submission | bindless + `multi_draw_indirect` (+count) | per-draw bind groups, CPU-culled instanced draws |
| Material/texture access | binding arrays, partially-bound | texture atlases + fixed slot sets |
| Terrain | virtual texturing, GPU clipmaps, compute-driven LOD | CPU quadtree LOD, pre-baked tiles, reduced draw distance |
| Geometry | mesh/task shaders where available, meshlet culling | vertex pipeline only |
| Lighting | Forward+ clustered, compute culling | simple forward, few lights |
| Post | TAA, hand-rolled upscaler | FXAA/none |
| Atmosphere | precomputed LUTs + volumetric | precomputed LUTs, analytic single-scatter |

Rules:
- `csp-render` exposes `RenderTier { Baseline, Advanced }`, chosen at init from queried
  `wgpu::Features`/`Limits`.
- **Both tiers must produce a *correct* image for every scene — and "correct" is now defined**:
  **zero silhouette/visibility differences** (compared on a 1-pixel-dilated coverage mask) and
  **≤2% of pixels with ΔE2000 > 3.0** against the tier-specific reference image. Measured by the
  screenshot harness (§8.5).
- Shaders are authored once with preprocessor branches for the binding model (WESL/naga_oil), never
  duplicated by hand. **Both binding models must compile in CI** via naga/WGSL validation — this
  runs on any machine and is the part of the render gate that is actually CI-runnable.
- `wgpu::hal` raw-Vulkan passthrough is **not** an escape hatch — using it forfeits Metal, DX12, and
  web. The honest options are: contribute upstream, or make it Advanced-tier-only behind a wgpu
  feature flag.
- **wgpu version: pinned 30.0.x** (matching `Cargo.lock`). *As of wgpu 30.0 (2026-07):* mesh shaders
  remain `Features::EXPERIMENTAL_MESH_SHADER`, with **native paths on Vulkan, Metal, and DX12 and
  WGSL supported** — no longer passthrough-only as earlier revisions of this plan stated — and the
  API is documented as subject to breaking change. Keep them R4/Advanced-only, opt-in, never on the
  critical path.
- *As of mid-2026:* WebGPU bindless is still at proposal/design stage with no browser preview, and
  `chromium-experimental-multi-draw-indirect` exists only behind the unsafe-WebGPU flag with ~0.1%
  field availability. The two-renderer position is current fact, not pessimism. **Revisit when two
  browsers ship bindless.**

### 5.2 Phases (R*, mapped to milestones)

| Phase | Lands with | Features |
|---|---|---|
| R0 | M0–M2 | Forward HDR, reverse-Z, PBR parts, analytic sky, planet = sphere + heightmap texture, camera-relative transforms |
| R1 | M3–M4 | Depth-precision validation at 10⁷ m, shadow cascades, reentry plasma/particles, editor viewport + GPU picking |
| R2 | M5 | CDLOD/quadtree terrain with per-patch f64 origins, precomputed-LUT scattering, ocean, planet-scale LOD |
| R3 | M9+ | Advanced tier: bindless + indirect, clustered Forward+, compute LOD, TAA |
| R4 | post-v1 | Virtual texturing, meshlet/mesh-shader path, terrain scatter, ray queries |

### 5.3 Budgets, and the machines they are measured on

**A performance budget without a reference machine is unmeasurable.** Named machines:

- **REF-A** (primary): Ryzen 5 5600 / RTX 3060 12 GB / 32 GB / Windows 11, pinned driver version
  recorded in the perf log / 1920×1080.
- **REF-B** (secondary): Mac mini M2 Pro / macOS, pinned OS version / 1920×1080.
- **Web** figures are Chrome stable on REF-A.
- **CI-HOST**: GitHub hosted runner, no GPU — used **only** for the CPU-only rows.

| Scene | Native (REF-A) | Web | Sim CPU budget |
|---|---|---|---|
| 200-part vessel in atmosphere, 1080p | 144 fps | 60 fps | **≤0.5 ms/tick** |
| 1000-part station in orbit | 60 fps | 30 fps | **≤2.0 ms/tick** |
| 300-part vessel landed, gear in contact | 144 fps | 60 fps | **≤1.0 ms/tick** |
| Map view, 500 on-rails objects | 144 fps | 60 fps | ≤0.2 ms/frame |
| Rails warp ×10⁵, 20 colonies | — | — | **≤2000 events/frame, ≤4 ms/frame**; exceeds ⇒ partial advance (§2.6.5) |
| Weld Stage 1, 1000-part station | — | — | **≤20 ms, synchronous** |
| Weld Stage 2 (voxelization), 300-part craft | ≤500 ms on a worker | **≤2 s, incremental across frames** (no worker exists on wasm) | off-tick, installed by command |
| Telemetry stream, 1000-part vessel @16 Hz | ≤256 KB/s | ≤256 KB/s | — |

Notes:
- The per-tick budgets are what make physics warp honest: ×2 needs 128 ticks/s, so ×2 on the
  200-part scene costs 64 ms of CPU per wall second. ×3/×4 unlock only when measured per-tick cost
  allows (§2.6.2).
- "Off the sim thread" is **native-only** language. On wasm, Stage 2 is incremental across frames.
  The previous plan's wasm "<2 s off the sim thread" was unimplementable.

### 5.4 Measurable render decisions

- **Depth strategy (R1):** render the 10⁷ m test scene with a 1 m reference object at 10⁷ m; if
  depth-fighting artefacts exceed 1 pixel at 1080p, add the far-field pass. Recorded in the perf log.
- **egui VAB kill criterion (§9's open evaluation, now a number):** if VAB interaction latency
  (pointer-down → visual response) exceeds **16 ms at 300 parts on REF-A**, the egui panel layer is
  replaced. Measured at M3 exit.

---

## 6. Milestones

### 6.0 Critical path, slack, and effort

**Critical path** (everything else waits on this chain):

```
csp-core clock + RNG KDF + StateHasher
  → the state-hash / replay / save-roundtrip HARNESS            ← M0, first-class deliverable
    → event queue + step_frame/advance_to
      → OrbitModel (conics) + SOI solving
        → weld Stage 1 + force model + contact solver
          → EffectivePart resolver (BuiltPart ∘ wear)
            → everything else
```

The **harness is on the critical path of every subsequent milestone**, because it is how you learn
you broke something. In the previous revision it was a parenthetical M0 exit criterion; it is now a
named deliverable with its own exit criteria (§7.1, M0 below).

**Schedule slack** — buildable in any order, cuttable without blocking anything:
render tiers beyond R0, the telemetry SPA and dashboards, audio.

**Effort units (EU)**, relative, not dates. 1 EU ≈ 70 h at §1.2's capacity.

| # | Milestone | EU | Exit criteria (measurable) |
|---|---|---:|---|
| **M0** | Scaffold, rails & **harness** | 2 | Workspace builds native + wasm in CI with the correct exclusions; winit+wgpu+egui window; schedules run in the §2.5 order with egui in `UiBuild`; **clock owned by `csp_sim::World`**; RNG KDF with hard-coded expected-output test; `clippy.toml` + `[workspace.dependencies]` + `xtask lint-deps` all **active and failing on violation**; **`StateHasher` per §7.1 + `csp-replay` round-trip harness + 1 trivial golden + determinism fingerprint file**. All of Appendix A closed. |
| **M1** | Orbit layer & home system | 3 | §3.0 system data authored + validator green; Terrin LEO velocity 2287 m/s ±1% by test; map view; rails warp to ×10⁵ via `advance_to`; SOI solved per §2.6.6 incl. the graze test; event queue with both classes + alarms; elements↔cartesian round-trip <1e-6 per §2.8; ×1-vs-×10⁵ equivalence test green (§2.6.1) |
| **M2** | Flight core (one force model, forever) | 4 | Weld Stage 1 + `SimpleBackend` semi-implicit Euler + PGS contact; thrust, pre-voxel analytic drag, gravity, ground contact; surface-rails latch; prebuilt craft flies launch→orbit→reentry→land on a controller; **20 t lander rests 10k ticks, ‖v‖<1e-6, constant hash**; `csp-bench` replays a command log → matching hash; golden replay suite green; per-tick budget met on REF-A |
| **M3** | Editor + effective-stat pipeline | 5 | Part snap graph; staging; scaling + variants; **`BuiltPart` + `EffectivePart` resolver** with stubbed wear; live Δv/TWR/burn-time; `MissionProfile` stub; craft save/load with stable `PartId`s; GPU picking; **VAB latency ≤16 ms at 300 parts on REF-A** |
| **M4a** | Flight fidelity — aero | 3 | Isotropic voxelizer with 66-direction geodesic table + barycentric interpolation + `ConfigMask` set; Mach/Reynolds Cd/Cl; **table-derived `Cm_α` within 5% of a direct per-direction computation**; fairing jettison changes shielding; Stage-2 install-by-command path incl. the debug byte-compare |
| **M4b** | Flight fidelity — thermal | 2 | Per-part thermal + conduction + reentry heating; reentry survivable and lethal in the right places; thermal state is pure-function-of-serialized-state (no time-triggered caches) |
| **M4c** | Breakup + **presentation floor** = **fun gate** | 2 | Structural/thermal breakup with momentum conservation to §2.4's defined tolerances; child bodies inherit parent aero tables; §3.7 presentation floor shipped; **vertical slice complete (M2+M3+M4a–c)** |
| **M5** | Interplanetary | 3 | Maneuver nodes + executor; Lambert/porkchop planner; encounter prediction; transfer-window events; R2 terrain + scattering; Duva reachable and landable — *"reachable" demonstrated in-sim (csp-sim `duva_reachable_in_sim_end_to_end`: spawn → SetManeuver → executor → Exit{terrin} → Enter{duva}); "landable" deferred per the §6.4 reduced-scope cut (landing needs a `dominant=duva` bubble world — tracked for post-v1)* |
| **M6** | Multi-vessel & docking | 3 | Two vessels in one bubble; soft-capture jointed island ≤16 joints; **latch merges graphs into one `WeldedBody` with a `DockSeam`** (merge test: mass/CoM/inertia equal to a from-scratch weld, bit-identical); undock splits; 1000-part station is one body; `ResourceTransfer` process; crew transfer |
| **M7** | Progression | 4 | Science data model + soft cap; labs with the §3.2 two-regime closed form and epsilon exhaustion; tech tree + upgrade tracks wired into the M3 resolver (frozen at build); funds; contracts with the §3.6 fact-based prerequisites; recovery |
| **M8** | RTS layer | 5 | §3.5.1 flow network incl. the allocation pass and `ColonyState` transitions; drills/converters/power; manufacture consumes resources + time; **proximity resource transfer** (works without orbital docking); supply missions; debris/object caps enforced; minimal life support; economy harness asserts the 2–5%/sim-month band and ≤32 MB at 100 sim-years |
| **M9a** | Automation | 3 | Rhai host: tick-entry-point `on_tick`, op budget, strikes, `persist` in the save; wake events as `ScriptWake`; poll detection; Rust ascent autopilot; feature-parity CI check green |
| **M9b** | Reliability | 3 | All of §3.4: hazard, `PartFailure` events, dormant hazard, failure modes, qualification programs, test-stand facility, part-test contracts, real `MissionProfile` confidence UI, revert policy in the UI |
| **M9c** | Interfaces | 3 | `csp-server` (axum/tokio, `ArcSwap` publish, bounded ingress, `Origin` allowlist, token file); Streamable-HTTP MCP at revision 2026-07-28 + `csp-mcp` stdio bin; `csp.mcp.tools.v1` with session-class tools marked; control lease + per-client budgets; **`ControlApi` frozen at the end of this milestone** |
| **M9d** | Dashboards & eval | 2 | TanStack Start SPA; widget registry; subscription-based WS at 16 Hz within the 256 KB/s budget; sidecar dashboard config; `csp-bench` scenario format; lockstep agent loop; first eval suite |
| **M10** | Ship v1 | 4 | Migration chain *begins* (§7.5); settings + input rebinding; audio; tutorial/first-flight; crash handler + bug-report bundle; all §5.3 budgets met on REF-A/REF-B and logged; web tier ships as the §6.3 demo |
| **post** | Expansion | — | `NBody`, CommNet, LLM assistant, wasmtime mods, EVA assembly, virtual texturing, automated routes, low-thrust-on-rails, T2 work |

**Total: 51 EU.** `ControlApi` freeze is at **end of M9c**, not "M9" — it cannot be frozen before the
autopilots (M9a) and the MCP tool surface (M9c) have exercised it.

### 6.1 Why the splits

- **M9 was four milestones wearing one hat** (API freeze + script host + three control-engineering
  problems + a web server + a dashboard app + MCP + a bench format + an eval suite + the entire
  reliability model), while its neighbours were a fraction of that. Split M9a–d.
- **M4 was four subsystems** at the point of maximum "one more tweak" risk, because it is also the
  fun gate. Split M4a/b/c, with the presentation floor in M4c so the gate is judged on something
  legible.
- **M3 and M8 stay single rows but carry explicit EU weights** (5 each) so the cut list has
  something to cut against. The editor is historically where solo projects lose a quarter.

### 6.2 Vertical slice & fun gate

**Vertical slice = M2 + M3 + M4a–c**: sandbox, one planet, build → launch → orbit → reentry → land,
with the real flight model and the presentation floor. The "am I having fun" gate is **M4c**, not
M2 — a point-mass rocket is never fun and would give a false negative.

**Reliability is deliberately last among v1 systems** (M9b). It is a multiplier on frustration if
the flight model, editor, and economy are not solid first, and it depends on usage tracking that
only exists once parts have modules and missions have length.

### 6.3 The web tier, defined

The previous plan was ambiguous between "playable demo only" and "playable at reduced scope".
Decided: **the web build is a fixed demo scenario** — Terrin only, part count capped at 60, no save
import/export, no dashboards, no MCP (`csp-server` is native-only), reduced textures, no VT. Saves
are **not portable** between web and desktop, because the data sets differ. Stated in §1.1 and M10.

### 6.4 Cut list — reordered

**Decide in this order** (previous ordering put the web tier first, which refunds almost nothing):

1. **R3 Advanced render tier → post-v1.** Pure addition on top of a working Baseline; recovers a
   full milestone's worth of GPU work.
2. **Life support → post-v1.** One toggleable subsystem; everything else works with it off.
3. **Third body (Duva) → post-v1.** Content and transfer-planner polish, not architecture.
4. **Orbital docking (M6) → post-v1, but *proximity transfer stays* (moved into M8).** Cutting M6
   entirely would leave M8's supply loop and the `Composite` import-only pull with **no delivery
   mechanism**. Proximity transfer needs no constraint solver — proximity + confirm + a rate — so it
   is cheap and must survive the cut.
5. **Web tier polish → demo-only or dropped.** Honest accounting: cutting web late recovers the
   Baseline-tier polish, the reduced-dataset authoring, and the nightly wasm perf work — **not** the
   architectural constraints (wasm-clean deps, no threadpools, Rhai feature parity), which are paid
   from day one and are non-refundable. It is ranked last because it is the cheapest thing left to
   cut by then, not because it is the most valuable.

**Minimum shippable v1** = M0, M1, M2, M3, M4a, M4b, M4c, M5(reduced: home + moon only), M7,
M8(with proximity transfer, no life support), M9a, M9b, M10 ≈ **33 EU**. Home planet + moon, build,
fly, progress, automate.

### 6.5 The cut trigger

A cut list with no trigger is a wish list. **Ritual: a quarterly review whose only job is to decide
whether the quarter's milestone was met. If it was not, execute the next cut-list item. No debate,
no "next quarter will be better".** Record the decision in `docs/quarterly/`.

---

## 7. Saves, versioning, and migration

### 7.1 State hash — the definition everything else depends on

Referenced by §2.6.1, §2.7, §7.5, §8; previously specified nowhere.

- **Algorithm: BLAKE3-256.** 32-byte output, lowercase hex in golden files.
- **Produced by a hand-written `csp_core::StateHasher` visitor with an explicit field walk —
  never by serde, never by derive order.** Rationale: with a derive-based hash, reordering a struct
  field reds every golden, the team learns to regenerate blindly, and the gate is silently void.
- **Canonical byte stream rules:**
  1. `HASH_SCHEMA_VERSION: u32` (little-endian) is hashed first.
  2. Integers: little-endian, fixed width. `bool` as `0u8`/`1u8`.
  3. **f64: `to_bits()` little-endian, after canonicalizing `-0.0 → 0.0`.**
  4. **NaN and ±inf must never enter sim state.** Debug builds `debug_assert!`; release builds raise
     `SimError::Fatal` at the tick boundary (§2.13). There is no NaN-payload question because NaN
     is forbidden.
  5. Arenas: `len: u32`, then entries in **ascending monotonic-ID order**, each prefixed by its ID.
     Empty slots are skipped entirely; free lists and slot generations are **not** hashed (they are
     serialized, but they are allocator state, not world state).
  6. `BTreeMap`: `len: u32` then entries in key order. `Vec`: `len: u32` then index order.
  7. `Option`: `0u8` | `1u8 || payload`. Enums: **explicit, stable `u16` discriminant** (declared in
     code, never reordered) followed by payload.
  8. `String`: `len: u32` then UTF-8 bytes.
- **Exclusion list = exactly §7.3**, plus session state (§2.2 category C) and the `SimEvent` ring
  buffer.
- **Bubble mask**: the ×1-vs-×10⁵ equivalence test (§2.6.1) hashes with bubble bodies excluded. The
  mask is a named, documented function, not an ad-hoc test hack.
- `HASH_SCHEMA_VERSION` must be bumped **in the same commit** that regenerates goldens, and the
  commit message must say why. `xtask bless-goldens` refuses to run if the version was not bumped
  when the hash changed for a non-behavioural reason.
- **A determinism fingerprint file sits beside every golden**: rustc version, target triple, exact
  versions of `parry3d-f64`/`rapier3d-f64`/`libm`/`rand_chacha`/`blake3`, `HASH_SCHEMA_VERSION`,
  `KDF_VERSION`, `DATA_HASH_VERSION`, data-fixture hash. Without it, every dependency bump is an
  unexplained red build — which is how determinism gates get switched off.

### 7.2 Format and what is serialized

**Format (adjudicated between the reviews):** a **plaintext, uncompressed, self-describing header**
followed by a **self-describing body**.
- Header: magic `CSPSAVE\0`, `u32 header_len`, then UTF-8 JSON:
  `{ save_version, game_version, hash_schema_version, kdf_version, data_hash_version,
     data_set_hash, compression: "deflate", body_format: "cbor" }`.
- Body: DEFLATE (`miniz_oxide`, pure Rust, wasm-clean) over **CBOR** (`ciborium`).
- **Replay logs stay `bincode`** — they are version-scoped by T3 anyway and are append-heavy.

Justification for not using bincode for saves, since opus #23 is right and this is the deciding
argument: bincode stores no metadata, upstream states backward-compatible struct changes are
*impossible*, and a migration function can only run *after* decoding the old bytes — which means
freezing the old struct definitions forever. A self-describing body makes field-tolerant decoding
possible, and **it costs nothing in determinism because the state hash is produced by the §7.1
visitor, not by the serializer.** That decoupling is the whole reason this choice is free.

**Serialized (authoritative):**
- sim tick; warp level; `active_vessel`; control leases
- all arenas: vessels, part instances, colonies, contracts, crew, scripts — plus **next-ID counters,
  free lists, and slot generations** (needed for exact re-derivation; hashed per §7.1 rule 5)
- `BuiltPart` per part instance; `PartUsage` per part instance; `FamilyQualification` per family
- event heap **including `event_seq` and `next_event_seq`**
- RNG index counters per `(tag, entity)` where a monotonic index is used (e.g. contract
  `offer_index`)
- per-source `source_seq` counters (§2.2.1)
- colony stores, `ColonyState`, flow-network levels; launch-site inventories
- research data inventory per key (collected, transmitted, recovered), lab assignments
- tech state; contract board + offer index + completed-contract facts (for §3.6 prerequisites)
- bubble vessel poses/velocities; **surface-latch records** (body-fixed pose per latched vessel)
- `DockSeam` records; craft files; script source + `persist` maps + strike counters
- notification log (incl. recorded dormant failures)

### 7.3 Never serialized — re-derived on load

Physics-backend internals (solver caches, broadphase, contact manifolds, warm-start impulses); all
derived caches (`WeldedBody`, inertia tensors, **aero voxel tables**, trajectory splines, terrain
patches, atmosphere LUTs); presentation state; session state; dashboards; the `SimEvent` ring buffer.

**Hard rule, which wins every argument with other sections: every derived cache must be a pure
function of currently-serialized state** (§2.4's quantized inertia trigger exists because of this
rule). This is what keeps saves independent of the physics backend and of parry/rapier versions —
the only route by which T3 could ever become possible.

### 7.4 When saving is refused

Rapier's sequential-impulse solver is warm-started, and warm-start impulses are not serialized, so
a save taken mid-contact or mid-soft-capture cannot reproduce the next step. Rather than narrow the
determinism test (opus's option b), **refuse the save** (option a) — it is one check and it removes
a whole class of unfixable test failures.

Saving is refused, with the reason named in the UI, when:
1. any **live joint island** exists (soft-capture window, active robotics, failure joint);
2. any bubble body has **live contacts and is not surface-latched** (i.e. mid-landing, bouncing,
   rolling);
3. a **Stage-2 weld is pending** for a vessel in or about to enter atmosphere (`SaveBlocking`);
4. the session is `SaveBlocking` from loader errors (§2.13).

**Crucially, landed play is unaffected**: a settled vessel is surface-latched (§2.6.4), has no live
contacts, and saves normally. The UI rule is "settle to rest, or be in free flight, to save" — and
because settling is automatic after 2 s, players will almost never notice. Autosave waits for a
savable moment rather than failing.

### 7.5 Determinism boundary at load

Re-derivation must be exact: re-welding a vessel from its part graph and its `BuiltPart`s must
produce **bit-identical** mass properties.

**Test (per-commit): save → load → N ticks, hash-compared against uninterrupted N ticks**, for
N = 10 000, across the scenario set. This test will fail often and each failure is a real bug —
usually a cache that is a function of history rather than of state.

### 7.6 Replays

A replay is a separate artifact: `{ fingerprint, save_seed, data_fixture_hash, initial snapshot,
Vec<(CommandKey, SimCommand)> }`, bincode. Used for golden tests, bug reports, and (later) mission
sharing. **Not a save format.**

### 7.7 Versioning, migration, and the pre-1.0 policy

- `save_version: u32` + `game_version` string in the plaintext header.
- **Pre-1.0 policy (decided, because the alternative taxes every commit for a user base of one):**
  before v1.0, a `save_version` bump **may invalidate saves**. The loader refuses with a clear
  message naming the versions, and **no migration function is written.** Migration chains **begin
  at the first public build** (an M10 deliverable). Writing years of migrations for a weekly-changing
  schema is waste.
- From v1.0: `csp-data` holds `schema/v1.rs`, `schema/v2.rs`, … each a **frozen, never-edited** copy
  of the save types, with `migrate_vN_to_vN+1`. Current code works only on the newest schema. A CI
  test asserts every fixture under `tests/fixtures/saves/` loads and migrates.
- A save that cannot be migrated is **refused with a message**, never silently half-loaded.
- **ID stability**: `PartId` strings are permanent. Renaming or deleting one requires a migration
  entry mapping old → new, or `Deprecated(replacement)`. The data validator fails in CI on an ID
  that disappears without one.
- **Data-set drift**: the save records the data-set hash (§3.1). Loading with changed data **warns
  and lists what changed** (part stats, biome maps, balance constants). Silent stat changes on
  existing craft are a support nightmare. Note that `BuiltPart` already froze the numbers that
  matter for in-flight craft; the warning is about *future* builds and about biome/contract keys.
- **The RON debug dump is write-only.** It is a diagnostic artifact and **never a load path** —
  RON cannot portably round-trip NaN/inf (and NaN is forbidden in state anyway, §7.1). Authored RON
  *input* data is fine: `f64::from_str` is correctly rounded.

### 7.8 Non-authoritative sidecar data

Explicitly outside the save, the command log, and the state hash: **dashboards**
(`$config_dir/csp/dashboards/*.json`), settings, keybinds, the aero side cache
(`$cache_dir/csp/aero/`), the perf log, and bug-report bundles. Each has its own
`schema_version` and its own, cheaper, forward-compatibility rules (unknown keys ignored).

---

## 8. Testing & CI

### 8.1 The testing pyramid (previously empty below golden replays)

A red golden hash currently says "something in the universe changed" and nothing more. These tiers
make a failure **diagnosable**, and they run in this order:

**(a) Property tests (`proptest`)** — the diagnostic layer:
- Kepler elements↔cartesian round-trip; universal-variable solver over the §2.8 `e × Δt` sweep
- `weld` Stage-1 mass-property invariance under part-ordering permutation
- momentum conservation on breakup (§2.4 tolerances, defined denominators)
- voxelizer determinism under AABB perturbation and under part-order permutation
- hazard-integral closed form vs a fine-stepped reference, within tolerance
- **`advance_to` vs tick-stepping equivalence** (§2.6.1) — the single most important property test
- event-heap total order under `(tick, event_seq)` ties
- colony flow-network allocation determinism under permuted module insertion order
- `CommandKey` ordering is a strict total order

**(b) Unit tests** — every closed-form integrator against a naive stepped oracle; the RNG KDF
against hard-coded expected bytes; `StateHasher` against hand-computed canonical streams.

**(c) Fixture tests** — every historical save version loads and migrates (post-1.0, §7.7).

**(d) Golden replays** — last, and only meaningful because (a)–(c) pass.

### 8.2 Gates

| Gate | What | Runs | Where |
|---|---|---|---|
| `cargo check` native | — | every commit | CI-HOST |
| `cargo check` wasm | `--workspace --exclude csp-server --exclude csp-bench --target wasm32-unknown-unknown` | every commit | CI-HOST |
| **wasm purity (positive)** | `cargo tree --target wasm32-unknown-unknown -p csp-app` must **not** contain `csp-server`/`csp-bench`/`tokio`/`rayon`; fails on tool error. **One narrow `tokio` exception: only as a direct dependency of `wasm-bindgen-futures`** — Cargo.lock shows `wasm-bindgen-futures` 0.4.79 declares a `tokio` edge, so a feature flip that activates it is legal through that edge alone; every other path to `tokio` is a violation | every commit | CI-HOST |
| Dependency rules | `xtask lint-deps` parses `cargo tree --format "{p}"` and asserts an explicit allow/deny list per crate; **fails on tooling error**, never passes on empty output | every commit | CI-HOST |
| Determinism lints | `clippy.toml`: `disallowed-methods` for `f64::{sin,cos,tan,asin,acos,atan,atan2,exp,ln,log,powf,powi,sqrt,cbrt,hypot}`; `disallowed-types` for `std::collections::{HashMap,HashSet}` and `std::time::*`; `SeedableRng::seed_from_u64`. Applied via `[lints]` in `csp-core`/`csp-sim`/`csp-phys`/`csp-data` **only** | every commit | CI-HOST |
| `cargo-deny` | bans, duplicate versions, **license allowlist** (§3.7) | every commit | CI-HOST |
| Rhai feature parity | `cargo tree -f "{p} {f}"` for `rhai` on native vs wasm must match; `unchecked`/`only_i32`/`f32_float` forbidden | every commit | CI-HOST |
| Data validators | parts, systems, tech tree, contracts, biomes: `schema_version` present, schema, cycles, reachability, ID stability, SOI/period consistency | every commit | CI-HOST |
| `xtask lint-attribution` | every non-authored asset has a `path \| source \| license \| url` line with an allowlisted license | every commit | CI-HOST |
| Property + unit tests | §8.1 (a), (b) | every commit | CI-HOST |
| **Golden replays (fast subset)** | ascent, reentry, landing+settle, docking merge, ×10⁵ warp jump, ×1-vs-×10⁵ equivalence — **suite must finish in <5 min** | every commit | CI-HOST |
| Save round-trip determinism | §7.5, N = 10 000 | every commit | CI-HOST |
| Shader validation | naga/WGSL compile for **both** binding models | every commit | CI-HOST |
| Code quality | `xtask lint-quality` — complexity budgets (cognitive ≤15/25, ≤100/200 lines, ≤8 args), `cargo-geiger`, duplication stats | weekly + dep-touching PRs | CI-HOST |
| Coverage | `cargo-llvm-cov`; floors in `ci/coverage-floors.json`; **ratchet: no decrease >0.5 pp** | per commit | CI-HOST |
| Dead code / deps | `clippy -D warnings` (live) + `cargo-machete` + `cargo-udeps` | weekly / nightly | CI-HOST |
| Docs | `cargo doc` `RUSTDOCFLAGS=-D warnings`; `missing_docs` warn on public API | per commit | CI-HOST |
| Mutation testing | `cargo-mutants` on determinism crates, ≤45 min, missed-mutant tracking | weekly | CI-HOST |
| MSRV | `cargo msrv verify` vs `rust-version = "1.95"` | weekly | CI-HOST |
| CPU perf budgets | sim ms/tick, rails events/frame + ms/frame, weld Stage-1 ms (§5.3 CPU column); fail on >10% regression | nightly | CI-HOST |
| Golden replays (full) | + colony century, pathological event density, 100-sim-year economy — **≤60 min** | nightly | CI-HOST |
| Physics oracle | `SimpleBackend` vs `RapierBackend` on **≤2 s contact-free analytic** scenarios only (§2.9) | nightly | CI-HOST |
| Cross-platform divergence **report** | T1 replays on mac-arm64, win-x64 — **report, never fail** | nightly | hosted |
| Baseline screenshots | lavapipe, loose tolerance, Baseline tier only | nightly | CI-HOST |
| **Advanced screenshots + fps budgets** | §5.1 correctness tolerance, §5.3 fps rows | **milestone exits + weekly** | **REF-A / REF-B**, results committed to `docs/perf/` |

**Hosted runners have no GPU.** The Advanced tier (bindless, `multi_draw_indirect`, mesh shaders)
cannot even be instantiated on lavapipe, and 144 fps is not a software-rasterizer number. Scheduling
those in nightly CI guarantees they get quietly skipped; they are therefore **reference-machine
gates with a committed perf log**, not CI gates. This is the honest split.

### 8.3 Fixture data — why golden replays are not red after every balance tweak

All balance constants live in `data/balance.ron`, the data set is hashed into the save, and goldens
hash sim state — so editing one number would red every replay, and the team would learn to bless
blindly.

- **Golden replays load `data/test-fixtures/<scenario>/`** — frozen, versioned data sets never
  touched by balance work. Changing a fixture is a deliberate commit that blesses goldens.
- **Shipping data gets its own cheaper gate**: "validates, loads, and runs 1 000 ticks with no NaN
  and no `Fatal`".
- The **economy harness** is a shipping-data tool, not a golden: it fast-forwards 100 sim-years,
  plots the currency curves, and asserts the §3.5 bands (2–5% funds drain per sim-month, save size
  ≤32 MB, object caps respected). It fails the nightly build on band violation.

### 8.4 Blessing workflow

`xtask bless-goldens` regenerates goldens **and** their fingerprint files, refuses to run on a dirty
tree, refuses to run if the state hash changed without a `HASH_SCHEMA_VERSION` bump or a documented
behavioural change, and emits a diff summary for the commit message.

### 8.5 End-to-end testing — the missing layer

The pyramid (§8.1) deliberately stops at golden replays; e2e sits above it and is *not* the same
thing. E2e exercises the system the way a player or agent would, through public surfaces only.

| Tier | Harness | What it proves | Where |
|---|---|---|---|
| **Sim e2e** | `csp-bench` scenario files (`scenario.ron`: data set + initial snapshot + command log source + goal predicate + tick budget) | Whole systems interact correctly over mission-scale time: launch→orbit→land, contract lifecycle, colony supply loop, agent completing a mission | per-commit smoke set (<5 min), nightly full |
| **App smoke e2e** | `csp --smoke --frames N --snapshot out.png` — headless launch, renders N frames via lavapipe/Baseline, writes a screenshot + state hash | The *integration* works: winit+winit GPU init, frame loop, schedules, egui, snapshot export — the thing unit tests structurally can't see | nightly on CI-HOST (lavapipe) |
| **Interface e2e** (M9c+) | `csp-server` on a fixture save; a test client drives MCP tool calls and the WS subscription protocol | Protocol versioning, auth/token flow, subscription decimation, command ordering of an external client vs golden | nightly |
| **Web e2e** (post-v1) | headless-browser smoke of the wasm build + telemetry SPA | The reduced web tier actually boots | weekly, when web tier exists |

Rules: e2e tests assert **observable behaviour** (state hash, telemetry values, goal predicates),
never internals; every e2e failure must decompose to a pyramid layer below — a failure that can
only be seen at e2e level means a missing property/unit test, and writing it is part of the fix.

### 8.6 Code-quality gates (CRAP, complexity, redundancy)

Quality is enforced by tooling that fails the build, not by discipline. Budgets are per-function
unless stated; exemptions require a `// lint-allow: <reason>` comment naming this section.

**Complexity budgets** — `[workspace.lints.clippy]` + `xtask lint-quality`:
- `cognitive_complexity` ≤ **15** warn / ≤ 25 error; `type_complexity` ≤ 10⁴; `too_many_lines`
  ≤ 100 warn / ≤ 200 error; `too_many_arguments` ≤ 8
- **CRAP score** (`rust-code-analysis` via `xtask lint-quality`, weekly): cyclomatic ≤ 10 with
  coverage ≥ 80% ⇒ CRAP ≤ ~30 threshold per function; report-only for the first quarter, then a gate
- `forbid(unsafe_code)` in `csp-core`, `csp-sim`, `csp-phys`, `csp-data`, `csp-script` — the
  determinism domain takes zero unsafe (wgpu-facing crates may use `deny`, not `forbid`)
- `cargo-geiger` report in the weekly quality job; new `unsafe` anywhere requires a comment
  justifying it at the call site

**Redundancy & dead code:**
- `dead_code`/unused imports/variables: already errors via `clippy -D warnings` — keep it so
- **`cargo-machete`** (unused deps) weekly + on dep-touching PRs; `cargo-udeps` nightly (slower,
  catches what machete misses)
- `cargo doc` built with `RUSTDOCFLAGS="-D warnings"` — broken intra-doc links fail
- `cargo dup`-style duplication is not a Rust gate; instead: `xtask lint-quality` fails on any
  function >200 lines (the usual duplication smell) and the weekly quality job reports
  `tokei` duplication stats for human review
- `missing_docs` lint = warn on all public API in `csp-*` crates (docs are the API contract)

**Coverage (cargo-llvm-cov):**
- Floors: `csp-core` ≥ **90%** lines, `csp-sim`/`csp-phys`/`csp-data` ≥ **80%**, `csp-view` ≥ 60%.
  `csp-app`, `csp-render`, `csp-ui`, `csp-server` — no floor (platform glue; e2e owns them).
- **Ratchet rule**: a PR may not *lower* any floored crate's coverage by >0.5 pp. The floor file
  (`ci/coverage-floors.json`) bumps upward only.
- Coverage is a tripwire, not a goal: 100% is not a target; uncovered *branches* in the
  determinism domain are what matter — `llvm-cov` region/branch report reviewed in the weekly job.

**Mutation testing (cargo-mutants):**
- Weekly on `csp-core`, `csp-sim`, `csp-phys`, `csp-data`, capped at 45 min. Missed mutants get a
  tracked issue or an `#[allow]`-equivalent with justification; the *trend* matters more than the score.
- Mutants are how we know the property tests actually bite — a suite that kills nothing is decoration.

**Static policy:**
- `[workspace.lints.rust]` + `[workspace.lints.clippy]` shared across crates; per-crate overrides
  only for the clippy.toml determinism lints (already scoped)
- `cargo fmt --check` per commit (already); `cargo clippy --workspace -- -D warnings` per commit (already)
- `cargo-semver-checks` on crates that publish APIs (`csp-script` post-M9c only)
- MSRV: `cargo msrv verify` weekly against `rust-version = "1.95"` in workspace.package

### 8.7 Rules of engagement (process, not tooling)

1. **No code lands without its tests.** Every milestone exit criterion implies the tests proving it
   were written in the same change. "Tests later" is how the §8.1 pyramid dies.
2. **Fix the root, not the test.** A red golden/property test is a bug until proven a deliberate
   behavioural change — deliberate changes bless via `xtask bless-goldens` (§8.4), nothing else.
3. **Flaky policy:** a test that fails nondeterministically gets *one* retry in CI, then it's
   quarantined to a `#[ignore]`d test + a tracking issue within 24 h. Quarantined determinism tests
   are P0 — a flaky determinism test is an actual determinism bug until disproven.
4. **Benchmarks:** `criterion` for hot paths (sim step, Kepler solve, weld Stage-1, voxelize,
   state-hash); `cargo bench` results committed to `docs/perf/` on REF-A, regression gate >10%.
5. **Test data is code:** fixtures under `data/test-fixtures/` version with schema_version like
   everything else; never generated on the fly in a way that can drift between CI and local.
6. **The quality job is weekly, not per-commit** (mutants, udeps, code-analysis, msrv): per-commit
   stays <5 min or it will be bypassed; slow gates live where their runtime is acceptable.

---

## 9. Risks

| Risk | Reality | Mitigation |
|---|---|---|
| **Solo scope** | 51 EU ≈ 3 570 h ≈ 6.9 years at 10 h/week. This is the dominant risk and it is not technical. | §1.2 capacity; §6.4 cut list ordered by actual refund; **§6.5 quarterly cut trigger**; 33 EU minimum v1; M4c fun gate before the expensive half |
| Determinism drift | Floats, hash-map order, arena order, thread counts, libm, backend versions, warp-as-measurement | T1 only (§2.7); §8.1 pyramid so failures are diagnosable; §7.1 fingerprints so dependency bumps are explained; `SimpleBackend` default |
| Interval-vs-tick divergence | Two integrators per subsystem silently make warp choice matter | §2.6.1 single-integrator rule + the ×1-vs-×10⁵ equivalence test as a per-commit gate |
| Timewarp correctness | A subsystem that can't publish a next-event time breaks rails warp | §2.6.5 is a hard architectural rule; debug assert that rails advance touched no stepping code; event-count budget with partial advance |
| Event-density collapse | 20 colonies × converters + 500 debris perpetually clamps or stalls warp | §2.6.5 Clamping/Silent classes, coarse-event rule, `Dormant` colonies, 2000-event budget, pathological nightly scenario |
| "Bleeding edge + web" | These are two renderers | §5.1 tier contract with a defined correctness tolerance; web is a fixed demo (§6.3) and is cut *last* because its cost is non-refundable anyway |
| Rapier / contact determinism | Warm-start impulses are unserializable | §7.4 save refusal + surface-rails latch so landed play is unaffected; Rapier confined to transients; oracle test scoped to contact-free cases |
| Stiff contact at a fixed 1/64 s dt | Substepping is forbidden by the headline rule | §2.9 PGS impulse solver + Baumgarte + settle latch; M2's 20 t-lander exit criterion |
| egui ceiling for VAB | egui is fine for panels, wrong for a 3D placement canvas | Editor viewport is `csp-render` + GPU picking; §5.4 kill criterion: >16 ms at 300 parts ⇒ replace the panel layer |
| Aero table fidelity | A coarse direction set makes SAS and autopilots oscillate | 66-direction geodesic + barycentric interpolation; M4a's `Cm_α` ≤5% exit criterion |
| wasm memory & threading | 32-bit, ~2 GB, single-threaded, no "off thread" | Fixed demo dataset (§6.3); Stage-2 weld incremental across frames; measured in CI |
| Script API churn | Freezing `ControlApi` too early vs too late | Freeze at **end of M9c**, after autopilots and MCP have exercised it; unstable and versioned before that |
| Content volume | Parts, biomes, tech nodes, contracts are weeks of authoring | Authoring tools + validators from M3; §3.0/§3.0.1 formats fixed now; CC0 + procedural |
| Balance | Three interacting currencies | All constants in `data/balance.ron`; economy harness with asserted bands (§8.3) |
| TanStack Start maintenance tax | ~daily patches across ~20 interdependent packages in a Rust project | Exact version pins; the SPA is schedule slack (§6.0) and can never block the critical path; tradeoff recorded in §1 |

---

## 10. Open questions (genuinely open; decide later, non-blocking)

- **Script surface syntax**: Rhai directly, or a kerboscript-flavoured DSL compiled to Rhai. Backend
  is settled (Rhai); only the ergonomics layer is open. Decide at M9a with a real ascent script.
- **Part art pipeline**: glTF + custom material extensions is the assumption; confirm at M3.
- **Terrain source**: procedural noise vs authored heightmaps vs hybrid. Hybrid is likely; decide at
  R2, because it determines whether virtual texturing is ever needed. *(Biomes are already decided
  and decoupled — §3.0.1 — so this question no longer threatens the science economy.)*
- **Low-thrust-on-rails** (ion burns under warp): worth the accuracy caveats? Post-v1.
- **Naming/identity**: "Clanker" is a working title — needs a real one before anything public.

Resolved and therefore removed: reputation (→ §3.6, no currency, fact-based prerequisites + a
derived display `standing`); save-game determinism implications (→ §7); background thrust (→ §2.6.2,
owner-confirmed none in v1); commercial intent and licensing (→ §1.2).

---

## Appendix A. Scaffold fixups — required before M1 starts

The committed `crates/` reads as "M0 done"; it is "M0 windowing done". These are concrete code debts,
not design questions. **M0 is not complete until every item here is closed**, because M1 onward
depend on them and because the scaffold is the first pattern an implementer copies.

1. **Move the clock out of the ECS.** `csp-view::Clock(SimTime)` is advanced by a bevy system
   (`advance_clock`) inside the `sim_step` schedule, and `csp-app` reads sim time from the ECS. That
   is authoritative sim state living in the ECS — exactly what §2.2 forbids. Fix: `SimTime` moves
   into `csp_sim::World`; `csp-view`'s `Clock` becomes a mirrored presentation-only copy written by
   `Sync`; `SimStep` becomes the single system calling `csp_sim::World::step_frame()`.
2. **Fix schedule ordering.** The loop runs `view.sim_ticks()` before `view.frame()`, and `frame()`
   runs `input` first — so `Input` executes *after* `SimStep`, inverting §2.5. Reorder to
   `Input → SimStep → Sync → Presentation → UiBuild → RenderExtract`.
3. **Move egui into `UiBuild`.** It currently runs inline in `csp-app::render`, reading
   `view.world` directly.
4. **Replace the RNG seeding path.** The scaffold folds `(save_seed, tag, entity, counter)` into one
   `u64` and calls `ChaCha8Rng::seed_from_u64` — 64 of 256 seed bits through a hand-rolled mixer,
   and `seed_from_u64` is a `rand_core` implementation detail. Fix: §2.7's BLAKE3 KDF producing the
   full 32-byte key, `KDF_VERSION` in the save header, and a hard-coded-expected-output test.
   Also reshape the API to the `StreamKey { tag, entity, counter }` struct so every call site is
   4-valued, and update the doc comments that quote the old `Rng::for_(...)` spelling.
5. **Write the state-hash / replay harness** (§7.1). It does not exist, and it is an M0 exit
   criterion and on the critical path of every later milestone.
6. **Add `clippy.toml`.** `disallowed-methods` (the stated libm enforcement) and `disallowed-types`
   (`HashMap`/`HashSet`/`std::time`) are not configured anywhere, so none of §2.1/§2.7's "enforced"
   rules are enforced. Apply via `[lints]` in the four determinism-domain crates only.
7. **Add `[workspace.dependencies]`, `[workspace.lints]`, `rust-toolchain.toml`, `deny.toml`.**
   Six crates independently declare `wgpu = "30"`, `egui = "0.37"` with caret ranges; MSRV 1.95
   (from `bevy_ecs` 0.20) is recorded nowhere; CI uses floating `stable`. For a project whose golden
   hashes are version-sensitive by design, that means hashes can change on an unrelated Tuesday.
   Exact `=` pins for the determinism domain (`libm`, `parry3d-f64`, `rapier3d-f64`, `rand_chacha`,
   `rand_core`, `blake3`, `ciborium`, `miniz_oxide`, `bincode`), caret for the rest. `Cargo.lock` is
   already committed — keep it.
8. **Fix the wasm CI gate.** `cargo check --workspace --target wasm32-unknown-unknown` currently
   passes only because `csp-server`/`csp-bench` are empty; the day axum/tokio lands it breaks and
   the reflex will be to weaken the gate. Add `--exclude csp-server --exclude csp-bench` **plus** the
   positive wasm-purity assertion (§8.2).
9. **Replace the dependency-rule one-liner.** `! cargo tree -p X 2>/dev/null | grep -E ...` passes
   silently when `cargo tree` fails (empty output → grep fails → `!` succeeds), and `"rand "`
   matches nothing — while `csp-core` does pull `rand_chacha`. Replace with `xtask lint-deps`
   (explicit per-crate allow/deny, fails on tooling error) plus `cargo-deny` `[bans]`, which also
   delivers §3.7's license audit.
10. **Declare `bevy_ecs` feature sets per target** (§2.5): `["std","multi_threaded",
    "async_executor"]` on native, `["std"]` on wasm. The scaffold's `["std"]`-only build cannot run
    anything in parallel, which contradicts §2.5's promise.
11. **Create the missing crates and data directories**, or annotate §2.1 with `(not yet created)` —
    done above. Missing crates: `csp-data`, `csp-phys`, `csp-ui`, `csp-script`, `csp-ai`. Missing
    dirs: `data/balance.ron`, `data/loc/`, `data/contracts/`, `data/systems/home/*`,
    `data/test-fixtures/`, `apps/telemetry/`, `xtask/`. Creating the empty crates now gives the
    dependency lint something to enforce.
12. **Implement `xtask lint-attribution`** with the §3.7 line format. Low stakes until the first
    borrowed asset — which is also when it is cheapest to add and easiest to forget.
13. **Add `LICENSE-MIT`, `LICENSE-APACHE`, `LICENSE-GAME`** and set `license` per-crate rather than
    workspace-wide (§1.2). The workspace currently declares `MIT OR Apache-2.0` for everything,
    which would let anyone sell the game.
