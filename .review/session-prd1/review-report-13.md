# PRD-13 review — Scene presentation & extract

**Scope:** `crates/csp-view/src/{flight.rs, terrain.rs, part_mesh.rs, art.rs, structures.rs, demo.rs}` (+ cross-checks into `csp-sim/src/{lib.rs, bubble.rs}`, `csp-phys/src/simple.rs`, `csp-core/src/terrain.rs`, `csp-data/src/validate.rs`, `data/systems/home/sites.ron`, `apps/demo/data/`).

**Verdict:** WIRED and largely honest. The terrain-vs-datum fix is real: `csp-phys`'s `TerrainSurface`, `csp-render`'s displacement, `csp-view`'s `ground_radius`/`surface_floor_radius` all sample the *same* `csp_core::terrain::HeightField::bake(seed, 1024, 512)` (`csp-phys/src/simple.rs:458`, `csp-render/src/terrain.rs:63`, `crates/csp-view/src/terrain.rs:34-36`), and the shipped launch site is authored where that field is ~datum-flat (+0.02 m, `data/systems/home/sites.ron`). The pad the craft spawns on does match the rendered pad. The real gaps are (a) the complex advertises facilities with zero gameplay behind them — the user's complaint verified in code — and (b) three residual terrain-vs-datum edge cases (async-bake flat stand-in, `with_sim` seed divergence, spawn altitude trusting authored `altitude_m`).

---

## Findings

### F1 — Space center presents facilities that have no gameplay behind them — MEDIUM-HIGH

- **Where:** `crates/csp-view/src/structures.rs:250-538` (the `SPACE_CENTER` layout), emitted at `crates/csp-view/src/flight.rs:2564-2584`.
- **Problem:** The scene draws a full KSC — launch mount with flame trench, service gantry with three arms reaching to the pad slot (`structures.rs:362-374`), VAB with a 28 m crawler door (`structures.rs:446-456`), crawlerway that correctly meets the VAB's south door (road L ends at door latitude — the layout is honest), propellant farm, west annex, support sheds — and none of it does or says anything. The module doc itself states visual-only (structures.rs:1-33). No pick, no tooltip, no state link: the propellant farm doesn't reflect `vessel_tanks`, the gantry never retracts or connects, the VAB door can't be entered — V (`KeyAction::ToggleVab`, `csp-ui/src/settings.rs:542`) opens a completely separate editor mode with no visual bridge to the building. `emit_launch_complex` is one-way vertex emission; there is no interaction path anywhere in `flight.rs`/`map.rs` for structures.
- **Evidence:** the user's complaint ("space center has no gameplay, graphics still weak") is confirmed here, not missing: the complex exists *only* as lit geometry sharing `packet.light`.
- **Impact:** a scene that reads as KSP's KSC implicitly promises KSC verbs (enter VAB, roll out, fuel up) that don't exist; the gap between what the scene shows and what the game does is the complaint's heartland.
- **Suggestion:** two honest moves, cheap first — (a) make the VAB/pad clickable affordances (map marker `MapLabelKind::Site` exists — map.rs:912-922 — extend it to "enter VAB"/"launch" affordances or at minimum a "scenery" tooltip); (b) connect visuals to sim state that already exists: gantry arm retraction + trench scorch state on liftoff (`debris_since`/`throttle` are already staged), crawlerway rollout animation driven by `launch_craft`, farm tanks tinted from `vessel_tanks` fill fraction.

### F2 — `with_sim` renders terrain the sim never contacts (seed divergence) — MEDIUM (latent)

- **Where:** `crates/csp-view/src/lib.rs:1355-1361` inserts `TerrainConfig::default()` for `with_sim`; `crates/csp-view/src/terrain.rs:88-95` `TerrainBody::synthesized` then seeds from `seed_of(body_id)` — a name hash, **not** `TerrainDef::noise_seed`.
- **Problem:** the sim's contact surface bakes `body.terrain.noise_seed` (`csp-phys/src/simple.rs:453-460`). A `with_sim` world whose physics env carries a TerrainDef renders a completely different heightfield than the solver contacts — the "buried pad" class at full ±0.4%·radius amplitude (≈2.4 km at Terrin).
- **Evidence:** `with_sim` callers today are `map.rs:1905` and `tests/vab_latency.rs:42` — tests/headless — so nothing player-facing hits it; but it is exactly the terrain-vs-datum mismatch this cluster was asked to hunt, latent for any headless screenshot/e2e path that reuses `with_sim`.
- **Impact:** any future render-through-`with_sim` consumer (bench screenshots, MCP-driven capture, tests asserting on the flight packet) silently measures a ground that isn't the contact surface.
- **Suggestion:** `with_sim` already calls `SiteTable::from_sim(&sim)` (lib.rs:1359) — do the same for terrain: build `TerrainConfig` from `sim.config()`'s body `terrain.noise_seed`/radius/ocean datum instead of `seed_of`, so the synthesized fallback only covers bodies truly absent from config.

### F3 — Flat-datum stand-in during the async heightfield bake — LOW-MEDIUM (transient)

- **Where:** `crates/csp-view/src/terrain.rs:186-192, 217-303`.
- **Problem:** on native, `TerrainCache::field` returns a zero-height stand-in `HeightField` while the ~0.5M-texel × 13-octave bake runs on a worker thread. Every sampler — drawn displacement, `ground_radius` (structure anchors), `surface_floor_radius` (camera floor, dust floor), `patch_might_hold_sea`, `vessel_shadow` — measures datum-flat during that window, while the sim keeps contacting the real field. When the bake lands, the drawn surface pops 0→real heights and the whole space center visibly re-anchors.
- **Evidence:** at the shipped launch site (+0.02 m) the window is invisible; for a vessel landed/parked on relief (up to ~±2.4 km at `AMPLITUDE_FRAC·radius`, `csp-core/src/terrain.rs:36`) the first seconds render it inside the flat sphere, and a downward camera drag can enter terrain the clamp will only learn about after the swap.
- **Impact:** a one-time "the world pops into place" moment per body per session; wrong-side-of-terrain rendering for non-pad situations during the window.
- **Suggestion:** prewarm the bake at `View::from_source` (system load) or gate the terrain packet on `pending.is_none()` — the current design honestly admits the trade-off in the doc comment but the pop is player-visible; at minimum keep `emit_launch_complex` from anchoring until the real field lands so buildings don't visibly jump.

### F4 — Spawn pose trusts authored `site.altitude_m`; nothing enforces "pad site ≈ flat" — MEDIUM (latent data hazard)

- **Where:** `crates/csp-view/src/lib.rs:2146-2190` (`altitude_m: site_alt + PAD_CLEARANCE_M - z_min`, `PAD_CLEARANCE_M = 0.0` at lib.rs:70); `crates/csp-data/src/validate.rs:2029-2125`.
- **Problem:** `launch_craft` restores the pad pose against datum + authored altitude — it never samples the heightfield. The sim contact surface is `radius + h·amplitude`. Today's sites.ron was deliberately authored at a +0.02 m flat spot (the file documents the +57 m lon-π counterexample in comments), but `validate_sites` checks only pose finiteness/range/one-launch-per-body — moving the site or changing `noise_seed` re-creates the buried-pad spawn with no tool warning, and the structures would re-anchor to the new terrain while the spawn would not.
- **Impact:** silent coupling between content and code that only comments guard; a data edit produces a spawn intersecting the contact surface (solver ejects it over settle ticks — a visibly wrong spawn).
- **Suggestion:** a validator or loader check that samples `HeightField` at each site's lat/lon and warns when `|altitude_m − h·amplitude|` exceeds ~1 m (a coarse bake is fine — it's data validation, not the frame path); or make the spawn altitude derive from the sampled contact radius, mirroring `surface_floor_radius`.

### F5 — Map site marker anchored to datum+altitude, not rendered terrain — LOW

- **Where:** `crates/csp-view/src/structures.rs:856-867` (`dir_fixed * (env.radius_m + site.altitude_m)`), consumed at `crates/csp-view/src/map.rs:908-915`.
- **Problem:** markers sit at `radius_m + altitude_m` while the complex sits at `ground_radius` — the marker/structure pair diverges by the local terrain height at sites on relief. At the launch site (+0.02) invisible; the two other authored sites carry `altitude_m: 0.0` on unverified terrain.
- **Impact:** marker floats/buries by local height at close zoom (gate is <6 radii, so hundreds of metres matters).
- **Suggestion:** same `ground_radius` sample the structures use — the map extract has `terrain_cfg`/`cache` available in `extract_flight`'s sibling system.

### F6 — Flight model keying drops part identity (Ember renders as Sparc's model) — LOW

- **Where:** `crates/csp-view/src/flight.rs:1428-1434` vs `crates/csp-view/src/vab.rs:342, 1154` vs `crates/csp-view/src/art.rs:375-432`.
- **Problem:** `vessel_parts` keys baked models as `model_key(None, kind, pointed)` — kind-family only — even though `craft.part_ids` is populated on the launch path (`lib.rs:2099-2106`) and kept parallel by the sim (`csp-sim/src/lib.rs:9670`). The VAB path keys by `part_id`, so `csp.engine.ember` → `engine_b` in the editor but `engine_a` in flight; `.booster`/`.fin`/`.fairing`/`.aero` variants likewise collapse to kind defaults.
- **Impact:** the same part changes silhouette class between build and flight; art pack investment is invisible for variant parts (also true for debris, which keys identically).
- **Suggestion:** pass `craft.part_ids.get(node.effective_index)` into `model_key` when `part_ids` is non-empty; keep the kind fallback for `part_ids: vec![]` crafts (`stock_flight_craft`, lib.rs:3100).

### F7 — Pod taper differs VAB↔flight — LOW

- **Where:** `crates/csp-view/src/part_mesh.rs:196-203` vs `:175-190`.
- **Problem:** `classify_weld_part` passes `None` for top diameter → `pod_profile` always tapers to `0.5·r_base`; `classify_effective` uses the resolved top-attach `diameter_m`. The stock pod's 0.625-on-1.25 ratio coincidentally equals the default, but any pod with a different top node class shows a different capsule taper in flight than in the editor.
- **Suggestion:** `CraftDef` doesn't carry attach nodes — either stash the resolved top diameter on `WeldPart` at weld time or accept the documented default (comment already admits the coincidence).

### F8 — Minor presentation nits — LOW

- **Pad berth vs craft width:** the flame-trench slot is x∈[−3,3] m (`structures.rs:284-296`); spawn has no width check — any craft part radius >3 m visually clips the mount pedestals. Today's catalog tops out ~0.625 m, fine; worth an authored-width sanity note.
- **Contact-shadow quads are flat at +5 cm over the footprint-max anchor** (`structures.rs:736-769`) — they float above dips inside the footprint. Cosmetic.
- **Per-frame rebuild:** `emit_launch_complex` re-tessellates the whole complex every extract (`structures.rs:580-848`) — a few thousand tris/frame plus allocs; fine at current scale, but it's unbounded work for a static object (a cached mesh would do).
- **`TerrainCache` pending-drop:** switching dominant bodies mid-bake drops the in-flight `Receiver`, wasting the bake (terrain.rs:225) — revisit re-bakes. Harmless, bounded by body count.

---

## Verified honest (no gap)

- **Spawn↔pad match:** `launch_craft` resolves the dominant body's `launch: true` site from `sim.config()` (lib.rs:2138-2148), restores the §2.6.4 record at the *current* tick's spin phase (`surface_pose_restore`, bubble.rs:316-339) with co-rotation `ω × r` on the CoM slot — the craft lands in the flame-trench slot at the site origin, between the mount pedestals, upright ENU. Belly rests at datum; terrain at the site is +0.02 m, apron plate top ~+0.12 — reads as parked on the pad.
- **Structures' ENU/spin math:** `site_frame` = `R(axis, ω·t)·surface_upright(lat,lon)` (structures.rs:124-143) — identical convention to `surface_pose_restore` and the shader's `unspin`; per-piece ground anchors re-sample `ground_radius` (max over footprint ring), so buildings never float or sink.
- **Pad berth honesty:** `spawn_site_occupied` counts InBubble hulls + re-materialized latched poses (csp-sim/lib.rs:5361-5420), checked pre-flight (view lib.rs:2405-2411) and re-checked at apply — a launch pad is a single berth, correctly enforced.
- **Latched vessels render:** `stage_latched` restores pose per frame with cached weld data (flight.rs:1055-1140) — parked craft stays on the pad, focusable, throttle unlatches.
- **Plume/sheath/dust honesty:** plumes scale by `throttle × delivered` (live `rated_n` share — flight.rs:2305-2396); plasma uses the same `flow_sample` the thermal tick integrates (flight.rs:997); dust anchors to `surface_floor_radius`, not datum (flight.rs:2788-2796); latched craft forces `contact:false` deliberately (documented, flight.rs:1109-1112).
- **`art.rs`:** bounds-checked csar decode, bounded inflate, payload-shape cross-check before upload, graceful fallback, `packs_decode` test gates drift — solid.
- **`demo.rs`:** embedded FILES manifest diffs against disk in a test (`demo_files_match_disk`); demo `sites.ron` values match shipping (comments differ, rask-mines intentionally absent); STARTER_CRAFT resolves to pod/s160/sparc — flight-capable.
- **`part_mesh`:** classifier/lathe/ogive/inflate math sound; normals verified outward in tests; `emit_model` inverse-transpose normals correct for the affine fit; zero-area and out-of-range indices dropped, never corrupt.

## Hand-off notes

- F1 is the user's complaint made concrete — it's a design/UX gap the parent should route to whoever owns the "space-center gameplay" answer (likely lands across csp-ui affordances + a state-driven structures pass).
- F2/F3/F4 are the remaining terrain-vs-datum edge cases; F2 and F4 are latent (today's content + call paths are correct), F3 is player-visible but transient.
- No code changes were made; this was a read-only review. Commands run: `cat`, `diff`, `grep`/`rg`, `sed` reads only.
