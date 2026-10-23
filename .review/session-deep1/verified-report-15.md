# Verification — review-report-15.md (Cluster C15, csp-view presentation world)

Verified against source at `/home/didi/workspace/Clanker-Space-Program/crates/`
(csp-view, csp-sim, csp-phys, csp-ui, csp-app, csp-render). Two findings were
additionally reproduced **empirically** with a throwaway integration test
(`cargo test -p csp-view`, since deleted): the second-launch focus pin and the
MapCameraF64 write order.

**Verdict: all 10 findings CONFIRMED.** No false positives; one minor
overstatement noted in Finding 1 (csp-app routes `SetThrottle` only, not
`SetAttitude`, through `flight_focus`).

---

### [HIGH] Finding 1 — CONFIRMED (empirically reproduced)

Flight focus is pinned to `vessels.first()` = lowest `VesselId`; a second
launch leaves camera/HUD/throttle on the parked starter craft.

- `World::vessels()` returns `&BTreeMap<VesselId, Vessel>` (csp-sim/lib.rs:2946)
  → ascending-id iteration. `capture_sync_input` pushes in that order
  (flight.rs:378). `flight_present` focuses `snap.vessels.first()`
  (flight.rs:642) and writes `ui.focus = Some(focus.id)` (flight.rs:658).
- `View::flight_focus()` returns `FlightUiData.focus` (lib.rs:1246-1251);
  `FlightUiData.focus` is written *only* there. `FlightEvent` is only
  `Orbit`/`Zoom` (flight.rs:49-63) — no focus-switch path exists anywhere.
- `apply_spawn_physical` inserts at `next_vessel_id++` and never removes
  (csp-sim/lib.rs:4631-4673). `seed_starter_craft` runs at every native boot
  (csp-app/lib.rs:266); the pad test proves the seeded craft latches and that
  `SetThrottle` on it unlatches (tests/pad_spawn.rs:81-96).
- csp-app routes `SetThrottle` through `flight_focus()` (csp-app/lib.rs:1169-1183).
  **Correction:** `SetAttitude` is NOT routed through `flight_focus` by
  csp-app (grep: only `SetThrottle` at :1173; csp-server/csp-bench pass explicit
  ids). The throttle/camera/HUD misdirection is unaffected.
- `tutorial_drive` reads the same `vessels.first()` for throttle/alt/apsides
  (tutorial.rs:67-84).

**Empirical:** scratch test seeded the starter craft, ran 10 s (latch),
called `launch_craft()`, stepped 5 frames: `vessels = [VesselId(0), VesselId(1)]`,
`flight_focus() = VesselId(0)`. Pressing Z would unlatch and launch the
*old* craft. Bug is real and live. No test in the suite stages ≥2 vessels.

---

### [MEDIUM] Finding 2 — CONFIRMED

`vessel_parts` fallback box is double-offset.

- `PartTransform::apply(p) = offset + rotation·p` (csp-phys/graph.rs:80-81).
- Fallback (flight.rs:488-498): `center = (lo+hi)/2`, shape
  `Box { lo: center−half, hi: center+half }` = body-frame `[lo, hi]`, AND
  `transform.offset = center`. `PartShape` geometry is part-local by contract
  (part_mesh.rs:56-57) and `extract_flight` emits
  `b.pos + b.rot.rotate(p.transform.apply(q))` (flight.rs:859) → box renders at
  `[lo+center, hi+center]` — shifted by `center` (≈ half the stack offset).
- Nuance: `p.lo`/`p.hi` (used untransformed by the sheath/dust AABB paths,
  flight.rs:928-939, 963-974) stay correct — so the displaced silhouette
  disagrees with its own VFX boxes, making the degradation visibly wrong.
  Fix as suggested: `Box { lo: −half, hi: +half }` or `PartTransform::IDENTITY`.

---

### [MEDIUM] Finding 3 — CONFIRMED (empirically reproduced)

`MapCameraF64` is written by both `map_present` (map.rs:510) and
`flight_present` (flight.rs:643/648) inside one unordered presentation tuple
(lib.rs:846-854); `extract_map`/`extract_flight` read `Res<MapCameraF64>`
(map.rs:596, flight.rs:765). In Map mode both systems write every frame —
last writer wins, order unspecified by bevy_ecs.

**Empirical:** in Map mode after presentation, `cam.valid = true`,
`distance_m = 6.0e10` (the map camera) — resolved order is flight→map on this
build, matching the report's claim. The hazard (order flip → blank/wrong map)
is structurally real. `multi_threaded` is native-only
(csp-view/Cargo.toml:34-36), so wasm's single-threaded executor is the second
code path the report cites. `extract_flight`'s wasted per-frame
`build_packet` (terrain quadtree + vertex emission under whichever camera won)
in Map/Vab modes confirmed at flight.rs:775-787 — it gates only on
`cam.valid && env`, never on mode.

---

### [MEDIUM] Finding 4 — CONFIRMED

`tutorial_drive` counts debris as `has_vessel`.

- `has_vessel: !snap.vessels.is_empty()` (tutorial.rs:81); `FlightBody.debris`
  is staged from `vessel.debris_since.is_some()` (flight.rs:405, 445) and
  debris shares the same `vessels` vec.
- csp-ui: steps 2–5 (`has_vessel && …`) gated at csp-ui/tutorial.rs:114-117;
  the rewind `step > STEP_LAUNCH && !has_vessel` at csp-ui/tutorial.rs:199-201.
- Confirmed the debris path is real: a root structural breakup marks the
  *same* vessel `debris_since = Some(tick)` in place (csp-sim/lib.rs:8637-8642)
  — `vessels.first()` literally becomes a debris body at an unchanged id;
  non-root fragments get fresh higher ids (csp-sim/lib.rs:8712-8715). A wrecked
  climb therefore keeps `has_vessel` true, suppresses the `vessel_lost` rewind,
  and keeps evaluating `alt_m`/apsides on wreckage. Confirmed.

---

### [LOW] Finding 5 — CONFIRMED

`launch_craft` mixes a fresh `to_part_graph` with the dirty-gated `analysis`.

- lib.rs:1268-1302: graph from `session.to_part_graph()` (always current;
  canonical `effective_index = 0..N`, csp-phys/session.rs:1357-1374) while
  `parts`/`part_costs`/`reliability` come from `v.analysis()` — refreshed only
  by `recompute()`, which only `vab_present` calls and only while
  `input.open` (vab.rs:218-223). `VabEvent::LoadCraft` applies through
  `apply_vab_input` unconditionally (vab.rs:156-158), so a same-frame
  mode-switch load leaves analysis stale; any UiBuild edit after the last
  `vab_present` of that frame is also unseen.
- Mismatch outcomes verified: extra/renumbered graph nodes vs a short
  `effective` → `WeldError::EffectiveIndexOutOfRange` (weld.rs:363-370) →
  `SimError::Recoverable` at spawn apply (csp-sim/lib.rs:4579-4580) — a silent
  rejected command; shifted `effective_index` → the *wrong* `WeldPart`s welded
  onto nodes (the worse case, no error at all). `analysis() == None` →
  `craft_def == None` → `return` at lib.rs:1302 (silent, no mode switch).
  Note: when `craft_def` is `Some` but the command will be rejected,
  `set_mode(Flight)` at lib.rs:1368 still runs — the player is dropped into an
  empty flight view, a slightly different failure shape than "no mode switch".
- `seed_pad_craft` calls `v.recompute()` first for exactly this reason
  (demo.rs:189-198, comment included).

---

### [LOW] Finding 6 — CONFIRMED

Parabolic polyline mean-anomaly is 2× the sim convention.

- Sim: `M = n·(t−τ)` with `n = sqrt(μ/|a|³)`, parabolic `a := −p`, and
  `M_par = (D + D³/3)/2` — docs (csp-sim/orbit.rs:15-24), `n` (orbit.rs:546),
  write path `0.5 * (d + d³/3)` (orbit.rs:724).
- View: `mean_anomaly_of_true` parabolic branch returns `t + t³/3` —
  i.e. `D + D³/3`, exactly 2× (map.rs:851-852). `n` at map.rs:817 matches the
  sim. `dt_s = (m − M_epoch)/n` (map.rs:824) therefore lands each sample at
  ~2× the intended anomaly offset (points still lie on the conic via
  `state_at` — coverage distortion, not corruption). Hyperbolic branch
  (`e·sinhF − F`, map.rs:855-856) matches csp-sim/orbit.rs:733-734 — verified
  consistent. Confirmed LOW.

---

### [LOW] Finding 7 — CONFIRMED

`patch_might_hold_sea` assumes `ocean_datum_m == radius_m`.

- terrain.rs:218-233 culls on `lo·amp ≤ max(amp·OCEAN_CULL_FRAC, 250)`; the
  call site (terrain.rs:287-291) has `datum` in scope but never passes it.
- Shader confirms the convention: `radius = select(planet.x, planet.z, ocean)`
  (terrain.wgsl:167), `h = h_norm·planet.w` (:168-169) — terrain is displaced
  from `planet.x` (env.radius_m), ocean is a flat disc at `planet.z` (datum).
  Correct predicate needs `lo·amp < (datum − radius_m) + margin`.
- Masked today: `data/systems/home/terrin/body.ron:45` authors
  `datum_radius_m = 6.000e5` = `radius_m` (:16); every other body has
  `ocean: None`. Data-latent only. Confirmed LOW.

---

### [LOW] Finding 8 — CONFIRMED

Pad fallback returns an arbitrary non-launch site.

- `sim.config().sites` is `BTreeMap<SiteId, SiteDef>` (csp-sim/lib.rs:1429,
  cloned at :2204). `.filter(body == dominant).reduce(|a,b| if b.launch { b } else { a })`
  (lib.rs:1314-1320) returns the last `launch:true` site, else the last site in
  id order — e.g. a mining site — not the documented equator `(0,0,0)`
  fallback at lib.rs:1321. Masked today by `terrin-launch` (`launch: true` in
  data/systems/home/sites.ron). Confirmed LOW.

---

### [LOW] Finding 9 — CONFIRMED

VAB renders an empty scene when `analysis` is `None`.

- vab.rs:230-242 builds `effective` from `analysis().map(…).unwrap_or(&[])`;
  at 243-254 every node needs both `body_transforms().get(&id)` AND
  `effective.get(&id)` → `analysis: None` ⇒ zero parts ⇒ empty viewport.
- `recompute` sets `analysis = None` + `analysis_error = Some(…)` on resolve
  failure (csp-ui/vab.rs:370-376), e.g. a loaded file with an unknown part id —
  exactly the "craft vanished" case described. Confirmed LOW.

---

### [LOW] Finding 10 — CONFIRMED

Surface-latched vessels re-weld every frame.

- `capture_sync_input` runs every `end_frame` (lib.rs:955-956); the
  `SurfaceLatched` arm calls `csp_phys::weld(&craft.graph, &craft.parts)`
  (flight.rs:430) plus `vessel_parts`' per-part `body_transforms` + AABB walks
  (flight.rs:451) on every staging pass, forever, per latched vessel. The pose
  record carries no collider (flight.rs:427-429 comment). Perf-only; the
  suggestion (cache keyed by vessel id + graph identity) is sound. Confirmed.

---

## Non-findings spot-checked

- **Pick decode**: confirmed safe — `pick_decode_ids` is snapshot-at-encode and
  `checked_sub(1)` decodes against it (csp-app/lib.rs:633-643); `pick_ids` are
  emitted 1-based (`vab.rs:339-340`).
- **Staged-mirror discipline**: confirmed — `end_frame` inserts
  `capture_sync_input` resources (lib.rs:951-956); systems read only
  `*SyncInput`/`*Snapshot` mirrors.
- **f64→f32**: `project_into` subtracts `eye` in f64 before the f32 cast
  (map.rs:966); `spin_phase` wrapped before cast (terrain.rs:274).
- Remaining non-findings (outbox hand-off, classify contract, pad-spawn math,
  `stock_flight_craft` ordering) consistent with the code read during this
  verification; no contradicting evidence found.

## Verification artifacts

- Scratch test `crates/csp-view/tests/zz_scratch_order.rs` created, run
  (`cargo test -p csp-view --test zz_scratch_order`, 2/2 passing probes:
  `BUG CONFIRMED: focus stuck on lowest id VesselId(0)`; map camera wins in
  Map mode), then deleted — no tree changes remain.
- No source files were modified; `git status` shows only the `.review/` dir.
