# Deep Review — Cluster C15 (csp-view presentation world)

Scope: `crates/csp-view/src/{lib,map,flight,terrain,part_mesh,vab,demo,tutorial}.rs`
plus `crates/csp-view/tests/{demo,pad_spawn,part_silhouettes,render_gate,startup_colonies,tutorial,vab_latency}.rs`.
Read fully; cross-checked `csp-sim` orbit conventions, `csp-phys` weld/transforms,
`csp-render` terrain shader/packet layout, `csp-ui` VabSession/TutorialState, and
`csp-app`'s consumption of the outbox/packets/pick-decode to validate data-flow claims.

Method notes: full `cargo test -p csp-view` suite is green on this box (incl. the
wgpu render gate). A scratch test forcing `SingleThreadedExecutor` on the
presentation schedule was used to probe executor-ordering sensitivity, then removed.

---

### [HIGH] Finding 1: Flight focus is pinned to the lowest VesselId — the seeded pad craft steals every later launch's throttle, camera, HUD, and tutorial telemetry

**File:** `crates/csp-view/src/flight.rs:578-586, 642-659` · `crates/csp-view/src/lib.rs:1246-1251` · consumer: `crates/csp-app/src/lib.rs:1169-1173`

**Problem:** `capture_sync_input` stages vessels in `sim.vessels()` order
(ascending `VesselId`), and `flight_present` unconditionally focuses
`snap.vessels.first()`. `View::flight_focus()` returns that same first vessel's id,
and csp-app routes `SimCommand::SetThrottle`/`SetAttitude` through it. There is no
focus-switch mechanism anywhere (`FlightEvent` has only `Orbit`/`Zoom`).
`SpawnPhysicalVessel` never removes existing vessels, so the seeded "Stock Hopper"
stays latched on the pad forever at the *lowest* id.

**Evidence:**
```rust
// flight.rs:642
let Some(focus) = snap.vessels.first() else { ... };
// lib.rs:1246
pub fn flight_focus(&self) -> Option<csp_sim::VesselId> {
    self.world.resource::<flight::FlightUiData>().focus.map(csp_sim::VesselId)
}
// csp-app:1169 — Z key
if let Some(vessel) = self.view.flight_focus() {
    ... SimCommand::SetThrottle { vessel, ... }
}
```
`capture_sync_input` (flight.rs:378-460) pushes vessels in ascending-id order and
the `SurfaceLatched` arm (421-456) deliberately keeps pad craft staged "visible
and focusable". `apply_spawn_physical` (csp-sim/src/lib.rs:4512+) inserts without
removing. The starter seed shipped in `View::seed_starter_craft` (lib.rs:759-761)
guarantees a latched low-id vessel exists from boot.

**Impact:** Player boots → Stock Hopper latched on the pad (id N) → builds a new
craft → LAUNCH (id N+1). `vessels.first()` is still the Stock Hopper: the chase
camera stays on the parked craft, the HUD shows its zeros, and pressing Z sends
`SetThrottle{vessel: N}` — *unlatching and launching the old craft* while the new
one sits inert. `tutorial_drive` (tutorial.rs:67-82) reads the same
`vessels.first()` for throttle/altitude/apsides, so the tutorial tracks the wreck
of the pad craft instead of the player's ship. Every second-and-later launch in a
shipping session hits this; no test covers ≥2 staged vessels.

**Suggestion:** Focus should follow the most recently spawned vessel (max id), or
better, carry an explicit focus id in `FlightInput`/`FlightUiData` that a
focus-cycle input and `launch_craft` (set to the just-spawned id once visible)
update. At minimum, prefer non-debris, and prefer the newest vessel.

---

### [MEDIUM] Finding 2: `vessel_parts` fallback box is double-offset — the "degraded, never absent" hull boxes render displaced by their own AABB center

**File:** `crates/csp-view/src/flight.rs:478-498`

**Problem:** The fallback `FlightPart` bakes its `PartShape::Box` in *body-frame*
coordinates (`center − half == lo`, `center + half == hi`) and then sets
`transform.offset = center`. `PartTransform::apply(q) = center + q`
(csp-phys/src/graph.rs:80-82), so the emitted box lands at `[lo+center, hi+center]`
— shifted by `center` from the intended `[lo, hi]` hull bounds.

**Evidence:**
```rust
let center = (lo + hi) * 0.5;
let half = (hi - lo) * 0.5;
FlightPart {
    lo, hi,
    transform: csp_phys::PartTransform::new(center, csp_core::DQuat::IDENTITY),
    shape: crate::part_mesh::PartShape::Box { lo: center - half, hi: center + half },
    // center - half == lo, center + half == hi  →  shape is authored in
    // body frame but applied through a transform that adds `center` again
}
```

**Impact:** Whenever the fallback fires (vessel `craft: None`, `body_transforms`
rejects, or all parts skipped — the documented "degraded, never absent" path),
each hull box renders displaced by roughly half its stack offset — the craft
appears exploded/shifted. Rare in practice, but it makes the degradation path
visible garbage rather than an honest box tier.

**Suggestion:** Emit a shape-local box `PartShape::Box { lo: -half, hi: +half }`
(the `transform.apply` then reproduces `[lo, hi]` exactly), or keep the body-frame
box and use `PartTransform::IDENTITY`.

---

### [MEDIUM] Finding 3: `MapCameraF64` is aliased by map AND flight systems with no ordering contract — correctness rides on unspecified executor order

**File:** `crates/csp-view/src/map.rs:389-399, 431-442, 592-600` · `crates/csp-view/src/flight.rs:564-570, 762-769` · `crates/csp-view/src/lib.rs:846-856`

**Problem:** `map_present` and `flight_present` are registered unordered in one
schedule tuple and both take `ResMut<MapCameraF64>`; `extract_map` and
`extract_flight` both read `Res<MapCameraF64>` in a later schedule. In Map mode
*both* systems write the resource every frame (map_present its system camera,
flight_present either the chase camera or `valid=false`), so the last writer wins
`extract_map`'s read — and bevy guarantees no order between them.

**Evidence:** Verified empirically: under both the default (multi_threaded) and a
forced `SingleThreadedExecutor` (the wasm feature set — csp-view/Cargo.toml:34-36
enables `multi_threaded` only on native), the resolved order happens to be
flight→map, so `map_present` wins and the map renders correctly today. But nothing
pins that: a bevy bump, an added conflicting system, or a `.chain()` reshuffle can
flip it.

**Impact:** If the order flips, Map mode renders either blank (`cam.valid=false`
from flight_present's no-vessel branch → `extract_map` emits `None`) or the whole
system view through a 60 m chase camera. Additionally, `extract_flight` runs every
frame in Map/Vab modes and builds a full `build_packet` terrain packet (quadtree
select + ocean 5×5 cull) plus vertex emission through whichever camera won —
wasted per-frame work, unpainted by luck of the app's mode gate.

**Suggestion:** Give flight its own camera resource (`FlightCameraF64`) — same
struct, different type. `extract_flight` could also early-out when the mode isn't
Flight (it can't see `ViewMode`, but `!cam.valid`/an input flag would do) to skip
the wasted terrain build.

---

### [MEDIUM] Finding 4: `tutorial_drive` counts debris as `has_vessel` — wreckage can complete flight steps and suppresses the "vessel lost" rewind

**File:** `crates/csp-view/src/tutorial.rs:74-90`

**Problem:** `has_vessel: !snap.vessels.is_empty()` — the staged set includes
`debris: true` bodies. `csp_ui::tutorial`'s honesty rules (`has_vessel` gates the
launch/throttle/climb/space steps; `!has_vessel` past `STEP_LAUNCH` rewinds to
LAUNCH with `vessel_lost`) assume "a focused vessel" means the player's craft.

**Evidence:** `FlightBody.debris` is staged (flight.rs:164) and debris stays in the
same `vessels` vec. A breakup mid-flight leaves debris InBubble → `has_vessel`
stays true → the rewind (`step > 2 && !has_vessel`) never fires, and
`alt_m`/`periapsis_m` keep evaluating on wreckage — `vessels.first()` may literally
be a debris body (debris gets fresh higher ids, but once the original is gone the
lowest debris id wins).

**Impact:** A craft destroyed during climb still ticks "Climb to 10 km"/"Reach
space" on the ballistic debris arc; a vessel breaking up in a closed orbit can
complete "Close the orbit" — the checklist's stated honesty rule ("advances only
when the player actually did the thing") is defeated exactly in the failure case a
new player most needs guidance through.

**Suggestion:** `has_vessel`/telemetry should exclude `v.debris` (e.g.
`vessels.iter().find(|v| !v.debris)`) — matching whatever the Finding-1 focus fix
produces — and a focused-vessel-becomes-debris transition should arguably feed the
rewind too.

---

### [LOW] Finding 5: `launch_craft` welds a fresh `to_part_graph` against the dirty-gated `analysis` without `recompute()` — mismatched craft or silent no-op on a stale cache

**File:** `crates/csp-view/src/lib.rs:1268-1302`

**Problem:** The craft def comes from `session.to_part_graph()` (always current)
plus `analysis.effective` (dirty-gated — refreshed only by `vab_present`, which
early-returns without recomputing when `input.open` is false, vab.rs:218-222).
Any session mutation that lands while the VAB is closed (`VabEvent::LoadCraft`
drained by `apply_vab_input` after a same-frame mode switch, or direct
`with_vab_session` plumbing) leaves `analysis` stale: `graph` (new) zips against
`parts`/`part_costs`/`reliability` (old) by `effective_index`. The seed path calls
`v.recompute()` first for precisely this reason (demo.rs:190-192's own comment).

**Impact:** The spawn either welds wrong parts onto new nodes or is rejected
(`part_costs not parallel`, weld error) — and `craft_def == None` returns
silently: the LAUNCH click produces no vessel, no mode switch, no feedback
(lib.rs:1302 `let Some(craft) = craft_def else { return };`).

**Suggestion:** Call `v.recompute()` (or read `analysis` only after it) inside
`launch_craft` before resolving, mirroring `seed_pad_craft`; and consider a
notice path (`vab_event(Notice)`) for the failed-resolve early return.

---

### [LOW] Finding 6: Parabolic polyline sampling uses 2× the sim's mean-anomaly convention

**File:** `crates/csp-view/src/map.rs:817-825, 849-852` vs `crates/csp-sim/src/orbit.rs:15-24`

**Problem:** The sim's parabolic encoding stores `M = (D + D³/3)/2` with
`n = sqrt(μ/p³)`, `a := −p`. `mean_anomaly_of_true`'s Barker branch returns
`D + D³/3` — twice the sim's M. `dt_s = (m − mean_anomaly_epoch_rad)/n` then mixes
the double-scale `m` against the sim-scale epoch anomaly: sample ticks land at the
wrong anomalies (for M0=0, each sample sits at ~2× the intended time offset).

**Impact:** Hyperbolic sampling is consistent (verified `e·sinhF − F` and
`n = sqrt(μ/|a³|)` both match), but parabolic orbit polylines cover a different
true-anomaly range than `OPEN_ORBIT_NU_FILL` intends — over-extended asymptote
tails and a sparser periapsis region. Points still land exactly on the conic via
`state_at`, so it's a fidelity distortion, not geometry corruption; parabolas are
a rare class.

**Suggestion:** Return `(t + t³/3) * 0.5` in the parabolic branch of
`mean_anomaly_of_true` to match the sim convention (or better, import the
convention so the two can't drift).

---

### [LOW] Finding 7: `patch_might_hold_sea` implicitly assumes `ocean_datum_m == radius_m`

**File:** `crates/csp-view/src/terrain.rs:215-233, 287-291`

**Problem:** The cull keeps an ocean instance when `min_height·amplitude ≤
max(amplitude·OCEAN_CULL_FRAC, 250)` — a bound around the *reference radius*. The
correct predicate is `radius_m + lo·amp < datum + margin`, i.e.
`lo·amp < (datum − radius_m) + margin`. `datum` is in scope at the call site but
never passed in. The terrain shader confirms the convention: terrain radius =
`planet.x + h·planet.w`, ocean disc at `planet.z` (terrain.wgsl:167-175).

**Impact:** Correct today because Terrin authors `datum_radius_m = 6.000e5 =
radius_m`. For any body authoring datum ≠ radius: datum lower → ocean instances
emitted under land everywhere the margin allows (overdraw); datum more than
~600 m higher → patches culled that should hold sea → visible holes in the ocean.

**Suggestion:** Pass `(datum − radius_m)` (or both) into `patch_might_hold_sea`
and compare `lo·amp` against it plus the margin.

---

### [LOW] Finding 8: `launch_craft` pad fallback returns an arbitrary non-launch site, not the documented equator origin

**File:** `crates/csp-view/src/lib.rs:1314-1323`

**Problem:** `reduce(|a, b| if b.launch { b } else { a })` over the dominant body's
sites returns the last `launch: true` site — but when *none* is flagged, it
returns the last site in iteration order (e.g. a `rask-mines`-style mining site),
not the "fixed-frame equator origin … for a system without sites" the comment
describes. Current data has `terrin-launch (launch: true)` so it's masked.

**Impact:** On a data set where the dominant body has sites but no launch pad, a
player's craft spawns at a random authored site (potentially a colony's doorstep)
instead of the documented equator fallback. Data-dependent, silent.

**Suggestion:** `.find(|s| s.launch)` (or `.filter(launch).next()`), keeping the
explicit `(0,0,0)` fallback for the no-launch-site case the doc promises.

---

### [LOW] Finding 9: VAB scene drops every part when `analysis` is absent — empty viewport instead of flight's box-tier degradation

**File:** `crates/csp-view/src/vab.rs:230-254`

**Problem:** `scene.parts` is built only for nodes with both a `body_transform`
and an `EffectivePart` (`effective.get(&id)`); when `analysis` is `None` (empty
session is fine, but a *resolution failure* on e.g. a loaded file with an unknown
part id leaves `analysis_error` set and `analysis: None`), every part is skipped
— the viewport renders a completely empty scene while the session still has
nodes. `flight.rs`'s `vessel_parts` has the honest-fallback tier for the same
situation; the VAB has none.

**Impact:** Loading a craft file that fails to resolve (or the catalog-less
`with_sim` world) shows a blank editor viewport — reads as "craft vanished" rather
than "resolve error".

**Suggestion:** Fall back to `PartShape::Box` over the collider/weld bounds (or at
least the session's `size_m` AABB) when `effective` misses a node.

---

### [LOW] Finding 10: Surface-latched vessels re-weld every frame

**File:** `crates/csp-view/src/flight.rs:427-455`

**Problem:** The `SurfaceLatched` staging arm calls `csp_phys::weld(&craft.graph,
&craft.parts)` (plus `vessel_parts`' per-part `body_transforms` + AABB walks) on
every `end_frame` staging pass, forever, per latched vessel — the pose record
carries no collider so it is re-derived each frame.

**Impact:** A permanently parked large craft (e.g. a 300-part landed station) pays
a full weld every frame for as long as it sits — the `vab_latency` test shows the
300-part resolve path is already the budget's hot spot. Perf-only, scales with
part count.

**Suggestion:** Cache the welded render data keyed by `(vessel id, craft
identity/graph hash)` — a latched vessel's graph is immutable until it unlatches.

---

## Non-findings verified (for the record)

- **Pick decode id-table race**: handled — csp-app snapshots `pick_decode_ids` at
  rasterize time and decodes the readback against that snapshot (csp-app:633-643);
  `VabPacket.pick_ids` is per-packet 1-based; `VabAction::Select` filters unknown
  ids (csp-ui/vab.rs:430-432).
- **Staged-mirror discipline**: systems read only `*SyncInput`/`*Snapshot`
  resources; all sim reads are confined to `View::end_frame` staging +
  `launch_craft`/`from_source` plumbing. `MapSyncInput`/`FlightSyncInput` pattern
  is consistent.
- **f64→f32**: `project_into` subtracts the f64 eye before projection everywhere
  (map, VAB, flight, terrain `origin_rel_eye` bake); `spin_phase` wrapped before
  f32 cast; `m4_wgsl`/`view_rot` transposes verified against WGSL column-major.
- **Demo manifest↔disk**: `demo_files_match_disk` diffs both directions; missing
  `colonies.ron` correctly reads as empty (`load_colonies_from`'s `exists` gate).
- **Outbox hand-off**: 1:1 UiBuild↔`take_ui_outbox` per frame verified against
  `run_frame`; per-frame assignment is safe.
- **`UiOutbox` overwrite races**: none — app drains once per `run_frame` before
  `begin_frame`, and UiBuild only runs inside `end_frame`.
- `vessel_parts`/`classify` contract: `EffectivePart`/`WeldPart` indexing,
  `effective_index` ordering, `iter()` ascending-id order all verified against
  csp-phys session/readout; authored colliders (octagon `Points`, ring+tip
  `Convex`) classify as intended.
- Pad-spawn math: welded hull vertices are body-frame; `z_min` + `PAD_CLEARANCE`
  + `surface_pose_restore` semantics verified — the pad_spawn test exercises it.
- `stock_flight_craft`: parts order matches `effective_index` 0..3; engine `+Z`
  thrust site consistent with the plume/`−dir` emission.
