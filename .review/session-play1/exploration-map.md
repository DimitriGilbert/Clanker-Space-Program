# Exploration Map — session-play1 (playability track, 15 clusters)

## Project overview

**Clanker Space Program** — a KSP-style space-program game in Rust. Workspace crates:

- `csp-core` — determinism primitives (SimTime, DVec3/DQuat, hash, RNG, errors).
- `csp-data` — serde schemas + loader/validator for `data/` RON content (parts, resources, system, techtree…).
- `csp-phys` — welded-body physics: part resolution (`EffectivePart`→`WeldPart`), weld, `SimpleBackend`, aero/thermal, craft session.
- `csp-sim` — authoritative deterministic world: command queue (`SimCommand`, §7.6 log), event heap, vessels (InBubble / SurfaceLatched / OnRails), orbits, program layer. `lib.rs` is 20 951 lines; the playability diff touches the bubble step (L3558–3646), command applies (L4481–4570), `ToBubble` (L5469–5533), and the new fuel-plan block (L7660–7872) plus tests.
- `csp-view` — `bevy_ecs` presentation world, read-only w.r.t. sim; six §2.5 schedules; owns session state (keymap mirror, VAB session, stage plan, focus pin, tutorial).
- `csp-ui` — pure-egui panels + session models (settings/keymap, VAB session, tutorial, navball, HUD). No winit/wgpu/sim edges.
- `csp-render` — wgpu pipelines (map/flight/terrain/vab) + WGSL; consumes clip-space packets only.
- `csp-app` — winit + wgpu + egui shell: input dispatch, warp controller, settings/craft-file IO, audio, crash/bug-report path, paint.
- `csp-bench`, `csp-server`, `csp-script`, `csp-ai`, `xtask` — NOT in this review's scope (stable per session-deep1).

**This session's scope:** the playability fixes that just landed — fuel consumption, held-key throttle axis + attitude deferral, VAB usability (palette placement, craft files, notices), map visuals + camera (cursor zoom, click-to-focus, vessel focus anchor), flight visuals (kitbash silhouettes, plume, plasma sheath, dust, sky, the GreaterEqual "purple screen" fix), audio latency (subprocess player), RecoverControl/ToBubble + focus pin, launch-captured stage plan, `key-*` physical tokens + AZERTY migration. Everything else was covered by session-deep1 (22 clusters).

**Determinism note:** the fuel/mass change (`propellant_plans` + per-tick `transfers::debit` + `WeldPart::engine` + `engine_can_draw`) lives in the hashed sim core — it is the only cluster that can shift the §7.1 state hash and golden replays. Everywhere else is presentation/session code.

---

## Clusters

### P01 — Keymap, physical tokens & AZERTY (input plumbing)
- `crates/csp-ui/src/settings.rs` — `KeyAction` enum incl. new verbs (`ThrottleUp/Down`, `Stage`, `RecoverControl`, attitude `Pan*`/`Roll*`, `ToggleHelp/Debug`, L185–322); `is_attitude`/`is_throttle_axis`/`accepts_repeat` classifiers (L288–321); `KeyBinding` + `NAMED_KEY_TOKENS`/`PHYSICAL_KEY_TOKENS` (L324–463); `physical_fallback`/`display_label` observed-glyph printing (L465–503); `Keymap` defaults binding `key-*` positions (L523–559), `action_for`/`bind`/`sanitize`/`hint(_labeled)`/`first_binding(_label)` (L561–687).
- `crates/csp-app/src/settings_io.rs` — settings file load/save with `Keymap::sanitize` on parse (L57–130); `key_token` logical map (L148–210); `keycode_token` physical `KeyCode`→`key-*` map (L219–328); `event_binding`, `char_to_physical_token`, `migrate_char_bindings`, `layout_dependent_binding_count`, `event_label` (L330–428).
- `crates/csp-app/src/lib.rs` — physical-first resolution in `key_press`/`action_of` (L1360–1390); rebind capture `intercept_rebind_key`/`captured_key` (L169–180, 1337–1359, 1693–1725); migration+notices in `assemble` (L341–371); `note_key_label` glyph recording (L1662–1670); `drop_rebound_holds` (L1625–1646).
- `crates/csp-view/src/lib.rs` — `SessionMirror.keys`/`key_labels` (L174–184); `set_live_keymap`/`set_key_label`/`stage_settings`/`rebind_target`/`set_rebind_target`/`bind_key`/`set_settings_notice` (L1215–1321).

**Focus (HIGH):** physical-first resolution order (physical token tried before logical — check both press AND release paths stay symmetric); `migrate_char_bindings` collision handling vs `sanitize` order-of-claim; `bind`'s one-action-per-key eviction; `intercept_rebind_key` swallow-until-release vs modifier-shifted tokens (physical match is deliberate — verify a captured `key-1` released as `!` can't wedge); `event_label` only records `key-*` tokens with single-char glyphs.

### P02 — Flight control verbs & read-only gates (app-side)
- `crates/csp-app/src/lib.rs` — throttle axis: `THROTTLE_STEP`/`THROTTLE_RAMP_PER_S` (L97–108), `throttle_held`/`throttle_axis` (L193–206), `throttle_press/release`, `tick_throttle_ramp`, `throttle_axis_step` (L1512–1612), snap-key rebase (L1767–1806); attitude: `ATTITUDE_RATE_RAD_S` (L108–110), `attitude_held`/`attitude_deferred`, `attitude_press/release`, `tick_attitude`, `attitude_blocker`, `apply_attitude` (L1392–1510); `dispatch` all arms incl. `Stage`, `RecoverControl`, `CycleFocus` (L1727–1860+); `focus_lost` (L1648–1660); `read_only` gates at L442–457, 954–960, 1029–1059, 1450, 1475, 1517–1525, 1566–1571, 1617–1621, 1770–1777, 1817–1822; `handle_ui_outbox` (L930–1078); `on_sim_fatal` (L501–528); `apply_live_settings` warp-cap demotion gate (L423–459); `apply_startup_difficulty` (L475–499).
- `crates/csp-view/src/lib.rs` — `flight_notice` (L1861–1876); `UiOutbox` request fields (L218–243).
- Deps: `csp-sim` `SetThrottle`/`SetAttitude` applies (L4488–4511), `PLAYER_SOURCE_ID` shared FIFO (`csp-view/src/lib.rs` L60–66).

**Focus (HIGHEST):** every verb a dead key can reach must land a visible notice — audit each `read_only`/mode/residence refusal for its cue text and that no command queues undrained; throttle axis semantics (axis persists across releases/rebases on snap keys, holds across a rails focus — L1577); `SetThrottle` rides the SAME `PLAYER_SOURCE_ID` FIFO as `SpawnPhysicalVessel` (issue-order contract); attitude deferral replays on `tick_attitude` once `attitude_blocker()` clears — check held-through-unlatch steering; `focus_lost` zero-rate discipline; read-only coverage of EVERY outbox arm (`craft_save`, `launch`, `maneuver`, `settings_apply` warp demotion) — deep1 Finding 6 (queued-silently class) was the template.

### P03 — Propellant consumption (determinism-load-bearing) ★
- `crates/csp-sim/src/lib.rs` — `FuelPlan` (L7660–7682), `propellant_mpu` (L7684–7697), `propellant_plans` pooled-coverage two-pass (L7699–7813), `apply_fuel_scales` (L7815–7838), `engine_can_draw` + `SetThrottle` clamp (L4488–4499, 7840–7872), the per-tick plan→debit→re-weld→re-scale pass in the bubble loop (L3558–3646), fuel tests (~L17259–17340).
- `crates/csp-sim/src/transfers.rs` — `debit`/`credit`/`reweld_if_mass_changed` (L480–620) — the debit path the tick now calls per vessel; also `vessel_resource` used by `engine_can_draw`.
- `crates/csp-phys/src/part.rs` — `WeldPart::engine: Option<EngineSpec>` + `EngineSpec{isp_vac_s, propellants}` + `tanks` mass convention (L274–328).
- `crates/csp-phys/src/resolve.rs` — `EngineSpec` resolution + `check_engine_propellants` mirror (L1041–1216, L1657–1837).
- `crates/csp-core/src/hash.rs` — `WeldPart::engine` in the §7.1 state-hash visitor (L185–200).
- Dep/data: `csp-data/src/part.rs` `ModuleDef::Engine.propellants` + `validate.rs::check_engine_module` (L2729–2816); `apps/demo/data/parts/csp.engine.*.ron`, `csp.tank.*.ron`, `resources.ron` (`mass_per_unit_kg`).

**Focus (HIGHEST — the only hash-bearing cluster):** pooled coverage `min(1, avail/Σ needs)` vs per-claimant (the documented free-impulse bug class — verify the shared denominator really lands debit == availability on the dry-out tick); `units = INFINITY` for un-holdable/massless propellants → zero coverage (starve, never free burn); scale composition `base·factor·coverage` re-applied after the contents-quantum re-weld (L3635–3642 — sites rebuilt at baseline then rescaled, must stay idempotent); debit ordering ascending `PartNodeId`/plan order; `engine_can_draw` semantics (craft-less/`engine:None` unmetered craft keep throttle — debug convention; any-spec-but-all-starved clamps to 0); `SetThrottle` gate can't relight a dry engine (test L17329–17337); whether tank-less `WeldPart`/`tank_contents` desync is possible after jettison; f64 summation order determinism in `demand`/`avail_of` BTreeMaps (sorted iteration — confirm); burn-executor interplay (`drive_node_burn` reads `rated_n` post-scale L7890–7909 — stalled burn = honest partial delivery).

### P04 — RecoverControl / ToBubble & focus lifecycle
- `crates/csp-view/src/lib.rs` — `recover_control` (L1480–1546), `to_bubble_eligible` (L1548–1563), `clear_flight_focus_override` (L1565–1573), pin release in `end_frame` (L1157–1164), focus rule `flight_focus` (L1575–1586).
- `crates/csp-view/src/flight.rs` — `FlightFocusOverride` set/seen/clear contract (L236–268), pin lifecycle in `flight_present` (L1093–1131).
- `crates/csp-sim/src/lib.rs` — `SimCommand::ToBubble` (L634), `apply_to_bubble` re-weld + pose + dead-event scrub + maneuver discard (L5469–5533), `TakeControl` lease apply (L4699–4741, lease semantics L368–465).
- `crates/csp-app/src/lib.rs` — `RecoverControl` dispatch + read-only gate (L1813–1823).
- `crates/csp-view/tests/flight_focus.rs` — newest-spawn focus regression.

**Focus (HIGH):** the pin's three release paths (staged-then-left, gone-from-arena, fresh launch) — verify no permutation leaves a stale pin retargeting focus/throttle; eligibility mirrors the drain's reject classes (rails + dominant parent + craft + env) — a divergent check gives pin-and-silent-reject; `TakeControl{Player}` preempts unconditionally — confirm it can't steal a lease the sim intends to keep (Script/Agent bound vessel while `ToBubble` rejects); map-focused target vs newest-on-rails fallback ordering; `max()` id pick for fallback (newest = highest id).

### P05 — Stage plan (launch-captured firing sets)
- `crates/csp-view/src/flight.rs` — `StagePlan` (L408–517): stages/total/vessel/min_id/craft_key, `for_launch` capture, `bound_to`, lazy `vessel_for` binding; `craft_fingerprint` FNV (L547–585); HUD mirrors `stages_left/total` (L350–357, 1152–1153).
- `crates/csp-view/src/lib.rs` — `stage_plan_of` decoupler→drop-set walk (L1943+); `fire_stage` refusal cases + JettisonPart issuance (L1764–1859); capture site in `launch_craft` (L1734–1760).
- `crates/csp-app/src/lib.rs` — `stage_press` + `Stage` dispatch gate (L1614–1623, 1809).
- `crates/csp-sim/src/lib.rs` — `apply_jettison_part` (L4552–4553 → L6006–6154; InBubble-only in v1).
- `crates/csp-view/tests/stage_plan.rs` — both shipped bug classes (eaten stage on latched/rails; plan firing at focus not bound vessel).
- Deps: `csp-ui/src/vab.rs` stage actions (`SetStage`/Insert/Remove/CompactStages L94–127, 508–567) feed the session's `stage_order`.

**Focus (HIGH):** lazy binding by `(min_id, craft_fingerprint)` — two launches of the same session craft produce identical fingerprints; verify `min_id` truly prevents binding a still-live older copy, and what happens when the bound vessel's craft is mutated by jettison (fingerprint drifts — re-binding already cached so fine, but check a SECOND plan of the same craft); front-first `remove(0)` vs `stage_order` direction; refusal matrix (empty/expended/unspawned/latched/rails/gone) each lands a cue without popping; `JettisonPart` InBubble-only — the pop correctly deferred to a bubble-resident bound vessel.

### P06 — Launch, pad spawn & starter seed
- `crates/csp-view/src/lib.rs` — `launch_craft` (L1588–1762): dirty-cache recompute, analysis→`CraftDef` (parts/costs/reliability), launch-site lookup, weld pre-check, `z_min`/`com_local` belly-rest altitude, §2.6.4 `SurfacePose`/`surface_pose_restore` at current spin phase + `ω×r` CoM velocity, plan capture, enqueue + mode switch; `PAD_CLEARANCE_M = 0` (L53–58); `stock_flight_craft` (L1981+); `seed_starter_craft`/`new_demo` (L912–954).
- `crates/csp-view/src/demo.rs` — `STARTER_CRAFT`, `seed_pad_craft` via `VabAction`s then `launch_craft` (L33–45, 168–212); embedded `FILES` manifest.
- `crates/csp-app/src/lib.rs` — `seed_starter_craft` call site (L323), launch outbox gate (L1029–1047).
- `crates/csp-view/tests/pad_spawn.rs` — rest-on-pad + settle→latch + terrain-vs-datum pin (the buried-pad bug).
- Deps: `csp-sim` `apply_spawn_physical` (L5009–5119), `csp-sim/src/bubble.rs` surface pose/restore; `apps/demo/data/systems/home/sites.ron` + `terrin/body.ron` (terrain seed).

**Focus (HIGH):** spawn pose math (altitude = site + clearance − z_min; `min(0.0)` guard for origin-below-parts; `vel` is CoM slot — `ω×r` at `pos + rot·com_local`, not origin); weld-failure refusal BEFORE enqueue (no silent Flight-with-no-vessel); stage-plan/focus-pin interaction on launch (clears prior pin — newest-spawn rule); launch-site selection (`launch: true`, ≤1 per body enforced by validator — check fallback equator path); terrain-vs-datum: rendered ground can sit +57 m above the datum sphere the sim contacts — the pad test pins the authored site's delta; demo `part_limit`/`file_io` enforcement halves in session vs app outbox.

### P07 — Flight visuals (kitbash, plume, sheath, dust, sky)
- `crates/csp-view/src/flight.rs` — `capture_sync_input` (InBubble + SurfaceLatched via `stage_latched`/`LatchedWeldCache` L519–545, 621–780): plume emitters from Thrust sites, `plasma_w_m2` via `csp_phys::flow_sample` × `CONVECTIVE_HEAT_FRACTION` (L671–681), `sun_direction` (L587–619), `vessel_tanks`, `part_kind`/`kind_tint`/band accents (L816–880+); chase camera solve + terrain floor (L1040–1091); `extract_flight` packet emission (part meshes, plume, sheath, dust, fallback sky).
- `crates/csp-view/src/part_mesh.rs` (836) — `PartShape` classifier: Box vs Lathe over resolved collider + engine/pod kind overrides; `LATHE_SEGMENTS=24`, apex rule; pure fns of `EffectivePart`/`WeldPart`.
- `crates/csp-render/src/flight.rs` (188) — `FlightPacket` (vertices + optional `TerrainPacket`); **the `GreaterEqual` depth fix** — fallback sky at NDC z=0 vs `Greater` against 0.0-cleared depth (L1–11, 71–80).
- `crates/csp-render/src/flight.wgsl` (28) — flat-color passthrough.
- `crates/csp-view/tests/part_silhouettes.rs` — pod/tank/engine silhouette classes; `tests/render_gate.rs` — real-device pixel gate for the "uniform purple" regression.

**Focus (HIGH):** depth-compare fix correctness (why `GreaterEqual` is right and whether anything else sits exactly at far plane); latched-vessel staging (`LatchedWeldCache` keyed on `(id, craft_fingerprint)` — staleness on jettison/unlatch; `retain` bound); plasma flux mirrored from the sim's own `flow_sample` (shared constants `PLASMA_ONSET/FULL` duplicated in `audio.rs` L96–98 — drift hazard); silhouette classifier precedence (engine/crew beat collider; `WeldPart::engine` vs Thrust-site fallback for unmetered craft); kitbash determinism (pure fns — no RNG); plume/sheath/dust emission math at f64→f32.

### P08 — Terrain & atmosphere
- `crates/csp-view/src/terrain.rs` (547) — `TerrainConfig`/`TerrainBody`, `config_from_bodies`, seeded `seed_of` fallback, `HeightField`/`TerrainCache`, packet builder.
- `crates/csp-render/src/terrain.rs` (1298) — mix64 lattice value-noise, cube-sphere quadtree `select_patches`, `patch_instance` f64-origin bake, `bake_transmittance` LUT, ocean batch, biome field decode.
- `crates/csp-render/src/terrain.wgsl` (422) — heightfield bilinear, **`spin` uniform + `rot_axis` body-fixed↔inertial sampling** (L23, 61–72 — terrain slides under landed craft at surface velocity), transmittance LUT sampling convention (endpoint- vs texel-center — L74–80), sky shell, ocean, biome tint.
- `crates/csp-view/src/flight.rs` — `FlightEnv` spin axis/phase/body_spin fields (L98–120, 626–649), chase-camera terrain-aware floor, terrain cache plumbing in `flight_present`/`extract_flight`.

**Focus (MEDIUM-HIGH):** the two texel-convention samplers must not be shared (documented half-texel bias); `u.spin` phase must equal the sim's §2.6.4 `surface_pose_*` convention exactly (ω·t at sim clock — a drift lands as ground sliding under a parked craft); quadtree determinism (BFS, budget bound); f64-origin bake residual (~5 cm claim); ocean emitted only where sampled heights reach datum; `seed_of` fallback FNV vs authored `TerrainDef::noise_seed` consistency.

### P09 — Map view & camera
- `crates/csp-view/src/map.rs` (2187) — `MapKeyEvent` (ToggleMode/CycleFocus/FocusBody/FocusVessel/Orbit/Zoom/Pan L130–158), `MapInput` incl. `focus_vessel` id anchor + `cursor_px` (L235–394): apply (zoom-toward-cursor math L335–359, pan L360–371, refocus L377–387); `sync_map` focus re-anchor on re-keyed snapshot (L461–487); `map_present` (picks, orbit info, node previews L610–939); `extract_map` starfield/markers/apsis/lines (L940–1185); camera solve + projection helpers shared crate-wide (L1343–1474); `capture_sync_input`/`sample_orbit` (L1186–1320); tests L1644–2187.
- `crates/csp-view/src/lib.rs` — `map_pointer_input` egui-pointer → Pan/click-to-focus/cursor staging, `drag_over_ui` latch (L512–616); `set_mode` refocus (L1403–1435).
- `crates/csp-render/src/map.rs` (159) + `map.wgsl` (48) — `MapVertex` uv/kind shaping (solid/disc/ring/glow), `MapPacket` bg/lines/markers.
- `crates/csp-ui/src/lib.rs` — `map_overlay` + labels + `ManeuverPanelState`/`maneuver_panel` (L194–380, 758–880).
- `crates/csp-app/src/lib.rs` — winit→`MapKeyEvent` translation constants (L85–95), LMB click-vs-drag threshold `CLICK_DRAG_PX` (L111–115).

**Focus (MEDIUM-HIGH):** zoom-toward-cursor math (`off·(1−new/old)` residual — verify asymptotic behavior and clamped-zoom edge cases); `focus_vessel` anchor: index re-anchoring in `sync_map` vs vessel leaving snapshot (the world-origin snap bug it documents); click-to-focus pick uses `map_present`-recorded projected picks (what the player saw) — NDC→px conversion parity; `egui_wants_pointer_input` + `drag_over_ui` latch: press-over-panel then drag-off must not pan (and vice versa — the never-cancelled rule); starfield at 1e28 m conditioning; `E_PARABOLIC_TOL` mirrored from sim.

### P10 — VAB usability (palette, placement, files, notices)
- `crates/csp-ui/src/vab.rs` (2206) — `VabAction` vocabulary (`CatalogPick`/`CancelPlacement`/Insert/Remove/CompactStages/`LoadBytes`, L60–127); `VabSession` (`pending_part`, `notice`, `craft_epoch`, `file_io`, `part_limit`, `catalog`, `to_bytes`/`from_bytes`); `catalog_pick`/`place_root`/`place_attach`/`select`/`load_bytes`/`attach_auto` candidate ordering (L571–720+); panels: `top_bar`, `vab_editor`, `palette_panel`/`palette_banner`, craft-files row (save/load name field), stage column, inspector, `vab_hint_bar` (L860–2206).
- `crates/csp-view/src/vab.rs` (402) — `VabEvent` (Orbit/Zoom/Pan/Pick/LoadCraft/Notice), `VabSessionRes` hosting, `VabInput` camera + `craft_epoch` one-shot refit (L77–110), `apply_vab_input` pick→Select routing, `vab_present`/`extract_vab`.
- `crates/csp-app/src/lib.rs` — `craft_store`/`craft_dir` (L153–158), `load_craft_store`/`save_craft_file`/`validate_craft_name` (L2129–2200), outbox craft save/load + notices + demo/read-only gates (L946–1077), GPU-pick plumbing (`pick_tx/rx`, `pending_pick`, click threshold).
- `crates/csp-render/src/vab.rs` + `vab.wgsl` — kitbash pass + object-id pick (dep; mostly stable M3 code but in the touched call path).
- `crates/csp-view/tests/vab_latency.rs` — §5.4 ≤16 ms kill criterion.

**Focus (HIGH):** `CatalogPick` state machine (empty→root, selected→attach, else arm pending; re-click cancels; background click disarms; failed attach keeps arm — find the un-cancellable state); `attach_auto` candidate ordering (bound nodes excluded, most-downward node preferred — mount-orientation regression noted in tutorial test); `validate_craft_name` before BOTH store insert and file write (deep1: separator names → phantom entries / traversal); demo `file_io=false` enforced in BOTH `load_bytes` and app outbox; `craft_epoch` camera refit on New/load (stale `focused_once` framing); pick → pending-placement consume path (`VabEvent::Pick` → `Select`).

### P11 — Flight HUD, navball & SFX cues
- `crates/csp-ui/src/lib.rs` — `PropellantGauge` (L165), `FlightHudInput` (L386–448), `flight_hud` (L497–688: SFX flash stack, "ON PAD — [key] to launch" latch state, telemetry rows, THR bar, STG readout honest single-stage-vs-expended, per-resource propellant bars, live-keymap hint line), `help_panel` controls card (L694+), read-only dialog text (L133–152), dev-gated debug panel.
- `crates/csp-ui/src/navball.rs` (542) — navball ball + heading tape over body-frame staged directions (up/north/east/prograde), horizon decomposition, heading_tape.
- `crates/csp-view/src/flight.rs` — `FlightUiData` mirrors (L279–358), `flight_present` writers (L1093–1208: focus, latch, speeds, attitude `stage_attitude`, osculating Ap/Pe/T-A/P/period with `E_PARABOLIC_TOL` gates), `FlightSfx`/`SfxPrev`/`sfx_edges` transition detector (L360–402, 1210–1260+), `FlightTank` alias.
- `crates/csp-view/src/lib.rs` — `build_ui` panel roster + `PanelFlags` help/debug (L283–458).

**Focus (MEDIUM-HIGH):** edge detection baseline on focus switch (re-baselines instead of spurious cut/contact edges — verify all seven cue types); `sfx` cue vocabulary is the contract `audio.rs::cue_shot` parses literally — drift detector warns but check completeness; latched-state readouts (prograde None while parked, `surface_speed` = `v − ω×r` ~0 claim); NaN handling in heading/pitch/roll rows; parabolic-sliver readout gating (Ap/T/period only outside tol, Pe outside band); navball far-hemisphere rim clamp.

### P12 — Tutorial (first-flight checklist)
- `crates/csp-ui/src/tutorial.rs` (541) — `TutorialStep`/`STEPS`/`STEP_LAUNCH`/`CLIMB_ALT_M`/`SPACE_FALLBACK_ALT_M`, `TutorialInput`/`TutorialState` progress + vessel-lost rewind to `STEP_LAUNCH`, `{key}` hint substitution via live keymap, `tutorial_overlay`.
- `crates/csp-view/src/tutorial.rs` (256) — `TutorialRes`, `tutorial_drive` (Presentation): newest NON-debris staged vessel (debris must not satisfy/suppress), `osculating_apsides` closed form, `TutorialInput` assembly.
- `crates/csp-view/src/lib.rs` — tutorial plumbing (`tutorial_active`/`set_tutorial_active`/`tutorial_state`, L1323–1342) + `m10_panels` wiring + top-bar Tutorial re-open (L432–436).
- `crates/csp-view/tests/tutorial.rs` — end-to-end VAB→launch→climb→orbit checklist through the real View.

**Focus (MEDIUM):** predicates vs staged mirrors (tutorial must advance only on real progress); vessel-lost rewind (debris exclusion rule L64–70); `osculating_apsides` degenerate/escape guards; `{key}` binding staleness (live map — OK by construction); overlay Dismiss/Restart session semantics.

### P13 — Audio (synthesized bank + low-latency subprocess backend)
- `crates/csp-app/src/audio.rs` (724) — `cue_shot` text→OneShot map incl. app-pushed refusal cues (L100–130); `CueTracker` multiset edge-diff (L132–165); `PLAYERS`/`player_args` low-latency flags (L167–222); `MixState` voices+shots (L224–287); `writer_loop` lead cap `MAX_PIPE_LEAD_S=0.05` (L289–323); `AudioBackend::open` settle check `PLAYER_SETTLE_MS`, `update` per-frame pump, inert-on-dead-child.
- `crates/csp-app/src/synth.rs` (383) — `OneShot` bank (9 variants), `render`/`render_raw` recipes, `Rumble`/`Whoosh` continuous voices, xorshift `Noise` (fixed seeds → zero-attribution assets), determinism tests.
- `crates/csp-app/src/lib.rs` — post-`end_frame` pump call (L897–910), `ui_confirm` (L913–928).
- Dep: `csp-view/src/flight.rs` `FlightSfx` cue queue (producer).

**Focus (MEDIUM):** `CueTracker` multiset matching correctness (same-text pushes one frame apart must double-fire — `ttl` ancestor rule); lead-cap pacing vs blocking write (can `written_frames` accounting drift under sleep granularity?); settle-check 60 ms per candidate (up to 3× = startup cost); child-exit mid-session → inert, never panic; `cue_shot` coverage vs every string `flight_notice`/`sfx_edges` can push (unmapped = warn-once — the vocabulary is the contract); `LEVEL_SMOOTH`/gains on throttle+plasma edges (mirrored PLASMA constants — drift vs `flight.rs`).

### P14 — Settings & session persistence
- `crates/csp-ui/src/settings.rs` — `Settings`/`AudioSettings`/`GraphicsSettings`/`GraphicsQuality`/`DifficultySettings`/`GameplaySettings` (L25–180), `SettingsPanelState` + `settings_panel` rebind UI (L689–1025).
- `crates/csp-app/src/settings_io.rs` — config-dir resolution, load/save, wasm no-op stubs (L22–146).
- `crates/csp-app/src/lib.rs` — `assemble` staging (L341–421), `apply_live_settings`/`persist_settings`/`apply_startup_difficulty` (L423–499), settings_apply outbox path incl. `window_size` authority + `drop_rebound_holds` + persist notice honesty (wasm "applied" vs "saved", L985–1028).
- `crates/csp-view/src/lib.rs` — `SettingsPanelRes`, `stage_settings`/`staged_settings`, notice channel; `m10_panels` (fatal dialog, L618–639+).
- Deps: `csp-app/src/bugreport.rs`, `logring.rs` (fatal bundle path — likely stable from deep1 but adjacent); `csp-app/src/warp.rs` `set_cap_multiplier` (dep).

**Focus (MEDIUM):** staged-vs-live split discipline (nothing may read the staged copy for behavior; `window_size` app-tracked authority over stale staged value); difficulty flags = construction-config write NOT in the command log (`set_reliability_config` — replay determinism rides the snapshot carrying it — verify); `life_support_enabled` rides `SetLifeSupportEnabled` before first tick; wasm persistence honesty; settings file forward-compat (`serde(default)` + sanitize).

### P15 — New regression-test surface
- `crates/csp-view/tests/stage_plan.rs` — staging refusal + vessel binding.
- `crates/csp-view/tests/flight_focus.rs` — newest-spawn focus rule.
- `crates/csp-view/tests/pad_spawn.rs` — pad-rest spawn, settle→latch, terrain-vs-datum.
- `crates/csp-view/tests/part_silhouettes.rs` — kitbash classes.
- `crates/csp-view/tests/render_gate.rs` — device-backed pixel gate (skips without adapter — honest but coverage-free on GPU-less CI).
- `crates/csp-view/tests/tutorial.rs` — checklist end-to-end.
- `crates/csp-view/tests/vab_latency.rs` — perf gate (borderline: may predate this track).
- `crates/csp-view/tests/demo.rs`, `startup_colonies.rs` — likely stable, verify.
- `crates/csp-app/src/lib.rs` test module (L2300–2753): `new_game_boots_with_a_pad_vessel`, `held_throttle_ramps_then_holds_on_release`, `read_only_refuses_launch_and_maneuver`, craft-name validation, rebind dispatch, settings Apply e2e, forced-fatal bundle.
- `crates/csp-sim/src/lib.rs` fuel tests (~L17259–17340: per-tick debit, dry-engine SetThrottle clamp).

**Focus (MEDIUM):** each test pins a specific shipped bug — verify the test actually fails on the pre-fix behavior (not tautological); `render_gate`'s silent skip when no adapter; sim fuel tests cover the pooled-coverage and starvation cases (two co-claimant engines, un-holdable propellant, jettisoned tank mid-burn).

---

## Totals

- **15 clusters.** ~30 files with real diff surface + ~15 dependency-only files flagged inline.
- **Review order suggestion:** P03 (determinism) → P02/P04/P05 (command-surface honesty, the repeated "silent refuse / eaten action / stale binding" bug classes) → P06/P07 (spawn pose + depth fix — the two headline regressions) → P01 (keymap correctness for every press/release path) → P10/P09 (interaction state machines) → P08/P11 → P13/P12/P14 → P15.

## Cross-cutting notes

1. **The `flight_notice`/`FlightSfx` cue channel is load-bearing for honesty** — nearly every refusal in P02/P04/P05/P10 routes through `View::flight_notice` or `VabEvent::Notice`; any path that refuses WITHOUT a cue is the known bug class these fixes were written against. Grep-worthy: every `return`/`Err`/`Recoverable` reachable from a key press.
2. **`PLAYER_SOURCE_ID` (id 1) FIFO sharing** between `csp-app` (SetThrottle/SetAttitude) and `csp-view` (SpawnPhysicalVessel, JettisonPart, ToBubble, TakeControl) is deliberate — verify nothing player-initiated enqueues on a different source id (warp=0, maneuver=2, settings=3 are documented as distinct).
3. **Mirrored constants:** `PLASMA_ONSET/FULL_W_M2` duplicated `flight.rs`↔`audio.rs`; `E_PARABOLIC_TOL` mirrored `map.rs`↔`csp-sim`; cue-text vocabulary `flight.rs`/`lib.rs`(view+app) ↔ `audio.rs::cue_shot`. Each is documented as intentional-mirror — a reviewer should confirm they're mechanically equal today.
4. **One-frame-latency contract:** UI-issued requests ride `UiOutbox`/`MapInputEvents`/etc. into next frame's Input — the map pointer, stage pop, and settings apply all depend on ordering `build_ui` → `map_pointer_input` → next `begin_frame`.
5. **Not verified:** exact git diff surface (no exec access). Borderline-stable files that may sit outside the playability diff: `csp-app/src/{warp,bugreport,logring,main}.rs`, `csp-sim/src/{bubble,maneuver,joints,events}.rs`, `csp-render/src/{vab.rs,vab.wgsl,lib.rs}`, `csp-view/tests/{demo,startup_colonies,vab_latency}.rs`, `csp-data` crate, `apps/demo/data/**`. A reviewer with `git log -3 --stat` should confirm the assignment and pull in anything missed.
