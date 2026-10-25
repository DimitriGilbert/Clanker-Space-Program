# Exploration Map — Ruthless Audit Session 2026-10-25

Working tree state at map time: DIRTY (another agent concurrently editing render/view/app files):
modified: csp-app/src/lib.rs, csp-render/src/{flight.rs,flight.wgsl,lib.rs,vab.rs,vab.wgsl}, csp-ui/src/vab.rs,
csp-view/src/{art.rs,flight.rs,part_mesh.rs,vab.rs}, data/art/{parts,terrain}.csar, xtask/src/bin/bake_art.rs;
untracked: shit-sky.jpg. Review targets CURRENT working-tree state at read time.

## 1. Project structure overview

KSP-style rocket/space game. Rust workspace (edition 2024, rust 1.95), wgpu 30 rendering,
bevy_ecs world, egui 0.37 UI, rhai scripting, strict determinism discipline (blake3/ChaCha/libm
exact pins, hash-frozen golden replays, unwrap/expect denied workspace-wide).

Dependency layering (PLAN §2.1): csp-core -> csp-data -> {csp-sim (over csp-core), csp-phys}
-> csp-script/csp-ai -> csp-view (ONLY crate joining sim+render) -> {csp-render, csp-ui} -> csp-app.
Side trees: csp-server (native-only HTTP/MCP telemetry), csp-bench (replay/perf/eval harness), xtask.

```
crates/
  csp-core    determinism primitives: hashing, RNG, math (DVec3/DQuat), time
  csp-data    RON data schema, loaders, validation, data hashing (data/ is the tuning source of truth)
  csp-sim     the simulation world: vessels, orbits, colonies, economy, research, events, saves
  csp-phys    craft physics: rigid-body integration, part resolve/weld/merge, aero, thermal, structural
  csp-render  wgpu renderers + WGSL shaders (flight, terrain, map, vab)
  csp-view    headless game shell: ECS schedules, render-extract packets, VAB view state, art decode
  csp-ui      pure egui panel fns (VAB editor UI, navball, settings, tutorial)
  csp-app     desktop/wasm shell: winit loop, egui-wgpu paint, audio, warp, settings IO, bug report
  csp-script  rhai guest host (no_std, op budget, API freeze tests)
  csp-ai      ascent autopilot (small)
  csp-server  native-only axum HTTP+WS+MCP telemetry server (library, no bin)
  csp-bench   golden replay, perf budgets, eval scenarios, 4 CLI bins
xtask/        lint-deps, lint-attribution, bless-goldens, build-web, bake_art, gen_biome_maps
apps/demo     prebuilt wasm web demo (dist/ is generated; index.html authored)
apps/telemetry React/Vite telemetry dashboard (TS, logic-bearing)
data/         RON game data + baked .csar art packs + golden test fixtures
assets/       bake inputs (CC0/NASA) — NOT read at runtime
```

Reviewable totals (wc -l, excludes target/, node_modules/, dist/, lock files, .csar/.jpg/.glb blobs):
- Rust src: ~116,400 LOC (crates) + 2,866 (xtask)
- WGSL shaders: 1,076 LOC
- Integration tests (crates/*/tests): ~5,365 LOC
- Cargo.toml manifests + build.rs: ~750 LOC
- apps/telemetry authored TS/TSX: ~1,080 LOC
- **Grand total reviewable: ~127,500 LOC**
- Test inventory: **879 `#[test]` fns** (+2 `#[ignore]`d visual dumps):
  csp-sim 337, csp-phys 157, csp-data 104, csp-bench 61, csp-view 59, csp-app 38, csp-ui 34,
  csp-core 42, csp-server 28, csp-script 14, csp-render 5, csp-ai 0. Most are inline `#[cfg(test)]`
  mod tests in src (sim/phys/ui/app/core/render); csp-data/csp-view/csp-phys/csp-script/csp-server/csp-bench
  also have crates/*/tests integration files.

## 2. Review clusters (21 = number of review agents to dispatch)

### C1. csp-core — determinism primitives (9 files, ~1,760 LOC)
Files: crates/csp-core/src/{lib.rs 25, hash.rs 781, rng.rs 319, quat.rs 232, vec3.rs 197, time.rs 93, error.rs 55}, Cargo.toml 19, clippy.toml.
Deps: none internal (blake3, rand_chacha, libm exact-pinned).
Focus: determinism-domain correctness — hash stability/schema versioning, ChaCha RNG streaming, quaternion/vector math edge cases (singularities), libm-only float paths.

### C2. csp-data — loaders & schema types (15 files, ~4,850 LOC)
Files: crates/csp-data/src/{load.rs 1030, part.rs 874, system.rs 219, biome.rs 360, contract.rs 372, techtree.rs 252, balance.rs 249, colony.rs 212, site.rs 91, resource.rs 79, experiment.rs 74, recipe.rs 58, ids.rs 144, loc.rs 28, lib.rs 120}.
Deps: csp-core, ron, serde.
Focus: RON parsing error handling, indexed-PNG biome decode, unknown-field strictness, ID interning, data shape coherence.

### C3. csp-data — validation + datahash + tests (11 files, ~7,050 LOC)
Files: crates/csp-data/src/validate.rs 5679, src/datahash.rs 229, tests/{parts.rs 246, home_system.rs 198, unknown_fields.rs 194, contracts.rs 149, balance.rs 123, colonies.rs 117, experiments.rs 72, techtree.rs 47}, Cargo.toml.
Deps: C2 types.
Focus: validation completeness (can bad data slip through?), determinism fingerprint hashing (file set walk, extension list incl. csar), test coverage honesty.

### C4. csp-sim/src/lib.rs — the world monolith (3 files, 21,484 LOC src)
Files: crates/csp-sim/src/lib.rs 21484, Cargo.toml, clippy.toml.
Focus: SimWorld construction, vessel lifecycle, command log (§7.6), tick loop, clock, bevy_ecs usage, resource/state management. Single 21.5k file — reviewer must section it (types / world ops / commands / vessels / integration points).

### C5. csp-sim — orbit & transfers (4 files, ~6,835 LOC)
Files: crates/csp-sim/src/{orbit.rs 2525, transfer.rs 2284, transfers.rs 1663, maneuver.rs 362}.
Focus: physics/simulation correctness — conic propagation, SOI transitions, transfer planner math, maneuver nodes, numerical stability.

### C6. csp-sim — colonies & contracts (2 files, ~6,820 LOC)
Files: crates/csp-sim/src/{colonies.rs 4937, contracts.rs 1883}.
Focus: colony simulation loop, resource production/consumption, contract generation/completion logic.

### C7. csp-sim — economy/research/tech/manufacture (4 files, ~4,025 LOC)
Files: crates/csp-sim/src/{economy.rs 1405, research.rs 1300, tech.rs 729, manufacture.rs 591}.
Focus: currency math, research tree progression, part unlock gating, manufacturing throughput.

### C8. csp-sim — reliability/events/persistence/life-support (7 files, ~7,500 LOC)
Files: crates/csp-sim/src/{reliability.rs 1784, joints.rs 1218, save.rs 960, life_support.rs 754, scripts.rs 737, events.rs 719, bubble.rs 325}.
Focus: part failure rolls (RNG discipline), joint constraints, save serialization versioning + determinism of saves, event budget (§2.6.5), physics-bubble handoff (float bubble vs orbit).

### C9. csp-phys — core integration trio (3 files, ~10,235 LOC)
Files: crates/csp-phys/src/{simple.rs 3548, resolve.rs 3543, session.rs 3143}.
Deps: csp-core, csp-data (part catalog); rapier/parry declared but per Cargo.toml comments NOT yet depended on by any crate.
Focus: rigid-body step correctness, part attachment resolution, CraftSession state machine (attach/detach/staging).

### C10. csp-phys — domain modules + tests (15 files, ~8,770 LOC)
Files: crates/csp-phys/src/{aero.rs 2033, weld.rs 1577, merge.rs 865, thermal.rs 831, readout.rs 706, structural.rs 544, part.rs 440, profile.rs 402, graph.rs 355, hull.rs 329, tensor.rs 324, backend.rs 199, lib.rs 145}, tests/{resolve_parts.rs 267, session.rs 244}, Cargo.toml + clippy.toml (17 entries — treat clippy.toml/Cargo.toml as one).
Focus: aerodynamic model, weld/merge invariants, heat propagation, structural stress accumulation, part-graph correctness.

### C11. csp-render — wgpu renderers + WGSL (10 files, ~3,900 LOC) [WORKING-TREE-MODIFIED]
Files: crates/csp-render/src/{terrain.rs 1623, terrain.wgsl 854, lib.rs 349, vab.rs 428, vab.wgsl 88, flight.rs 233, flight.wgsl 86, map.rs 159, map.wgsl 48}, Cargo.toml.
Deps: wgpu 30, bytemuck.
Focus: rendering pipeline correctness — reverse-Z depth (Greater compare, 0.0-clear), vertex packing (Pod derives), terrain heightfield/sky/atmosphere shader math, pick-pass encoding, uniform layouts vs WGSL structs. NOTE: flight.rs/flight.wgsl/vab.rs/vab.wgsl/lib.rs are mid-edit by the other agent.

### C12. csp-view — flight/map/terrain extract (5 files, ~6,950 LOC) [flight.rs, part_mesh.rs, art.rs MODIFIED]
Files: crates/csp-view/src/{flight.rs 2840, map.rs 2197, terrain.rs 634, part_mesh.rs 946, art.rs 396}.
Deps: csp-sim, csp-render, csp-data, miniz_oxide (.csar decode).
Focus: RenderExtract packet producers — camera math, LOD/culling, orbit-line building, .csar art-pack decode correctness, per-part mesh generation.

### C13. csp-view — shell, VAB view, demo + ALL tests (14 files, ~5,950 LOC) [lib-adjacent vab.rs MODIFIED]
Files: crates/csp-view/src/{lib.rs 2397, vab.rs 1119, tutorial.rs 278, demo.rs 252}, tests/{render_gate.rs 559, demo.rs 261, stage_plan.rs 191, tutorial.rs 176, vab_latency.rs 165, pad_spawn.rs 141, part_silhouettes.rs 138, flight_focus.rs 58, startup_colonies.rs 34}, Cargo.toml.
Focus: app state management (View/ViewMode switching, §2.5 frame loop begin/sim_step/end, command enqueue), headless View API, VAB session bridging, demo scenario constraints; render_gate = the offscreen render verification (see Part B).

### C14. csp-ui — egui panels (5 files, ~7,110 LOC) [vab.rs MODIFIED]
Files: crates/csp-ui/src/{vab.rs 3932, lib.rs 1073, settings.rs 1019, navball.rs 542, tutorial.rs 541}.
Deps: egui, csp-phys (CraftSession lives here), csp-data, csp-core; NO wgpu/winit (pure).
Focus: UI/UX logic — VAB editor interactions (attach planner, symmetry, snapping), Δv readout, navball projection math, settings schema/round-trip.

### C15. csp-app — desktop/wasm shell (9 files, ~6,320 LOC) [lib.rs MODIFIED]
Files: crates/csp-app/src/{lib.rs 3409, audio.rs 839, settings_io.rs 708, warp.rs 593, synth.rs 383, bugreport.rs 237, logring.rs 77, main.rs 18}, Cargo.toml.
Deps: everything; winit 0.30, egui-wgpu/egui-winit, env_logger.
Focus: app state machine + window/event handling, scene paint pass ordering (terrain then flight, clear color, reverse-Z target), time-warp scheduling, audio backend probing, settings file IO, bug-report CLI, wasm entry.

### C16. csp-script + csp-ai (5 files, ~1,900 LOC)
Files: crates/csp-script/src/lib.rs 1464, csp-script/tests/api_freeze.rs 169, csp-script/Cargo.toml; csp-ai/src/lib.rs 222, csp-ai/Cargo.toml.
Deps: rhai =1.27.0 no_std (op budget, no wall clock), csp-sim.
Focus: guest sandbox security/budget enforcement, host API surface freeze, ascent autopilot correctness.

### C17. csp-server — HTTP/WS/MCP telemetry (11 files, ~5,930 LOC)
Files: crates/csp-server/src/{mcp.rs 1124, http.rs 1059, backend.rs 1029, dashboard.rs 472, serve.rs 289, ingress.rs 273, telemetry.rs 144, lib.rs 45}, tests/e2e.rs 1293, PROTOCOL.md 93, Cargo.toml.
Deps: axum/tokio (native-only, wasm ban list).
Focus: security — Origin allowlist/DNS-rebinding, bearer token, bound sizes; MCP JSON-RPC engine correctness; backpressure in ingress; e2e test fidelity.

### C18. csp-bench — replay/perf/eval harness (15 files, ~7,760 LOC)
Files: crates/csp-bench/src/{replay.rs 1628, perf.rs 1112, scenarios.rs 1020, scenario.rs 720, agent.rs 607, economy.rs 478, contracts.rs 308, eval.rs 237, lib.rs 37, build.rs 206, bin/{csp-mcp.rs 149, csp-eval.rs 133, csp-replay.rs 112, csp-perf.rs 102}}, tests/eval_suite.rs 523 (16 entries — build.rs counts with lib).
Focus: determinism replay (record/verify, hash freeze, fingerprint sidecars), perf budget rows, eval scoring; bins are the only runnable game-drivers without a window.

### C19. xtask — repo tooling (6 files, ~2,870 LOC) [bake_art.rs MODIFIED]
Files: xtask/src/{main.rs 393, lint_deps.rs 942, lint_attr.rs 319, bin/bake_art.rs 900, bin/gen_biome_maps.rs 312}, Cargo.toml.
Focus: asset pipeline (bake_art: GLB weld/subdivide/normal-split -> .csar v2), lint rule engines, bless-goldens refusal logic (dirty tree, partial artifacts, schema-version gate), build-web atomic staging.

### C20. workspace config, CI, attribution, demo shell (7 files, ~700 LOC)
Files: Cargo.toml (workspace 1: pins/lints), deny.toml, rust-toolchain.toml, .github/workflows/ci.yml, assets/ASSETS.md [MODIFIED], data/ATTRIBUTION.md [MODIFIED], apps/demo/index.html.
Focus: dependency pin policy (determinism domain exact pins), clippy lint gates, CI completeness vs PLAN §8.2, license/attribution completeness for shipped art.

### C21. apps/telemetry — TS dashboard (15 files, ~1,080 LOC)
Files: apps/telemetry/src/routes/{index.tsx 159, __root.tsx 24}, src/lib/{ws.ts 245, dashboard.ts 119, utils.ts 7}, src/lib/widgets/{orbit-map.tsx 136, graph.tsx 69, gauge.tsx 50, table.tsx 39, registry.tsx 31}, src/components/ui/card.tsx 22, scripts/finalize.mjs 12, vite.config.ts, package.json, dashboards/default.json (routeTree.gen.ts is generated — skim only).
Focus: WS reconnect/backpressure handling, dashboard widget rendering, orbit-map projection.

Context docs (not review targets, but authoritative): PLAN.md (1,967 lines — the spec; reviewers should cross-check), crates/csp-server/PROTOCOL.md, docs/perf/.

## 3. Cluster count = 21 review agents

## 4. RUN / CAPTURE INTEL

### 4.1 Build / test / run commands (from .github/workflows/ci.yml + xtask)
- Format: `cargo fmt --all -- --check`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings` (unwrap_used/expect_used denied workspace-wide)
- Tests: `cargo test --workspace`
- Wasm gate: `cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown`
- Repo lints: `cargo run -p xtask -- lint-deps` and `cargo run -p xtask -- lint-attribution`
- Golden replays: `cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/trivial`
- Regenerate goldens: `cargo run -p xtask -- bless-goldens [--reason "..."]` — HARD-REFUSES on dirty tree (tree IS dirty now)
- Web demo: `cargo run -p xtask -- build-web` (wasm-bindgen-cli 0.2.129 required)
- Run game (desktop): `cargo run -p csp-app` — needs a display (winit); NOT possible in this env
- Bug report: `cargo run -p csp-app -- --bug-report` (headless session replay, no GPU)
- Headless game drivers: `cargo run -p csp-bench --bin csp-{replay,perf,eval,mcp}`

### 4.2 crates/csp-view/tests/render_gate.rs — the offscreen renderer
What it does: creates a wgpu device with NO surface (`InstanceDescriptor::new_without_display_handle`,
`Backends::PRIMARY`, `PowerPreference::LowPower` — prefers llvmpipe when present, `compatible_surface: None`),
renders the flight scene exactly like csp-app's scene pass (clear color [0.02,0.02,0.05,1] linear,
reverse-Z depth cleared 0.0, terrain draw then flight draw), reads pixels back via
`copy_texture_to_buffer` + `map_async` + `device.poll(Wait)`. 320x240, `Rgba8UnormSrgb`
(320*4 = 1280 divisible by wgpu's 256-byte row alignment).
Tests:
- `boot_scene_has_real_geometry` — seeded new-game view must rasterize real geometry (pixel-stat assertions)
- `empty_flight_scene_is_what_the_gate_rejects` — negative control (the shipped "uniform purple" bug shape)
- `dump` — #[ignore] scratch: writes the seeded flight frame to **/tmp/frame.ppm** (P6 PPM, 320x240, camera 25 m)
- `dump_sky` — #[ignore] scratch: writes **/tmp/sky_{north,equator,south}.ppm** (terrain sky pass alone, star map)
No PNG output — PPM only; `magick` (ImageMagick) IS installed for conversion. No env vars needed
(no WGPU_BACKEND required; adapter auto-selected; no DISPLAY needed — surfaceless).
data/ resolution: `csp_view::data_root()` tries `./data` then `CARGO_MANIFEST_DIR/../../data` — works from any CWD.

### 4.3 Can the main app run headless?
NO. csp-app is winit-windowed (`EventLoop::new()`, `create_window`); only `--bug-report` runs headless (no GPU/render).
No `--headless`, no screenshot mode, no in-app capture. DISPLAY is unset and **Xvfb/xvfb-run are NOT installed**,
so the native app cannot open a window in this environment.

### 4.4 GPU availability (verified this session)
- `/dev/dri`: card1 + renderD128 present (AMD Renoir integrated GPU)
- vulkaninfo: **GPU0 = AMD Radeon Graphics (RADV RENOIR), Vulkan 1.4.354, Mesa 26.2.4** (real GPU);
  **GPU1 = llvmpipe CPU (lavapipe), conformance 1.3.1.1** (software)
- ICDs installed: lvp, radeon, intel, hasvk, nouveau, dzn, etc. (dzn fails instance creation harmlessly)
- Conclusion: offscreen wgpu rendering works out of the box here — hardware (RADV) and software (lavapipe) both available.

### 4.5 EXACT screenshot commands

Flight / 3D scene (pad craft, chase camera — WORKS TODAY):
```
cargo test -p csp-view --test render_gate dump -- --ignored --nocapture
magick /tmp/frame.ppm .review/session-ruthless-20261025/screenshots/flight_scene.png
```
Sky dome / star map (3 orientations — WORKS TODAY):
```
cargo test -p csp-view --test render_gate dump_sky -- --ignored --nocapture
for f in north equator south; do magick /tmp/sky_$f.ppm .review/session-ruthless-20261025/screenshots/sky_$f.png; done
```
Also useful (prints pixel-diversity stats, catches "uniform purple" regressions):
```
cargo test -p csp-view --test render_gate -- --nocapture
```

Orbit map — NO existing dumper. Closest alternatives, in order of fidelity:
1. (Best, needs ~50 new lines by the fixing agent) clone the `dump` test pattern but drive map mode:
   `View::new()` + `seed_starter_craft()`, switch mode via the `map::MapInput`/`MapInputEvents` resources
   (see csp-view/src/lib.rs ~1450-1500: `set_map_viewport`, mode query), `view.take_map_packet()`
   (lib.rs:1478), then `csp_render::map::MapRenderer::new(device, FORMAT)` + `.draw(...)` + the same
   copy/readback/PPM block. Everything needed is public.
2. Headless chromium over the prebuilt wasm demo (real app, but dist/ may be stale vs in-flight edits, and
   WebGPU in headless chrome needs software WebGPU — unverified):
   `chromium-browser --headless=new --enable-unsafe-swiftshader --window-size=1280,800 --virtual-time-budget=20000 --screenshot=<out>.png file://$PWD/apps/demo/dist/index.html`
3. apps/telemetry `orbit-map.tsx` widget is a telemetry dashboard widget, NOT the game's map — do not present it as such.

VAB / editor UI — no offscreen egui path exists (egui-winit needs a window; no Xvfb). Alternatives:
1. (Scene only, no egui chrome) same new-dump-test pattern with `view.take_vab_packet()` (lib.rs:1552)
   + `csp_render::vab::VabRenderer` — 3D editor scene incl. pick pass.
2. Headless chromium + wasm demo (caveats as above) — the only way to capture actual egui UI panels in this env.

Server dashboard: csp-server is library-only (no bin); the HTTP dashboard + token gate is exercised by
tests/e2e.rs. Screenshotting it would require a small bin or test harness; low audit value.

### 4.6 Test inventory summary (static `#[test]` count)
| crate | tests | where | flavor |
|---|---|---|---|
| csp-sim | 337 | inline src | sim correctness, determinism |
| csp-phys | 157 | inline + tests/{resolve_parts,session} | physics |
| csp-data | 104 | inline + 8 test files | data validation gates |
| csp-bench | 61 | inline + tests/eval_suite | replay/perf/eval |
| csp-view | 59 (+2 ignored dumps) | inline + 9 test files | headless view, render gate |
| csp-core | 42 | inline | hash/rng/math |
| csp-app | 38 | inline | shell/audio/warp/settings |
| csp-ui | 34 | inline | panel logic |
| csp-server | 28 | inline + tests/e2e | HTTP/MCP incl. security |
| csp-script | 14 | inline + tests/api_freeze | guest API freeze |
| csp-render | 5 | inline | vertex packing/pipeline bits |

## 5. Project-specific gotchas
1. **Concurrent agent**: files are being edited right now (list at top). Re-stat before reviewing; do not cargo-build simultaneously — target/ lock contention. Coordinate: one cargo invocation at a time.
2. **Dirty tree blocks bless-goldens**: `cargo run -p xtask -- bless-goldens` refuses on any uncommitted change.
3. **Determinism domain**: csp-core/csp-data/csp-sim/csp-phys carry exact dep pins + per-crate clippy.toml + denied unwrap/expect. Any hash-affecting change needs `HASH_SCHEMA_VERSION` bump. Golden replays: `data/test-fixtures/*/` (replay.bin, expected-hash.txt, fingerprint.json) — committed artifacts, never hand-edit.
4. **data/ is runtime-required** for csp-view tests and the app; auto-resolved from manifest dir fallback. data/art/*.csar are BAKED binaries (format in xtask bake_art.rs header) — rebuilt via `cargo run -p xtask --bin bake_art`, sources in assets/ (never read at runtime).
5. **Reverse-Z everywhere**: depth cleared 0.0, `Greater/GreaterEqual` compare — a historical bug source (the "uniform purple" incident documented in render_gate.rs header). Render reviewers must check depth compare ops in every pass.
6. **wasm purity**: csp-server/csp-bench/xtask are native-only; wasm gate in CI; wgpu/egui versions sensitive (egui needs `default_fonts` feature or first layout panics).
7. **csp-sim/src/lib.rs is a 21.5k-line monolith** — budget reviewer time; it is the highest-risk single file in the repo.
8. rust-toolchain 1.95 / edition 2024; let-else and 2024 idioms in use.
