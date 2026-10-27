# Review Report C24 — App shell (winit loop, input, UI outbox)
Session full-20261026 · Target: working tree at HEAD 7dd4b86 (`csp-app/src/lib.rs` has no
uncommitted diff; the W2.6 work landed in ff897d4/7dd4b86). All 51 inline tests pass
(`cargo test -p csp-app --lib`). Every code line L1–3308 read; warp.rs / csp-view glue
cross-read for context.

---

### [SEVERITY: HIGH] Finding 1: New Game boots an empty world — no starter craft, no pad vessel
**File**: `crates/csp-app/src/lib.rs:1084-1094` (the `SessionOp::NewGame` branch), root cause split with `crates/csp-view/src/lib.rs:3132-3146`.
**Problem**: The starter-craft seed lives only in the app *constructor* (`lib.rs:406-414`: "M10 first run: seed the stock pad craft … a Flight view with nothing InBubble draws nothing but the clear color (the shipped 'purple void')"). The NewGame branch calls `self.view.new_game()`, which on native replaces the whole View via `View::new()` (`csp-view/src/lib.rs:3132-3146`) — and `View::new()` does **not** seed (seeding is `new_demo()`-internal at `csp-view/src/lib.rs:1937` and the app-side `seed_starter_craft()` call at `lib.rs:412`, neither of which runs here). Nothing re-seeds afterwards.
**Evidence**: Verified empirically with a temporary integration test against the public API (`View::new()` → `seed_starter_craft()` → step → `flight_focus().is_some()` ✓ → `new_game()` → step → `flight_focus()` is **None** — panic "New Game must boot a pad vessel"); test file deleted after the run. The shipped test `new_game_boots_with_a_pad_vessel` (`lib.rs:3493`) only exercises the constructor path, not the session op.
**Impact**: Game menu → New Game lands the player in the exact "purple void" first-run state the seed exists to prevent: no vessel, no flight focus (every flight verb a "NO VESSEL" dead key), and an empty VAB session (the seed also pre-loads the craft for editing). The verb is coded and reachable but functionally degraded — a primary menu action.
**Suggestion**: Seed inside the NewGame branch after `self.view.new_game()` (guard `!self.view.is_demo()` exactly like `new_inner`), or better, move the native seed into `View::new_game()` itself so the "fresh world" constructor always boots playable. Add the session-op path to a test.

### [SEVERITY: HIGH] Finding 2: Physics warp ×2 is unreachable under any rails-warp refusal — the whole ascent runs at ×1
**File**: `crates/csp-app/src/lib.rs:1430-1434` (press-side gate) and `lib.rs:1307-1316` (per-frame `demote(0)` heartbeat).
**Problem**: `rails_warp_refusal()` (`csp-sim/src/lib.rs:3707-3755`) reports the **rails**-warp conditions: a bubble-resident vessel under thrust, in unsettled contact, or in atmosphere. The app applies it to the *entire* ladder above ×1: `warp_step_request` refuses **every** step-up while it holds (including ×1→×2, a physics-warp step), and `on_step_report` heartbeat-demotes `effective` to rung 0 every frame it holds.
**Evidence**: PLAN §2.6.2's regime table: "Physics warp | ×2 | 128 ticks/s — *more CPU work, same dt* | **bubble occupied**; demoted by the controller" — the allowed-when for ×2 is bubble-occupied, i.e. precisely the thrust/contact/atmosphere situations the refusal describes; only the rails row carries "no bubble-resident vessel under thrust, unsettled contact, or in atmosphere". The existing test `warp_press_always_says_why` (`lib.rs:4127-4198`) only presses WarpUp *after* requesting rungs ≥2 under refusal — the ×1→×2 press under refusal is never exercised.
**Impact**: The archetypal physics-warp use — a long atmospheric ascent burn — can never use ×2; the player asks and gets "WARP REFUSED — BUBBLE-RESIDENT VESSEL N IN ATMOSPHERE" (honest, but the capability the PRD sanctions is missing). ×2 remains reachable only while coasting in vacuum inside the bubble, where it is nearly pointless. Also bites in reverse: a ×2 legitimately engaged in vacuum coast is force-demoted to ×1 the moment the vessel dips into atmosphere.
**Suggestion**: Treat the rails refusal as capping the ladder at rung 1, not rung 0: in `warp_step_request`, refuse only steps whose preview target is rung ≥2; in `on_step_report`, `demote(1)` instead of `demote(0)` when the reason is a rails refusal (keep `demote(0)` for fired `Clamping` events per §2.6.5). The §2.6.3 budget derate still applies underneath, unchanged.

### [SEVERITY: HIGH] Finding 3: Wall-side throttle axis survives Load/NewGame world rebuilds — first tap can ignite at the stale value
**File**: `crates/csp-app/src/lib.rs:2358-2385` (`throttle_axis_step`'s delta-0 unlatch path), `lib.rs:2209-2244` (`throttle_press` rebase rules), and `lib.rs:1060-1096` (`run_session_op` — no axis reset).
**Problem**: `throttle_axis` / `throttle_axis_vessel` / `throttle_hold_vessel` are never reset when a session op rebuilds the world. The rebase rules only fire when `throttle_axis_vessel != flight_focus()` — but vessel ids are deterministic from 0 in every world (`csp-sim/src/lib.rs:2504` `next_vessel_id: 0`), so a loaded save whose focused vessel carries the same id as the pre-load axis vessel defeats the rebase. The P02-F2 rule ("latched focus keeps the axis — a queued SetThrottle may still be in flight", codified by test `latched_focus_keeps_the_throttle_axis` at `lib.rs:4204`) then preserves the stale value, and in the rebuilt world there is provably no queued `SetThrottle` for it.
**Evidence**: Trigger chain on the real flow: fly the delivered craft with Z (axis = 1.0) → land/relatch (b698eb2's own scenario; `body()` gone) → F5 (save-while-latched) → later F9 → restored vessel latched, same id → `throttle_press`: `axis_vessel == focus` → no rebase; `applied = None` (latched) → axis stays 1.0 → Shift tap: delta 0, but the undelivered-unlatch branch (`SurfaceLatched && !throttle_queued && next > 0`) enqueues `SetThrottle(1.0)` → **pad ignition at full throttle from a 1% tap**. Same class via New Game once Finding 1 is fixed (both worlds' starter crafts are id 0). No assignments to the axis fields exist anywhere in `run_session_op`/`do_load`/`do_revert` paths.
**Impact**: Unintended full-throttle unlatch (or a first tap landing at an arbitrary pre-load percentage) immediately after Load — the third bug in this exact class (dd3612a, b698eb2 were the first two); the vessel-id rebase heuristic cannot see world rebuilds.
**Suggestion**: Reset the wall-side axis state wherever `world_rebuilt` is set in `run_session_op` (`throttle_axis = 0.0; throttle_axis_vessel = None; throttle_hold_vessel = None; attitude_deferred = false`) — or give the axis an explicit `on_world_rebuild()` hook (see Finding 7). Optionally also clear `last_refusal` so the post-step refusal latch re-announces in the new world.

### [SEVERITY: MEDIUM] Finding 4: "WARP LIMITED — EVENT/TICK BACKLOG" posts every frame while `report.partial` holds — unlatched cue spam
**File**: `crates/csp-app/src/lib.rs:1323-1333`.
**Problem**: The `partial` branch demotes (once — subsequent frames emit nothing) but pushes the identical `flight_notice` **every frame** the backlog persists. Unlike the rails-refusal path, which latches on `last_refusal` (`lib.rs:1300-1306`), there is no dedup. The cue queue (`FlightSfx.cues`, `csp-view/src/flight.rs:575-590`) is unbounded with TTL-only eviction (90 frames), and `notice_overlay` (`csp-ui/src/lib.rs:702-729`) renders **every** live cue as a stacked chip.
**Evidence**: Under a sustained pathological event backlog (§2.6.5's designed 20-colony density scenario), `partial` holds frame after frame; at 60 fps the steady state is ~90 identical chips stacked top-center, plus one new one per frame — the screen-filling flood the `last_refusal` latch was built to prevent on the refusal side.
**Impact**: UI flooding exactly when the sim is already struggling; also floods the audio cue channel.
**Suggestion**: Latch exactly like `last_refusal` (post only on the false→true edge, or at most re-post on a several-second cooldown).

### [SEVERITY: MEDIUM] Finding 5: `--serve` does not share the game's saves dir despite the stated contract
**File**: `crates/csp-app/src/lib.rs:2974` vs `lib.rs:490-496` + `lib.rs:1213-1245`.
**Problem**: `serve_cli`'s own doc ("The session shares the game's own `saves/` dir so a `session.save` is a real §7.2 CSPSAVE the menu can load") is false in the tree: the host session writes to `settings_io::config_dir().join("saves")` (`$XDG_CONFIG_HOME/csp/saves`, absolute), while the app's `save_dir` is `<cwd>/saves` (`craft_dir = "crafts"` → `parent()` = `""` → relative `saves/`), and `stage_save_list` scans only that directory.
**Evidence**: `config_dir()` (`settings_io.rs:35-51`) always joins the `csp` app dir; the two paths coincide only if the game is launched with CWD == the config dir, which nothing does.
**Impact**: A save written through the hosted session (`session.save` / MCP) is invisible to the in-game Game menu, and vice versa — host-mode/app state divergence that silently breaks the documented interop ("the menu can load").
**Suggestion**: One source of truth: have `serve_cli` use the same resolution the app uses (export a `saves_dir()` next to `craft_dir`), or move both to `config_dir()/saves` deliberately and update the app's `save_dir` accordingly.

### [SEVERITY: LOW] Finding 6: `stage_save_list` slurps entire save bodies to read a header
**File**: `crates/csp-app/src/lib.rs:1232-1234`.
**Problem**: The comment says "O(header) per save — `inspect_save` never inflates a body", but the code does `std::fs::read(&path)` (full file) before calling `inspect_save`, which only needs `CSPSAVE\0 || len:u32 || JSON` (`csp-sim/src/save.rs:449-470`).
**Impact**: At startup and after every save, every save file is fully read into memory. Bounded frequency (not per-frame), so cost is modest, but the comment is factually wrong and a 1000-part station save is MBs.
**Suggestion**: Read a bounded prefix (`File::open` + `.take(64 KiB)` — the framing makes the header length available in the first 4 bytes; anything longer is a corrupt file and can be refused as such).

### [SEVERITY: LOW] Finding 7 (DRY/CRAP): read-only refusal boilerplate ×10, and the throttle-axis state machine has no single owner
**File**: `crates/csp-app/src/lib.rs` — refusal strings at 559-567, 1051-1054, 1399-1405, 1543-1554, 1565-1569, 1602-1605, 2058-2059, 2141-2142, 2390-2392, 2703-2705; axis fields at 246-280 + methods 2209-2385.
**Problem**: (a) The §2.13 read-only refusal is hand-written at ten call sites with ad-hoc wording ("WARP NOT APPLIED" / "LAUNCH REFUSED" / "ACTION NOT APPLIED" …) — a new gated verb will forget one. (b) The throttle axis is five struct fields (`throttle_held`, `throttle_axis`, `throttle_axis_vessel`, `throttle_hold_s`, `throttle_hold_vessel`) with invariants spread across three interlocking methods and their doc comments; this shape has now produced three shipped bugs (dd3612a, b698eb2, Finding 3) because each new edge case patches one site.
**Impact**: Maintainability on the hottest input path; Finding 3 is the direct product of (b).
**Suggestion**: (a) `fn read_only_refusal(&mut self, verb: &str)` posting `format!("SESSION IS READ-ONLY — {verb}")`. (b) Extract a `ThrottleAxis` struct owning the five fields with `press/release/tick/step/world_rebuilt` methods — the explicit `world_rebuilt` hook is both the Finding 3 fix and the place the next edge case will be forced to live. The `App` struct (~45 fields) would also benefit from an `InputState` group for the key/pointer/held state, but that is optional.

---

## PRD coverage table

| Requirement (PLAN §) | Status | Evidence |
|---|---|---|
| §2.2.1 command ordering — per-producer Ui source FIFOs, commands absolute not relative | **WIRED** | `WARP_SOURCE_ID/MANEUVER/SETTINGS/PROGRESSION` consts L69-83; shared player FIFO with launch (L84-88); throttle ramp integrates wall dt into one absolute `SetThrottle` per frame (L2280-2335) |
| §2.2.1 held-axis integration in csp-app, one-frame latency, exactly-once dispatch | **WIRED** | keys dispatch between frames, drain at next `sim_step`; panel outbox drains at frame head before the same frame's step (L864-1012); warp press → `pending_step` → next `decide()` fold, once |
| §2.2.1 lease/reject framing — rejects delivered on originating channel, not silent | **WIRED** | `on_step_report` surfaces Ui-class rejects as HUD cues, Script/Agent log-only (L1269-1281) |
| §2.6.2/§2.6.3 warp ladder UX — refusal reasons to the player (complaint #4) | **WIRED** (feedback) / **DEGRADED** (×2) | Per-press "WARP REFUSED — <offender>" (L1430-1434), ceiling naming SETTINGS LIMIT/LADDER TOP (L1411-1424), budget notice (L1440-1446), persistent-notice latch (L1295-1306); ×2 gating wrong — Finding 2 |
| §2.6.5 Clamping demotion + partial backlog demotion | **WIRED** | L1251-1333 (spam latch missing — Finding 4) |
| §2.13 Fatal → halt + read-only + §2.14 bundle | **WIRED** | `on_sim_fatal` L624-645, panic hook L2951-2963, `--bug-report` L3034-3072; read-only gates on every verb path |
| §2.12 `--serve` host mode | **WIRED** | `serve_cli` L2972-3001: headless GameHost (World !Send respected), SPA packaging, safe `ServerConfig::default` (127.0.0.1 + generated token; auth details C27); save-dir claim false — Finding 5 |
| §3.7/M10 presentation floor — audio backend on the cue channel | **WIRED** | post-`end_frame` `FlightSfx`/`FlightSnapshot` drain L1014-1027, `ui_confirm` L1367-1378 |
| M10 settings + input rebinding (physical-first, migration, capture swallow, held verbs, focus loss) | **WIRED** | `press_edge` L1917-1973, `intercept_rebind_key` L2564-2587, `drop_rebound_holds` L2406-2423, `focus_lost` L2431-2443; stale-axis-on-rebuild gap — Finding 3 |
| §7/§7.4 session verbs (save/load/revert/new-game at drained boundary, name safety) | **WIRED** except NewGame | drained-boundary application L966-1011, `safe_slot_name` L3183-3191, atomic rename L1135-1142; NewGame broken — Finding 1 |

## Top 3 issues
1. **Finding 1 (HIGH)** — New Game boots an empty world (no starter seed after `View::new_game()`); empirically reproduced.
2. **Finding 2 (HIGH)** — ×2 physics warp unreachable under the rails refusal; PLAN §2.6.2 sanctions it while the bubble is occupied (the entire ascent).
3. **Finding 3 (HIGH)** — stale wall-side throttle axis survives Load/NewGame (deterministic vessel-id collision) → first Shift tap can unlatch at full throttle.
