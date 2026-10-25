# Verified Report 12 — csp-view render-extract layer (verified against HEAD 1e8c6da, working tree clean for crates/)

All six findings re-verified against the CURRENT source; no concurrent-edit fixes landed (crates are unmodified since 1e8c6da). Line numbers below are current-HEAD lines; original cites that drifted are noted.

### Finding 1: .csar image entries unvalidated → renderer panic instead of degradation — CONFIRMED
**Original**: Image branch of `decode` trusts header `w/h/layers/mips` and inflates the payload with no length cross-check (the mesh branch has one); renderer slices `tex.data[src..src+level_bytes]` and panics; one bad entry aborts the whole pack.
**Verification**:
- art.rs:224-243 (image branch): `let data = r.inflate()?;` → `Entry::Image(TerrainTex { w, h, layers, mips, means, data })` — no `data.len()` check, no `w>0/h>0/layers>0` check, no `mips <=` full-chain check. Mesh branch art.rs:247-249 has exactly the asymmetry claimed: `if raw.len() != nv * 32 + ni * 4 { return Err(...) }`.
- Panic sites, csp-render/src/terrain.rs (write_art_tex now at 1201-1262, called from upload_art 1267-1288 on first draw of real art): line 1228 `tex.data[src..src + level_bytes].to_vec()` and line 1235 `&tex.data[s..s + row]` — direct slice panics ("range end index out of range") whenever Σ level_bytes overclaims `data.len()`. Separately, `create_layers` (762-780) calls `device.create_texture`, which panics on wgpu validation for `w==0`, `h==0`, or `mip_level_count` exceeding the full chain for the claimed dims.
- No defense in between: `TerrainTexSet::finish` (art.rs:136-149) only hashes `data.len()/w/h` into a content key — never validates the length. `packs_decode` (art.rs:385-395) asserts only `terrain.is_some()`, five model keys, `albedo.layers == 4`, `water.mips > 4` — a short-payload or dims-drifted rebake ships green and panics at first upload.
- Cross-ref confirmed: `write_csar` (xtask/src/bin/bake_art.rs:110-160) writes dims + deflate payload with no checksum and no length-vs-dims assert (zero `assert`/`debug_assert` in that file); the format itself carries no protection, so decode is the only line of defense.
- All-or-nothing confirmed: `decode` aborts on first bad entry (`k => return Err(...)` art.rs:303; any `Err` read likewise), and `load()` (art.rs:94-121) drops the entire pack — every part model → kitbash — on a single bad entry.
- Nuance: the in-repo bake builds dims and payload from the same loop (`image_entry`, bake_art.rs:489-533), so a normal rebake won't trigger this today; the panic path requires format drift, a future bake bug, or pack corruption — exactly the class §2.13 ("a corrupt asset degrades, it doesn't kill", art.rs:158) and the drift-detector test claim to cover, and don't. HIGH stands.

### Finding 2: InBubble vessel polyline cache defeated by epoch in key — ~8.4k Kepler solves per map frame — CONFIRMED
**Original**: `capture_sync_input` stamps a fresh osculating fit `epoch: sim.clock()` per frame; `map_present` compares whole `OrbitState` (PartialEq incl. epoch) so the cache can never hit; ~65 fits + ~8,400 state_at solves per map frame at the debris cap.
**Verification**:
- map.rs:1210-1224 (capture_sync_input): InBubble arm fits `cartesian_to_elements` every call and stamps `epoch: sim.clock()` (line 1219).
- map.rs:673-682 (map_present): `is_none_or(|(cached, _)| cached != orbit)` — whole-state compare; `OrbitState` derives `PartialEq` over `(parent, elements, epoch)` (csp-sim/src/orbit.rs:212-220), and the epoch advances every frame → guaranteed miss.
- Epoch-invariance of the polyline confirmed: `state_at` = `elements_state_at(&s.elements, mu, tick_delta_seconds(t, s.epoch))` (orbit.rs:1034-1038), and `sample_orbit` (map.rs:1268-1324) samples t over `[epoch, epoch+period]` — positions depend only on `t − epoch`, so with identical elements the 129-point polyline is bit-identical regardless of epoch.
- Magnitude confirmed: `ORBIT_LINE_SAMPLES = 128` (map.rs:51) → 129 samples per line; `max_debris: 64` (data/balance.ron:15) + the live craft → ~65 fits + ~8.4k solves per map frame after a breakup.
- Unconditional fit confirmed: lib.rs:1213-1214 `end_frame` inserts `map::capture_sync_input(&self.sim)` with no map-open gate — the osculating fits run every frame even with the map closed.
- Nuance (does not change the verdict, sharpens the fix): for free-falling InBubble debris the fitted elements also drift frame-to-frame under bubble integration, so keying on `(parent, elements)` alone would still miss exactly; the report's alternative — epsilon-quantized invalidation — is the variant that actually eliminates the churn. Also note map.rs:1195-1199 documents per-frame re-sampling as intended for the live vessel; the cost problem at 64-debris scale is the part that's real.

### Finding 3: static per-part data re-derived/deep-cloned per frame for live vessels — CONFIRMED
**Original**: `vessel_parts` (full `body_transforms` fold, per-part collider-vertex AABB fold, `classify_weld_part`) runs per InBubble vessel per frame with no cache; `sync_flight` deep-clones `Vec<FlightBody>`; `emit_model_part` allocates a fresh `Vec<ModelTri>` per part per frame beside a sibling scratch-Vec pattern.
**Verification**:
- flight.rs:865: `VesselState::InBubble` arm calls `vessel_parts(vessel.craft.as_ref(), &body.collider.hulls, art)` unconditionally — the `LatchedWeldCache` fingerprint path exists only for `SurfaceLatched` (stage_latched, flight.rs:945-974: `(vessel id, craft fingerprint)` miss → weld).
- flight.rs:1228 (`weld::body_transforms` fold), 1243-1249 (per-part collider-vertex AABB fold), 1250 (`classify_weld_part` — the ring-profile sort/rescan) all run inside `vessel_parts`, per call. All static per craft topology, invalidated only by staging/jettison — exactly what the latched cache exists to avoid.
- flight.rs:1299-1302 (`sync_flight`): `snap.vessels = staged.vessels.clone();` — full deep clone incl. `parts: Vec<FlightPart>` (each `PartShape::Lathe` carries a heap `Vec` of stations, part_mesh.rs:60-72), `plumes`, `tanks` — so every capture is built twice per frame.
- flight.rs:2282: `let mut mtris = Vec::new();` fresh per `emit_model_part` call (per part per frame), while the kitbash path two loops above reuses a scratch via `tris.clear()` (flight.rs:2416, 2430). Pure avoidable churn, as claimed.

### Finding 4: blob-shadow anchor wrong under camera clamp — CONFIRMED
**Original**: `solve_chase_camera` clamps the eye out of the terrain but reports stale `input.distance_m`; `vessel_shadow` recovers the craft as `eye − z_out·distance_m`, displaced by `|eye−focus| − input.distance_m` whenever the clamp engaged.
**Verification**:
- flight.rs:1350-1360: `if r < min_r { eye = env.body_center + rel * (min_r / r); }` — eye rescaled radially; flight.rs:1392: `distance_m: input.distance_m` — the pre-clamp distance is reported.
- terrain.rs:373: `let craft_rel = cam.eye - env.body_center - z_out * cam.distance_m;`. The `z_out` here is genuinely the chase camera's post-clamp basis: `sky_view_rot` (terrain.rs:337-344) recovers column 2 as `−vp[3][c]`, and `solve_chase_camera` computes its z_out from the *clamped* eye (flight.rs:1364-1371, `eye − focus.com`). So the recovered craft is off by exactly `(|eye−focus| − input.distance_m)·z_out` ≠ 0 whenever the clamp fires.
- `craft_alt` from the displaced craft flows into the atmo uniform (`atmo` array z-slot, terrain.rs build_packet ~527) — misreported altitude confirmed. Called every flight frame via build_packet (terrain.rs:470) from extract_flight (flight.rs:2339).

### Finding 5: synchronous 524k-texel × 13-octave heightfield bake in-frame on body change — CONFIRMED
**Original**: `TerrainCache::field` bakes `HeightField::bake(seed, 1024, 512)` lazily on a `(body_id, seed)` key miss, synchronously inside Presentation/RenderExtract on the frame a body becomes dominant.
**Verification**:
- csp-view/terrain.rs:195-210: key-miss → `Arc::new(rt::HeightField::bake(seed, HEIGHTFIELD_W, HEIGHTFIELD_H))` inline; `HEIGHTFIELD_W = 1024`, `HEIGHTFIELD_H = 512` (terrain.rs:30-31) → 524,288 texels.
- csp-render/terrain.rs:165-180: bake evaluates `height_normalized` per texel; height_normalized (128-136) runs fbm3 with 4 + 5 + 4 = 13 octaves, all unconditional. Cost basis exact.
- Trigger paths confirmed in-frame: `surface_floor_radius` → `cache.field` (terrain.rs:276), called from `solve_chase_camera` (flight.rs:1355) in `flight_present` (Presentation); and `build_packet` → `cache.field` (terrain.rs:424) from `extract_flight` (flight.rs:2339, RenderExtract). Key is `(body_id, seed)` so every dominant-body swap (SOI transfer) re-bakes on the transfer frame.

### Finding 6: `Box::leak` of entry names per `art::load()` per View construction — CONFIRMED
**Original**: `decode` leaks each entry name to `'static` for the `Vec<(&'static str, Entry)>` return type; `load()` runs per `View` construction, so the leak is monotonic across View lifetimes.
**Verification**:
- art.rs:222: `let name: &'static str = Box::leak(name.into_boxed_str());` inside the per-entry loop; signature `fn decode(b: &[u8]) -> Result<Vec<(&str, Entry)>, String>` (art.rs:207).
- lib.rs:937: `let art = art::load();` in `from_source` (the construction path for `View::new` at 792 and `new_demo` at 955) — both packs' entry names leak on every View construction (app restart, every integration test that builds a View). Leaks also persist for entries parsed before a later entry errors. Slow but real, in a workspace that otherwise polices allocation churn.

### Follow-ups (one-liners, all confirmed at HEAD)
- **All-or-nothing tri emit pops** — CONFIRMED: `emit_tri` (flight.rs:2082-2096) projects all corners and drops the tri whole if any is behind the eye plane ("pushed whole or dropped whole"); same rule in the model path (2286-2288) and kitbash loop (~2437). Pops unlit geometry crossing the eye plane (plume/dust quads with the camera inside them).
- **Light hemisphere keyed to first staged vessel, not resolved focus** — CONFIRMED: flight.rs:2396-2407, `snap.vessels.first().map(|b| b.com − e.body_center …)` builds `hemi` — wrong up-vector for part-lighting ambient whenever the focus is not `vessels[0]`.
- **Map camera snaps to world origin for a frame when focus `state_at` fails** — CONFIRMED: map.rs:691-696, `.unwrap_or(DVec3::ZERO) + input.pan_offset` — the camera solves at the origin + pan offset for that frame (InBubble fitted hyperbolic/failed states can produce the `None`).
- **`TerrainTexSet::finish` silently drops the whole terrain art set** — CONFIRMED: art.rs:136-137, `(self.albedo?, self.normal?, self.water?)` → `None` with no log when any of the three is missing — all-or-nothing with zero observability.

## Summary
6 confirmed, 0 dismissed, 0 fixed-by-concurrent-edit. All four follow-up one-liners confirmed.
