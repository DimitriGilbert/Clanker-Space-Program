# Review report — P09: Map view & camera

Scope: newly-landed playability code (commits `7ca6fa4` "usable orbit map + free camera",
`d8cd46f` "vessel on map") in `crates/csp-view/src/map.rs`, `crates/csp-view/src/lib.rs`
(`map_pointer_input` ~512-616, `set_mode` ~1403-1435), `crates/csp-render/src/map.rs` +
`map.wgsl`, `crates/csp-ui/src/lib.rs` (`map_overlay` 194-380, `maneuver_panel` 755-880),
`crates/csp-app/src/lib.rs` (constants ~85-115, pointer/wheel handlers ~1954-2022).

## Findings

### F1 — MEDIUM: `View::set_mode` refocus ignores vessel focus; `MapKeyEvent::ToggleMode` is dead in production

**File:** `crates/csp-view/src/lib.rs:1407-1435`

**Problem:** The production map-toggle path (`KeyAction::ToggleMap` → `csp-app/src/lib.rs:1755-1761`,
top-bar buttons → `csp-app/src/lib.rs:938-944`) calls `View::set_mode`, which hand-duplicates
`MapInput::refocus` — but only the *body* half:

```rust
let focus = self.world.resource::<map::MapInput>().focus;
let refocus_dist = ...bodies.get(focus)
    .and_then(|b| b.soi_m)
    .map(|soi| (5.0 * soi).clamp(1.0e5, 2.0e11))
    .unwrap_or(6.0e10);
```

When the camera is focused on a vessel, `focus` is `bodies.len() + i` (the vessel-half
convention, `map.rs:238-240`), so `bodies.get(focus)` returns `None` → `refocus_dist =
6.0e10` — the whole-system "no SOI" fallback. `MapInput::refocus` (`map.rs:377-387`)
frames a vessel at `3·|a|` clamped to `[MIN, MAX]`; `set_mode` never reaches that branch.

**Evidence:** `MapKeyEvent::ToggleMode` — the variant whose `apply` calls the correct,
vessel-aware `refocus` — is produced by **no production code** (grep: only the enum def,
`map.rs:286`, and tests at `map.rs:1883/1885/2072`). Every player-facing toggle routes
through `set_mode`.

**Impact:** Focus a vessel in map mode, press `M` (or use the top bar) to leave and
`M` again to return: the camera snaps from the intended ~3·a framing to 6×10¹⁰ m — a
vessel the size of a pixel under a tracking lock the player can't see. Recoverable
(scroll/Tab), but jarring and inconsistent with the documented "re-derive framing like
CycleFocus's refocus" comment. Secondary: `set_mode` hardcodes `1.0e5`/`2.0e11` instead of
`MIN_DISTANCE_M`/`MAX_DISTANCE_M` (private in `map.rs:83-84`) — a silent drift risk.

**Suggestion:** Have `set_mode` resolve the same vessel-half framing as `refocus` — e.g.
read `focus_vessel` + `MapSnapshot` and reuse `default_distance`/the 3·a rule — or push a
`MapKeyEvent::ToggleMode`/`Refocus` through `MapInputEvents` so there is one refocus
implementation. Export or centralize the distance clamps.

### F2 — MEDIUM: maneuver panel lists InBubble vessels; "Set node" on the flying vessel is a silent refusal

**Files:** `crates/csp-view/src/lib.rs:487-494` (panel input), `crates/csp-ui/src/lib.rs:826-872`
(panel), `crates/csp-sim/src/lib.rs:5587-5588` (rejection), `crates/csp-app/src/lib.rs:836-842`
(rejection channel).

**Problem:** `capture_sync_input` (`map.rs:1186-1218`) now stages InBubble vessels into
`MapSnapshot::vessels` via their fitted osculating conic — correct for drawing. But
`map_panels` feeds **all** snapshot vessels to `maneuver_panel`'s vessel `ComboBox`, and the
`(VesselId, OrbitState, Option<ManeuverNode>)` mirror tuple carries no residence flag for the
panel to filter on. `SetManeuver` on an InBubble vessel rejects with
`Recoverable("maneuver: vessel not on rails")` — which surfaces only as `log::warn!` in the
`report.rejected` drain, never as a player-facing `flight_notice`/panel notice.

**Impact:** While flying (the exact moment a player reaches for the node panel), the vessel
picker offers the active vessel, the "Set node" click appears accepted, and nothing happens —
the textbook silent-refusal bug class this session hunts. Same for a landed-but-not-latched
craft that still fits a conic.

**Suggestion:** Add an `on_rails: bool` (or `Residence`) field to the staged
`MapSyncInput`/`MapSnapshot` tuple — `capture_sync_input` already distinguishes the two
paths (`v.orbit()` vs `v.body()`) — and either filter the panel list or mark the row
inert. If filtering is rejected on "show all vessels" grounds, route maneuver-command
rejections for `MANEUVER_SOURCE_ID` back into `flight_notice` (the key/reason is already
in `report.rejected`).

### F3 — LOW: click-focus honors a click whose press began on egui UI

**File:** `crates/csp-view/src/lib.rs:552-615`

**Problem:** The `clicked` gate is `over_ui = ctx.egui_wants_pointer_input()` evaluated on
the *release* frame: `is_pointer_over_egui() && !any_down` at the release position, plus
`is_using_pointer()`. egui's `primary_clicked()` fires for any press+release inside its
click distance/time regardless of where the press landed. A press on a panel's border
pixel followed by a release a couple of px off the panel (within egui's click threshold)
passes the gate: release pos is off-UI, `any_down` is false, no widget `is_using_pointer`
→ a `FocusBody`/`FocusVessel` event fires from a click that started on UI. The
`drag_over_ui` latch (`lib.rs:570-575`) deliberately covers only the Middle/Secondary
pan buttons.

**Impact:** Camera unexpectedly re-centers/re-frames on a misclick at a panel edge.
Narrow window (egui click threshold ≈ few px), low harm — but it is a hole in the
"clicks on panels are egui's business" contract the function documents.

**Suggestion:** Extend the press-origin latch to primary clicks: record
`press_over_ui |= is_pointer_over_egui() || egui_is_using_pointer()` when
`button_pressed(Primary)` is true, clear on release, and gate `clicked` on
`!press_over_ui` — symmetric with the pan-drag rule.

### F4 — LOW: focused vessel/body with an unsolvable conic snaps the camera to the world origin

**File:** `crates/csp-view/src/map.rs:683-688`

**Problem:** The camera focus resolves `scene.vessel_pos.get(i)` / `scene.body_pos.get(i)`
`.flatten()` with `.unwrap_or(DVec3::ZERO)` — a per-frame `state_at` failure on the focused
target lands the look-at at the system origin (the star), with the HUD distance and framing
preserved. `state_at` can fail transiently: the InBubble osculating fit
(`capture_sync_input`, `map.rs:1202-1214`) produces elements that `state_at` may not solve
at the snapshot tick (degenerate/near-radial states during integration extremes).

**Impact:** Rare, one-frame (or persistent, if the conic stays unsolvable) camera jump to
the origin instead of staying at the last good focus — the same "snap to world origin"
class `focus_vessel` anchoring fixed for the removal case (`map.rs:460-468`), left open
for the solve-failure case.

**Suggestion:** Keep the previous frame's focus position when this frame's slot resolves
to `None` (a `last_focus` field on `MapScene`), or fall back to the orbit parent's
position — anything but `DVec3::ZERO`.

### F5 — LOW (hygiene): `MapUiData` stale fields + `ToggleMode` dead variant

**File:** `crates/csp-view/src/map.rs:619-622`, `crates/csp-view/src/map.rs:137`

- `map_present` sets `ui.mode` then early-returns in Flight mode **without clearing**
  `labels`, `picks`, or `orbit_info` — they hold the last map frame's contents
  indefinitely. All current consumers gate on mode (`build_ui` only calls `map_panels` in
  Map mode; `map_pointer_input` early-returns), so it is inert today, but a future reader
  of `picks`/`orbit_info` outside the map-mode gate would consume stale data. Cheap fix:
  clear the three collections before the early return.
- `MapKeyEvent::ToggleMode` is never enqueued by `csp-app` (see F1) — its doc comment
  "`M` — flip between modes" describes a path only tests exercise. Either route
  `KeyAction::ToggleMap` through it or mark it test/internal.

## Verified clean (focus items checked, no defect found)

- **Zoom-toward-cursor math** (`map.rs:335-359`): `pan += off·(1 − new/old)` with `off` the
  cursor's focus-plane offset at the *old* distance is the correct fixed-point invariant
  (the world point under the cursor stays under the cursor). Geometric convergence: with
  repeated zoom-in at fixed cursor, `pan_offset` asymptotes to the cursor point becoming
  the new center. Clamped edge cases handled by using the *post-clamp* `new/old` ratio —
  a fully-clamped scroll (`new == old`) contributes zero pan; partial clamp scales the
  residual honestly. `ny` flip and `w/h` guards are correct; physical-px units are
  consistent (`cursor_px` staged as points×ppp, `viewport_px` fed `gpu.config` physical
  px at `csp-app:799-805`). Regression test `zoom_tracks_the_cursor_point` (`map.rs:1938`)
  covers the identity-basis case.
- **`focus_vessel` anchor** (`map.rs:241-248`, `sync_map` 469-487): id re-anchoring survives
  snapshot re-keying; removal drops to `default_focus` + `refocus`; the `focus ≥
  bodies.len() ⟺ focus_vessel.is_some()` invariant holds across `CycleFocus`/`FocusBody`/
  `FocusVessel`/`set_mode`.
- **Pick NDC→px parity** (`lib.rs:599-608` vs `csp-ui/src/lib.rs:267-270`): identical
  `(x*0.5+0.5)*w`, `(0.5−y*0.5)*h` conversion; picks recorded in the same `map_present`
  frame they are hit-tested against (schedule order Sync → Presentation → UiBuild,
  `lib.rs:1166-1169`).
- **`egui_wants_pointer_input` + `drag_over_ui`** (`lib.rs:537-615`): vendored egui 0.37
  semantics verified — `wants_pointer_input = is_using_pointer || (is_over_egui &&
  !any_down)`, so the over-egui clause is dead while any button is held and the
  press-frame latch (`is_pointer_over_egui || egui_is_using_pointer`, `|=`-accumulated,
  cleared only when *no* pan button is down) correctly blocks press-on-panel→drag-off
  pans while preserving the never-cancelled rule for press-on-map→drag-over-panel. The
  winit-side LMB orbit drag keeps the same rule (`csp-app:1954-2006`: press gated,
  release unconditional, `last_cursor` updated on consumed moves so a drag crossing a
  panel pauses rather than jumps).
- **Starfield at 1e28 m** (`map.rs:1505-1540`): `world − eye = dir·1e28` stays f64-clean;
  `clip.w > CLIP_EPS` rejection bounds NDC to finite f32 (~5e36 worst case at the eye
  plane — clipped by the rasterizer); `depth_stencil: None` in the pipeline
  (`csp-render/src/map.rs:118`) means bg/lines/markers composite by draw order — the
  ~0 depth value is irrelevant. Seeds `{4i+1..4i+4}` per star don't collide.
- **`E_PARABOLIC_TOL` mirror**: `map.rs:97` = `csp-sim/src/orbit.rs:131` = `1e-9`; the
  elliptic/open split, `p_latus`'s `a := −p` band handling, `mean_anomaly_of_true`'s
  Barker `M = (D + D³/3)/2`, and `n = √(μ/|a³|)` all match the sim's conventions
  (`orbit.rs:18`, `569-593`).
- **InBubble osculating-conic draw** (`map.rs:1186-1218`): `(com_world − body_center,
  body.vel)` pairing matches the sim's own `apply_to_rails_at` conversion
  (`csp-sim/src/lib.rs:5251`); `v.body()` is `Some` only for `InBubble`, so
  `SurfaceLatched` vessels are correctly skipped; `epoch = sim.clock()` makes the fitted
  conic evaluate to the live position at the snapshot tick and re-samples each frame as
  documented.
- **"Can't change ref" fix = click-to-focus**: `FocusBody`/`FocusVessel` retarget the
  camera without the cycle key (`map.rs:312-330`); star-parented orbits resolve
  `parent_pos(star) = Some(ZERO)` because `body_system_pos` returns `Ok(ZERO)` for the
  root (`map.rs:1235`), so star-orbiting vessel lines/markers/apsides draw correctly;
  covered by `focus_events_retarget_the_camera` (`map.rs:1892`).

## Minor observations (not flagged as defects)

- `sample_orbit` closed branch (`map.rs:1277`): `t_ticks.max(0)` is dead defensiveness —
  `epoch_ticks ≥ 0` and all offsets are non-negative, so the clamp can't engage. Harmless.
- `map_panels` node ETA label (`map.rs:745`) and `maneuver_panel`'s armed-row
  (`csp-ui:845`) both `saturating_sub` a past node tick to "T−0 s" — a node that slipped
  past the clock reads as imminent rather than elapsed. Cosmetic.
- Picks are recorded for every *projected* marker including anchors just off-screen; a
  click within `PICK_RADIUS_PX` (14 px) of the window edge can focus a marker whose center
  is off-screen. Deliberately generous hit-test; fine.
- `emit_polyline` re-emits a join disc at every interior vertex plus both endpoints —
  O(128) extra quads per orbit line, by design for crack-free joints.
