# UI & Rendering — the presentation stack

Canonical reference for everything the player sees and touches: the bevy_ecs
view world, the egui panel layer, the wgpu renderer, and the winit/egui/warp
shell that drives them. Written against the working tree at commit `7dd4b86`
("parachutes deploy via Space staging" — the `ff897d4` movable-panels/clean-sky
work plus the staged-deployable staging change)
— the tree, not any older HEAD.

Companion docs: [ARCHITECTURE.md](ARCHITECTURE.md) (crate graph, determinism),
[SIMULATION.md](SIMULATION.md) (sim world, commands, ticks),
[GAMEPLAY-GUIDE.md](GAMEPLAY-GUIDE.md) (player-facing flows),
[DATA-AND-SCRIPTING.md](DATA-AND-SCRIPTING.md) (RON schemas, `data/`),
[BUILD-AND-DEV.md](BUILD-AND-DEV.md) (build, tests, xtask), [AGENTS.md](../AGENTS.md).
Plan references (`PLAN.md §N`) are to the repo-root design doc.

The four crates, and the only direction data flows:

| Crate | Role | Key files |
|---|---|---|
| `csp-view` | bevy_ecs presentation world; mirrors sim → render/UI packets | `crates/csp-view/src/{lib,flight,map,vab,terrain,structures,part_mesh,art,demo,tutorial}.rs` |
| `csp-render` | wgpu device, pipelines, WGSL; knows nothing of `csp-sim` | `crates/csp-render/src/{lib,terrain,terrain.wgsl,map,map.wgsl,flight,flight.wgsl,flight_shadow.wgsl,vab,vab.wgsl}` |
| `csp-ui` | pure egui panels + the VAB session model + settings/keymap | `crates/csp-ui/src/{lib,program,game,vab,settings,tutorial,navball}.rs` |
| `csp-app` | winit shell, input dispatch, warp controller, audio, save IO | `crates/csp-app/src/{lib,warp,audio,synth,settings_io,bugreport,logring,main}.rs` |

Dependency rules that shape everything (PLAN §2.1, enforced in code comments
and `xtask lint-deps`): `csp-ui` never sees `bevy_ecs`/`csp-sim`/`wgpu`/`winit`;
`csp-render` never sees `csp-sim`; `csp-view` is the only crate that sees both
`csp-sim` and `csp-render`; `csp-app` never reads the ECS world to build UI.

---

## 1. The frame contract: sim world vs view world

### 1.1 Two worlds, one owner

`csp_view::View` (`crates/csp-view/src/lib.rs:1519`) owns:

- `view.sim: csp_sim::World` — the authoritative sim, living **outside** the
  ECS (PLAN §2.2). The sim is deterministic, 64 Hz integer ticks, headless.
- `view.world: bevy_ecs::World` — the presentation world. Mirrors and session
  resources only; **no authoritative sim state ever lives here**.
- `view.schedules` — the six named schedules (PLAN §2.5), run only through
  `begin_frame` / `sim_step` / `end_frame`.

Frame order, driven by `csp-app` (`crates/csp-app/src/lib.rs:864` `run_frame`,
header doc at `:1`):

```
winit RedrawRequested → App::render (lib.rs:841)
  run_frame:
    take_ui_outbox → handle_ui_outbox     (last frame's UI requests)
    warp.observe/decide/target_tick → enqueue SetWarp + AdvanceTo
    view.begin_frame()                    Input schedule
    view.sim_step(target)                 ONE step_frame call, always
    view.end_frame()                      Sync → Presentation → UiBuild → RenderExtract
  paint:                                  tessellate FullOutput, draw scene pass + egui pass
```

Schedule membership (`crates/csp-view/src/lib.rs:2066-2092`):

- **Input**: `map::apply_map_input`, `vab::apply_vab_input`, `flight::apply_flight_input` — drain event queues staged by `csp-app`.
- **SimStep**: single-threaded executor; the actual `step_frame` call sits in `View::sim_step` (a bevy system cannot reach `self.sim`).
- **Sync**: `sync_mirrors` + `map::sync_map` + `flight::sync_flight` — copy staged post-step values into mirrors. The **sole writers** of `Clock`, `WarpMirror`, `FrameStats` (`lib.rs:435`).
- **Presentation**: `map::map_present`, `vab::vab_present`, `flight::flight_present`, `tutorial::tutorial_drive` — camera solves, LOD, VFX, cue edges.
- **UiBuild**: `build_ui` then `map_pointer_input.after(build_ui)` — the entire egui pass.
- **RenderExtract**: `map::extract_map`, `vab::extract_vab`, `flight::extract_flight` — produce plain-data render packets.

The extraction model (PLAN §2.5 "Appendix A"): schedules never read `sim`.
Sim reads happen in exactly two staging phases inside `View::end_frame`
(`lib.rs:2221`): `SyncInput` (clock/warp/ticks), `map::capture_sync_input`,
`flight::capture_sync_input` (which reuses `flight::LatchedWeldCache` so a
parked craft is not re-welded per frame, `flight.rs:844`), plus the direct
staging calls `stage_vab_tech` (`lib.rs:3548`) and `stage_program`
(`lib.rs:3575`). Systems only ever see staged resources.

### 1.2 Mirror inventory (what mirrors what, and when written)

| Resource | Written by | Cadence | Content |
|---|---|---|---|
| `Clock`, `WarpMirror`, `FrameStats` (`lib.rs:131-158`) | `sync_mirrors` | every frame | sim clock, stored warp level (§2.6.3: interpretation is `csp-app`'s job), frame/tick counters |
| `SessionMirror` (`lib.rs:285`) | `View::set_gpu_info`/`update_session`/`set_live_keymap`/`set_key_label` | per frame from app | adapter/backend, wall dt, warp requested/effective, Fatal state, live keymap + observed key labels |
| `ProgramMirror` (`lib.rs:231`) | `View::stage_program` | every `end_frame` | the whole progression surface — see §3.7 |
| `ScriptLogMirror` (`lib.rs:276`) | `View::push_script_log` | per step | rolling 64-line script console tail |
| `map::MapSnapshot` / `MapScene` / `MapUiData` / `MapPlanning` | `sync_map` / `map_present` / UiBuild | frame | orbit states, polylines, labels, alarm book |
| `flight::FlightSnapshot` / `FlightUiData` / `VesselPropulsionStaged` / `LiveVesselCount` | `sync_flight` / `flight_present` | frame | InBubble+latched vessels, HUD figures, Δv walk |
| `vab::VabSessionRes` | UiBuild + `vab_event` plumbing | on event | the working craft (§5) |
| `GameMenuRes`, `SettingsPanelRes`, `PartMenuRes`, `PanelFlags`, `tutorial::TutorialRes` | UiBuild / app plumbing | session state | panel visibility & widget state (§2.2-C: never sim state) |

`EguiPass` (`lib.rs:327`) bridges the whole egui pass inside UiBuild: `ctx`,
this frame's `RawInput` (staged by `csp-app` from `egui_winit`), and the
completed `FullOutput` that `csp-app` consumes via `View::take_ui_frame`.

`UiOutbox` (`lib.rs:350`) is the UI→app hand-off: requests emitted during
UiBuild (`warp_step`, `mode`, `launch`, `craft_save`/`craft_load`,
`maneuver`, `program: Vec<SimCommand>`, `settings_apply`, `game_save`/
`game_load`, `revert`, `new_game`) are drained by `csp-app` at the **next**
frame's input phase — PLAN §2.5's accepted one-tick UI latency.

### 1.3 View modes and transitions

`ViewMode { Flight, Map, Vab }` (`lib.rs:85`). Derived, not stored: VAB open
(`vab::VabInput.open`) wins, else map mode (`map::MapInput.mode == Map`), else
Flight (`View::mode`, `lib.rs:2522`).

`View::set_mode` (`lib.rs:2540`) owns the transition choreography: entering
VAB closes the map display (its overlay would draw under the editor);
re-entering Map runs `MapInput::refocus` (the fixed refocus, not a
hand-duplicated body-only version that used to snap vessel focus to the
6e10 m system distance). Mode switches come from three places — the top bar's
Flight/Map/VAB buttons (outbox `mode`), the `ToggleMap`/`ToggleVab` keys
(`csp-app/src/lib.rs:2653-2668`), and `View::launch_craft` (always lands in
Flight). Entering Map anchors: `MapInput::refocus` re-derives focus +
`default_distance` from `BodyTable`/`MapSnapshot` (`map.rs:407`, `:428`).

Launch (`View::launch_craft`, `lib.rs:2765`) is the biggest single transition:
recompute the VAB analysis cache → build `CraftDef` + aggregate BOM →
pre-flight refusals (`launch_preflight`, `lib.rs:3441`: vessel cap, funds,
site stockpile, pad occupancy — a parked build is recovered, not dead-refused)
→ capture the revert snapshot (`pre_launch`) → enqueue `ManufactureCraft`
(site'd launch; funds+stock debit at order, delivery `SurfaceLatched` at
`ConstructionComplete`) or `SpawnPhysicalVessel` (site-less sandbox worlds) →
push the `StagePlan` → `set_mode(Flight)`. Every refusal keeps the player in
the VAB with the reason on the session's notice row (`launch_refused`,
`lib.rs:3515`).

Load/revert/new-game replace the whole sim through `rebuild_from_snapshot`
(`lib.rs:1574`): config channels re-installed, `RhaiHost`/`AscentAutopilot`
re-installed, every sim-keyed session mirror reset, mode forced to Flight.

---

## 2. Input routing end-to-end

### 2.1 winit → csp-app dispatch

`App::window_event` (`crates/csp-app/src/lib.rs:2778`) processes each winit
event in a fixed order:

1. **Rebind capture first** (`intercept_rebind_key`, `lib.rs:2564`): while the
   settings panel listens for a new binding, the next press is the binding —
   it never reaches egui or normal dispatch, and the captured physical key
   stays swallowed until release.
2. `Focused(false)` → `focus_lost()` (clears held attitude/throttle axes).
3. `note_key_label` records the glyph the physical key produced on this
   keyboard layout (AZERTY `z` vs QWERTY `w`) into `SessionMirror::key_labels`.
4. **egui gets first pass** (`egui_state.on_window_event`), but `consumed`
   guards individual handlers rather than early-returning — a swallowed LMB
   release must still end a camera drag.
5. Event-specific routing:
   - `KeyboardInput`: dispatched unless a **text edit** is focused AND the key
     is `Key::Character` (a focused field has no use for Esc/F-keys/arrows,
     but egui reports `consumed` for every key while any widget holds focus —
     honoring it verbatim dead-keys the whole board). Releases **always**
     dispatch (a swallowed release leaks `attitude_held` or keeps the
     throttle ramping). `lib.rs:2862-2868`.
   - Left `MouseInput`: `map_dragging = !consumed && pressed`; the press also
     becomes a VAB GPU-pick candidate (`lmb_click_candidate`) whose release
     position is the pick pixel.
   - `CursorMoved` while dragging → orbit deltas per mode
     (`MapKeyEvent::Orbit` / `VabEvent::Orbit` / `FlightEvent::Orbit`).
     `last_cursor` updates even on consumed moves (else a drag crossing a
     panel takes one large jump).
   - `MouseWheel` → multiplicative zoom (`exp(−lines)` — scroll-up is
     zoom-in in every camera convention).

`key_press` resolves the event through the **live** `Keymap`
(`settings_io::keycode_token` physical first, `key_token` logical fallback),
then `dispatch_with_key(action, held_token)` (`lib.rs:2606`) runs one arm per
`KeyAction` — mode toggles, warp steps, throttle snap/axis, `Stage`
(mode-agnostic except VAB, where it says so), `CycleFocus` (map-only),
`RecoverControl` (calls `View::recover_control`), panel toggles, WASD/QE split
(attitude in Flight, camera pans in Map/VAB). The match is exhaustive by
design — a new `KeyAction` variant is a compile error, not a dead verb.

### 2.2 The rebindable keymap

`csp_ui::settings::KeyAction` (`crates/csp-ui/src/settings.rs:190`) is the
vocabulary (24 actions incl. `QuickSave`/`QuickLoad`/`RevertToLaunch`).
`Keymap` (`settings.rs:540`) is one-action-one-key with eviction. The flow:
the settings panel arms `rebind_target` → `csp-app` intercepts the press →
`View::bind_key` writes the staged map → **Apply** (outbox `settings_apply`)
makes it live, re-mirrors it into `SessionMirror::keys` (every hint bar prints
the live map, never the staged copy), drops holds on re-bound keys, and
persists via `settings_io`. Settings file schema/IO: `csp-app/src/settings_io.rs`.

### 2.3 The warp controller (PLAN §2.6.3)

`csp-app/src/warp.rs` — `WarpController` owns the ladder
`WARP_LADDER = [1, 2, 10, 100, 1_000, 10_000, 100_000]` (`warp.rs:33`), the
request/effective split, demotion hysteresis (§2.6.3 inequality:
`BUDGET_FRACTION = 0.5` of a 60 fps frame, promote after `PROMOTE_FRAMES = 30`
consecutive qualifying frames), and an integer-ns tick accumulator
(`target_tick`). The sim never measures anything — the controller observes
wall-side per-tick cost (frame `step_frame` cost ÷ ticks advanced,
`lib.rs:1007`) and emits logged `SetWarp`/`AdvanceTo` commands only.

Refusal surfacing (`warp_step_request`, `lib.rs:1398`): a press that the
settings cap or ladder top would eat says so (`request_preview` /
`multiplier_at` / `capped_below_ladder_top`, `warp.rs:367-393`); the sim's own
`rails_warp_refusal()` reason lands on the cue channel **every** press (a held
refusal during an atmospheric climb used to eat later presses silently); a
rung above `promotable_ceiling()` still queues but says why nothing changed.
Post-step, a persistent refusal re-invokes `demote` every frame, which also
clears the promotion streak. A world swap (load/revert/new game) calls
`warp.rebase()` so the accumulator never chases a stale absolute tick
(`lib.rs:983`).

---

## 3. egui panel inventory (csp-ui)

Every panel is a pure function: plain-data input structs in, plain-data
output structs out; the sim is never read (crate doc,
`crates/csp-ui/src/lib.rs:1-10`). All are drawn by `build_ui`
(`crates/csp-view/src/lib.rs:510`) inside `ctx.run_ui`.

### 3.1 Always-on chrome

| Panel | Anchor | Purpose / commands | Visibility |
|---|---|---|---|
| Top bar (`top_bar`) | `csp-ui/src/vab.rs:2169` | Mode buttons Flight/Map/VAB (outbox `mode`), LAUNCH (`launch` → `View::launch_craft`), warp readout + −/+ (`warp_step`), Tutorial / ? Help / Program toggles (folded back into session state by `build_ui`, `lib.rs:823-836`) | every mode |
| Program status strip (`program_status`) | `csp-ui/src/program.rs:852` | funds/science/active-contracts readout in EVERY mode; clicking it opens the Program window | every mode |
| Notice overlay (`notice_overlay`) | `csp-ui/src/lib.rs:702` | the §3.7 global cue stack — « text » lines in `NOTICE_AMBER` on the shared dark chip; drawn in every mode so refusals that fire in Map/VAB are visible (`flight_present` ages TTLs everywhere) | whenever cues exist |
| Game menu (`game_menu`) | `csp-ui/src/game.rs:61` | drag-anywhere strip, top-right: Save/Quicksave, Load slot list, Revert to launch (gated on `revert_tick`, arms the "not a reroll" confirm, `game.rs:138-170`), New game (confirm) → outbox `game_save`/`game_load`/`revert`/`new_game` | every mode |

### 3.2 Flight-mode panels

| Panel | Anchor | Purpose / commands |
|---|---|---|
| Flight HUD (`flight_hud`) | `csp-ui/src/lib.rs:747` | The telemetry block — ORBIT (Ap/Pe altitudes, times-to-apsis, period), SURFACE (alt, surface/orbital speed, VS, heading, pitch/roll), VESSEL (mass, thrust now/full, TWR local+surface, accel m/s²+g, Δv remaining incl. staged drops), PROP (throttle, tank gauges, mass flow, time-to-depletion, burn time, Isp), staging/parts/warp counts, the P1 FPS row (smoothed fps + frame ms; hidden until the first 10-frame window fills), controls hint. **Movable** `egui::Area` (`.movable(true)`, `lib.rs:753-756`) on the shared `hud_frame()` chip. Input: `FlightHudInput` built from `flight::FlightUiData`. |
| Navball (`navball` + `heading_tape`) | `csp-ui/src/navball.rs:203`/`:331` | KSP-style attitude ball + heading tape, pure egui painter over body-frame vectors (`NavballInput`). Its own **movable** Area (`lib.rs:1012-1020`). |
| Actions strip (`flight_action_bar`) | `csp-ui/src/program.rs:2542` | Bottom-right window: "⚛ run science (N)" one-click, transmit/recover pooled rows, deploy-parachute rows, Recover vessel / Terminate, "more…" opens Program. Emits `ProgramCommand`s resolved against the same `ProgramMirror` as the window. |
| Right-click part menu (`part_menu`) | `csp-ui/src/program.rs:2432` | KSP-style per-part action list at the click pixel. State `PartMenuState` (`program.rs:2228`) — `build_ui` closes it when the program mirror's focus moves off its vessel (`lib.rs:729-733`). Picking: `flight::pick_part` (§5.3). |

### 3.3 Map-mode panels

| Panel | Anchor | Purpose / commands |
|---|---|---|
| Map overlay (`map_overlay`) | `csp-ui/src/lib.rs:337` | Focus name, camera distance, body/vessel labels, focused orbit info, key hints. |
| Maneuver panel (`maneuver_panel`) | `csp-ui/src/lib.rs:1181` | Vessel picker (**on-rails vessels only** — a `SetManeuver` on an InBubble vessel rejects at drain, so it is not offered, `lib.rs:888-895`), prograde/normal/radial Δv + tick → `SetManeuver`/`DeleteManeuver` via outbox `maneuver` (`lib.rs:904-915`). |
| Planning panel (`planning_panel`) | `csp-ui/src/lib.rs:1465` | W2.4/M5: alarm book (arm/dismiss `SetAlarm`), transfer-window planner (origin/destination/vessel pickers; vessels staged with eligibility notes; plan row with Δv/ETA; `WatchTransferWindow`; "create departure node" → `SetManeuver`). Pure planner math runs view-side; sim verbs ride the program outbox. |

### 3.4 Session panels (drawn every mode by `m10_panels`, `lib.rs:1432`)

| Panel | Anchor | Purpose |
|---|---|---|
| Settings window (`settings_panel`) | `csp-ui/src/settings.rs:754` | Keymap rebind (arms capture), Graphics: vertical sync + render scale (P1, §7.2.2) + ui_scale, warp cap, audio volumes, new-game difficulty flags, reset-to-defaults. Apply rides `settings_apply`; the app owns file IO. |
| Crash dialog (`crash_dialog`) | `csp-ui/src/lib.rs:219` | §2.13 Fatal: reason + bug-report bundle path ("send this to the developer"). Session turns read-only. |
| Tutorial checklist (`tutorial_overlay`) | `csp-ui/src/tutorial.rs:318` | 10-step first-flight checklist (`STEPS`, `tutorial.rs:70`; VAB steps coached from inside the editor). Progress predicates run in Presentation (`csp-view/src/tutorial.rs:62` `tutorial_drive`) against staged mirrors only. Dismiss/Restart are session state; progress survives hide/show. |
| Debug panel (`debug_panel`) | `csp-ui/src/lib.rs:170` | Dev scaffold: adapter/backend, tick/frame counters, warp requested/effective. Off by default; `ToggleDebug` key. Hidden in VAB. |
| Help card (`help_panel`) | `csp-ui/src/lib.rs:1044` | Controls list from the LIVE keymap + observed key labels. `ToggleHelp`/top-bar "?". |

### 3.5 VAB panels — see §4

`vab_editor` (`csp-ui/src/vab.rs:2258`) draws the palette, craft tree,
inspector+staging+readout, craft-file bar; `action_bar` (`:2471`) the
symmetry/mount/undo strip; `vab_pointer_input` (`:2304`) the pointer/hotkey
pass that runs **last** (see §5.4).

### 3.6 The Program window (`program_panel`)

`csp-ui/src/program.rs:1787` — one scrollable window, sections in order
(`sect(ui, …)` anchors at `program.rs:1806-2186`):

- **OVERVIEW** — funds / science / banked ledger.
- **CONTRACTS** (`contracts_section`, `:889`) — offer rows with params,
  reward/deadline, prerequisite gating; `AcceptContract`.
- **VESSELS** (`:998`), **CREW** (`:1064`, hire by kind/board/transfer),
  **COLONIES** (`:1204`, found/join/leave, module+store rows, resource and
  crew transfers vessel↔colony, evacuate/abandon, schedule inspection; SITES
  sub-section `:1375`), **AGENDA** (`:1433`, in-flight transfers + alarm
  rows).
- **AUTOMATION** (`:1610`) — `LoadScript`/`UnloadScript`/`SetScriptEnabled`,
  engage/disengage ascent autopilot, script-log tail.
- **RESEARCH** (`:1837`) — tech-node rows (✓/○, cost, prereq/afford gates,
  `ResearchTech`) + upgrade tracks (`ResearchUpgradeLevel`).
- **SCIENCE** (`:1923`) — the focused vessel's context line, experiment rows
  (runnable/blocked-with-reason, `RunExperiment`), pooled data rows
  (transmit fraction now vs recover full later), recovery actions.
- **LABS** (`:2082`) — lab slots, per-lab "research <pooled row>" start
  buttons (crew aboard staff them), live projects with accrued science/data
  left/ETA + stop (`StartResearch`/`StopResearch`).
- **FACILITIES** (`:2146`, `facilities_section` `:1477`) — build-order ETAs,
  test-stand bench buttons per part family (`RunBenchTest` with unit counts,
  hazard-multiplier readout).
- **RELIABILITY** (`:2153`) — per-part wear/failure rows for the focus.
- **LOG** (`:2186`) — the notification ring (PartFailed/OnRails/…).

Every panel command (`ProgramCommand`, `program.rs:556` — 30+ variants) is
resolved to a `SimCommand` by `program_command` (`csp-view/src/lib.rs:1001`)
against the same `ProgramMirror` the panel was drawn from. A command whose
mirrored context vanished (focused vessel gone, stale row index) is **dropped
with a cue** ("COMMAND DROPPED — …"), never misfired and never silently eaten
(`lib.rs:773-791`).

### 3.7 The ProgramMirror — what mirrors what

Staged every `end_frame` by `View::stage_program` (`csp-view/src/lib.rs:3575`):
funds/science/banked + hire economics; the contract board (offers + active
rows with live progress); the tech tree + tracks; the focused vessel — chosen
as the flight focus (InBubble/latched, pin-aware) else the map's focused
on-rails vessel — with its residence, experiments (runnable + `blocked`
reason strings), pooled data keys, chute rows, reliability rows, latched
flag, recovery site, crew researchers (id+skill) and lab slots; the world
arenas (vessels, roster, colonies, sites, build orders, in-flight transfers,
alarms, autopilot label); the script-log tail. Focused-vessel commands ride
`mirror.vessel`; id-addressed commands pass through and reject at the drain
if stale. `science_ctx` stages on the **situation** (space situations have no
biome; zipping on the biome used to drop every orbital `RunExperiment` —
W1.2, documented at `lib.rs:3661-3668`).

HUD toasts from the sim: `View::sim_step` diffs the sim's notification ring
against `notice_cursor` and posts one `flight_notice` per new
`PartFailed`/`IgnitionMisfire` naming the **focused** vessel
(`drain_part_notices`, `lib.rs:3335`) — program-level and other-vessel
entries stay ring-only (the LOG section carries them).

---

## 4. The VAB editor

### 4.1 The session model and its state machine

The working craft is `csp_ui::vab::VabSession` (`csp-ui/src/vab.rs:375`),
hosted in the view world as `vab::VabSessionRes` (`csp-view/src/vab.rs:73`).
The craft graph itself is `csp_phys::session::CraftSession`
(`csp-phys/src/session.rs:249`) — a rooted tree of `SessionNode`s keyed by
monotonic, never-reused `PartNodeId`s (serialized with the craft file, the
hard M3 stability requirement). Each node: a `BuiltPart` (def+scale+variant+
frozen upgrades), an optional `AttachEdge` (parent-node ↔ child-node pair),
a `PartTransform`, and a `stage: u32`.

Every mutation goes through `VabSession::apply(VabAction)`
(`csp-ui/src/vab.rs:877`) — the editor's SessionCommand analogue. The
`VabAction` vocabulary (`vab.rs:154-276`): `New`, `SetRoot`, `AttachAuto`,
`CatalogPick`, `CancelPlacement`, `Drop`, `PlaceAtHover`, `Grab`, `Remove`,
`Reroot`, `Select`, `SetStage`, `SetScale`/`SetStretch`/`SetVariant`,
`SetSymmetry`, `SetAttachMode`, `Rename`, `LoadBytes`, `SetTargetDeltaV`/
`SetMinLaunchTwr`, `InsertStage`/`RemoveStage`/`CompactStages`, `Undo`/
`Redo`, `Translate`, `RemoveOnly` (children promote to grandparent).

The **held-part model** (KSP's VAB; module doc `vab.rs:30-41`): a palette
click never places — it lifts the part onto the cursor (`Carried::Placing`,
`vab.rs:116`). The ghost floats until: a click on a craft part (the drop,
consumed through `plan_attach` — side face surface-mounts, cap click stacks),
a click on empty build area of an **empty** craft (root), or cancel
(Esc/right-click/banner). Placed parts move the same way (`Grab` +
`CraftSession::reattach`, keeping node id/build/subtree). `VabSession::carried`
is the one accessor every affordance reads.

**Known defect (verified session-full-20261026, C17-F1, HIGH)**: the pick pass
rasterizes decoration (selection shells, ghosts, markers) tagged pick 0 in the
same `Greater`-wins stream, and the selection shell strictly encloses its part
— so **clicking a craft part to place/drop usually decodes as background and
click-selected-to-lift deselects instead** (empirically ~96-100% miss). The
empty-craft root click still works. Until fixed, place/drop via whatever part
pixels still decode (or the empty-craft path); see §9's defect ledger.

Attachment rules live in `csp-phys`: `CraftSession::add_part` /
`snap_transform` (`session.rs:913`, minimal-arc rotation mating the two
attach nodes), size-class compatibility on authored axial nodes, and
synthesized surface nodes (`surface_node_id`, `session.rs:785`). The
placement plan (`plan_attach`/`plan_move`, `vab.rs:1493`/`:1526`) chooses
from `AttachMode { Auto, Axial, Side, End }` (`vab.rs:70`) — Auto reads the
hovered `AttachRegion { TopCap, BottomCap, Side }` reported by the view's
hover pick. Radial symmetry ×1/×2/×4 (`SetSymmetry`). **Bottom attach works**:
the VAB camera's pitch clamp is full ±π so the −z cap and its `bottom` node
are visible and hittable (`csp-view/src/vab.rs:47`).

Staging: per-node `stage: u32` (higher fires earlier), edited in the
inspector; `InsertStage`/`RemoveStage`/`CompactStages` maintain the table.
The plan rides `flight::StagePlan` into launch (below).

Readouts: `VabSession::recompute` (`vab.rs:849`) is dirty-gated — runs
`csp_phys::readout::analyze` + `session.validate` + `body_transforms` only
after mutations (a pick does not re-resolve). The inspector shows per-stage
Δv/TWR/burn-time rows plus the `MissionProfile` comparison
(target Δv / min launch TWR). Validation (`csp_phys::session::Validation`,
`session.rs:605`) gates the launch button via `is_launchable`.

Persistence: `VabSession::to_bytes` (`vab.rs:2082`) → the app's craft store
(`saves`-adjacent `craft/` dir; name validated `csp-app/src/lib.rs:3205`,
write `:3227`); load returns through `VabEvent::LoadCraft(bytes)`. The demo
build refuses file IO (`set_file_io(false)`).

Undo/redo: bounded snapshot stack in `VabSession` (`can_undo`/`can_redo`,
`vab.rs:589`), wired to Ctrl+Z / Ctrl+Shift+Z (+ Ctrl+Y) and the action bar.

### 4.2 The view side (`csp-view/src/vab.rs`)

Data flow (module doc `:1-21`): `VabEvent` queue → `apply_vab_input`
(Input) → `VabInput` + session ops; `vab_present` (Presentation) solves the
camera and the **CPU hover pick** (`hover_pick`, `:660` — cursor ray vs part
slab AABBs, `slab_hit` `:702`, producing the `HoverHit { node, local,
region }` the attach plan consumes); `extract_vab` (RenderExtract) emits the
`VabPacket`. `VabScene`/`PartMesh`/`GhostMesh` carry the drawn craft, ghost,
and attach-node markers; `emit_environment` (`:843`) the editor backdrop.

**GPU picking**: each `VabPacket` carries `pick_ids` + `pick_generation`
(`csp-render/src/vab.rs:76-94`). On a click, `csp-app` rasterizes the same
vertices into an offscreen R32Uint object-id target (`VabRenderer::encode_pick`,
`csp-render/src/vab.rs:404`) and copies the one clicked pixel back
(`map_pick`, `:462`). The readback lands a frame or two later; the app
snapshots the id table + pick generation at encode time and **drops the
readback as background if the generation moved** (a session replacement —
New/load/undo/redo — between encode and landing; `pick_generation` bumps on
every wholesale swap while `craft_epoch`, which only the camera refit
watches, bumps on New/load alone, C17-F3 — `run_frame` pick drain,
`csp-app/src/lib.rs:874-895`). One pick in flight at
a time; an in-bounds click that cannot rasterize still resolves as
background so the click lands (P10-F1/F2/F3). **Caveat**: the pickable stream
also carries decoration tagged pick 0 (ghosts, selection shells, markers —
documented at `csp-render/src/vab.rs:87-89`, emitted from
`csp-view/src/vab.rs:793-823`); in the `Greater`-wins pick pass the enclosing
selection shell beats its own part's surface, which is the root of the C17-F1
click-gesture defect above.

### 4.3 Pointer/picking flow (who owns which click)

`build_ui` collects `ui_masks: Vec<egui::Rect>` from **every** floating window
drawn this pass (top bar, editor panels, help, settings, crash, tutorial,
menus) and runs `vab_pointer_input` **last**, so a press on any UI card never
reaches the craft behind it (C14-F2). `vab_pointer_input`
(`csp-ui/src/vab.rs:2304`):

- Press anchors the gesture (`press_node`, `press_on_viewport`) — a press
  over a panel never becomes a scene gesture; a viewport press that slides
  over a panel keeps its orbit suppression.
- `orbit_block` — while carrying a part or pressing on one, LMB is a
  placement/move, not a camera drag (anchored at the press, not live cursor).
- Drag-lift: press on a part that becomes a drag = `Grab` (one attempt per
  press; a refusal falls through to camera orbit).
- Release: a lifted part over the **palette** is deleted (drag-to-bin); over
  the viewport it `Drop`s; a palette drag resolves through the same `Drop`.
- Right-click over viewport: carrying → cancel; else remove hovered part.
- Hotkeys (Undo/Redo/Del/Esc/X-symmetry) gated on `text_edit_focused`, never
  window focus.

### 4.4 Launch → flight staging hand-off

`flight::StagePlan` (`csp-view/src/flight.rs:644`) captures the session's
stage table at launch (`stage_plan_of`, `lib.rs:4061`); `StagePlans`
(`flight.rs:758`) holds one plan per launch, bound to its vessel by
`(min_id, craft fingerprint)` once the spawn lands. `View::fire_stage`
(`lib.rs:3189`) pops the focused vessel's next stage set (`StageActions`,
`flight.rs:603`: `deploys` + `jettisons`) and issues **one logged `DeployPart`
per staged deployable (parachutes) BEFORE one logged `JettisonPart` per
drop-set node** — the staged-chute rule of `7dd4b86`: a chute staged alongside
the decoupler that drops its own subtree must get its deploy record while the
node still exists (a `DeployPart` naming a jettisoned node just rejects). All
of this only after the bound vessel checks bubble-resident (on a latched/rails
vessel the commands would all reject while eating the stage). Every outcome
posts a cue, naming deploys vs jettisons ("STAGED — 1 PART(S) DEPLOYED"). Known
honest limitation (doc at `flight.rs:616-632`): a jettisoned part leaves the
sim outright — no debris fragment — and engines are globally armed so a firing
separates but never ignites.

---

## 5. Flight view

### 5.1 Staging and the chase camera

`flight::capture_sync_input` (`flight.rs:950`) stages every InBubble **and**
surface-latched vessel as a `FlightBody` (`flight.rs:253`: id, pos/rot/vel,
throttle, debris flag, contact flag, `plasma_w_m2`, part AABBs, live plume
emitters with delivered-thrust share, tanks, latched flag) — ascending
`VesselId`, so **the focus rule is `last()` = newest spawn**
(`focus_vessel`, `flight.rs:1965`): a fresh launch takes camera, HUD, and
`SetThrottle` routing with it. `FlightFocusOverride` (`flight.rs:334`) pins
focus for recover-control (releases on seen-then-left, vessel-gone, or a
fresh launch). The `TakeControl{Player}` lease transfer issues on the pin's
`seen` edge in `end_frame` (`lib.rs:2261-2283`) — never optimistically at
press time.

`flight_present` (`flight.rs:1730`) solves the chase camera
(`solve_chase_camera`, `flight.rs:1524`, FOV 0.9 rad, terrain-floor aware via
`terrain::surface_floor_radius`) and derives the HUD mirrors: attitude
(`stage_attitude`, `flight.rs:1974` — heading/pitch/roll, navball
body-frame up/north/east/prograde, surface-relative speed = v − ω×r) and the
propulsion walk (`stage_propulsion`/`delta_v_walk_from` → per-stage Δv, TWR,
burn time — the Kerbal Engineer-equivalent readouts).

### 5.2 VFX: plume, plasma, dust (the §3.7 floor)

All emitted into the flight packet's **unlit stream** (zero normal = fragment
passthrough, `flight.rs:2386-2418`):

- **Engine plume** (`emit_plumes`, `flight.rs:2446`) — cone per live Thrust
  site, keyed to **delivered** thrust (rated_n rewritten each tick by the
  §3.4 derate × pooled propellant coverage — a dry or misfired engine draws
  no exhaust).
- **Reentry plasma sheath** (`emit_sheath`, `flight.rs:2543`) — emissive
  shell grown off each hull AABB, intensity `plasma_i` normalized over
  5e3…8e4 W/m² convective flux (`PLASMA_ONSET_W_M2`/`PLASMA_FULL_W_M2`,
  `flight.rs:46-48`) from the same `flow_sample` the thermal tick consumes.
  Display normalization only — lethality is the sim's temperature record.
- **Ground-contact dust** (`flight.rs:2893-2958`) — translucent tan ring at
  the contact point when the last step recorded contact.

### 5.3 Part picking (right-click menu)

`flight::pick_part` (`flight.rs:1653`) — pure presentation math: project the
focus's part collider AABB corners through the solved f64 chase camera into
egui points; frontmost containing part wins (nearest projected corner depth),
near-miss picks the closest projected part center (≤48 px,
`PICK_NEAR_PX`), a click inside the hull bounds but on nothing resolves
`FlightPick::Vessel` (whole-vessel menu). Called from `build_ui` only when
egui's own pointer is idle over the scene (`lib.rs:700-726`).

### 5.4 Focus and command routing

`View::flight_focus` (`lib.rs:2733`) is the throttle/attitude command target
(a surface-latched pad craft still counts — `SetThrottle` unlatches it).
`View::recover_control` (`lib.rs:2635`) is the T-key dead-stick recovery:
map-focused (else newest eligible) on-rails vessel → pre-checked
`to_bubble_eligible` (`lib.rs:2688`, including the apply's fallible
`state_at` + re-weld halves) → `ToBubble` + focus pin; refusals surface as
notices, and a drain-time `ToBubble` rejection releases the pin
(`sim_step`, `lib.rs:2185-2198`).

---

## 6. Map view

### 6.1 Rendering

`map_present` (`csp-view/src/map.rs:1279`) and `extract_map` (`:1992`).
Precision: the home system spans ~4e10 m; every world position is subtracted
from the f64 eye **before** the f32 cast — vertices reach `csp-render`
already in clip-space NDC (`map.rs` module doc). A separate fractional
`present_tick` (advanced by wall-dt × warp, clamped to the snapshot) keeps
rails-warp markers from strobing on the integer-tick grid (`map.rs:1300-1320`).

Drawn: a fixed world-anchored starfield (700 stars at 1e28 m — rotation
parallax only, `emit_starfield`), body/vessel orbit polylines (128 segments,
cached per `OrbitState`; parent-centred lines ride the parent's current
position), body markers + focus ring, vessel markers, maneuver-node markers
(green), apsis markers, launch-site ring markers (`structures::SiteTable`),
SOI rings, encounter markers (enter/exit/impact colors, `map.rs:115-119`),
and the planned-transfer conic (`PLAN_COLOR`). All unlit, shaped in the
fragment stage (map.wgsl kinds: solid line / soft disc / ring / radial glow).

### 6.2 Interaction

Camera: left-drag orbit (`MapKeyEvent::Orbit`), wheel zoom at cursor
(`map_pointer_input` routes scroll-zoom to the point under the cursor),
middle/right-drag pan, `CycleFocus` key. Click-to-focus: a primary click
within `PICK_RADIUS_PX` of a projected marker focuses it
(`map_pointer_input`, `csp-view/src/lib.rs:1344` — runs after `build_ui` so
egui pointer state is complete; `egui_wants_pointer_input` gates every
action, with a latched `drag_over_ui` for panel-crossing drags whose release
must never leave the camera moving).

### 6.3 Planning surface (W2.4 / M5 — Kerbal Alarm Clock equivalent)

`MapPlanning` (`map.rs:820`) — session state; the sim's event heap is
authoritative. `AlarmEntry` (`:800`) rows arm `SetAlarm` (Manual, matched by
label) and `WatchTransferWindow` (Window, matched by body pair);
`note_fired` (`:859`, called from `View::sim_step` on every
`AdvanceReport::fired_events`) marks entries fired on their first matching
event **and records unmatched fires as rows of their own** (a script's or
MCP-set alarm still surfaces). Unfired entries past `tick + 2 s` lapse
honestly. Book capped at 32 (`ALARM_BOOK_CAP`), armed rows never evicted.

`PlannedTransfer` (`map.rs:836`) stages the last `plan_transfer` result: the
Lambert/porkchop plan (`porkchop_query`, `:985`), the resolved departure
node for the picked vessel (+ refusal text when the ejection did not solve),
and the first predicted SOI encounter. `resolve_planning_command`
(`:1041`) turns panel commands into `SetAlarm`/`WatchTransferWindow`/
`SetManeuver` on the program outbox; pure planner calls run on the spot.

Data flowing in from the sim: `MapSnapshot` (`map.rs:499`) — vessel orbit
states + on-rails flags + maneuver nodes + tick, staged by
`capture_sync_input` each `end_frame`.

---

## 7. The renderer (csp-render)

### 7.1 Device & frame lifecycle

`Gpu` (`crates/csp-render/src/lib.rs:176`): wgpu 30, `Backends::PRIMARY`,
HighPerformance adapter, **`required_features: Features::empty()`**
(`lib.rs:227` — Baseline everywhere; see §10), sRGB surface format
(non-sRGB fallback warns loudly — output renders ~2× dark), frame
latency 2. The present mode is a P1 parameter (`Gpu::new`, live via
`Gpu::set_present_mode`): the settings' `vsync` (`auto`/`off`) maps to
`AutoVsync`/`AutoNoVsync` in `csp-app` — a change reconfigures the
surface on the same path `resize` takes (compositors that always
vsync quietly absorb an `off`). Uncaptured errors are logged-and-deduped
(§2.13 Cosmetic class), device-lost degrades to per-frame reconfigure.
`begin_frame` (`:345`) returns `Ok/Reconfigure/Skip`. 4× MSAA on every
surface-bound pipeline via one shared `MSAA_SAMPLES = 4` constant and
the `MsaaColor` resolve target (`lib.rs:60-130`) — there was no AA
anywhere before C11-F1. The scene render scale (P1, §7.2.2) sizes the
MSAA target and the optional resolve intermediate; `scene_target_size`
(`lib.rs`) is the pure target math (`round(surface × scale)` per axis,
1-px floor, clamped, unit-tested).

### 7.2 Pass/pipeline inventory

**Flight mode runs TWO scene passes per frame** (§7.2.1's depth
partition): `scene-far` (cleared 0.02/0.02/0.05, MSAA, no resolve,
depth cleared 0.0 — the far field) then `scene` (color `LoadOp::Load`,
the same depth attachment cleared AGAIN, MSAA resolve to the swapchain —
or to the §7.2.2 intermediate below render scale 1.0). Map/VAB keep one
scene pass (clear + resolve, `csp-app/src/lib.rs:1887-2030`), then
`Gpu::blit_scene` (no-op at scale 1.0, §7.2.2) and a separate **egui
pass** (no depth, `LoadOp::Load`) — wgpu requires pass/pipeline depth
formats to match, and egui's pipeline has none
(`csp-app/src/lib.rs:2059`).
Far-pass draw order (`csp-app/src/lib.rs:1767-1824`): terrain far
phase (sky backdrop + far instances + far ocean) → flight far stream
(the fallback sky). Near-pass order: map packet (Map mode) → VAB
packet (Vab mode) → Flight mode: **near terrain → flight craft stream
→ near ocean** (the ocean is a third, alpha-blended draw after the
craft so water resolves over an already-rasterized hull, C11-F3).

| Renderer | Pipelines | Notes |
|---|---|---|
| `MapRenderer` (`map.rs:80`) | one unlit pipeline, three meshes (bg stars / lines / markers) | no depth state — the scene pass attaches depth only in Flight/VAB |
| `TerrainRenderer` (`terrain.rs:670`) | `terrain_pipe`, `ocean_pipe` (depth-test-only variant), `sky_pipe` (fullscreen, always-pass depth), + two depth-only shadow pipelines `vs_shadow0/1` (`shadow.rs`, 3a — see §7.4) | bind groups: group(0) = 0 uniforms, 1 heightfield R32Float, 2 transmittance LUT RGBA32Float, 3 biome slots R8Uint, 4–6 art albedo/normal arrays + water normal, 7 sampler, 8 cascade `SU` uniform; group(1) = shadow depth D2Array + comparison sampler (visible pass only — see §7.4). Since §7.2.1 there are TWO group(0) sets over the same textures — near and far uniform rows — swapped per phase; the pipelines themselves are shared |
| `FlightRenderer` (`flight.rs:75`) | one pipeline; owns the surface-sized reverse-Z depth target | depth compare `GreaterEqual` because the fallback sky sits ON the far plane; since §7.2.1 a second vertex buffer (`far_vertices`) draws the far stream through the same pipeline in the far pass |
| `VabRenderer` (`vab.rs:123`) | color pipeline + offscreen pick pipeline (R32Uint object-id) | `encode_pick` rasterizes + copies one pixel |

Shared lighting: `PartLight` (`lib.rs:33`) — one directional sun + two-stop
hemisphere ambient; the VAB uses the neutral studio default, flight uses the
staged sun. Reverse-Z throughout (clear 0.0 = far, `Greater`/`GreaterEqual`
wins).

### 7.2.1 The two-pass depth partition (PLAN §2.3's far-field pass)

The single-frustum reverse-Z depth strategy (§2.3: near 0.1, infinite
far, `Depth32Float`) cracks beyond ~10⁷ m — the perf row
`render.depth-fight-1e7` (§5.4's R1 measurement, 311 040
order-dependent pixels at 1080p for a 1 m object crossing a surface at
10⁷ m) called in the PRD's prescribed fix: a **second far-field
pass**, not log-depth (log-depth breaks early-Z and interpolation).

**The split** is `FAR_SPLIT_M = 10⁶ m`
(`csp_render::flight::FAR_SPLIT_M`) — far enough that nothing
gameplay-near changes (physics bubble 20 km, shadow cascade coverage
~20 km, chase camera ~60–300 m, dump horizons < 30 km), near enough
that 10⁷ m sits at z = 0.1 in the far pass's best-placed binade.

**The algebra** (the cheaper math §2.3 allows): the reverse-Z
projection's clip.z row in `(P·V)` is exactly `[0,0,0,n]` (every other
z-row term is zero and the camera-relative view's bottom row is
`[0,0,0,1]`), so `clip.z` is the constant `n` and NDC z = n/d. The far
frustum is the SAME matrix with that one row set to `[0,0,0,10⁶]` —
xy columns and the w row are bit-identical, so far geometry keeps its
screen position and only depth remaps (`z = split/d`). The far pass
reuses the near pass's pipelines wholesale: the terrain draws swap a
second group(0) (same textures, far uniform row) and the flight far
stream bakes `z = split/d` per vertex CPU-side (`vs_main` is a
passthrough — no shader variant exists). The clip test
`0 ≤ clip.z ≤ clip.w` becomes `split ≤ d` — a **staleness safety
net**, never a coverage mechanism: routing (below) sends a patch far
only when its nearest point clears the split, so the clip never trims
visible surface.

**Pass order + depth handling** — far first, near on top, clear
between: the far pass attaches the shared depth target with its own
0.0 clear (sky backdrop + far terrain/ocean + far flight stream; its
depth store is `Discard` — the near pass clears the attachment again
before reading it, so the far depths are write-only); the near pass
attaches the SAME target, clears it again, and draws near
terrain → craft → near ocean. Coverage is by WHOLE-QUAD routing: a
patch routes far only when its nearest point ALONG THE VIEW AXIS —
`dot(patch_origin − eye, fwd) − patch_extent ≥ split`, with
`patch_extent_m` = cube half-diagonal + relief amplitude the
conservative centre-to-surface reach — clears the split. The forward
(view-axis) depth is what the far pass's clip test `split ≤ clip.w`
measures, and it is `radial·cos(off-axis)` — up to ~25 % shorter
than the radial distance at the frame's side reaches; a radial-only
rule clipped side-of-frame quads out of both passes (the second
sky-through-planet hole). Straddlers — radial or angular — route
near and draw whole (exactly the pre-split behaviour for them; the
original patch-CENTRE rule left their near fringes clipped and drawn
by no pass), so the far pass's clip test is a pure staleness safety
net that never removes visible surface. Cross-pass, "near wins where
both exist" is the correct winner for every wholly-far quad; the one
approximation is a near-routed straddler's beyond-split fringe,
which rides the near pass's unconditional-over policy in the grazing
band at the split ring (the same documented policy as the craft case;
measured 3 px at the split-ring gate's 800 km vantage). No
depth-bias or clip-range remap is needed, and the near pass's z
encoding is unchanged bit-for-bit (the bit-identity proof:
dump_surface / dump_low / dump_flight are byte-identical
before/after; all their geometry routes near, so the far pass records
sky only and the near pass is today's pass with `LoadOp::Load` over
an identical backdrop).

**What draws where**: terrain/ocean patches whose forward-nearest
point clears the split, and the sky (sky writes no depth — `Always`
compare, no write — so it can never occlude) draw far; near patches
(including every straddler), craft, plumes, the launch complex draw
near. Craft geometry NEVER routes far — a craft at 10⁷ m from
the camera is the map regime; the near pass draws it over far terrain
unconditionally, a documented stable approximation (no flicker, no
correct craft-vs-far-terrain occlusion promised at interplanetary
range). The §5.4 probe (`csp-view/src/depth_probe.rs`) composes its
scene the same way — surface through the far pass, reference object
through the near pass — and certifies zero order-dependent pixels.

**Why a partition and not a finer float**: an f32 depth value carries
2²⁴ steps total; the resolvable depth difference at range d is
~d·2⁻²⁴ m regardless of the projection's near (ulp(z)·d²/n with
z = n/d — the n cancels). 1 m at 10⁷ m is ~1.1 ulp of depth in the
near pass (~1.3 in the far pass; ~0.89 m and ~0.75 m per ulp
respectively), and a frame-width 1 m swing there would need ~2³²·⁴
steps to resolve at every pixel — beyond any single f32 depth
mapping. The split still buys two things: the far band's z values
move from the 10⁻⁸ binade floor (z/ulp ≈ 1.1·10⁷) up to 0.1
(z/ulp ≈ 1.3·10⁷, the best-placed binade), and — the load-bearing
half — the pass partition fixes cross-pass ordering structurally,
which is what eliminates the order-dependence the row measures.

**Cost**: gameplay-range frames draw the same geometry as before (all
patches near ⇒ the far pass records the sky only — the sky draw moved
passes, it did not duplicate); frames with far-field geometry add one
extra terrain draw call (the far instance range) and one extra ocean
draw, plus one extra render-pass begin/end. The render gate's wall
time is unchanged within noise (§8.2's log).

### 7.2.2 The render-scale path (P1 — settings-driven resolution scaling)

`GraphicsSettings::render_scale` (`csp-ui/src/settings.rs`, 0.5–1.0 in
0.05 steps, default 1.0) shrinks the SCENE while the UI stays native —
the player's fill-rate control at high resolutions. The pipeline
shape, end to end:

1. **Apply**: `csp-app`'s `apply_live_settings` clamps the value and
   calls `Gpu::set_scene_scale` → `refresh_scene_targets`, the SAME
   recreate path a `Resized` event takes (the MSAA target rebuilds at
   `scene_target_size(surface, scale)`). The mode renderers' depth
   targets follow through `App::resize_scene_attachments`, shared by
   the resize handler and the scale change — one path, no third
   variant.
2. **Scene render**: every scene pass (far + near, map, VAB) renders
   into the scaled MSAA target against the scaled depth targets
   (`FlightRenderer::resize` / `VabRenderer`'s scene half). Geometry
   is projected to NDC — resolution-independent — so the scaled frame
   is the same image at fewer pixels; only MSAA edge quality scales.
   At scale 1.0 the pass resolves STRAIGHT INTO the swapchain view,
   the pre-P1 path verbatim — no intermediate texture, no blit pass,
   byte-identical frame content (what the render gate's dumps pin).
3. **Resolve + upscale** (scale < 1.0 only): the pass resolves into a
   scaled single-sampled intermediate (`SceneResolved`,
   RENDER_ATTACHMENT | TEXTURE_BINDING), then `Gpu::blit_scene`
   records one fullscreen-triangle pass (`blit.wgsl`, linear filter —
   a straight upscale, scale ≤ 1 means downscaling never happens)
   into the swapchain frame, and the egui pass composites over it at
   NATIVE resolution — crisp text is the whole point of the two-step
   resolve/blit instead of rendering everything small.

**What scales and what doesn't** (the audit): the scene color MSAA
target and the flight/VAB scene DEPTH targets scale (they share one
pass — wgpu requires matching sizes). The shadow cascade maps (2048²
fixed, `shadow.rs`), the VAB pick pass (`pick_targets` at SURFACE size
— its rasterization is indexed by physical click pixels), the egui
frame, and the swapchain itself stay native. The far-field partition
(§7.2.1) keys on clip-space depth, not pixels, and both of its passes
share the scaled color+depth attachments — resizing them together
cannot change the partition (the render gate pins the two-pass shape
at its own size; scaling only resamples it). Camera viewports feed
full surface size (`run_frame`); aspect is identical (uniform per-axis
scale), so projections are unchanged.

The FPS readout that makes the scale worth reaching for: the flight
HUD chip's FPS row (`fps`/`frame_ms` on `FlightHudInput`), fed by
`csp-app`'s `FrameTimingSmoother` — the §2.5-legal wall-clock dt,
mean over a 10-frame window, into `View::set_frame_timing` →
`FlightUiData` → UiBuild. Pair it with vsync off: fps climbing means
CPU-bound, standing still means GPU-bound — lower the scale.

The F3 debug panel carries the phase split that resolves the question
outright: `frame split: cpu X ms · present Y ms` (`cpu_frame_ms`/
`gpu_wait_ms` on `DebugPanelInput`, fed by `csp-app`'s
`FramePhaseSmoother` through `View::set_frame_phase` — "present" is
the acquire + submit/present wait, where vsync or a saturated GPU
parks the CPU thread). Present-dominated ⇒ GPU/vsync; cpu-dominated ⇒
game code or the player's CPU. The same INFO line that lands at GPU
init — `render: adapter "…" (backend), surface <fmt>, WxH, <present
mode>, scale N.NN`, re-logged on every vsync/scale flip — is the
support surface to attach beside it.

### 7.3 WGSL inventory

| File | Entry points | Renders |
|---|---|---|
| `map.wgsl` (48 lines) | `vs_main`/`fs_main` | Unlit map primitives; vertices pre-projected on CPU; fragment shapes quad kind 0 line / 1 disc / 2 ring / 3 glow |
| `vab.wgsl` (111) | `vs_main`/`fs_main` + pick fragment | Per-pixel-lit editor parts (hemisphere ambient × baked occlusion, key light, Blinn specular, fresnel rim); pick writes the 1-based object id |
| `flight.wgsl` (208) | `vs_main`/`fs_main` | Same per-pixel lighting as VAB minus pick, plus the unlit passthrough (`nrm ≈ 0`); ACES (Narkowicz) tone-map shared with terrain so lit hulls match the ground. Since 3b: the direct-sun terms multiply the cascade `sun_visibility` (a C16-F3 duplication of terrain.wgsl's receive half — no WGSL include mechanism) |
| `flight_shadow.wgsl` (69) | `vs_shadow0`/`vs_shadow1` (3b craft cast, depth-only) | Drops the flight stream into §7.4's cascade map. A dedicated module on purpose: the cast pipelines ride a minimal group(0) (`SU` alone at binding 8), and sharing flight.wgsl left its `light`/group(1) declarations dangling against that layout, which de-registered the uniform binding on-device (the cast silently discarded — see the file header) |
| `terrain.wgsl` (1391) | `vs_terrain`/`fs_terrain` (`:414`/`:893`), `vs_sky`/`fs_sky` (`:1132`/`:1263`), `vs_shadow0`/`vs_shadow1` (`:440`/`:448`, depth-only cascade pass; the shared displacement is `terrain_displace`, `:385`) | Quadtree terrain, ocean, and the sky/atmosphere/starfield — see §8; the shadow entry points feed §7.4's cascade map |
| `blit.wgsl` (P1) | `vs`/`fs` | §7.2.2's render-scale upscale — one fullscreen triangle sampling the scaled resolved scene (linear) into the swapchain |

All six are parse+validate gated by a unit test
(`shaders_parse_and_validate`, `csp-render/src/lib.rs:484`) so shader
regressions die in `cargo test`, not at device init.

### 7.4 Terrain: quadtree, heightfield, LUT, ocean, biome tint

CPU quadtree over the six cube-sphere faces (`PatchId`, `select_patches`,
`csp-render/src/terrain.rs:73`/`:164` — split while `dist(eye,node) < K·size`,
horizon-culled, budget-bounded, BFS-deterministic). All leaves share one
unit grid mesh (`unit_grid`); the draw is a single instanced call.
Deepest level 13 ≈ 146 m patches at a 600 km body (M5 §5.2-R2 relief
density — fine enough to resolve the Mountain bake's ~1.8 km texels).
Precision: each instance bakes `origin − eye` in f64→f32
(`patch_instance`, `:458`); residual vertex error ~5 cm at a 600 km body.
Skirts hide T-junction cracks.

Height source: deterministic seeded lattice value-noise FBM on the unit
sphere, baked once per body into an R32Float equirect `HeightField`
(`csp-view/src/terrain.rs` `TerrainCache`; constants shared with
`csp_core::terrain` so the physics contact surface samples **bit-identically**
to the drawn surface). Since M5 §5.2-R2 the bake is per authored
`TerrainDef::relief` profile (`HeightField::bake_profiled`): Legacy
1024×512, Mountain 2048×1024 — and the amplitude is authored
(`radius_m · amplitude_frac` into `TerrainBody::amplitude_m`/
`uniforms.planet.w`). A `heightmap: Some(_)` ref is the unimplemented
R4/native hook.

**Bake scheduling / the arrival contract (M5 §5.2-R2 arrival polish).**
The bake runs on a background thread (native; C12-F5 — the frame a body
becomes dominant no longer stalls on it) and `TerrainCache::field`
serves `None` until the real field lands. There is **no stand-in**: the
packet for those frames is *sky-only* — `heightfield: None`, zero
terrain/ocean instances, disarmed vessel blob — so the player sees the
scattering sky (haze below the horizon on atmospheric bodies), the
craft and the launch complex, never a flat datum sphere standing in
for mountains. CPU-side takers of the field (chase-camera floor,
structure anchors) fall back to the bare datum radius for exactly
those frames; the first packet with a field swaps the full displaced
surface in (the field's content key forces the height-texture
re-upload). The wait frames also DISARM the shadow cascades — both in
the view (`extract_flight` hands `draw_shadow` the zeroed row while
pending) and structurally in the renderer (`draw_shadow` disarms any
armed row handed with an instance-less packet: the cascade passes
would early-return without clearing the depth array, and receivers
would sample its never-cleared contents — zeros on a fresh session,
the previous body's stale map after an SOI switch — through live
matrices, silently killing the craft's sun term). The flight HUD
mirrors the wait as a one-line
"GENERATING SURFACE …" chip row (`FlightHudInput::terrain_pending`,
written by `extract_flight`). Map view draws no terrain anywhere (its
packet is lines/markers only) and VAB is scene-less, so the wait state
is flight-only. wasm has no threads: the bake runs inline at the first
`field` poll, so the `None` wait state is unreachable there by
construction (the demo already pays the one-time startup hitch).

Scattering: `bake_transmittance` (`terrain.rs:577`) CPU-ray-marches the
Bruneton-style T(altitude, μ) LUT (96 steps) for the §3.0 exponential
atmosphere; the shader consumes it for sky color and terrain aerial
perspective — the Baseline "precomputed LUTs, analytic single-scatter".

Ocean: a second instanced batch of the same grid at the body's ocean datum
radius, emitted only for leaf patches whose sampled heights reach the datum
(`patch_might_hold_sea`, `csp-view/src/terrain.rs:337`); airless/oceanless
bodies emit nothing.

Biome tint: the authored index map only **tints** — it never moves a vertex
(§3.0.1 decoupling). `BiomeField` slots (≤16) map through
`TerrainUniforms::biome_colors`; the tint map itself is the 256×128 equirect
`biomes.png` (quadrupled from 64×32 in texture-quality 4c) — sampling is fully
dims-driven (`textureDimensions` in WGSL, decoded width/height in `biome_at`);
`ramp_albedo`/`class_weights` in WGSL blend
grass/rock/sand/snow by height/slope/latitude with detail-noise dither, or
the real-art albedo/normal arrays (`TerrainArt` from `data/art/terrain.csar`)
when the packet carries them (`uniforms.art.x`).

**Triplanar detail + per-tile de-repetition (texture-quality 4b).** The
real-art path samples all three projection planes with |n|⁴ weights over
world-anchored UVs (`tri_planes`/`tile_uv`, `terrain.wgsl:724`/`:804` — the
anchor quantizes the frame so `d·k`'s f32 ulp never jitters a texel).
Since 4b each tile cell applies ONE of the eight dihedral transforms of
the square (identity/rot90/rot180/rot270 × optional mirror — 3 bits:
flip-u, flip-v, swap) to its within-tile UV before the fetch, selected by
an integer hash of (cell, plane, layer) (`tile_bits` — `hash_i3`'s
avalanche with a top-3-bit cut). The cell index is `floor` of the *same*
f32 coordinate whose `fract` is the sampled UV, so the transform can
never disagree with the wrap (no border seam strip), and the hash inputs
are pure functions of the body-fixed direction — the world-stability
contract holds; camera motion cannot shimmer or crawl the pattern.
Albedo and normal sample through the identical transform (lockstep), and
the `class_means` normalization is preserved because every dihedral map
is a texel bijection (each cell still shows the whole tile). Cost: ~20
ALU per sample coordinate, zero extra fetches. Distance-band table
(scales × base tiling; base tiles are 9/22/12/16 m per class):

| band | old | 4b | fade |
|---|---|---|---|
| albedo mid (aerial large-variation layer) | 0.41× | **0.47×** (~19–47 m) | 40 km |
| albedo near (boot/rover grain) | 5.9× | **8.5×** (~1.1–2.6 m) | 1.8 km |
| normal near (matches albedo near) | 5.9× | **8.5×** | 1.8 km |
| normal fine (finest grain, filtered) | 15.7× | **24×** (~0.4–0.9 m) | 1.8 km² |

The micro-relief bump's top noise octave stays at freq 16000 — the
shader's STABILITY ceiling for direct lattice samples — with 4b's extra
near-field detail coming from a second incommensurate 12200 octave
behind a 1.8 km boot fade (`bump_fix_near`, `terrain.wgsl:525`) plus the
finer filtered normal retilings above. `uniforms.art.z = 1` (render-gate
`DUMP_DEREPETITION=0` A/B dumps only; production writes 0) restores the
pre-4b plain periodic tiling verbatim for before/after evidence pairs.

The vessel blob shadow (`uniforms.shadow`, `vessel_shadow`,
`csp-view/src/terrain.rs:497`) anchors the focused craft's shadow to the
surface; `csp-view/terrain.rs::build_packet` (`:560`) assembles the whole
`TerrainPacket` per frame from the staged `FlightEnv` + chase camera.

**Shadow cascades (R1 core, landed 3a — ARMED since 3b; the R1 debt is
closed).** `csp-render/src/shadow.rs` owns the whole cascade core: the
pure f64 per-frame solve (`solve_cascades` — two sun-aligned ortho
cascades fit to the chase camera's frustum slices at ~0–2 km and
~2–20 km, light basis fixed per sun direction, ortho extents quantized
onto a √2 world-texel ladder and the box center texel-snapped so
sub-texel camera motion reproduces bit-identical matrices), the GPU map
(one 2-layer 2048² `Depth32Float` array, 2×2048²×4 B = 33.5 MB, sampled
through a `LessEqual` **comparison sampler** — all core WebGPU,
`Features::empty()` untouched), and two depth-only pipelines
(`vs_shadow0/1`) whose vertex stage shares `terrain_displace` with
`vs_terrain`, so the depth pass displaces vertices identically to the
visible pass. The receiving half lives in `terrain.wgsl`:
`sun_visibility` picks the cascade by eye-distance (conservative against
view-Z), applies normal-offset + slope-scaled reference bias tuned for the
M5 relief ceiling (~37° slopes), blends a band at the split, and multiplies
the **direct sun term only** of terrain and ocean; the `sun_occlusion`
ray-march remains the fallback beyond 20 km and — bit-identically — whenever
the armed flag is off. The cascade uniform rides group(0) binding 8 while the
depth array + comparison sampler live in their own group(1) — bound only by
the visible pass, because a usage scope counts every bind group set in a
render pass (used or not), so the depth passes must never bind the texture
they render into (and csp-view builds `TerrainUniforms`/`TerrainPacket` with
exhaustive literals, so the cascade data stays in a csp-render-owned buffer
either way — see §7.2).
The receiving bias moves the comparison reference TOWARD the sun
(normal-offset along `+n` plus a subtracted slope bias — map depth is
forward-Z-along-travel, lit ⇔ ref ≤ stored), pinned by the
`bias_moves_reference_toward_the_sun` unit test.

**The armed path (3b) — who computes what, per flight frame:**

1. *View solve* (`csp-view/src/flight.rs`, RenderExtract — §2.5:
   presentation-only, never the sim): inside `extract_flight`'s terrain
   gate, `cascade_uniforms` recovers the chase basis from the solved
   `view_proj` (row 3 is `−z_out` under reverse-Z, the same recovery
   `sky_view_rot` uses) and hands `solve_cascades` the forward/up/
   `FLIGHT_FOV_Y`/viewport aspect/near 0.1 + the staged `FlightEnv` sun.
   The product rides the flight packet (`FlightPacket::shadow`,
   consumed via `View::take_flight_packet` like every other packet
   row). **Arm predicate**: solve only while sun·up at the chase focus
   exceeds `SHADOW_ARM_SUN_UP = 0.06` (≈3.4° elevation — just above
   `sun_occlusion`'s 0.035 full-occlude and the blob's 0.04 disable, so
   no band exists where the cascade replaces a dimmer fallback; below
   it, an ortho sun-fit degenerates into shadows too long for a 20 km
   box and the ray-march's long soft terminator shadows own the look).
   Disarm is the all-zero row, and `draw_shadow` writes it every frame
   (armed or not) so no stale armed matrices survive a sunset or mode
   switch.
2. *App frame loop* (`csp-app` `paint`): when the mode is Flight and
   the packet carries terrain, `FlightRenderer::stage_cascade_cast`
   uploads the flight stream, then `TerrainRenderer::draw_shadow`
   records both cascade passes on the encoder BEFORE the scene pass
   (`draw_shadow` → terrain `draw` → flight `draw` → `draw_ocean` —
   the order 3a's device test pinned). `draw_shadow` first re-syncs the
   frame's uniforms/heightfield (`sync_resources`, shared with `draw`)
   so the depth pass displaces against exactly what the visible pass is
   about to draw. Map and VAB frames never reach the call — their
   scenes stay shadow-free by design.
3. *Casting*: terrain instances (never ocean — water receives only) plus
   **the craft**: `ShadowCore` carries a second pair of depth-only
   pipelines built from the dedicated `flight_shadow.wgsl` module
   (`vs_shadow0/1` over the `FlightVertex` layout, riding their own
   minimal group(0) — the `SU` uniform alone at binding 8; see §7.3
   for why that module is separate), and each cascade layer's pass
   records one additional whole-stream draw after the terrain batch —
   hulls, debris and the launch-complex structures land in the map;
   unlit vertices (sky quads, plumes, dust, contact quads — zero
   normal, placeholder `world`) are degenerated outside the clip
   volume so emissive geometry never casts. The vessel **blob shadow
   stays** this phase (harmless overlap under the sharper craft
   shadow; removal is later polish).
4. *Receiving*: terrain/ocean as above, and `flight.wgsl`'s own
   `sun_visibility` twin (C16-F3 duplication — no WGSL include
   mechanism; cross-referenced in both files) multiplies the craft
   pass's direct-sun diffuse and Blinn specular terms by the same
   cascade lookup (eye-distance = `length(world)`, the same split/
   blend/bias rows), so a ridge line crossing the sun darkens hull and
   ground together. Disarmed or beyond coverage ⇒ full light: the
   pre-cascade craft shading is the fallback. The craft's terrain bias
   rows are the terrain-tuned texels — hull self-shadowing at cascade-0
   texels is peter-panned away rather than acned (the blob covers the
   contact case this phase).

Device coverage: the render gate constructs and draws the full armed
frame (its dawn boot scene arms whenever the spawn-time sun clears the
threshold; the `dump_surface`/`dump_flight` probes render armed
deliberately and their `DUMP_SHADOWS=0` A/B knob isolates the cascade
contribution), and csp-render's own
`armed_cascade_passes_validate_on_device` runs an armed solve + terrain
cast + craft cast + visible pass — asserting zero wgpu validation
errors; skips adapter-less hosts unless `CSP_SHADOW_GATE_REQUIRED=1`.

**The 3b "high-sun artifact" — RESOLVED (mirrored cascade read).**
During arming, the pad probe reported 79% of ground falsely shadowed
at 76° sun. Root cause: `cascade_one` computed
`uv = cd.xy·0.5 + 0.5`, but WebGPU rasterization writes NDC y=+1 at
texture row 0 — the read must MIRROR y (`0.5 − cd.y·0.5`, applied to
both terrain.wgsl's and flight.wgsl's `cascade_one`). The unmirrored
form sampled the vertically-flipped light column, comparing each
receiver against up-to-km-scale phantom sunward depth — the error
grew with the receiver's NDC-y spread, which is why near-zenith sun
(high light-space y leverage) was worst and mid elevations looked
plausible while actually under-shadowing. Post-fix evidence: the pad
probe is bit-identical armed vs disarmed (0/76800 ground px — correct:
a CPU sun-ray march finds no occluders there), a whole-field physics
survey (1 007 directions, 91-point site grids) confirms the authored
relief under the probe's fixed sun casts only isolated small shadows
(no multi-texel patches), and the device readback test pins that both
casts land. The out-of-volume guard (outside the ortho box ⇒ lit,
never edge-clamped) stays as correct CSM hygiene.

**Ridge-facet contrast levers (facet-tuning pass — the "soft blotches"
weakness, settled by a render-and-look loop).** Three levers in
`terrain.wgsl`, judged at a side-lit comparison vantage (`dump_surface`
at lat 51, lon 124, heading 210, alt 710, pitch 0.25 — the pinned sun
sits ~21° up and the camera-vs-sun azimuth dot is 0.04, so sun-facing
and leeward facets interleave in one frame; HEAD baseline vs candidate
A (MACRO_TILT 3.0 on the same gradient taps, a 1→0.45 day-ambient ramp
over 8–18 km, a flat 0.55 inscatter scale) vs candidate B (the shipped
structure), B's evidence and A's spec each untrusted until re-rendered
here). The settled values: `MACRO_GAIN = 2.5` (`terrain.wgsl:479`)
multiplies the heightfield gradient scale inside `terrain_normal_fix`
(`:480` — the same four bilinear taps feed
`k = u.planet.w/denom * MACRO_GAIN`, `:487`; a pure slope scale, no new
octaves, so the STABILITY lattice ceiling holds) — at gain 1 the ~4.2 km
gradient baseline flattens the Mountain spectrum's true facet tilts and
whole ranges read as soft blotches. `AMB_DAY = 0.75` (`terrain.wgsl:918`)
scales the dayness=1 endpoint of the hemisphere-ambient mix (`:1161`)
only — the sky fill otherwise reaches sun-facing and leeward facets
alike and floors exactly where the banding must read; the night floor
and the airless override are untouched. `SHADE_K = 0.70`
(`terrain.wgsl:933`) gates the terrain pass's aerial inscatter (`:1177`)
by the struck facet's direct-lit fraction (`lit_frac`: ocean `:1034`,
terrain macro normal `:1148`), so leeward haze stops lifting shadow
bands toward horizon-sky brightness while lit facets and the sky pass
keep full airlight. A was rejected on the rendered evidence, not on
paper: its global far-field dimming strips aerial perspective — in the
alt 3.5 km sweep distant ranges read murky, near-navy, with the
amplified gradient's crest speckle standing proud of the lost haze —
while B's lit-fraction gate kept depth haze and still gained the
banding. Regression fences verified by eye at the settled values: the
pad probe frame (`dump_surface` defaults) and flight flagship
(`dump_flight` defaults — plume, craft, beach character and far headland
intact, no new banding) re-rendered unchanged in character against HEAD,
and a front-lit boot-range frame (lat −30, lon 116, heading 45, alt 150)
pools shadows deeper without crushed blacks or flattened fill;
`cargo test -p csp-render`'s naga gate (`shaders_parse_and_validate`) is
green. The coupling the previous design's docs missed: the cascade
receive bias's `tan_slope` (`terrain.wgsl:658`, consumed as the
`(1 + tan_slope)` envelope in `cascade_one`'s reference-depth and
normal-offset terms, `:615`/`:632`) is derived from the same macro
normal, so MACRO_GAIN amplifies the shading gradient and the
receive-bias factors together — empirically clean on the pad contracts,
where the pad probe frame re-rendered identical to HEAD in character
(same pale spalled ground, same exposure, same soft near field: no
darkening, no flattening, no new banding), the expected flat-ground case
since the pad's amplified gradient is ~0. Honest residual, unchanged by
this pass: the foreground's soft cast-shadow blobs are
heightfield-geometry-driven (cascade/`sun_occlusion` occluders, not
shading) and present identically at HEAD; these levers reshape the
mid/far-field sun/shade banding, not the blob pattern.

### 7.5 Sky, atmosphere, and the star field (current state)

`fs_sky` (`terrain.wgsl:1322`): fullscreen triangle; per-pixel scattering ray
in f32 with the shell-quadratic constants pre-baked in CPU f64
(`uniforms.misc.z/w` — computing |pa|²−r² in f32 costs ~±300 m of limb
wobble); 32-step jittered march with per-step jitter (partial cancellation,
no banding); Rayleigh + Mie (aerosols hug the low shell) through the LUT;
ground strikes swap the backdrop for haze; ACES tone-map.

The star field (`star_cell`/`star_layer`/`star_field`, `terrain.wgsl:1228`/
`:1259`/`:1287`) is fully procedural — crisp point stars hashed out of a 3D
direction-space lattice (~750 bright + ~700 faint incl. a Milky-Way band),
each projected onto the direction sphere with a Gaussian core + faint halo,
and a per-pixel ray-footprint floor (`sig_ang`) so **no star ever goes
sub-pixel** — dense sub-pixel points read as film grain, which was the
"grainy static sky" complaint.

Daylight star gate (`terrain.wgsl:1432`): stars survive only where the
shell scatters essentially nothing into the ray —
`star_vis = 1 − smoothstep(6e-4, 4e-3, ins_lum)`. The previous
`exp(−ins_lum·160)` never reached zero, so bright stars speckled the
day sky and arrived "way too early" at altitude; the hard smoothstep
guarantees zero stars on every blue sky (day zenith integrates ~0.03–0.10)
while the night side (ins→0) and the black sky above ~18–20 km keep them.
This landed in `ff897d4`.

### 7.6 Part meshes, art, structures

- `part_mesh.rs` (`csp-view`): the M4c kitbash classifier — a resolved
  `Box` stays a box; a `Cylinder`/`Convex` collider **lathes its own
  silhouette** (24 segments); kind overrides reshape engines (mount + bell)
  and crewed pods (capsule taper). `classify_effective` (VAB) /
  `classify_weld_part` (flight), `emit`/`emit_smooth`/`emit_model`, and
  `inflated` (selection shells, the plasma sheath basis). Pure functions of
  resolved part data — deterministic, no RNG.
- `art.rs`: real `.csar` packs (baked from CC0 sources by
  `xtask bake_art`; format documented in the module doc) embedded with
  `include_bytes!`, decoded once into `ArtAssets`; any decode failure
  degrades to the procedural kitbash. Part models emit through
  `part_mesh::emit_model`. Terrain is a cfg-selected pair of packs:
  native embeds `terrain.csar` at **1024²** (the ambientCG sources at
  full 1K resolution — identity decode, no resampler in the bake), the
  §6.3 wasm demo embeds `terrain_web.csar` at **512²** ("reduced
  textures", Lanczos downscale); the two statics are `#[cfg]`-gated so
  each binary carries exactly one. Every consumer downstream is
  dims-driven from the CSAR header — upload sizing (`write_art_tex`),
  `class_means` normalization, and `terrain.wgsl`'s world-space tile
  scales — nothing hardcodes the texel size. GPU cost: 1024² RGBA8 ×
  (4 albedo + 4 normal + 1 water) layers = 36 MiB at level 0, ≈48 MiB
  with full mip chains (native), vs 9 / 12 MiB for the 512² web pack.
- `structures.rs`: the launch site as real geometry — pad with flame trench,
  gantry, VAB building, crawlerway, propellant farm — anchored to the
  **rendered** terrain height (re-sampled per footprint), emitted into the
  flight view's lit stream (`emit_launch_complex`, `:893`). `SiteTable`
  (`:70`) is a data-derived constant from `sites.ron`.

### 7.7 Presentation floor (M4c) inventory — with anchors

PLAN §3.7's M4c exit rows and where each lives:

| Floor item | Implementation |
|---|---|
| Silhouette-correct kitbashed parts for the stock craft | `part_mesh.rs` classifier + `flight::emit_model_part` (`flight.rs:2607`); real models when art decodes |
| Engine plume VFX | `flight::emit_plumes` (`flight.rs:2423`), delivered-thrust keyed |
| Reentry plasma | `flight::emit_sheath` (`flight.rs:2520`), convective-flux driven |
| Ground-contact dust | flight.rs:2870-2940 ring, §2.9 geometric predicate wired |
| Basic SFX | M4c shipped the **cue channel** only: `FlightSfx`/`sfx_edges` (`flight.rs:575`/`:1887`) edge-detect ignition/cut, staging, breakup, contact, latch/liftoff, vessel-left — HUD flashes via `notice_overlay`. PLAN §3.7's as-shipped note documents the deferral to M10. |
| Audio (M10 exit row, the deferred half) | `csp-app/src/{audio,synth}.rs` — real backend: pure-function synth bank (`synth.rs`, fixed-seed xorshift — zero asset files, zero license rows) + a PCM mixer streaming to a **subprocess** player (`pw-play`/`paplay`/`aplay`, low-latency flags, bounded pipe lead); graceful silence when nothing can be spawned. Consumes `FlightSfx` + `FlightSnapshot` read-only after `end_frame` (`csp-app/src/lib.rs:1018-1027`). |

Note: PLAN §3.7's as-shipped note says "no audio stack exists in the tree" —
that was true at M4c and is now stale: the M10 backend above exists. The
note's *history* is accurate; the tree has moved past it.

Also on the floor: the navball instrument (`navball.rs`), the flight HUD
telemetry block, and the map view (M1) predate M4c.

### 7.8 Web demo tier (§6.3)

`View::new_demo` (`csp-view/src/lib.rs:1926`) + `demo.rs`: reduced `data/`
set embedded via `include_bytes!` (`apps/demo/data/`, diffed against disk by
test), Terrin-only scenario, `DEMO_PART_LIMIT = 60`, craft-file import/export
refused, fixed save seed (`DEMO_SAVE_SEED`). wasm's `View::new` **is**
`new_demo`; native tests exercise the same construction. Demo/self-seeded
starter craft: `demo::seed_pad_craft` — also the shipping build's first-run
seed via `View::seed_starter_craft` ("Stock Hopper", `lib.rs:1954`).

---

## 8. Rendering tiers vs PLAN §5 — status

**PLAN §5.1 requires** `csp-render` to expose `RenderTier { Baseline, Advanced }`
chosen at init from queried features/limits. **Code**: no `RenderTier` type
exists anywhere in the tree; `Gpu::new` always requests
`Features::empty()` (`csp-render/src/lib.rs:227`), so every build is Baseline.
`GraphicsSettings::quality` (`csp-ui/src/settings.rs:87`) is
persisted-but-consumed-by-nothing — its own doc says "no render tier exists
to select (P14-F1)" and the panel deliberately shows no control for it.
This is **consistent with the §6.4 cut list** (cut #1: "R3 Advanced render
tier → post-v1" — bindless+indirect, clustered Forward+, TAA are all absent,
confirmed by `rg RenderTier|bindless|Forward+|TAA` over `csp-render`), but the
§5.1 *tier enum rule itself* is unrealized: the plan's mechanism is "two
tiers behind one selector"; the tree is "one tier, no selector".

Other §5 rows, plan vs code:

- **§5.1 "shaders authored once with preprocessor branches for the binding
  model (WESL/naga_oil); both binding models compile in CI"** — not
  implemented: WGSL is single-model, gated only by the naga
  parse+validate unit test (`csp-render/src/lib.rs:476`). Moot while only
  Baseline exists, but it means the CI-runnable half of the render gate is
  weaker than specified.
- **§5.1 correctness harness ("zero silhouette differences, ≤2% ΔE2000>3,
  measured by the screenshot harness §8.5")** — a headless render gate
  exists (`csp-view/tests/render_gate.rs`: boot-scene-has-real-geometry,
  washed-out/empty-scene rejection cases, `--ignored` PPM dumps for
  sky/low/flight/surface; `render_dump_extra.rs`), but there is no two-tier
  comparison and no ΔE2000 machinery — only one tier exists.
- **§5.2 phases**: R0 (forward, reverse-Z, analytic sky, camera-relative) ✓;
  R1 (depth validation at 1e7 m, plasma, editor + GPU picking) ✓, and its
  last open term — **shadow cascades — is closed** (terrain-overhaul 3a
  landed the core, 3b armed it: §7.4's full path — the view's per-frame
  `solve_cascades` riding the flight packet, the app's `draw_shadow`
  before the scene pass, terrain + craft casting, terrain/ocean/craft
  receiving; the `sun_occlusion` ray-march stays the far-field and
  disarm fallback, and the vessel blob shadow remains pending removal
  as later polish);
  R2 (quadtree, LUT scattering, ocean, planet-scale LOD) ✓; R3 (Advanced) —
  cut to post-v1 per §6.4; R4 — absent, as planned.
- **§5.1 Baseline post row "FXAA/none"** — code ships 4× MSAA instead
  (`MSAA_SAMPLES`, C11-F1); a superset of "none", not a divergence in kind.
- **§5.4 VAB kill criterion (16 ms at 300 parts on REF-A)** — measured:
  `csp-view/tests/vab_latency.rs` times one op + recompute + full
  `end_frame` at a 300-part craft; 16 ms is the release number, the debug
  assertion is a 200 ms regression alarm, and the printed breakdown is the
  perf-log record.

---

## 9. Known UI issues — current status

The four review-seed complaints had uncommitted work at seed time; that work
landed in commit `ff897d4` and is the tree's behavior — **but** the
session-full-20261026 verified review (reports C17/C23, empirically confirmed)
found live defects in the same tree, listed in the ledger below.

1. **Fixed-position panels / can't drag.** PARTIALLY FIXED. The Areas are
   movable — the telemetry readout block and the navball carry
   `.movable(true)` (`csp-ui/src/lib.rs:753-756` and `:1012-1020`); the
   Actions strip, action bar, and Game menu strips are `title_bar(false)`
   windows (drag-anywhere, e.g. `game.rs:71-75`, `vab.rs:2476-2481`); the
   Program window drags by its title bar. Still fixed: the top bar and the
   notice overlay (centered, non-interactable — deliberate: cues are
   transient). Two verified residual defects (C23, empirically reproduced):
   (a) the **readout block cannot be dragged by its own text** — the row
   labels (`lib.rs:786-810`) are default-selectable `ui.label`s that win the
   hit-test over the Area's move widget, and the panel is a wall of labels;
   (b) the **navball's first-open parking is ~242 px too high** (clamped
   against egui's nominal area size — no `.default_size`), overlapping the
   readout block until dragged.
2. **Grainy sky / stars too early.** FIXED: the star field is sparse
   resolved points with a resolution-aware sub-pixel floor, and the daylight
   gate is the hard `smoothstep(6e-4, 4e-3, ins_lum)` (`terrain.wgsl:1287`
   `star_field`, `:1432` the gate) replacing the never-zero
   `exp(−ins_lum·160)`. The old faint-layer grain root cause is documented
   in the shader comments ("the old 11–23%-occupancy faint layer was the
   'grainy static' sky").
3. **Yellow notices unreadable / flight-data contrast.** FIXED: every
   floating HUD strip wears the shared dark chip — `HUD_CHIP_FILL` (9,13,19,
   210) + hairline edge (`csp-ui/src/lib.rs:62-91` `hud_frame`/
   `paint_hud_chip`), notices render `NOTICE_AMBER` (255,202,84) on it with
   single-line `TextWrapMode::Extend` (`lib.rs:702-729`), and the telemetry
   block is chip-backed (`lib.rs:758-761`) — **one exception**: the
   painter-drawn `heading_tape` fills `Color32::from_black_alpha(160)` with
   a gray-120 stroke instead (`navball.rs:331-341`). Verified by the
   `csp-ui/tests/hud_dump.rs` scratch rasterizer (renders the real draws
   over a bright worst-case gradient and reports contrast; run with
   `cargo test -p csp-ui --test hud_dump -- --ignored --nocapture`).
4. **Research/science UI confusion.** REWORKED: science is reachable where
   the player looks — the always-visible Actions strip and the right-click
   part menu issue the same `RunExperiment`/transmit/recover/deploy verbs
   as the Program window (`program.rs:2542`/`:2432`); pooled rows state
   transmit-fraction-now vs recover-full-later; the LABS section explains
   the escrow/accrual model in-row (`program.rs:2082-2140`: accrued ⚛ /
   data left / ETA / "bank the accrued science and release the rest");
   blocked experiments print reasons instead of dead-clicking; and the
   program status strip keeps funds/science visible in every mode.

### Known defects (verified session-full-20261026 — reports C17/C23)

Confirmed live in this tree; §4.1/§4.3's gesture descriptions apply *modulo*
these:

- **VAB pick-0 decoration breaks click gestures** (C17-F1, HIGH): the pick
  stream rasterizes pick-0-tagged selection shells/ghosts/markers over the
  click target in a `Greater`-wins pass — click-a-part-to-place decodes as
  background and click-selected-to-lift deselects (~96-100% miss).
- **Click-armed part + viewport drag is a dead gesture** (C17-F2): with a
  palette-armed part, an LMB viewport drag produces no orbit, no drop, no
  pick.
- **`notice_overlay` is not in the pointer mask** (C17-F7): a right-click on
  a cue chip resolves as a viewport gesture and can delete the part behind
  it.
- **Readout text-drag + navball mis-park** (C23) — see item 1 above.
- **Tutorial rewinds on its own coached recovery path** (C23-F3): the
  unscoped `vessel lost` check resets the checklist when you recover the
  vessel (GAMEPLAY-GUIDE §5 item 7).

Related fixed complaint — **warp refusing silently**: every refusal path now
speaks (§2.3 above: cap/ladder-top, per-press `rails_warp_refusal`, and
budget-limited previews, `csp-app/src/lib.rs:1398-1447`).

---

## 10. Testing the presentation stack

- `csp-view/tests/render_gate.rs` (1062) — headless wgpu device; boot-scene
  real-geometry gate, washed-out/empty rejection cases, `--ignored` PPM dumps.
- `csp-view/tests/render_dump_extra.rs` (449) — additional dump scenes.
- `csp-view/tests/vab_latency.rs` (165) — the §5.4 kill criterion.
- `csp-ui/tests/hud_dump.rs` (393) — HUD contrast rasterizer.
- `csp-view/tests/{flight_focus,flight_plume,part_silhouettes,pad_spawn,stage_plan,startup_colonies,demo,tutorial}.rs`
  — per-surface behavior tests.
- `csp-render/src/lib.rs::tests::shaders_parse_and_validate` — naga gate on
  every WGSL file.
- Inline tests: `csp-view/src/lib.rs` (L5152+), `csp-ui/src/vab.rs`
  (L3741+), `csp-ui/src/program.rs` (L2682+), `csp-app/src/lib.rs` (L3314+).

## 11. Working rules (read before editing this stack)

- Never read `view.world` to build UI — input through `set_*`/`*_event`
  plumbing, output through `take_*` (the `View` doc, `lib.rs:1524-1530`).
- New `SimCommand` variants must ship with a production issuer in
  csp-ui/csp-view/csp-app or an explicit `// ui-unreachable:` opt-out at the
  declaration — `cargo xtask lint-reach` enforces it.
- A click either does something or says why not (§3.7 dead-click rule):
  dropped panel commands post cues (`lib.rs:773-791`), refused launches
  keep you in the VAB with the reason, disabled buttons carry hover reasons.
- egui consumption gates **presses**, never releases; VAB pointer passes run
  last against the complete UI mask set.
- Anything presentation-side is session state (§2.2-C): never logged, never
  hashed, never serialized into saves.
