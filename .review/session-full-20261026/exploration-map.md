# Exploration Map — session-full-20261026 (full-product review, 35 clusters)

Supersedes `.review/session-prd1/exploration-map.md`. Written against the CURRENT working tree
(HEAD `6cc2cb4` + uncommitted W2.6/UX work). All line counts verified today with `wc -l`/`rg`.

## 1. Project overview

**Clanker Space Program** — KSP-style build/fly sim + RTS progression, per `PLAN.md` (R3 spec).
Rust workspace, edition 2024: `bevy_ecs`-standalone + wgpu 30 + egui 0.37; deterministic headless
sim core (64 Hz integer ticks, libm-only, BLAKE3 state hash, ChaCha8 KDF RNG, golden replays).
**~149.8k LOC** of Rust/TS in crates/ (up from ~135k at seed time — Phase 1–4 + W2.6 added ~14k).

**What changed since session-prd1** (commits a0b9356 → 6cc2cb4 + uncommitted):
- `ac7a5ae`/`a02e496` Phase1: science loop in space, parachute part, auto InBubble→OnRails,
  RecoverVessel/TerminateVessel, starter craft has canister+battery, tutorial teaches science.
- `973eb74` Phase2: the "missing half" player surface — progression/colony/crew/contract panels.
- `9e420af` Phase3: real VAB editor — bottom attach, undo, move, occupancy.
- `e37369e` Phase4: save/load/revert (Game menu, F5/F9/F10), `--serve` server hosting, restore
  hardening, **`xtask lint-reach`** (a SimCommand cannot ship without a production issuer in
  csp-ui/csp-view/csp-app or an explicit `// ui-unreachable:` opt-out at the declaration).
- `1096007`/`dd3612a`/`b698eb2`/`6cc2cb4`: launch/parked-build/throttle/warp-accumulator fixes.
- **Uncommitted (W2.6 + UX pass)**: manufacture-costed launches (in tree? — see §2 manufacture row),
  FACILITIES section, HUD toasts for part failures, right-click part menus + flight action bar,
  movable HUD/navball panels, notice chips with amber-on-dark contrast, warp refusal reasons,
  star-gate smoothstep fix (`csp-app/src/{lib,warp}.rs`, `csp-render/src/terrain.wgsl`,
  `csp-ui/src/{game,lib,program,tutorial,vab}.rs`, `csp-view/src/{flight,lib}.rs` — 11 files,
  +1,552/−153).

Crate table (verified counts; code/test split where inline tests exist):

| Crate | Role | LOC |
|---|---|---|
| `csp-core` | SimTime, DVec3/DQuat, StateHasher, RNG KDF, errors | 1.9k (8 files) |
| `csp-data` | RON schemas — now split into `biome/colony/contract/site/system/techtree.rs` modules — loaders, validators, data-set hash | 11.1k src (validate.rs 6.6k) + 1.4k tests |
| `csp-phys` | Weld, EffectivePart, SimpleBackend (semi-implicit Euler + PGS), aero/thermal/structural, CraftSession | ~19.6k |
| `csp-sim` | Authoritative world. `lib.rs` **24,461 lines** (code L1–11880, inline tests L11881–24461 ≈12.6k) + 17 feature modules ~23.5k | ~48k |
| `csp-script` | Rhai host + frozen ControlApi | 1.8k + 0.2k test |
| `csp-ai` | Ascent autopilot only | 222 |
| `csp-view` | bevy_ecs presentation world. `lib.rs` 5,963 (code L1–5064, tests L5065+), `map.rs` 3,610 (code L1–2813), `flight.rs` 3,293 (code L1–2942) | ~18k + 2.9k tests |
| `csp-render` | wgpu Baseline pipelines + WGSL (terrain.wgsl 875) | ~4.1k |
| `csp-ui` | egui panels. **NEW `game.rs`** (Game menu). `vab.rs` 4,938 (code L1–3738), `program.rs` 2,854 (code L1–2677 — was 505!) | 10.9k + 0.4k test |
| `csp-app` | winit shell, input, warp controller, audio, settings IO, bugreport, `--serve` | 7.9k (lib.rs 4,800; code L1–3308) |
| `csp-server` | axum HTTP/WS + Streamable-HTTP MCP + hosted telemetry SPA | 4.8k + 1.3k e2e |
| `csp-bench` | Replay/perf/eval/scenarios + 4 bins | ~6.7k |
| `xtask` | lint-deps, lint-attribution, **lint-reach (NEW)**, bake_art, gen_biome_maps, **`bin/parts/mod.rs` 1,317 — procedural part-mesh authoring (replaced Kenney kit)** | ~4.2k |
| `apps/telemetry` | TanStack/Vite SPA (~13 files; `routeTree.gen.ts` + `dist/` generated — excluded) | ~1.1k TS |
| `apps/demo` | wasm demo: index.html + reduced data tree (16 parts, parity with `data/`) | content |
| `data/` | **16 parts** (was 11: +chute/dockport/lab/legs/probe/tank.life), **6 contracts** (+rescue/colony_resupply/survey.data), 9-node techtree (was 4), authored Rask colony, sites.ron starting_stock, 5 eval-scenarios, test-fixtures | content |

## 2. PRD feature vs observed state — UPDATED (was §2 of seed)

Legend: **[WIRED]** reachable · **[PARTIAL]** reduced vs spec · **[MISSING]** absent.

| PRD feature (PLAN §) | Status | Evidence (file:line, current tree) |
|---|---|---|
| Deterministic core (§2.2–2.7) | **[WIRED]** | `csp-sim/src/lib.rs`; golden replays reblessed (a02e496) |
| Orbit layer + maneuver nodes (§2.8) | **[WIRED]** — now player-reachable end-to-end (auto-rails) | auto bubble→rails `csp-sim/src/lib.rs:4036,4770,4866`; manual `ToRails` remains agent-only (`lib.rs:644` opt-out) |
| Transfer planner + `WatchTransferWindow` (M5) | **[WIRED]** — was HIDDEN | map planning surface: alarm book + staged transfer plan `csp-view/src/map.rs:769–913` (`AlarmEntry`, `ALARM_BOOK_CAP:127`) |
| Weld/aero/thermal/structural (§2.4) | **[WIRED]** | `csp-phys/{weld,aero,thermal,structural}.rs` |
| Joints/docking (§2.4 M6) | **[PARTIAL — improved]** — `csp.dockport.s125.ron` part now EXISTS; sim complete; **UI still future scope** | part in `data/parts/`; opt-out `csp-sim/src/lib.rs:778` "docking port UI is future scope" |
| Science loop (§3.2 M7) | **[WIRED]** — was PARTIAL/tx-only | run-in-space (Phase1), transmit/recover via Program; recovery reachable (parachute + auto-rails re-entry); lab research **[WIRED]**: `StartResearch`/`StopResearch` `csp-ui/src/program.rs:2079–2136`, lab part `csp.lab.mk1.ron` |
| Tech tree (§3.3) | **[WIRED]**, content grew 4→9 nodes incl. facility unlocks (lab, test_stand) | `data/techtree/home.ron:22,34,67` |
| Economy (§3.5) | **[WIRED]** — was PARTIAL | funds charged at manufacture order; `HireCrew` `program.rs:600–610`; recovery credit `program.rs:2385–2419`; salary via hire cost |
| Contracts (§3.6) | **[WIRED]** — 6 templates (was 6 uncompletable ×2; rescue/colony_resupply added) | `data/contracts/*.ron`; board in `program.rs` |
| Crew (§3.5 M6-D) | **[WIRED]** — was HIDDEN | roster + `HireCrew`/`BoardCrew` `program.rs:480,600–610`; colony posting/transfer in colony section |
| Colonies (§3.5.1 M8) | **[WIRED]** — was HIDDEN | colony panel rows `program.rs:247–305`; `FoundColony` `program.rs:654,1396`; authored Rask outpost `data/systems/home/colonies.ron` (Solar/Battery/Drill/Smelter, schema v2) |
| Manufacture (§3.5 M8) | **[WIRED]** — was bypassed | `launch_craft` rides `ManufactureCraft` `csp-view/src/lib.rs:2765–2787,3018`; debit at order from seeded pad stockpile `data/systems/home/sites.ron:46–48` (metal 2000/composite 400); craft delivers `SurfaceLatched` at `ConstructionComplete`; FACILITIES section (build ETAs, qualification, bench buttons) `program.rs:391,1470–1590` |
| Logistics transfers (§3.5) | **[WIRED]** — was HIDDEN | `JoinColonyInventory` `program.rs:215`; resource-transfer rows `program.rs:369`; vessel↔colony UI in colony section |
| Life support (§3.5 M8) | **[WIRED]** (with crew boarding now possible) | `csp-sim/src/life_support.rs`; toggled at startup `csp-app/src/lib.rs` |
| Reliability + bench + revert (§3.4 M9b) | **[WIRED]** — was PARTIAL | `RunBenchTest` buttons `program.rs:742–743,1590`; PartFailed/IgnitionMisfire HUD toasts (W2.6, uncommitted); revert-to-launch F10 `csp-ui/src/game.rs:113–149` with §3.4 disclosure |
| Space center / facilities | **[PARTIAL → gameplay exists]** — FACILITIES section (test stand, qualification, build ETAs) + structures render; no free-camera KSC mode | `program.rs:1470+`; `csp-view/src/structures.rs` |
| Editor/VAB (M3) | **[WIRED — Phase3 fixed the complaints]** — bottom attach, undo, move, occupancy landed (9e420af); uncommitted +59 more | `csp-ui/src/vab.rs` (code L1–3738) |
| Flight controls/HUD/navball | **[WIRED]**; uncommitted pass makes readout block + navball **movable** and re-chips notices | `csp-ui/src/lib.rs` (`.movable(true)`, `NOTICE_AMBER`, `hud_frame`) — uncommitted |
| Map + alarms (M1/M5 §4) | **[WIRED]** — alarms were MISSING | `map.rs:769+`; `SetAlarm` via planning panel |
| Scripting (§2.10 M9a) | **[WIRED]** — was HIDDEN | AUTOMATION section: `LoadScript`/`UnloadScript`/`SetScriptEnabled` `program.rs:715–730`, mapping `csp-view/src/lib.rs:1236–1249`; script-log notices `csp-app/src/lib.rs:1347–1357` |
| Autopilot (§2.11) | **[PARTIAL → ascent WIRED]** — `EngageAutopilot` now a ProgramCommand (`program.rs:733`, `csp-view/src/lib.rs:1250–1253`); transfer optimizer / design assistant / LlmAdapter still absent | `csp-ai/src/lib.rs` (still 222 lines) |
| Save/load game (§7) | **[WIRED]** — was HIDDEN | Game menu `csp-ui/src/game.rs` (slots/quicksave F5/load F9/new-game); revert F10; unified CSPSAVE codec for MCP too (e37369e) |
| Replays/bench/eval (§7.6, §8.5) | **[WIRED]** | `csp-bench`; known pre-existing failure: flight-launch golden bincode decode vs W2 churn (per STATUS.md) |
| Rendering tiers (§5) | **[PARTIAL]** Baseline only (cut-list OK); sky/star fix uncommitted | `csp-render`; `terrain.wgsl:1082–1103` star gate smoothstep — uncommitted |
| Terrain/atmosphere/ocean (R2) | **[WIRED]** | `csp-render/terrain*`, `csp-view/terrain.rs` |
| Presentation floor + audio (§3.7/M10) | **[WIRED]** | `csp-view/flight.rs`, `csp-app/{audio,synth}.rs` |
| Settings/keys, tutorial | **[WIRED]**; tutorial teaches science since Phase1 | `csp-ui/{settings,tutorial}.rs` |
| "Planes/rovers/stations" pillar | **[PARTIAL content]** — legs, probe core, dock port, lab added (station-ish); still no wings/wheels | `data/parts/` (16) |
| Third body / interplanetary (M5) | **[PARTIAL]** — Duva/Rask + transfer planner now visible in map; no authored interplanetary content beyond bodies | `data/systems/home/` |
| Web demo (§6.3) | **[WIRED]** | `csp-view/demo.rs`, `apps/demo/` |
| Command reachability guarantee | **[WIRED — NEW]** | `xtask/src/lint_reach.rs`; only **9** `ui-unreachable:` opt-outs remain in `csp-sim/src/lib.rs` (L552,564,631,644,674,692,778,939,997) — was ~25/40 unreachable |
| Migrations, N-body, CommNet, EVA, VT, LLM | **[MISSING]** — correctly deferred post-v1 | — |

**Bottom line:** the Phase1–4 + W2.6 arc closed nearly every "sim-complete, UI-missing" gap the
prd1 review catalogued. The review question shifts from *reachability* to *quality of the now-
wired surface*: correctness of the new glue (save/restore parity, manufacture debits, auto-rails
edge cases), the uncommitted UX pass, and the still-open content/architecture gaps (docking UI,
advanced renderer, planes/rovers, LLM design assistant).

## 3. User-complaint → cluster mapping (6 live UX issues, all with UNCOMMITTED work)

1. **Science/recovery undiscoverable** → **C22** (ui-program: right-click part menu `program.rs:2223–2535`, flight Actions strip, pooled-science rows — uncommitted) + **C19** (flight-presentation: `pick_part` right-click raycast `csp-view/src/flight.rs:1600–1622` — uncommitted) + **C23** (tutorial/hints).
2. **Panels fixed-position, can't drag** → **C23** (ui-hud: `.movable(true)` readout block + navball Areas in `csp-ui/src/lib.rs` — uncommitted).
3. **Grainy sky / stars too early** → **C16** (render: star gate `smoothstep(6e-4, 4e-3, ins_lum)` replacing `exp(-ins_lum·160)` in `terrain.wgsl:~1095` — uncommitted; also check dithering/noise upstream of `fs_sky`).
4. **Warp refuses silently** → **C25** (app-periph: `warp.rs` `request_preview`/`multiplier_at`/`capped_below_ladder_top`) + **C24** (app-shell: `warp_step_request` notice path + `rails_warp_refusal()` surfacing — uncommitted) + warp-accumulator anchoring (6cc2cb4) + stale-axis/throttle fixes (dd3612a, b698eb2).
5. **Yellow notices unreadable / flight-data contrast** → **C23** (`NOTICE_AMBER`, `hud_frame`, `paint_hud_chip` chip backgrounds — uncommitted in `csp-ui/src/lib.rs`).
6. **Research/science UI confusing** → **C22** (program research section rework: escrow/accrual explanation `program.rs:2079–2136` — uncommitted) + **C08** (sim research semantics reviewers should sanity-check against the panel copy).

## 4. Clusters (35 = number of review agents)

Non-test line counts; inline `#[cfg(test)]` ranges excluded from the 6k budget (test lines listed
for load awareness). Every reviewable file appears exactly once.

### C01 — Determinism primitives (csp-core) — 8 files, ~1.9k
`crates/csp-core/src/{lib 26, error 77, hash 788, quat 232, rng 329, terrain 198, time 93, vec3 197}.rs`
+ `clippy.toml`. StateHasher visitor, KDF. **Focus:** logic/state-management — hash visitor must
cover every field Phase1–4 ADDED (saves, manufacture orders, notification ring, alarm book mirror,
crew/colony mirrors are view-side but sim-side arenas grew).

### C02 — sim lib §A: command contract + World shell — 1 file, L1–3634 (~3.6k)
`crates/csp-sim/src/lib.rs` L1–3634. `SimCommand` enum L534–~1215 (681 lines — the whole verb
surface, now with 9 `ui-unreachable:` annotations), `WorldSnapshot`, `World` + config installers.
**Focus:** PRD-coverage — read each opt-out annotation for honesty; snapshot field coverage vs the
new save/restore path.

### C03 — sim lib §B: tick machine + apply handlers — 1 file, L3635–8619 (~5.0k)
`crates/csp-sim/src/lib.rs` L3635–8619: `enqueue`:3635, `step_bubble_range`:3987 (incl. auto
bubble→rails detection :4770/:4866), `apply_command`:5223 + all `apply_*` (science :5368,
parachute deploy :7232/:7434, colony/manufacture/crew applies added in Phase2/W2.6).
**Focus:** logic + state — auto-rails edge cases (clearing conic, boundary oscillation), rollback
discipline on rejected applies, manufacture debit atomicity.

### C04 — sim lib §C: frame/hash/restore/breakup — 1 file, L8620–11880 (~3.3k)
`crates/csp-sim/src/lib.rs` L8620–11880: `step_frame`:8620, `state_hash`:8754/:8767, `restore_*`
per-arena (crafted-save hardening from e37369e lives here), `apply_breakup`, `hash_vessel`,
parachute decel refresh :8902. **Focus:** security (crafted saves) + logic (restore reinstalls
dropped config channels — regression risk).

### C05 — sim lib §D: inline test suite — 1 file, L11881–24461 (~12.6k test lines)
`crates/csp-sim/src/lib.rs` tests. Huge but tests-only. **Focus:** PRD-coverage — do tests cover
the NEW player-shaped flows (auto-rails roundtrip, manufacture-costed launch, save/revert
fidelity, colony found/supply via UI verbs) or only API surfaces?

### C06 — Orbit + maneuver + bubble — 3 files, ~3.3k
`crates/csp-sim/src/{orbit 2536, maneuver 362, bubble 422}.rs`. **Focus:** logic — universal
solver numeric parity now that players actually reach orbit; SOI transitions under warp.

### C07 — Transfer planner + transfer processes — 2 files, ~4.3k
`crates/csp-sim/src/{transfer 2556, transfers 1708}.rs` (Lambert/porkchop + `WatchTransferWindow`;
resource/crew transfer processes — NOW player-reachable via map panel + colony UI).
**Focus:** playability — is the planner output usable in the map panel; transfer-process determinism.

### C08 — Progression: science/tech/economy/contracts — 4 files, ~5.8k
`crates/csp-sim/src/{research 1340, tech 878, economy 1434, contracts 2160}.rs`. Researchers,
escrow/accrual, funds ledger (hire cost, salary), seeded board. **Focus:** logic + complaint#6 —
does the sim semantics match what the new panel copy promises; contract predicates vs the 6
templates' completability (2 were uncompletable at seed).

### C09 — Colony flow network — 1 file, ~5.2k
`crates/csp-sim/src/colonies.rs` (5,172). Allocation pass, day/night power, drills/converters,
states, inspections. Now VISIBLE + foundable via UI. **Focus:** logic + PRD-coverage — first real
exercise of this machinery by players; event budget, dormant bookkeeping, Rask outpost balance.

### C10 — Reliability + joints — 2 files, ~3.1k
`crates/csp-sim/src/{reliability 1889, joints 1256}.rs`. Hazard Λ/θ, qualification (bench test now
reachable), soft-capture lifecycle. **Focus:** logic — bench-test economics vs FACILITIES buttons;
docking still UI-less (opt-out :778).

### C11 — Manufacture + life support + save codec + events + script host — 5 files, ~3.9k
`crates/csp-sim/src/{manufacture 682, life_support 758, save 836, events 719, scripts 874}.rs`.
**Focus:** logic + state — manufacture now on the launch hot path (build order → stock debit →
`ConstructionComplete` → berth retry); CSPSAVE codec is now the player save format (atomicity,
refusal rules); NotificationKind widened for HUD toasts.

### C12 — Physics: integration + contact — 4 files, ~4.9k
`crates/csp-sim/src/../csp-phys/src/{simple 4254, backend 199, tensor 324, lib 147}.rs`.
SimpleBackend, PGS contact, settle latch (the b698eb2 rail-early-return fix territory).
**Focus:** logic — relatch/delivery-berth upright mechanics (W2.6), 20t-lander criterion.

### C13 — Physics: part model + readouts — 5 files, ~5.6k
`crates/csp-phys/src/{resolve 3691, part 466, graph 355, readout 706, profile 402}.rs`.
EffectivePart resolution, Δv/TWR readouts, MissionProfile. **Focus:** logic — wear + parachute
spec resolution feeding the new decelerator UI.

### C14 — Physics: environment (aero/thermal/structural/weld/hull) — 5 files, ~5.4k
`crates/csp-phys/src/{aero 2037, thermal 832, structural 552, weld 1622, hull 370}.rs`.
**Focus:** logic — re-entry heating vs parachute survivability (the new recovery path leans on it);
aero table determinism.

### C15 — Physics: CraftSession + merge + tests — 4 files, ~5.1k
`crates/csp-phys/src/{session 3584, merge 866}.rs` + `tests/{resolve_parts 384, session 244}.rs`.
VAB attach-graph model (Phase3 bottom-attach/undo/move/occupancy) + docking merge.
**Focus:** logic — the editor fixes landed here (9e420af): occupancy grid, undo stack, move
semantics; merge is exercised only via sim joints (UI still future).

### C16 — Renderer + render-gate tests — 9 files, ~3.5k code + 1.5k tests
`crates/csp-render/src/{lib 494, terrain.rs 1553, terrain.wgsl 875 (star-gate fix UNCOMMITTED), map.rs 159, map.wgsl 48, flight.rs 233, flight.wgsl 104}.rs` + `crates/csp-view/tests/{render_gate 1062, render_dump_extra 449}.rs`.
**Focus:** complaint#3 — grainy sky: verify smoothstep gate bounds + look for noise/dither source;
complaint root may also be LUT sampling. Baseline-tier honesty vs §5.1.

### C17 — Editor (VAB) — 5 files, ~5.8k code
`crates/csp-ui/src/vab.rs` (code L1–3738; tests L3739–4938) + `crates/csp-view/src/vab.rs` 1414 +
`crates/csp-render/src/{vab.rs 550, vab.wgsl 107}` + `crates/csp-view/tests/vab_latency.rs` 165.
**Focus:** playability — verify the Phase3 fixes (move rocket, remove-part gluing, bottom attach,
undo) actually hold; uncommitted +59 lines in vab.rs; 16 ms latency criterion.

### C18 — Map view + planning surface — 1 file, code L1–2813 (+ tests to 3610)
`crates/csp-view/src/map.rs`. Camera/focus/orbit lines + W2.4 alarm book (`AlarmEntry`, grace
ticks, cap) + staged transfer-window plan. **Focus:** logic + playability — alarm fire/graft
dedup vs `PlayerAlarm`/`TransferWindow` events; planner staging vs `C07` semantics.

### C19 — Flight presentation + part picking — 1 file + 3 tests, code ~2.9k
`crates/csp-view/src/flight.rs` (code L1–2942; tests L2943–3293; right-click `pick_part`:1600–1622
UNCOMMITTED) + `tests/{flight_focus 91, flight_plume 167, part_silhouettes 149}.rs`.
Kitbash extract, plume, plasma, dust, chase cam + the new part-action pick raycast.
**Focus:** complaint#1 — pick correctness (AABB projection, collider freshness after staging).

### C20 — Scene: structures, meshes, art, terrain cache, demo — 6 files + 2 tests, ~4.2k
`crates/csp-view/src/{structures 867, part_mesh 947, terrain 787, art 518, demo 322, tutorial 353}.rs` + `tests/{demo 272, tutorial 184}.rs`. Note `part_mesh` pairs with `xtask/bin/parts` (C35).
**Focus:** playability — terrain-vs-contact datum (buried pad class); structures now imply REAL
facilities (test_stand) — visual/gameplay parity.

### C21 — View shell: schedules, mirrors, program-command glue, launch path — 1 file + 3 tests, code ~5.1k
`crates/csp-view/src/lib.rs` code L1–5064 (tests L5065–5963) + `tests/{pad_spawn 181, stage_plan 202, startup_colonies 34}.rs`. `ProgramMirror` staging, `program_command` mapping
(:1042–1500 — every ProgramCommand→SimCommand edge), `launch_craft`:2765→`ManufactureCraft`:3018,
GameMenu wiring, automation install (:2990–2991). Uncommitted +139.
**Focus:** HIGHEST glue — every new Phase2/W2.6 verb crosses here; dropped-command notices;
revert-point capture; `--serve` host bridging.

### C22 — Program window + Game menu — 2 files, code ~2.9k
`crates/csp-ui/src/program.rs` (code L1–2677; tests L2678–2854; +719 UNCOMMITTED) + `csp-ui/src/game.rs` 198 (+15 uncommitted). The progression surface: science, tech, contracts, crew, colonies, facilities, automation, recovery, research; right-click part menu + Actions strip (uncommitted); Game menu (save slots F5/F9, revert F10 + disclosure).
**Focus:** playability + complaints #1/#6 — discoverability of the new affordances; research copy
honesty; save/revert UX; DRY-complexity (this file quintupled since seed).

### C23 — HUD, navball, settings, tutorial — 5 files, ~4.3k
`crates/csp-ui/src/{lib 1721 (movable panels + notice chips UNCOMMITTED), navball 542, settings 1046, tutorial 619 (+17 uncommitted)}.rs` + `crates/csp-ui/tests/hud_dump.rs` 393 (NEW, untracked).
**Focus:** complaints #2/#5 — movable Areas' first-open parking, chip contrast/painting, notice
wrap width; KeyAction vocabulary now includes save/revert keys.

### C24 — App shell: winit loop, input, UI outbox — 1 file, code ~3.3k
`crates/csp-app/src/lib.rs` code L1–3308 (tests L3309–4800; +180 uncommitted). Input dispatch,
held-axis integration, `--serve` host mode, script-log/notice routing, `warp_step_request`
feedback (uncommitted), failure toasts. **Focus:** complaints #4/#1 — warp refusal reasons,
read-only session notice, one-frame-latency contract for every dispatched command.

### C25 — App periphery: warp controller, audio, settings IO — 7 files, ~3.1k
`crates/csp-app/src/{warp 792 (code L1–406; preview/multiplier API UNCOMMITTED), audio 839, settings_io 766, synth 383, bugreport 244, logring 92, main 27}.rs`.
**Focus:** logic — warp accumulator anchoring (6cc2cb4) + demotion ladder + the new
press-time preview math; bugreport must redact save paths.

### C26 — Scripting + AI — 3 files, ~2.2k
`crates/csp-script/src/lib.rs` 1773 + `tests/api_freeze.rs` 182 + `csp-ai/src/lib.rs` 222.
ControlApi frozen surface; ascent autopilot (now player-engageable). **Focus:** PRD-coverage —
api_freeze vs the new UI verbs; §2.11 shortfall (no transfer optimizer/design assistant/LlmAdapter)
is a known scope gap, note only.

### C27 — csp-server: HTTP/WS/MCP — 10 files, ~4.8k src
`crates/csp-server/src/{backend 1256, mcp 1124, http 1059, dashboard 472, serve 289, ingress 273, telemetry 255, lib 45}.rs` + `tests/e2e.rs` 1293 + `PROTOCOL.md`. Now also the HOSTED telemetry
surface (`--serve`, e37369e). **Focus:** security — token auth, Origin allowlist, bounded ingress,
`sim.command` generic tool reach (every command incl. save/load) vs §2.12 contract; MCP save
format unification regression.

### C28 — Telemetry SPA — ~13 files, ~1.1k TS
`apps/telemetry/src/{router.tsx, routes/{index,__root}.tsx, lib/{dashboard,utils,ws}.ts, lib/widgets/{gauge,graph,orbit-map,registry,table}.tsx, components/ui/card.tsx, vite-env.d.ts}` + `vite.config.ts`, `tsconfig.json`, `package.json`, `dashboards/default.json`. (`routeTree.gen.ts`, `dist/` generated — excluded.) **Focus:** logic — WS subscription paths vs server wire protocol; does it now mirror program data (funds/contracts/crew/colonies) the Game UI also shows.

### C29 — Bench: replay/perf/agent/eval — 9 files, ~4.6k
`crates/csp-bench/src/{replay 1826, perf 1270, agent 607, eval 237, lib 37, build.rs 212}.rs` + `bin/{csp-replay 127, csp-perf 190, csp-eval 133}.rs`. **Focus:** logic — golden-replay schema churn
(known failing flight-launch bincode decode, pre-existing per STATUS.md); perf budgets vs the +14k LOC.

### C30 — Bench: scenarios + economy/contracts harness + mcp bin — 6 files, ~3.3k
`crates/csp-bench/src/{scenarios 1061, scenario 729, economy 479, contracts 308}.rs` + `bin/csp-mcp.rs` 149 + `tests/eval_suite.rs` 544. **Focus:** PRD-coverage — do scenarios exercise the new
player loop (manufacture launch, colony resupply, save/revert) or only mechanics; 5 eval-scenarios
in `data/eval-scenarios/` (owned by C34).

### C31 — Data schemas + loaders — 15 files, ~4.4k
`crates/csp-data/src/{part 903, load 1080, ids 144, datahash 229, resource 79, experiment 74, recipe 58, balance 251, loc 28, biome 393, colony 212, contract 372, site 112, system 219, techtree 252}.rs`.
The six new domain schema modules (biome/colony/contract/site/system/techtree) split out since seed.
**Focus:** logic — schema_version handling (colonies.ron is v2), unknown-field strictness, datahash
covers new files.

### C32 — Data validator + tests — 10 files, ~6.6k (single unsplittable file; 10% over soft cap — flag to agent)
`crates/csp-data/src/{validate.rs 6580, lib.rs 121}.rs` + `tests/{parts 303, home_system 198, unknown_fields 195, colonies 104, contracts 149, balance 123, experiments 72, techtree 47}.rs`.
**Focus:** PRD-coverage — validator rules vs new content (chute/dockport/lab/legs/probe, colonies
v2, sites starting_stock, 9-node tree, 6 contracts).

### C33 — Content: parts catalog + tech + recipes + rules — ~24 RON files
`data/parts/*.ron` (16), `data/techtree/home.ron` (9 nodes, facility unlocks), `data/experiments/exp.materials_study.ron`, `data/recipes/*.ron` (3), `data/resources.ron`, `data/balance.ron`, `data/loc/en.ron`, `data/ATTRIBUTION.md`. **Focus:** playability/balance — starting_stock + part costs vs manufacture debits; tech gating (9 nodes now unlock real parts/facilities); demo-tree parity (`apps/demo/data/parts` also 16 — diff them).

### C34 — Content: world, contracts, fixtures, demo — ~40 files
`data/systems/home/{system.ron, solen|terrin|rask|duva/{body,biomes}.ron, sites.ron (starting_stock:46–48), colonies.ron}` + `data/contracts/*.ron` (6) + `data/eval-scenarios/*.ron` (5) + `data/test-fixtures/{flight-launch, trivial}/` (goldens) + `apps/demo/{index.html, data/**}`.
**Focus:** PRD-coverage — Rask outpost EC math (tidally-locked claim), contract completability
(rescue/colony_resupply templates are new), fixture drift vs W2 churn.

### C35 — Tooling, parts-mesh generator, CI — ~10 files, ~4.2k
`xtask/src/{main 406, lint_deps 942, lint_attr 320, lint_reach 236 (NEW), bin/{bake_art 655, gen_biome_maps 312, parts/mod.rs 1317 (procedural part meshes — the art source)}}.rs` + workspace `Cargo.toml`, `deny.toml`, `rust-toolchain.toml`, `.github/workflows/ci.yml`, `docs/perf/perf-log-devbox-release.ron`.
**Focus:** logic/process — does CI run `lint-reach` (the reachability guarantee)? lint-deps
allowlist vs current crate set; parts/mod.rs mesh math feeds part_mesh classifier (C20).

## 5. Dispatch guidance

**Total: 35 clusters = 35 review agents.**

Priority order (uncommitted-work and complaint-heavy first):
1. **C22, C23, C24, C25, C16** — the six live UX complaints all land here; the fixes are UNCOMMITTED, so review the working tree, not HEAD.
2. **C21** — the glue every new verb crosses (program-command mapping, launch/manufacture path, game menu).
3. **C03, C04, C11** — auto-rails, crafted-save hardening, manufacture/save on the hot path.
4. **C17** — verify Phase3 editor fixes hold.
5. **C09, C08, C18, C19** — the newly-visible RTS/orbit/planning surfaces.
6. **C27** — `--serve` exposure + `sim.command` save/load reach (security).
7. Then C05 (test coverage of the new loop), C29/C30, C32–C34 (content sufficiency), remaining mechanics (C06/07/10/12–15/20), foundations (C01/02/26/28/31/35).

Notes for the parent: `csp-ui/tests/hud_dump.rs` is untracked (new); `.review/session-prd1/STATUS.md` has uncommitted edits (ignored here); `apps/telemetry/dist/`, `apps/demo/dist/`, `routeTree.gen.ts`, `target/`, `Cargo.lock` excluded as generated.
