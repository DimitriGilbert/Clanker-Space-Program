### [SEVERITY: HIGH] Finding 1: .csar image entries trust header dims/payload with zero validation — corrupt pack panics in the renderer instead of degrading
**File**: crates/csp-view/src/art.rs:[224-243] (panic site: crates/csp-render/src/terrain.rs:1266-1270)
**Problem**: The mesh branch of `decode` validates its payload explicitly (`if raw.len() != nv * 32 + ni * 4 { return Err(...) }`), but the image branch reads `w`, `h`, `layers`, `mips` from the header and inflates the payload without ever cross-checking `data.len()` against the dimensions the renderer will index. `load()` then hands the `TerrainTex` straight to the renderer, whose upload slices `tex.data[src..src + level_bytes]` for each mip level — an out-of-bounds slice panic (or a wgpu width-0 validation panic for `w == 0`) at the first terrain draw. This contradicts the module's own contract ("a corrupt asset degrades, it doesn't kill", art.rs:158) and §2.13's no-panic rule, and the drift detector does not catch it: `packs_decode` (art.rs:386) only asserts `terrain.is_some()` and a couple of dims — a rebake that emits short payloads or disagrees with its own header ships green and panics at startup. Additionally, `decode` aborts the *entire* pack on the first bad entry (`k => return Err(...)`; any `Err` in `load()` discards all previously decoded entries), so one corrupt texture drops every part model to kitbash — the coarsest possible degradation granularity.
**Evidence**:
```rust
1 => {
    let (w, h) = (u32::from(r.u16()?), u32::from(r.u16()?));
    let (layers, mips) = (u32::from(r.u8()?), u32::from(r.u8()?));
    ...
    let data = r.inflate()?;
    out.push((name, Entry::Image(TerrainTex { w, h, layers, mips, means, data })));
```
vs. the mesh branch two lines below:
```rust
let raw = r.inflate()?;
if raw.len() != nv * 32 + ni * 4 {
    return Err("mesh payload size mismatch".to_string());
}
```
**Impact**: A truncated-but-zlib-valid (or rebake-drifted) `terrain.csar` panics at the first flight frame rather than falling back to the procedural surface; a single bad entry silently kills the whole art catalog. Player-visible symptom is a crash on launch, not a fallback.
**Suggestion**: After `inflate()`, validate `data.len() == layers * Σ_{m<mips} (max(1,w>>m) * max(1,h>>m)) * 4` and reject non-positive `w`/`h`/`layers`, mirroring the mesh check. Degrade per entry (log + skip) instead of aborting the whole pack, and extend `packs_decode` to assert the length invariant for every image entry.

### [SEVERITY: MEDIUM] Finding 2: InBubble vessel polyline cache is defeated every frame by the epoch in the cache key — O(vessels × 129) Kepler re-solves per map frame
**File**: crates/csp-view/src/map.rs:[1216-1223] and map.rs:[673-682]
**Problem**: `capture_sync_input` fits a fresh osculating conic for every InBubble vessel every frame stamped `epoch: sim.clock()`. `map_present`'s invalidation test compares the *whole* `OrbitState` (`cached != orbit`, `PartialEq` includes `epoch`), so the equality can never hold across frames for an InBubble vessel — its 129-point polyline is fully re-sampled every map frame, even when the elements are bit-identical (coasting debris). `state_at` is `elements_state_at(&s.elements, mu, t − s.epoch)` (csp-sim/orbit.rs:1035-1038), so a full-period sampled polyline is epoch-invariant — the epoch in the key buys nothing but the re-sampling. The per-frame osculating fit itself also runs unconditionally in `View::end_frame` (lib.rs:1213) even with the map closed.
**Evidence**:
```rust
elements: csp_sim::orbit::cartesian_to_elements(rel, body.vel, env.mu_m3_per_s2).ok()?;
Some((*id, OrbitState { parent: dom.clone(), elements, epoch: sim.clock(), ... }, ...))
```
```rust
let stale = scene.vessel_lines.get(id).is_none_or(|(cached, _)| cached != orbit);
if stale { let line = sample_orbit(model, orbit); ... }
```
**Impact**: With the breakup cap at `max_debris: 64` (data/balance.ron), opening the map after a breakup costs ~65 osculating fits + ~8,400 Kepler solves per frame in Presentation — frame drops in exactly the scenario (debris field, map view) where the player inspects the aftermath. Worse on the wasm demo tier.
**Suggestion**: Key the vessel-line cache on `(parent, elements)` (or store the epoch separately and compare elements only); alternatively quantize the invalidation (re-sample when elements move beyond an epsilon). Gate the osculating fit in `capture_sync_input` on the map being open, or fit lazily in `map_present` from staged `(pos, vel)` pairs.

### [SEVERITY: MEDIUM] Finding 3: InBubble craft silhouette/transform data is re-derived and deep-cloned every frame — the caching that exists for latched vessels is missing where the vessel is actually moving
**File**: crates/csp-view/src/flight.rs:[865], [1190-1269], [1299-1302], [2282]
**Problem**: `capture_sync_input` calls `vessel_parts` for every InBubble vessel every frame: a full `weld::body_transforms` graph fold (flight.rs:1228), a per-part collider-vertex AABB fold (1243-1249), and a per-part `classify_weld_part` (1250) whose `ring_profile` sorts and rescans the collider point set — all static per craft topology, invalidated only by staging/jettison. `sync_flight` then deep-clones the whole `Vec<FlightBody>` (parts, profile `Vec`s, `Arc` clones) into the mirror, and `emit_model_part` allocates a fresh `Vec<ModelTri>` per part per frame (`let mut mtris = Vec::new()`, flight.rs:2282) immediately after the sibling kitbash path demonstrates the scratch-`Vec` reuse pattern (flight.rs:2416/2430). `LatchedWeldCache` exists precisely to avoid this work — but only for parked craft, not the live one.
**Evidence**:
```rust
let parts = vessel_parts(vessel.craft.as_ref(), &body.collider.hulls, art);   // every frame, every InBubble vessel
...
snap.vessels = staged.vessels.clone();                                        // full deep clone per frame
...
fn emit_model_part(...) { let mut mtris = Vec::new(); crate::part_mesh::emit_model(m, mlo, mhi, &mut mtris); ... }
```
**Impact**: A §5.4 300-part vessel costs ~1,000+ small allocations and a redundant classify pass per frame at 60 fps (~60k+ allocs/s), on top of the double construction (capture + sync clone). GC-less native jank and allocator pressure exactly proportional to craft size; the per-part `Vec::new()` in the model path is pure avoidable churn.
**Suggestion**: Reuse `LatchedWeldCache`'s `(vessel id, craft fingerprint)` caching for InBubble vessels (the fingerprint already exists), clone `Arc<Vec<FlightPart>>` through sync instead of deep-cloning, and pass a scratch `&mut Vec<ModelTri>` into `emit_model_part` like the `tris` scratch two loops above.

### [SEVERITY: MEDIUM] Finding 4: The blob-shadow anchor recovers the craft as `eye − z_out · distance_m`, which is wrong whenever the terrain clamp has moved the eye
**File**: crates/csp-view/src/terrain.rs:[373] with crates/csp-view/src/flight.rs:[1350-1361, 1392]
**Problem**: `solve_chase_camera` clamps the eye out of the terrain (`eye = env.body_center + rel * (min_r / r)`) when a drag would bury it, but still reports `distance_m: input.distance_m` — not the actual post-clamp `|eye − focus|`. `vessel_shadow` reconstructs the craft as `cam.eye − env.body_center − z_out * cam.distance_m`, i.e. assumes the reported distance is the true eye-focus distance along the recomputed `z_out`. When the clamp engages, the recovered "craft" is displaced along the view axis by `|eye − focus| − input.distance_m`.
**Evidence**:
```rust
// flight.rs — clamp moves the eye, distance_m is not updated:
if r < min_r { eye = env.body_center + rel * (min_r / r); }
...
MapCameraF64 { view_proj: ..., eye, distance_m: input.distance_m, valid: true }
```
```rust
// terrain.rs — trust the stale distance:
let craft_rel = cam.eye - env.body_center - z_out * cam.distance_m;
```
**Impact**: With the camera dragged low over reliefled terrain (the clamp's exact trigger), the vessel blob shadow anchors tens to hundreds of meters away from the craft and `craft_alt` (fed to the atmo uniform) is misreported — a visibly sliding/offset shadow on the ground during landed scenes with a low camera.
**Suggestion**: Store the solved focus (or the true post-clamp distance) in `MapCameraF64` — e.g. add a `focus: DVec3` field written by `solve_chase_camera` — and have `vessel_shadow` use it directly. Presentation-only data, no sim read needed.

### [SEVERITY: MEDIUM] Finding 5: Heightfield bake (524k texels × 13 fbm octaves) runs synchronously inside Presentation/RenderExtract on the frame a body becomes dominant
**File**: crates/csp-view/src/terrain.rs:[195-210, 276, 424]
**Problem**: `TerrainCache::field` bakes lazily on a `(body_id, seed)` key miss: `HeightField::bake(seed, 1024, 512)` = 524,288 `height_normalized` evaluations, each 3 fbm3 calls totalling 13 octaves of trig-heavy value noise. The first trigger is inside `solve_chase_camera`'s floor probe during `flight_present` (via `surface_floor_radius`) or `build_packet` in `extract_flight` — a synchronous, single-frame stall. The key is per dominant body, so an SOI transfer mid-flight (`FlightEnv::body` swaps) re-bakes on the transfer frame.
**Evidence**:
```rust
if self.key.as_ref() != Some(&key) {
    self.field = Some(Arc::new(rt::HeightField::bake(seed, HEIGHTFIELD_W, HEIGHTFIELD_H)));
    self.key = Some(key);
}
```
**Impact**: A tens-to-hundreds-of-milliseconds frame hitch at launch (first flight frame) and again on every dominant-body change — a visible stutter at exactly the moment a SOI transition is already the most dramatic event on screen.
**Suggestion**: Pre-bake authored bodies at `View` construction (or in `config_from_bodies`), keeping the lazy path only for synthesized fallback bodies; or amortize the bake across frames (double-buffer: serve the synthesized flat floor while the Arc bakes).

### [SEVERITY: MEDIUM] Finding 6: `Box::leak` of every pack entry name on each `art::load()` — an unbounded leak across View constructions
**File**: crates/csp-view/src/art.rs:[222]
**Problem**: `decode` leaks each entry's name into `'static` to satisfy the `Vec<(&'static str, Entry)>` return type. `load()` runs once per `View` construction (`from_source`, lib.rs:937) — every `View::new`/`new_demo` (app restart, every integration test that builds one) leaks both packs' entry names forever. The leak also persists for entries parsed before a later entry aborts the decode.
**Evidence**:
```rust
let name = String::from_utf8_lossy(r.take(nlen)?).into_owned();
let name: &'static str = Box::leak(name.into_boxed_str());
```
**Impact**: Small per call (a few hundred bytes × entries) but strictly monotonic across View lifetimes — a genuine (if slow) memory leak in the loading path, in a workspace that otherwise polices allocation churn.
**Suggestion**: Return `Vec<(String, Entry)>` and key `ArtInner::models`/name matching by `&str` — the two call sites (`load`, `TerrainTexSet`) already match on borrowed `&str` literals, so nothing needs ownership.

Follow-ups: all-or-nothing triangle emit (flight.rs:2080) pops unlit geometry that crosses the eye plane (plume/dust quads when the camera sits inside them — GPU clips the near plane correctly via `w=1` passthrough, so this only bites at the eye plane itself; real fix is polygon clipping at `w = eps`); `packet.light`'s hemisphere up is keyed to `snap.vessels.first()`, not the resolved focus (flight.rs:2401); `map_present` snaps the camera to the world origin for a frame when the focused vessel's `state_at` fails (`unwrap_or(DVec3::ZERO)`, map.rs:695); `TerrainTexSet::finish` drops the whole terrain art set with no log when any one of the four textures is missing (art.rs:135).

## Verdict

Camera math is solid: f64 camera-relative projection, reverse-Z consistency, spin conjugates, and the sky-basis recovery are all correct and well tested — no blockers there.
The real problems sit at the decode and data-flow boundaries: unvalidated .csar image payloads that can panic the renderer (F1), and per-frame re-derivation of static orbit/part data whose caches exist but are keyed or applied wrongly (F2, F3).
Terrain has a synchronous bake hitch on every dominant-body change (F5) and a shadow anchor that breaks under its own camera clamp (F4).
REQUEST CHANGES: F1 before any rebake lands; F2/F3 before breakup-scale scenarios ship; F4–F6 are cheap, well-isolated fixes.
