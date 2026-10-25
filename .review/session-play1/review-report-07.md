# Review Report — P07: Flight visuals (kitbash, plume, sheath, dust, sky)

Scope: `crates/csp-view/src/flight.rs`, `crates/csp-view/src/part_mesh.rs`,
`crates/csp-render/src/flight.rs`, `crates/csp-render/src/flight.wgsl`,
`crates/csp-view/tests/{part_silhouettes.rs,render_gate.rs}` + deps
(`csp-phys::thermal`, `csp-render::terrain`, `csp-app::audio`,
`csp-sim::bubble::surface_pose_*`).

## Verified correct (per the focus list)

- **`GreaterEqual` depth fix is right and complete.**
  `perspective_revz` (map.rs:1365–1373) is an *infinite* far-plane
  reverse-Z: `clip.z = near`, so projected geometry satisfies
  `ndc.z = near / −z_view ∈ (0,1]` — z = 0 is reached only at
  infinity. The *only* vertex in the flight stream emitted at z = 0 is
  the fallback sky quad (flight.rs:1588–1598), so `Greater` against a
  0.0-cleared buffer culled it to the clear color — exactly the
  "uniform purple" bug. `GreaterEqual` (csp-render/flight.rs:71–80)
  admits the sky and nothing else changes semantics: no other emitter
  can produce z = 0 (project_into produces z > 0 for every in-front
  point; f32 rounding to 0 would need ~1e45 m). Cross-pass: the
  terrain sky uses `Always` + no depth write (terrain.rs:824–826) and
  terrain/ocean use `Greater` against the same 0.0-cleared shared
  target — also never at the far plane — so the mixed-compare setup
  is coherent. In-order rasterization guarantees the sky (verts 0..6)
  writes depth 0 before craft geometry tests it. `render_gate.rs`
  pins both halves on a real adapter.
- **`LatchedWeldCache` staleness is sound.** A latched craft is
  immutable sim state (jettison is InBubble-only; any mutating command
  unlatches first), `entries.retain` (flight.rs:654–659) drops entries
  the frame the vessel leaves `SurfaceLatched` — bounded by the live
  latched count — and the `(id, fingerprint)` pair re-welds on any
  craft swap (id reuse can't occur; ids are monotonic).
  `craft_fingerprint` gaps (part identity, collider, modules are not
  hashed — only graph/mass/com) can't manifest under that invariant;
  the key is belt-and-braces.
- **Plasma flux mirrors the sim.** flight.rs:675–681 recomputes
  `CONVECTIVE_HEAT_FRACTION · ½ · density · speed³` from
  `csp_phys::flow_sample` — textually identical to
  `thermal.rs:320`/`594`, same `v_rel` (co-rotation included), same
  datum altitude. Parity holds today.
- **Silhouette precedence is consistent.** `classify` checks engine →
  crew → collider (part_mesh.rs:141–166); `part_kind` checks
  `engine.is_some() || Thrust` → crew → tanks → science → port
  (flight.rs:821–847). Same order, and `resolve.rs:1697–1722` always
  produces `EngineSpec` + `Thrust` site as a pair, so the two engine
  probes agree on every part that exists today.
- **Kitbash determinism.** `part_mesh` is pure functions of the
  resolved part; the only sorts use `f64::total_cmp` (no RNG, no
  HashMap iteration). Plume flicker keys off the sim clock, not a
  frame counter — presentation-time only, never sim state.
- **Emission math.** All geometry stays f64 until `project_into`/
  `emit_tri`/`emit_tri3` cast at push; all-or-nothing tri emission
  keeps the unindexed `TriangleList` aligned (test at
  flight.rs:1841–1898 pins it). Plume inner-shell-first ordering is
  correct under `GreaterEqual`+depth-writes (flight.rs:1447–1456).
- **`sun_direction`** correctly derives star→dominant-body in the
  translated bubble frame with an honest fixed fallback; spin
  phase/axis plumbing matches `surface_pose_capture`'s
  `R(axis, ω·t)` convention (bubble.rs:177–213 vs flight.rs:632–637).

## Findings

### 1. MEDIUM — SFX cues and continuous audio track `vessels.last()`, ignoring the focus pin

- **Where:** `crates/csp-view/src/flight.rs:1218` (`sfx_edges` calls
  `focus_vessel(vessels)` = `.last()`); `crates/csp-app/src/audio.rs:509`
  (`let focus = snap.vessels.last()`); vs the real focus at
  `crates/csp-view/src/flight.rs:1128–1131`
  (`focus_override.target.and_then(find).or_else(focus_vessel)`).
- **Problem:** `flight_present`'s actual focus — the chase camera,
  `ui.focus` (the `SetThrottle`/attitude command target via
  `View::flight_focus`, lib.rs:1581–1586), and every HUD mirror —
  honors `FlightFocusOverride`. The per-focus SFX edge detector and
  the audio backend's engine/plasma voice levels do not: they always
  follow the newest staged vessel.
- **Evidence:** the pin persists for as long as the pinned vessel stays
  staged — `target` is only cleared when the vessel leaves the staged
  set after `seen`, leaves the arena, or a fresh launch claims it
  (flight.rs:1121–1127, lib.rs:1157–1164/1758). A `ToBubble`'d vessel
  keeps its old `VesselId`, so after `recover_control` with any newer
  vessel co-staged, the pin is *permanently* diverged from `.last()`
  for the rest of that flight. Concretely: throttling the recovered
  craft fires no ENGINE IGNITION cue (edges watch the newest vessel's
  unchanged throttle) and the engine voice stays at its level;
  conversely a newer vessel's throttle/contact/latch edges flash cues
  and drive audio for a vessel the camera isn't following.
  `sfx_edges`'s own doc (flight.rs:1216) claims "the focus rule is
  `focus_vessel`'s" — inconsistent with `flight_present` 100 lines
  above it.
- **Impact:** missing/spurious cue flashes and wrong-vessel
  engine/plasma audio during the exact recovery flow the pin exists
  for. No sim impact (presentation only) but a live playability desync.
- **Suggestion:** compute the staged focus once in `flight_present`
  (pin-then-fallback) and share it — pass the resolved `focus: Option<&FlightBody>`
  into `sfx_edges`, and mirror it for audio (e.g. carry the focused id on
  `FlightUiData`/`FlightSnapshot`, or have `AudioBackend::update`
  accept the focused vessel rather than `vessels.last()`).

### 2. MEDIUM — Contact dust is anchored to the sim datum, not the rendered terrain

- **Where:** `crates/csp-view/src/flight.rs:1763`
  (`env.body_center + dir * (env.radius_m + 0.3)`), `extent`/`low`
  from `p.lo`/`p.hi` at 1730–1752.
- **Problem:** the dust ring hugs the datum sphere at `radius_m + 0.3`,
  but the surface the player sees is `radius_m + h·amplitude_m`
  (`amplitude_m = 0.004 · radius`, terrain.rs:35/87; ~2.4 km max on a
  600 km body). The same datum-vs-terrain gap already produced the
  buried-pad defect and motivated the *camera* floor's
  `surface_floor_radius` tap (flight.rs:1037–1047) — the dust was not
  given the same treatment.
- **Evidence:** the puff is drawn in the flight vertex stream *after*
  the terrain pass writes depth. Wherever the baked heightfield sits
  more than ~0.3 m above the datum — i.e. effectively every landing
  site with relief — the ring is under the rendered ground and fails
  the depth test; where terrain dips below datum it floats visibly
  in mid-air. `b.contact` is a sim-datum contact flag, so the VFX
  triggers exactly when its placement is most likely wrong.
- **Impact:** the §3.7 touchdown cue is invisible or floating at most
  real touchdown sites; only near-datum-flat ground shows it correctly.
- **Suggestion:** place the ring at the sampled terrain height under
  `low` — reuse `surface_floor_radius` (single-center tap is enough;
  the ±1.5-texel max is a camera-safety bound the dust doesn't need)
  with a small epsilon, and drop the whole emission when
  `packet.terrain.is_none()`… or keep the datum fallback for the
  no-terrain path deliberately.

### 3. LOW — Convective-flux formula is re-derived inline; a thermal-model change drifts the VFX/audio silently

- **Where:** `crates/csp-view/src/flight.rs:675–681` vs
  `crates/csp-phys/src/thermal.rs:320` (and its second copy at :594).
- **Problem:** `plasma_w_m2` shares `flow_sample` but rewrites
  `CONVECTIVE_HEAT_FRACTION * 0.5 * ρ * v³` as a literal expression.
  The sheath's intensity — and the audio whoosh reading the same
  field — is documented as tracking "the same heating the sim
  integrates," but only the *inputs* are shared; the per-area
  transform is a third copy of the formula.
- **Impact:** any change to the convection model in thermal.rs (a
  different exponent, an area/obscuration term, a second fraction)
  leaves the VFX normalizing a lookalike — the exact failure the
  comment claims to avoid. Presentation-only.
- **Suggestion:** expose the per-area convective input from csp-phys
  (e.g. `pub fn convective_per_area_w_m2(flow: &ThermalFlow) -> f64`)
  and call it from both thermal.rs sites and flight.rs.

### 4. LOW — `PLASMA_ONSET/FULL_W_M2` duplicated in audio.rs; sun fallback literal duplicated in flight.rs

- **Where:** `crates/csp-app/src/audio.rs:96–98` =
  `crates/csp-view/src/flight.rs:46–48` (values equal today:
  5.0e3 / 8.0e4); sun fallback `(0.4, 0.3, 0.86)` at flight.rs:593
  vs :1621.
- **Problem:** two normalizations of the same staged field live in two
  crates (audio is allowed to depend on csp-view — it already reads
  `snap.vessels`). The doc comments acknowledge the mirror but nothing
  mechanically ties them; the cue-vocabulary table got a drift
  detector (`unmapped` warn, audio.rs:496–502) while the window got
  none.
- **Impact:** silent glow/whoosh desync if one side is retuned.
- **Suggestion:** export the constants (or better, the normalized
  `plasma_i`) from `csp_view::flight` — `FlightUiData` already mirrors
  HUD quantities — or at minimum a unit test asserting the two
  literal pairs are equal. Same for the sun fallback unit vector —
  hoist it to one `const`.

### 5. LOW — `part_kind` and `classify_weld_part` use different engine probes

- **Where:** `crates/csp-view/src/flight.rs:822–827`
  (`wp.engine.is_some() || has Thrust site`) vs
  `crates/csp-view/src/part_mesh.rs:196–201` (Thrust sites only).
- **Problem:** the livery classifier treats `WeldPart::engine` as an
  engine signal the silhouette classifier ignores. They agree today
  only because `resolve.rs:1697–1722` sets both in the same match arm.
  A `WeldPart` that ever carries `engine: Some` without a `Thrust`
  site (a future module shape, a migration of unmetered records)
  renders Engine-tinted as a plain cylinder/box instead of the bell —
  and the reverse case (site without spec, the unmetered-craft path)
  is already handled only because `part_kind` ORs both.
- **Impact:** tint/shape divergence on a future or migrated data edge;
  no current-content failure.
- **Suggestion:** make `classify_weld_part` take the same predicate —
  e.g. `part.engine.is_some() || any(Thrust)` — so the two
  classifiers cannot drift, or document explicitly that they
  intentionally differ.

### 6. INFO — `stage_latched` never checks `SurfacePose.body` against the environment dominant

- **Where:** `crates/csp-view/src/flight.rs:705–709, 727–761`;
  `SurfacePose.body` at `crates/csp-sim/src/bubble.rs:147`.
- **Problem:** the pose record carries the body it was captured
  against; `surface_pose_restore` implicitly assumes it names the
  current environment dominant. `stage_latched` skips the record
  entirely if env is missing but never compares `pose.body` to
  `dominant_body()` — a mismatched record (if one can ever exist) is
  restored against the wrong body rather than skipped.
- **Impact:** none if the sim's invariant ("a latch record always
  names the environment dominant") holds — by construction it
  appears to; flagged so the invariant is confirmed rather than
  assumed. One-line check is cheap insurance in a path that already
  prefers "skip honestly" over guessing.

## Notes on the new visual pass

- The two-shell plume's draw order (inner core before mantle) is
  required *and* correct: with depth writes on, a first-drawn mantle
  front wall is strictly nearer than the enclosed core and would cull
  it under either compare function (flight.rs:1447–1456).
- The hull-hugging sheath via `inflated` is geometrically sound:
  station-order clamping prevents self-intersection on flat parts
  (part_mesh.rs:482–493), and the margin-clamped-to-z_span case means
  flat parts get a coincident shell that wins `GreaterEqual` ties —
  acceptable and presumably intended.
- Translucent layers (plume/sheath/dust) all write depth in the one
  pipeline. Emission order parts→plumes→sheath→dust is a reasonable
  painter's compromise; nothing observed produces a wrong image, but
  per-vessel overlap between translucent shells is resolved by depth,
  not sorting — a cosmetic limitation, not a defect.
