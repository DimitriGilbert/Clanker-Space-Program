# Doc verification — docs/UI-AND-RENDERING.md (session-full-20261026)

Target: `/home/didi/workspace/Clanker-Space-Program/docs/UI-AND-RENDERING.md`
vs the ACTUAL working tree. Method: every load-bearing claim opened in source
and confirmed; cross-checked against this session's verified findings
(`.review/session-full-20261026/verified-report-C17.md`, `-C23.md`).

## Summary

- Claim groups checked: **35** — **29 VERIFIED**, **3 MINOR-INACCURATE**,
  **3 INACCURATE**, **0 UNSUPPORTED**.
- Anchor fidelity is exceptional for every crate/file NOT touched by HEAD
  (`csp-ui`, `csp-render`, `csp-view/map.rs`, `csp-app`) — dozens of
  `file:line` anchors land on the exact symbol.
- **Doc pins itself to commit `ff897d4`, but HEAD is `7dd4b86`**
  ("parachutes deploy via Space staging"), which touched
  `csp-view/src/{lib,flight}.rs`. This produces one substantive staleness
  (§4.4) plus ~20 lines of anchor drift in those two files. Every other
  anchor is exact or within 1-5 lines.
- Worst problems are concentrated in **§9 "Known UI issues — current
  status"**: it asserts FIXED where this session's verified, empirically
  reproduced findings say PARTIALLY FIXED, and it omits the confirmed HIGH
  VAB pick-0 defect that breaks the very gestures §4.1/§4.3 describe as
  working.

**Verdict: PASS-WITH-FIXES** — the reference body (§1-§8, §10-§11) is
verified accurate and a new dev could genuinely work from it; §9's status
claims and §4.4's staging description must be corrected before the doc can
be trusted as a statement of current behavior.

## Claims table

| # | Doc claim (section) | Verdict | Evidence |
|---|---|---|---|
| 1 | Six named schedules + exact membership, `csp-view/src/lib.rs:2066-2092` (§1.1) | VERIFIED | `input`/`sim_step`/`sync`/`presentation`/`render_extract`/`ui_build` built at exactly 2066-2092; system lists match verbatim (incl. `tutorial_drive` in Presentation, `map_pointer_input.after(build_ui)`). |
| 2 | Frame order: `run_frame` `csp-app/src/lib.rs:864`, `render` `:841`, outbox drained first, one `sim_step` call | VERIFIED | `fn render` 841, `fn run_frame` 864; body matches (take_ui_outbox → handle_ui_outbox → warp → begin/sim/end_frame). |
| 3 | Sim reads only in two staging phases in `end_frame` (`lib.rs:2221`); `LatchedWeldCache` reuse (`flight.rs:824`) | VERIFIED (anchor drift) | `end_frame` at 2221 exact; `LatchedWeldCache` now `flight.rs:844`, reused by `capture_sync_input` (doc + code confirm). |
| 4 | Mirror inventory table (§1.2): resources, writers, cadence, `UiOutbox` request list | VERIFIED | `Clock` 132 / `FrameStats` 138 / `WarpMirror` 150, `ProgramMirror` 232, `ScriptLogMirror` 277 (64-line cap confirmed in its doc), `SessionMirror` 286 (+ `set_gpu_info`/`update_session`/`set_live_keymap`/`set_key_label` at 2295-2352), `EguiPass` 327, `UiOutbox` 351 with exactly the documented request fields. |
| 5 | `ViewMode` derived not stored; `set_mode` choreography; 6e10 m refocus history (§1.3) | VERIFIED | `mode` 2522, `set_mode` 2540 with the described VAB-closes-map + `MapInput::refocus` (P09-F1 comment names the 6e10 m bug verbatim). Enum at `lib.rs:85` (doc says 84 — off by one). |
| 6 | Launch transition: preflight gates (vessel cap, funds, site stockpile, pad occupancy, parked-build recovered), revert snapshot, `ManufactureCraft`/`SpawnPhysicalVessel`, `set_mode(Flight)` | VERIFIED (anchor drift) | `launch_craft` 2765; `launch_preflight` now 3441 (doc 3420) with all five gates exactly as described incl. the parked-build recovery branch; `pre_launch = Some(snapshot)` at 2995; `launch_refused` now 3515. |
| 7 | Load/revert/new-game via `rebuild_from_snapshot` (`lib.rs:1574`) resets mirrors, forces Flight | VERIFIED | `rebuild_from_snapshot` at 1574; StagePlans/MapPlanning re-inserted (1596). |
| 8 | §2.1 winit dispatch order: rebind capture first (2564), focus-lost, key labels, egui-first with per-handler `consumed`, text-edit rule (`lib.rs:2862-2868`), releases always dispatch, `map_dragging = !consumed && pressed`, wheel `exp(−lines)` | VERIFIED | All anchors exact; the `typing` gate (`text_edit_focused` && `Key::Character`) and unconditional release dispatch are at exactly the cited lines. |
| 9 | `KeyAction` is "25 actions incl. QuickSave/QuickLoad/RevertToLaunch" (`settings.rs:190`) | **MINOR-INACCURATE** | Enum at 190 exact, but it has exactly **24** variants (ToggleMap…RevertToLaunch, enum ends line 247). Count is wrong, membership is right. |
| 10 | `Keymap` one-action-one-key with eviction (`settings.rs:540`); arm → intercept → `bind_key` → Apply goes live + persists | VERIFIED | `pub struct Keymap` at 540; flow as described (settings.rs:777 region confirms no `quality` row / zero-consumers comment, used for row 27). |
| 11 | Warp controller (§2.3): ladder `[1,2,10,100,1e3,1e4,1e5]` (`warp.rs:33`), `BUDGET_FRACTION=0.5`, `PROMOTE_FRAMES=30`, preview helpers `warp.rs:367-393`, per-press `rails_warp_refusal`, `rebase` at `lib.rs:983`, per-tick cost at `:1007` | VERIFIED | Every constant and anchor exact (`WARP_LADDER` warp.rs:33, 0.5 at :37, 30 at :46; `multiplier_at` 367 / `request_preview` 376 / `capped_below_ladder_top` 383 / `promotable_ceiling` 393; refusal surfaced at csp-app 1430 inside `warp_step_request` 1398-1447). |
| 12 | Panel inventory completeness — right-click part menu + Actions strip + FACILITIES + AUTOMATION sections covered (§3.2/§3.6) | VERIFIED | Landmine check passed: `part_menu` 2432 + `PartMenuState` 2228, `flight_action_bar` 2542, `sect(ui,"AUTOMATION")` 1833, `sect(ui,"FACILITIES")` 2146 + `facilities_section` 1477 — all anchors exact; §3.6 section list and ORDER match the code (OVERVIEW 1806 … LOG 2186). |
| 13 | `ProgramCommand` "30+ variants" (`program.rs:556`) | VERIFIED | 32 variants; anchor exact. |
| 14 | `ProgramMirror` staged every end_frame by `stage_program` (`lib.rs:3554`); `science_ctx` stages on situation (W1.2, `lib.rs:3640-3648`) | VERIFIED (anchor drift) | `stage_program` now 3575 (doc 3554), `stage_vab_tech` now 3548 (doc 3527) — called from `end_frame` at 2253/2256; W1.2 situation-staging comment now at 3661-3668, content verbatim. |
| 15 | `drain_part_notices` posts one toast per new `PartFailed`/`IgnitionMisfire` naming the focused vessel (`lib.rs:3314`) | VERIFIED (anchor drift) | Now at 3335; `notice_cursor` advance + focused-vessel naming confirmed. |
| 16 | VAB session model (§4.1): `VabSession` (`vab.rs:375`), `VabAction` vocabulary (`:154-276`), held-part model, undo/redo bounds, `to_bytes` (`:2082`) | VERIFIED | All anchors exact; the doc's `VabAction` enumeration matches the code's 28 variants one-for-one. |
| 17 | Attach rules: `plan_attach`/`plan_move` (`:1493`/`:1526`), `AttachMode` (`:70`), symmetry, bottom attach via full ±π pitch clamp (`csp-view/src/vab.rs:47`) | VERIFIED | `PITCH_LIMIT = π` at exactly vab.rs:47 with the pole-to-pole doc; `hover_pick` 660 / `slab_hit` 702 / `emit_environment` 843 exact. |
| 18 | GPU picking: `pick_ids` + `craft_epoch`, epoch-guarded readback dropped-as-background (`csp-app/src/lib.rs:874-895`), one pick in flight, P10-F1/F2/F3 fallback | VERIFIED as described | The drain at 874-895 is exactly as documented — but see row 33: the doc never discloses that pick-0 decoration in the same stream breaks the decoded gestures. |
| 19 | §4.3 pointer flow: ui_masks from every floating window, `vab_pointer_input` last, `orbit_block` anchored at press, drag-lift once per press, drag-to-bin, hotkeys gated on `text_edit_focused` | VERIFIED | `vab_pointer_input` at vab.rs:2304 exact; behavior matches (C17's verified trace of the same state machine concurs). |
| 20 | §4.4: `fire_stage` "pops the focused vessel's next drop set and issues one logged `JettisonPart` per node" | **INACCURATE (stale vs HEAD)** | At HEAD `7dd4b86`, `fire_stage` (`lib.rs:3189`) issues **a `DeployPart` per staged deployable BEFORE the `JettisonPart`s** (staged-chute rule; `flight.rs:597-629`). True at the doc's pinned `ff897d4`, wrong for the actual working tree. The doc's own header insists it describes "the tree, not any older HEAD". |
| 21 | §5 flight view: `focus_vessel` = `last()` newest spawn, `FlightFocusOverride` pin, FOV 0.9 rad, `PICK_NEAR_PX` 48, plasma 5e3…8e4 W/m², plume keyed to delivered thrust, dust ring | VERIFIED (anchor drift) | All facts confirmed (`FLIGHT_FOV_Y=0.9` flight.rs:39, `PICK_NEAR_PX=48.0` :1640, plasma constants :46-48, focus rule verbatim); flight.rs anchors drifted ~+20 lines (StagePlan 644, StagePlans 758, capture_sync_input 950, solve_chase_camera 1524, pick_part 1653, flight_present 1730, focus_vessel 1965, emit_plumes 2446, emit_sheath 2543, dust ~2893-2958). |
| 22 | §6 map: all anchors and constants | VERIFIED | map.rs untouched by HEAD: `map_present` 1279, `extract_map` 1992, `present_tick` logic 1313-1320, 128 orbit samples (:51), 700 stars (:71) at 1e28 m (:75), encounter colors 112-119, `AlarmEntry` 800, `MapPlanning` 820, `PlannedTransfer` 836, `note_fired` 859, cap 32 (:128), `porkchop_query` 985, `resolve_planning_command` 1041, `MapSnapshot` 499, `refocus` 407, `default_distance` 428, `map_pointer_input` csp-view:1344 — all exact. |
| 23 | §7.1-7.3 renderer: `Gpu` (:176), wgpu 30, `Features::empty()` (:227), MSAA 4 (`lib.rs:60-130`), `begin_frame` (:345), clear 0.02/0.02/0.05, draw order map→vab→terrain→craft→ocean, egui pass `LoadOp::Load` no depth (`csp-app/src/lib.rs:1779-1806`); WGSL table (48/107/104/1108 lines, entry points, terrain anchors 334/641/870/991) | VERIFIED | Every item exact: wgpu 30.0.1 (Cargo.lock), `MSAA_SAMPLES=4` at :65, clear color at csp-app:1718-1720, ocean-after-craft with the C11-F3 comment, egui-pass no-depth rationale comment verbatim; `wc -l` on all four WGSL files matches to the line; `fs_pick` at vab.wgsl:105. |
| 24 | §7.4-7.6 terrain quadtree/LUT/ocean/biome, part_mesh, art, structures | VERIFIED | `PatchId` 72, `select_patches` 158, `patch_instance` 449, `bake_transmittance` 568 (csp-render); `TerrainCache` 195, `patch_might_hold_sea` 346, `vessel_shadow` 496, `build_packet` 548 (csp-view); `inflated` 584, `classify_effective` 175, `classify_weld_part` 197; `SiteTable` 70, `emit_launch_complex` 781; `xtask bake_art` + `include_bytes!` .csar packs confirmed. |
| 25 | §7.5 sky/star field: `star_cell`/`star_layer`/`star_field` 907/938/966, `fs_sky` 991, daylight gate `smoothstep(6e-4, 4e-3, ins_lum)` replacing `exp(−ins_lum·160)` | VERIFIED | All anchors exact; the gate is at terrain.wgsl:1104 inside the 1090-1105 comment block that documents the old never-zero exponential and the grain root cause. |
| 26 | §7.7 M4c floor inventory + M10 audio backend | VERIFIED | `FlightSfx`/`sfx_edges`, synth subprocess player (`pw-play`/`paplay`/`aplay`) in `csp-app/src/{audio,synth}.rs`, consumed read-only post-end_frame — as documented. |
| 27 | §8: no `RenderTier` type anywhere; `GraphicsSettings::quality` persisted-but-consumed-by-nothing; consistent with §6.4 cut #1 | VERIFIED | `rg "RenderTier\|bindless\|Forward+\|TAA"` over csp-render: zero matches; `Gpu::new` requests `Features::empty()` at lib.rs:227; settings.rs:88-94 doc says verbatim "consumed by NOTHING today: no render tier exists to select (P14-F1)" and :777 confirms no panel row. |
| 28 | §8 other rows: MSAA superset of "FXAA/none", shadow cascades absent, vab_latency = §5.4 probe with 16 ms release / 200 ms debug | VERIFIED | No shadow maps in tree; `vab_latency.rs:159` `budget_ms = if cfg!(debug_assertions) { 200.0 } else { 16.0 }` (matches C17-F5's verification, including that CI never runs the release assert — the doc does not claim CI enforces it). |
| 29 | §9.1: fixed-position panels "FIXED for the main flight overlays: the telemetry readout block and the navball are movable egui Areas" | **INACCURATE (overstates)** | The Areas are `.movable(true)` at exactly `csp-ui/src/lib.rs:753-756` / `:1012-1020`, but this session's verified C23 findings (both empirically reproduced, egui 0.37 source + headless harness) show: (a) the readout block **cannot be dragged by its own text** — default-selectable labels win the press over the Area's move widget, and the panel is a wall of labels; (b) the navball's first-open **parks 242 px too high** (clamped against egui's nominal 600×400 area size, `[360,200]` instead of `[412,442]` on 960×600) and overlaps the readout block. Status is PARTIALLY FIXED, not FIXED. |
| 30 | §9.2: grainy sky / stars-too-early FIXED via resolved star field + hard smoothstep gate | VERIFIED | Row 25 evidence; §9.2's anchors (`:966`, `:1104`) exact. |
| 31 | §9.3: "every floating HUD strip wears the shared dark chip" (`HUD_CHIP_FILL` 9,13,19,210) | **MINOR-INACCURATE** | Chip constants exact (`lib.rs:62-66`) and notices/hud verified — but `heading_tape` (`navball.rs:332-341`) fills `Color32::from_black_alpha(160)` + gray-120 stroke: the one painter-drawn strip off the rule (verified C23-F5). "Every" is false. |
| 32 | §9.4: science/recovery reworked — Actions strip + part menu verbs, LABS escrow copy, blocked-with-reason, status strip in every mode | VERIFIED | "⚛ run science (N)" at program.rs:2592, "bank the accrued science and release the rest" at :2133, blocked reasons, `program_status` at :852 with funds/science/contracts. |
| 33 | §9 header: "that work landed in ff897d4 and is now the tree's behavior" — i.e. the known-issues section reflects current status | **INACCURATE (by omission)** | §9 lists only the four review-seed complaints as fixed/closed. It does not reflect this session's CONFIRMED, verified UI defects in the same tree: the VAB pick-0 decoration in the pick stream (C17-F1, HIGH — the selection shell/ghost tagged pick 0 rasterize over the click target in a `Greater`-wins pass, killing "click a craft part to place it" ~96-100% of the time and click-selected-to-lift; source-verified here: csp-render/src/vab.rs:87-89 doc + csp-view/src/vab.rs:793-823 emit), the click-armed-part+viewport-drag dead gesture (C17-F2), undo/redo resetting camera zoom/pan via `craft_epoch` (C17-F3), `notice_overlay` missing from the pointer mask (C17-F7), tutorial rewind on its own coached recovery (C23-F3), and the two C23 movable-panel defects in row 29. A reader of §9 + §4.1/§4.3 would believe click-to-place works; it does not. |
| 34 | §10 test inventory: render_gate (1062), render_dump_extra (449), vab_latency (165), hud_dump (393), the named per-surface tests, `shaders_parse_and_validate`, inline-test anchors | VERIFIED (anchor drift) | `wc -l` matches all four counts exactly; all named files exist (flight_focus, flight_plume, part_silhouettes, pad_spawn, stage_plan, startup_colonies, demo, tutorial); naga gate at csp-render lib.rs:476; inline `mod tests` at vab.rs:3741 / program.rs:2682 / csp-app:3314 (doc ±4), csp-view lib.rs now 5152 (doc 5065 — drift from 7dd4b86). |
| 35 | Anchor precision across csp-view files touched by HEAD | **MINOR-INACCURATE** | ~20 anchors in `csp-view/src/lib.rs` and `flight.rs` drifted by +3 to +87 lines (rows 3, 6, 14, 15, 20, 21, 34). Substance unchanged except row 20. All other crates' anchors are exact to the line. |

## Missing topics (completeness gaps)

A new dev could work on this stack from §1-§8 + §10-§11 alone — the gaps
below are what they would still lack (worst first):

1. **Staged deployables in `fire_stage`** — the `DeployPart`-before-
   `JettisonPart` parachute rule that is the tree's current staging
   behavior (7dd4b86); §4.4 describes only the jettison half.
2. **A known-defect ledger** — §9 presents the complaint list as closed;
   the C17/C23 verified defects (pick-0, dead drag gesture, undo camera
   reset, readout drag, navball park, notice unmask, tutorial rewind)
   are nowhere in the doc.
3. **`ui_scale` application mechanics** — the settings panel exposes it,
   but how the factor reaches the egui context (zoom / pixels-per-point)
   is undocumented.
4. **egui font/texture/tessellation setup** in `csp-app`'s paint path —
   the frame contract says "tessellate FullOutput" and stops there.
5. **The part menu's DATA ABOARD / VESSEL sub-sections**
   (`program.rs:2503`/`:2506`) — §3.2's part-menu row only gestures at
   verbs.
6. **The VAB gesture-state field inventory** (`press_node`, `move_node`,
   `drag_part`, `orbit_block`, `press_viewport`, …) — §4.3 documents the
   behavior but not the ten fields that implement it, which is what an
   editor of that code actually needs.
7. **Map interaction tuning constants** — `PICK_RADIUS_PX = 14` and
   friends are named but never valued.
8. **Unlit contact-shadow quads** for structures/parts
   (`flight.rs:2714`) — mentioned only in passing under §8's shadow row.
9. **Window resize / DPI / surface-size handling** — device-lost
   reconfigure is documented; ordinary resize and scale-factor changes
   are not.
10. **The chip-rule exception list** — §9.3 states the rule as universal;
    either the doc should name `heading_tape`'s exception or the code
    should adopt the shared chip (C23-F5).
