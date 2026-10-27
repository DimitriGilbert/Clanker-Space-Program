# Doc verification — docs/ARCHITECTURE.md vs working tree
Session: session-full-20261026 · Verifier: independent (did not author the doc) · Date: 2026-10-26
Method: every load-bearing claim traced to file/symbol in the working tree; lints (`lint-reach`) run; enums counted by brace-depth parse, not eyeball.

## Top summary

| Verdict | Count (claim groups) |
|---|---|
| VERIFIED | 41 |
| MINOR-INACCURATE | 5 |
| INACCURATE | 1 |
| UNSUPPORTED | 2 |

**Overall: PASS-WITH-FIXES.** The doc is unusually accurate: dependency graph, frame/data-flow hop names, all NEVER-table gates, all seven PLAN-divergences (code side), routing table, magics/constants, module lists, and LOC figures all check out. One substantive error (SpawnPhysicalVessel characterization), four stale counts/anchors, two claims unverifiable from code alone.

## Claims table

| # | Claim (doc §) | Verdict | Evidence / correction |
|---|---|---|---|
| 1 | §1 12 crates + xtask, ~150k LOC, edition 2024 | VERIFIED | `ls crates/` = 12; `find crates xtask -name '*.rs' \| xargs wc -l` = 150,267; root Cargo.toml `edition = "2024"`. |
| 2 | §1/§5.1 `TICK_HZ = 64`, dt 1/64 immutable, `SimTime` private field, only mutator `try_advance` | VERIFIED | csp-core/src/time.rs:9 `TICK_HZ: u64 = 64`, :21 `pub struct SimTime(u64)` private, :42 `try_advance`. |
| 3 | §1 glossary: WARP_LADDER `[1,2,10,100,1_000,10_000,100_000]`, 1.5×/30-frame hysteresis | VERIFIED | csp-app/src/warp.rs:33 ladder exact; :44 `PROMOTE_MARGIN: f64 = 1.5`, :46 `PROMOTE_FRAMES: u32 = 30`. |
| 4 | §1 bubble 20 km from `data/balance.ron` | VERIFIED | data/balance.ron:8 `bubble_radius_m: 20_000.0`. |
| 5 | §1 `VesselState::InBubble` / `SurfaceLatched`, `PatchedConics` in orbit.rs | VERIFIED | csp-sim/src/bubble.rs:198-212 enum; orbit.rs PatchedConics throughout. |
| 6 | §1 lease `Player \| Script(ScriptId) \| Agent(u32)` | VERIFIED | csp-sim/src/lib.rs:460-467. |
| 7 | §1 CSPCRAFT magic in csp-phys/session.rs; CSPSAVE\0 + JSON header + DEFLATE(CBOR) in csp-sim/save.rs | VERIFIED | session.rs:144 `CRAFT_FILE_MAGIC = b"CSPCRAFT\0"`; save.rs:7,97 `SAVE_MAGIC = b"CSPSAVE\0"`, DEFLATE(miniz_oxide)-over-canonical-CBOR. |
| 8 | §2 csp-view owns both sim and bevy_ecs world; `SimWorld` re-export; egui pass inside ui_build (`EguiPass`, `set_egui_input`, `take_ui_frame`) | VERIFIED | csp-view/src/lib.rs:17 `pub use csp_sim::World as SimWorld`, :327 EguiPass, :2290/:2482 set_egui_input/take_ui_frame, :2092 ui_build systems. |
| 9 | §2 bevy_ecs standalone, confined to csp-view; multi_threaded native-only | VERIFIED | workspace bevy_ecs `default-features=false`; grep: bevy_ecs only in csp-view; csp-view/Cargo.toml:37-38 `[target.'cfg(not(wasm32))']` + `["std","multi_threaded","async_executor"]`. |
| 10 | §3 dependency edges for all 12 crates + xtask | VERIFIED | Every edge in the table matches crate Cargo.tomls (incl. csp-app: core+render+view, csp-server native target-dep, csp-ui dev-dep, "no direct csp-sim edge" comment present; csp-bench: core,data,sim,script,ai,phys,server; xtask: csp-bench,image,miniz_oxide). |
| 11 | §3 csp-render depends only on csp-core; never csp-sim | VERIFIED | csp-render/Cargo.toml single workspace dep csp-core; no csp-sim anywhere in it. |
| 12 | §3 LOC per crate (1.9k/12.4k/21.0k/50.0k/2.0k/0.2k/21.0k/3.0k/12.3k/7.9k/6.1k; lib.rs 24.5k; vab.rs 4.9k; program.rs 2.9k; validate.rs 6.6k) | VERIFIED | wc -l: 1940/12410/21037/50043/1955/222/21213/2989/12316/7943/6102; lib.rs 24461; vab.rs 4938; program.rs 2859; validate.rs 6580. |
| 13 | §3 csp-bench is 7.7k | MINOR-INACCURATE | Actual 7,909 lines (≈7.9k). Trivial drift. |
| 14 | §3 csp-sim "17 modules" list | VERIFIED | Exactly those 17 .rs module files + lib.rs. |
| 15 | §3 `SimCommand` (52 variants) | MINOR-INACCURATE | Enum has **53** variants (51 struct-form + `SetWarp(u32)` + `AdvanceTo(u64)`; brace-depth parse of csp-sim/src/lib.rs:534). |
| 16 | §3 csp-ui "KeyAction (21 actions)" | MINOR-INACCURATE | `pub enum KeyAction` (csp-ui/src/settings.rs:190) has **24** variants (incl. QuickSave/QuickLoad/RevertToLaunch/OpenProgram — stale count). |
| 17 | §3 data/: 16 parts incl. chute + dockport, 6 contract templates, home system Solen/Terrin/Rask/Duva, one authored colony | VERIFIED | 16 files in data/parts (csp.chute.s125.ron, csp.dockport.s125.ron present); 6 in data/contracts; data/systems/home/{solen,terrin,rask,duva}; colonies.ron "colony.rask-mines". |
| 18 | §3 telemetry SPA TanStack Router/Start + Vite, exact-pinned | VERIFIED | apps/telemetry/package.json: @tanstack/react-router 1.170.46, react-start 1.169.4, vite 8.3.5, all exact, no carets. |
| 19 | §4.1 `settings_io::event_binding`, `App::dispatch/dispatch_with_key`, `throttle_dispatch`+`tick_throttle_ramp`, `attitude_press`/`tick_attitude`, `View::fire_stage`/`recover_control`, `request_session_op(SessionOp)` | VERIFIED | All symbols exist: settings_io.rs:633; csp-app/src/lib.rs:2597/2606/2160/2280/2000/2040/1050; SessionOp :3167; csp-view fire_stage :3189, recover_control :2635. |
| 20 | §4.1 source-id constants 0/1/2/3/4 (PLAYER_SOURCE_ID=1 in csp-view) | VERIFIED | lib.rs:72-83 WARP=0, MANEUVER=2, SETTINGS=3, PROGRESSION=4; csp-view/src/lib.rs:78 `PLAYER_SOURCE_ID: u32 = 1`. |
| 21 | §4.2 WarpController observe/decide/target_tick; MAX_FRAME_DT=0.1; AdvanceTo logged; rails_warp_refusal soft gate (thrust/contact/atmosphere) | VERIFIED | warp.rs:153/194/272; lib.rs:67 `MAX_FRAME_DT: f64 = 0.1`; csp-sim/src/lib.rs:3707 refusal checks InBubble throttle>0, ground/vessel contact, atmosphere. |
| 22 | §4.3 Input schedule systems `map::apply_map_input`, `vab::apply_vab_input`, `flight::apply_flight_input` | VERIFIED | lib.rs:2067-2071. |
| 23 | §4.3 anchor "`View::build_schedules` region, ~line 2066" | MINOR-INACCURATE | No fn `build_schedules` exists; schedules built inline in the View constructor at lib.rs:2064-2094 (line number itself accurate). |
| 24 | §4.3 staging (`SyncInput`, `map/flight::capture_sync_input`, `stage_vab_tech`, `stage_program`→ProgramMirror) then Sync→Presentation→UiBuild→RenderExtract in that order; UiOutbox consumed next-frame Input | VERIFIED | end_frame lib.rs:2221-2289 exact order; UiOutbox fields (launch/program Vec<SimCommand>/maneuver/craft_save/load/settings_apply/game_save/load/revert/new_game/warp_step/mode) all present :351-395. |
| 25 | §4.3 sync/presentation/extract system names (`sync_mirrors`, `map_present`, `vab_present`, `flight_present`, `tutorial_drive`, `build_ui`, `map_pointer_input`, `extract_map/vab/flight`, FlightSfx) | VERIFIED | lib.rs:2076-2092 + flight.rs:575 FlightSfx. |
| 26 | §4.3 sim_step & sync pin SingleThreadedExecutor | VERIFIED | lib.rs:2073/2075. |
| 27 | §4.4 paint order Terrain→Flight/Vab→Map + egui FullOutput; VAB pick via mpsc readback | VERIFIED | App::paint (lib.rs:1627+); pick channel lib.rs:154-155/431; csp-render/src/vab.rs:462 `map_pick(mpsc::Sender<u32>)`; GPU init mpsc for wasm :336. |
| 28 | §4.5 launch path: recompute, CraftDef + BOM, stock_flight_craft fallback, preflight refusals (`launch_preflight`, `is_part_unlocked`), parked-latch auto-`RecoverVessel`, `pre_launch = sim.snapshot()`, `ManufactureCraft`, `ConstructionComplete`, `StagePlan::for_launch` | VERIFIED | lib.rs:2765-3060 (launch_preflight :3441, pre_launch :2995, ManufactureCraft enqueue :3017, RecoverVessel :2976); manufacture.rs:251; flight.rs:681; tech.rs:71; stock_flight_craft lib.rs:5002. |
| 29 | §4.5 "(The instant free spawn `SimCommand::SpawnPhysicalVessel` still exists but is the MCP/test path — its declaration carries a `// ui-unreachable:` marker)" | INACCURATE | **No `ui-unreachable` marker on SpawnPhysicalVessel** (csp-sim/src/lib.rs:585-601 doc block has none; the marker at :552 binds to *SpawnVessel*, the elements-only spawn). And it is not MCP/test-only: production csp-view code issues it — lib.rs:3030-3043 issues it as the player launch verb's fallback when no authored launch site exists ("Sandbox/test worlds only; the shipped data set always authors `terrin-launch`"), and the wasm demo's LAUNCH button emits it (csp-view/src/demo.rs:216, DEMO_PART_LIMIT=60 :25). `xtask lint-reach` passes precisely *because* it has a production issuer (its opt-out list omits it). Fix: reword to "SpawnPhysicalVessel is issued by the player path only as the no-launch-site fallback and by the wasm demo; the elements-only SpawnVessel is the MCP/test-only variant (marker there)". |
| 30 | §4.5 step-1 anchor "`CraftSession::recompute`" | MINOR-INACCURATE | `recompute` is `csp_ui::VabSession::recompute` (csp-ui/src/vab.rs:849), operating on an inner CraftSession; CraftSession (csp-phys/src/session.rs) has no such method — the analysis fn is `csp_phys::readout::analyze` (readout.rs:143), which the §3 crate table itself anchors correctly. |
| 31 | §5.1 one step_frame ≤ MAX_TICKS_PER_FRAME=69_120 ticks (18 sim-min), `partial: true` | VERIFIED | lib.rs:214 const; :269 comment "69_120 ticks = 1080 sim-seconds (18 min)"; partial field :1355. |
| 32 | §5.2 step_frame loop: TickShadow → drain_boundary (stamp at dequeue, `apply_command`, RejectKind rollbacks) → t_next min → `step_bubble_range` → `advance_interval_subsystems` → `clock.try_advance` → `fire_due_events` | VERIFIED | lib.rs:8620-8720 read line-by-line; all symbols at :1703/:7548/:3987/:7668; epoch-anchored-orbits proof in module doc :24. |
| 33 | §5.2 SUBSYSTEM_ORDER 12-slot const in events.rs; Clamping vs Silent; per-vessel tracked opt-in | VERIFIED | events.rs:97-110 `[Subsystem; 12]`; :119/:123; SetVesselTracked semantics lib.rs:565+. |
| 34 | §5.3 CommandKey ordering `Script(0)<Autopilot(1)<Ui(2)<External(3)`; per-source FIFOs; source_seq serialized; queue not state until stamped | VERIFIED | lib.rs:421-429 discriminants; :2197 `BTreeMap<(SourceClass,u32), VecDeque<PendingCommand>>`; WorldSnapshot.source_seqs. |
| 35 | §5.3 external limits: 32 cmds/client/drain, 64 KiB, bounded IngressQueue | VERIFIED | csp-server/src/ingress.rs:30 `MAX_MESSAGE_BYTES = 64*1024`, :36 `MAX_COMMANDS_PER_CLIENT: u32 = 32`. |
| 36 | §5.4 StateHasher BLAKE3-256 hand walk, length prefixes, u16 discriminants, NaN-faulting try_f64; state_hash/state_hash_masked; HASH_SCHEMA_VERSION=21 | VERIFIED | csp-core/src/hash.rs:207 `= 21`, :315 try_f64; lib.rs:8754/8767. |
| 37 | §5.5 RNG: derive_key KDF_VERSION=1, StreamTag {Reliability=0, Ignition=1, Contracts=2, Generation=3}, ChaCha8, seed_from_u64 lint-banned, fixed arity `{CONTRACTS, 0, …}` | VERIFIED | csp-core/src/rng.rs:17/28-32/75; clippy.toml bans `SeedableRng::seed_from_u64`; contracts.rs:10/176. |
| 38 | §5.6 NEVER-table gates: per-crate clippy.toml disallowed-types (std::time::*, HashMap/HashSet) + disallowed-methods (f64 transcendentals) in the 4 determinism crates only | VERIFIED | Identical clippy.toml in csp-core/csp-data/csp-phys/csp-sim; **no root clippy.toml**; lists match the doc exactly. |
| 39 | §5.6 workspace `unwrap_used`/`expect_used` deny | VERIFIED | root Cargo.toml `[workspace.lints.clippy]`. |
| 40 | §5.6 deny.toml bans rand (and rayon); lint-deps; lint-reach ui-unreachable mechanism; CI wasm check `--exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown` | VERIFIED | deny.toml `[bans] deny=[rand, rayon, bevy]`; xtask/src/lint_reach.rs (ran: OK); .github/workflows/ci.yml:47-48 exact command. |
| 41 | §5.7 artifact triple replay.bin/expected-hash.txt/fingerprint.json (ARTIFACT_FILES); bless-goldens refuses dirty tree / un-bumped schema without --reason; replay driven by logged AdvanceTo; a workspace test pins the registry; perf gate >10% (REGRESSION_TOLERANCE 0.10) vs docs/perf/perf-log-devbox-release.ron | VERIFIED | replay.rs:41-45, test `every_registered_scenario_has_a_verified_golden` :1257; xtask/src/main.rs:57-67 guards; perf.rs:408; baseline file present. |
| 42 | §6.1 audio subprocess pw-play/paplay/aplay over piped stdin; GameHost dedicated thread + ArcSwap; VAB pick stale-epoch drops | VERIFIED | audio.rs:16-17; backend.rs:341/:35 ArcSwap/:826 GameHost; app stale-epoch logic lib.rs:736-769 region. |
| 43 | §6.2 wasm/native table: `wasm_start` (`wasm_bindgen(start)`), console_log vs env_logger, web_time, EmbeddedSource/`View::new_demo` (DEMO_PART_LIMIT=60), saves `saves/*.cspsave` + `crafts/*.cspcraft`, cspGpuReady/cspGpuInitFailed hooks, audio module cfg(not(wasm)) | VERIFIED | lib.rs:3120-3121; csp-data lib.rs:88; view lib.rs:1926; demo.rs:25; app lib.rs:1135/364; apps/demo/index.html:60-61; audio.rs:42. |
| 44 | §6.2 Rhai `=1.27.0`, `default-features=false, features=["no_std"]`; bevy_ecs features ["std"] baseline | VERIFIED | root Cargo.toml; csp-view/Cargo.toml. |
| 45 | §6.3 settings: config_dir/settings_path, collected-error load, backfill_missing_bindings, migrate_char_bindings | VERIFIED | settings_io.rs:35/55/74/187/689. |
| 46 | §6.5 write_bug_report(BugReportInput); triggers on_sim_fatal/panic hook/`--bug-report` | VERIFIED | bugreport.rs:38/118; main.rs:7; lib.rs:624. |
| 47 | §7.1 SimError four classes Fatal(&'static str)/SaveBlocking/Recoverable/Cosmetic; step_frame Err only on Fatal; corrupt save refused whole | VERIFIED | csp-core/src/error.rs:16-31. |
| 48 | §7.2 sim never logs; AdvanceReport carries diagnostics; reliability::Notification → `View::drain_part_notices` toasts | VERIFIED | reliability.rs:372; view lib.rs:3335; AdvanceReport fields all 9 as listed (lib.rs:1317-1360 incl. effective_target, script_log). |
| 49 | §8 routing table (22 rows spot-checked: resolve.rs, save.rs migrate pattern, mcp.rs+PROTOCOL.md, telemetry/src, scenarios.rs+test-fixtures, synth/audio, bake_art→.csar→art.rs, ATTRIBUTION.md, navball, game.rs) | VERIFIED | All referenced files/symbols exist; data/art/{parts.csar,terrain.csar} present; xtask bins bake_art.rs/gen_biome_maps.rs. |
| 50 | §9 D1: InstallAeroTables exists, marked ui-unreachable, never issued by app/view, sim lazily recomputes via `ensure_aero` keyed by graph hash; no "refuses to launch" aero gate | VERIFIED | Marker lib.rs:674-675; zero non-test issuers outside csp-sim (grep); ensure_aero :6846; cache graph_hash :2464-2469; launch_preflight has no aero-table refusal. |
| 51 | §9 D2: no RapierBackend; soft-capture joints in SimpleBackend; rapier3d-f64/parry3d-f64 pinned workspace-only, unused | VERIFIED | No RapierBackend type anywhere (only doc mentions in joints.rs:14, replay fingerprint fields); no crate depends on rapier/parry (root comment "no crate depends on these yet"). |
| 52 | §9 D3: four per-pass packets not one Frame; Gpu::begin_frame returns surface-frame wrapper | VERIFIED | MapPacket/VabPacket/FlightPacket/TerrainPacket + 4 renderers exist; `csp_render::Frame` (lib.rs:421) is the wgpu surface-frame wrapper from begin_frame (:345) — doc's careful wording is exactly right. |
| 53 | §9 D4: per-crate clippy.toml only in 4 determinism crates, no root file | VERIFIED | See #38. |
| 54 | §9 D6: WARP_LADDER exactly [1,2,10,…,100_000]; ×2 physics warp cap | VERIFIED | warp.rs:33. |
| 55 | §9 D7: event budget 2000/frame + 1024/batch implemented; kinds grown beyond plan (SoiRescan, TransferWindow, SalaryDue=9, Insolvent, colony kinds) | VERIFIED | lib.rs:224/252; events.rs:157/244/253. |
| 56 | §9 D8: Program window (csp-ui/program.rs) covers economy/contracts/tech/science/reliability/crew/colonies/labs/automation; game.rs + F5/F9/F10 save/load/revert (`App::do_save`/`do_load`, `View::revert_to_launch`); launch is costed ManufactureCraft; lint-reach gates reachability | VERIFIED | program.rs 2859 lines; game.rs; lib.rs:1102/1153; view lib.rs:3110 revert_to_launch; lint-reach ran OK. |
| 57 | §1 "~1/10 real scale" solar system | UNSUPPORTED | Design-intent claim; weakly corroborated (Terrin radius 6.0e5 m = 600 km ≈ 1/10 Earth) but a full check needs all BodyDef radii vs real bodies — not worth the reader's trust either way. Check `data/systems/home/system.ron` radii if it matters. |
| 58 | §5/§9 PLAN.md section quotes (§2.2, §2.5, §2.6.x, §2.9, §2.10, §3.4, §6.4…) | UNSUPPORTED | PLAN-side assertions (what PLAN specifies) not verified in this pass — every *code-side* half of each divergence was verified. Spot-check PLAN §2.5 (single Frame) and §2.9 (rapier) wording if PLAN fidelity matters. |
| 59 | §3 "docs/perf/ — the committed perf budget log (CI perf gate's baseline)" | VERIFIED | docs/perf/perf-log-devbox-release.ron present; referenced by §5.7 claim in ci.yml/perf.rs. |
| 60 | §9 "no docs/quarterly/ cut-review ritual exists (only docs/perf/)" | VERIFIED | docs/ contains ARCHITECTURE/SIMULATION/GAMEPLAY-GUIDE + perf/ only (plus new untracked docs). |

## Missing topics (completeness — would a new dev get the architecture?)

Priority order:

1. **Terrain model** — how renderer terrain, `csp-core/src/terrain.rs`, physics ground contact (`SimpleBackend` environment), and `gen_biome_maps` relate. Only a routing-table row exists; the terrain/collision contract is invisible.
2. **VAB data model chain** — `VabSession` (csp-ui) wrapping `CraftSession` (csp-phys) → `CraftDef` (csp-sim), who owns the working craft, how crafts/*.cspcraft persistence fits. The doc names the types but never draws this chain (and got it wrong once — #30).
3. **Mirror/staging inventory semantics** — the actual rule for adding a new mirror resource (`FlightSnapshot`, `FlightFrame`, `StagePlans`, `LatchedWeldCache`) and what may read `sim` where; §4.3 implies but doesn't enumerate the contract.
4. **Command-log/replay codec** — bincode layout, fingerprint sidecar fields, how replay restores `source_seq`; deferred to SIMULATION.md? Say so explicitly.
5. **Scripting surface** — ControlApi/IoControlApi semantics, op budgets, poll detection (`rails_warp_refusal`'s script branch is undocumented here).
6. **Agent/MCP path** — LocalGame lockstep loop, GameHost attach, tool inventory; one paragraph + PROTOCOL.md pointer.
7. **View/render error paths** — ViewError, GpuError, VabError, device-loss reconfigure; only SimError taxonomy is given.
8. **Save/craft slot model** — quicksave slot naming, atomic tmp-write, save-blocking semantics at the app level.
9. **Testing map** — inline unit tests, `render_gate.rs` headless render test, csp-server e2e, eval-suite; only goldens are described.
10. **Demo build** — how `apps/demo` data set is generated/reduced and what "no save IO" gates concretely (UiOutbox verb filtering).

## Verdict

PASS-WITH-FIXES. Required fixes (all small):
1. §4.5 parenthetical: correct the SpawnPhysicalVessel claim (#29) — remove the false marker statement, state the no-launch-site fallback + demo usage, and move the "marker" claim to SpawnVessel if that was the intent.
2. Counts: 52→53 SimCommand variants; 21→24 KeyActions (#15, #16).
3. Anchors: `CraftSession::recompute` → `VabSession::recompute` (+ `csp_phys::readout::analyze`) (#30); `View::build_schedules` → "the View constructor's schedule setup (~line 2066)" (#23).
Optional: csp-bench 7.7k→7.9k (#13).
