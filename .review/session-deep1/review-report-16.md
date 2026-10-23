# Deep Review — Cluster C16: `csp-render` (wgpu passes + WGSL)

**Files reviewed (read in full):**
- `crates/csp-render/src/lib.rs` (272) — Gpu/frame lifecycle, `GpuBuffer` growth
- `crates/csp-render/src/map.rs` (129) + `map.wgsl` (27)
- `crates/csp-render/src/flight.rs` (188) + `flight.wgsl` (28)
- `crates/csp-render/src/vab.rs` (371) + `vab.wgsl` (40) — pick pass + readback
- `crates/csp-render/src/terrain.rs` (1210) + `terrain.wgsl` (394)

**Cross-read (callers/contract):** `crates/csp-app/src/lib.rs` (scene pass, resize, pick wiring; lines 545–1105, 1240–1330), `crates/csp-view/src/{map,flight,terrain,vab}.rs` (packet construction, projection convention, `solve_orbit_camera`, `build_packet`, `extract_*`), `crates/csp-view/tests/render_gate.rs` (headless pixel gate). `cargo check`/`cargo test -p csp-render`: clean, 5/5 tests pass (including the naga shader-validation gate and the LUT sanity test).

## Verified clean (checked, no findings)

- **Pipeline/pass depth-state pairing is consistent everywhere**: Map pipeline has `depth_stencil: None` and is only drawn in Map mode (no depth attached); VAB/flight/terrain/sky/pick pipelines all declare `Depth32Float` and are only drawn into passes attaching `Depth32Float` views. Packet production is mode-gated upstream (`extract_map`/`extract_vab` emit `None` outside their mode, so `map_packet`/`last_vab_packet` are `None` in Flight — the no-depth map pipeline can never enter a depth-attached pass). The prior egui crash class does not reproduce.
- **Reverse-Z convention is uniform and deliberate**: `perspective_revz` gives `ndc.z = near/(−z_view) ∈ (0,1]`; depth clears to 0.0; VAB/terrain `Greater`, flight `GreaterEqual` (needed for the z=0 fallback sky — documented and pinned by `render_gate.rs`), sky pass `Always` + no write. Terrain is drawn before the flight vertex stream, so the LUT sky can't be overdrawn by the fallback gradient (the fallback is only emitted when `packet.terrain.is_none()` — flight.rs:789).
- **f64-origin bake is sound**: `patch_instance` does `dir0·R − eye` in f64 → f32; the shader only builds `R·(dir − dir0) + dir·h` (small deltas); the test `patch_origin_bake_preserves_precision` bounds reconstruction at ~cm scale. `patch_cube` outputs are ≤√3 — f32-safe by construction.
- **Pick readback**: R32Uint target + own depth (view keeps the dropped texture alive via refcount), `Greater` + clear 0.0 consistent with the color pass, single-pixel `copy_texture_to_buffer` with `bytes_per_row: None` is legal for height-1 copies, `map_async` correctly issued after `queue.submit`, decode snapshots `pick_ids` at encode time (correct against later packet turnover), `pick_in_flight` serializes maps, winit `CursorMoved` delivers *physical* px matching the physical-pixel pick target.
- **Buffer growth**: `GpuBuffer::upload` recreates before `set_vertex_buffer`, old buffer's deferred destruction is handled by wgpu; grid/index buffers are Uint16-safe (361 verts, 1944 indices); `Vec` vertex-buffer layouts match struct sizes and WGSL inputs (`VabVertex` 32 B: offsets 0/12/28; `pick` is `@interpolate(flat)`).
- **`TerrainUniforms` layout** matches WGSL `U` (all members 16 B; `m4_wgsl` transposes row-major f64 → column-major f32; `view_rot`/`view_proj` verified consistent with `solve_orbit_camera`'s `view = Rᵀ`).
- **Skirt geometry**: outer ring shares edge uv (clamped), drops radially by `patch_extent·SKIRT_FRAC + 2 m`; walls are quads between identical directions, `cull_mode: None` keeps them double-sided.
- **Queue writes** (`write_buffer`/`write_texture` inside `draw` while a pass is open) all order before the submission — legal.

---

### [HIGH] Finding 1: Plume/plasma/dust emitters drop single vertices — one behind-eye vertex misaligns the whole flight triangle stream

**File:** `crates/csp-view/src/flight.rs:813–817` (`emit` closure), used at **902–904** (engine plume fan), **945–950** (plasma sheath), **1015–1017** (contact-dust fan)

**Problem:** `emit` pushes a vertex only when `project_into` succeeds; a failed projection pushes *nothing*. The flight packet is an unindexed `TriangleList`, so dropping 1 or 2 vertices of a 3-vertex triangle shifts the triple boundary for **every subsequent vertex in the packet** — all later triangles are assembled from misaligned vertices.

**Evidence:** The craft-part emitter does it correctly — all-or-nothing:
```rust
let [Some(c0), Some(c1), Some(c2)] = [t.a, t.b, t.c].map(|q| project_into(...)) else { continue };
```
with the comment "A tri with any corner behind the eye plane drops whole … rather than emitting a corrupted fan" (flight.rs:855–861). But the same file's `emit` helper — used by the plume fan (`emit(v, tip, col); emit(v, q0, col); emit(v, q1, col)`), the plasma sheath faces (six per-vertex `emit`s per face), and the dust fan — reintroduces exactly the failure the comment rejects.

**Impact:** `project_into` returns `None` when `clip.w ≤ CLIP_EPS` (at/behind the eye plane) or non-finite. That is reachable in normal play: the chase camera zooms to `distance_m = 5` (`FlightEvent::Zoom` clamps to 5 m) while plumes extend `6 + 30·throttle` m behind the craft and craft extents can exceed 5 m — orbiting puts plume/dust vertices behind the eye plane. When it triggers, the entire tail of the vertex stream — craft parts, dust, plumes — rasterizes as garbage triangles for the frame: scene-wide flickering artifacts, not a local glitch. It also silently evades `render_gate.rs` (the gate only samples a healthy camera position).

**Suggestion:** Give the fan emitters the parts rule — project all triangle corners first, skip the triangle whole on any `None`:
```rust
let tri = |v: &mut Vec<_>, a,b,c, col| {
    if let [Some(a),Some(b),Some(c)] = [a,b,c].map(|p| project_into(vp, eye, p)) {
        for pos in [a,b,c] { v.push(FlightVertex{pos, color: col}); }
    }
};
```
The VAB packet already enforces this per-part.

---

### [MEDIUM] Finding 2: Flight viewport is only set on `Resized` — aspect is stuck at 1.0 if no post-GPU-init resize ever arrives

**File:** `crates/csp-app/src/lib.rs:1248–1252` (only call site); contrast `set_map_viewport`/`set_vab_viewport` re-fed every frame at 688–693. Default: `crates/csp-view/src/flight.rs:84` (`viewport_px: (1.0, 1.0)`)

**Problem:** `FlightInput::default().viewport_px = (1.0, 1.0)` ⇒ aspect = 1.0. The only `set_flight_viewport` call is inside the `Resized` handler, and it is gated on `if let (Some(g), Some(f)) = (&self.gpu, &mut self.flight_renderer)`. Any `Resized` that arrives before `finish_gpu_init` (guaranteed on the deferred/async wasm init path; also possible natively on backends that don't emit an initial `Resized` before the first `RedrawRequested`) is swallowed by that guard and the flight camera keeps aspect 1.0 **permanently** — until the user happens to resize. Map and VAB self-heal because `run_frame` re-feeds their viewports each frame; flight was left off that list.

**Evidence:** `render_gate.rs:259` calls `view.set_flight_viewport(W, H)` manually precisely because nothing else does it — production has no equivalent.

**Impact:** Entire flight scene (terrain, sky rays via `misc`, craft stream — `build_packet`/`solve_orbit_camera` share the same `viewport_px`, so it is at least self-consistent) renders at the wrong aspect on a non-square window: stretched/squashed planet and craft. On wasm this can persist for the whole session.

**Suggestion:** Add `self.view.set_flight_viewport(config.width, config.height)` next to the map/vab per-frame calls in `run_frame` (csp-app/src/lib.rs:688–693), or call it in `finish_gpu_init` (after line 576).

---

### [MEDIUM] Finding 3: Transmittance LUT is baked endpoint-centered but sampled interior-centered — systematic ≤½-texel bias

**File:** `crates/csp-render/src/terrain.rs:539–541` vs `crates/csp-render/src/terrain.wgsl:79–80`

**Problem:** The bake places texels at the range endpoints:
```rust
let mu = -1.0 + 2.0 * col as f64 / (LUT_W - 1) as f64;   // texel 0 → μ=−1
let h  = atmo.top_m * row as f64 / (LUT_H - 1) as f64;   // row 0 → h=0, row 31 → top
```
but `trans()` inverts with the interior-texel-center convention:
```wgsl
let uu = (mu * 0.5 + 0.5) * f32(d.x) - 0.5;
let vv = clamp(h / u.planet.y, 0.0, 1.0) * f32(d.y) - 0.5;
```
The `·d − 0.5` form is correct for the heightmap (bake uses `lon = −π + 2π(i+0.5)/w`, and `height_at`/`sample_dir` match exactly — terrain.rs:199, wgsl:47). For the LUT it must be `·(d − 1)` (no `−0.5`). The error is `t − 0.5` texels along each axis: ±0.5 texel max, 0 in the middle, suppressed at the exact endpoints by clamping. In altitude that is up to ≈ `top/(2·(LUT_H−1))` ≈ 1.1 km for a 70 km top — ~0.2 e-folds at a 5.6 km scale height, i.e. up to ~20 % transmittance error mid-altitude; in μ up to ~0.008.

**Evidence:** `transmittance_lut_is_sane` (terrain.rs:1189) validates the *bake* against the closed form but never the shader-side coordinate mapping — so the mismatch is test-invisible.

**Impact:** Sky/haze band and aerial-perspective transmittance sit at a systematically shifted altitude/μ — subtle horizon-haze offset, worst for thin atmospheres on small bodies. No crash, deterministic, but wrong vs the module's claimed exact-mirror sampling discipline.

**Suggestion:** Change `trans()` to `let uu = (mu*0.5+0.5) * f32(d.x - 1);` and `let vv = clamp(h/u.planet.y, 0,1) * f32(d.y - 1);` (drop the `−0.5`), or bake the LUT at interior centers `(col+0.5)/(LUT_W)`/`(row+0.5)/(LUT_H)` to match the shader.

---

### [LOW] Finding 4: `max_level > 16` guard is a `debug_assert!` — release builds silently wrap `u16` cell indices

**File:** `crates/csp-render/src/terrain.rs:299–305` (guard), 340–346 (`x: id.x * 2 + dx`)

**Problem:** `PatchId.{x,y}` are `u16`. Level-16 children would need index `2·65535+1`; the code knows this ("this guards a tuned-up `LodConfig`") but only debug-asserts. In a release build a `LodConfig { max_level: 17, .. }` wraps `x * 2 + dx` → wrong cells → misplaced/overlapping patches with no error.

**Impact:** Latent until someone tunes `LodConfig` beyond the default 12; then produces wrong geometry with no diagnostic.

**Suggestion:** `assert!` (it is a presentation-constant contract, not a hot path — it runs once per `select_patches`), or clamp `level.min(16)`, or widen `x`/`y` to `u32`.

---

### [LOW] Finding 5: Biome texture re-upload keys on `Arc::as_ptr` — ABA hazard on stale texture

**File:** `crates/csp-render/src/terrain.rs:1031–1051`

**Problem:** `uploaded_biome_key` is `Arc::as_ptr(b) as usize` — an allocation address. If a `BiomeField` is ever dropped and a new one allocated at the recycled address, `biome_key == uploaded_biome_key` skips the re-upload and the shader keeps sampling the *previous* body's biome map. Today it is benign because `TerrainConfig.bodies` (a `BTreeMap`) owns every `Arc` for the session's lifetime, so addresses can't recycle while referenced — but nothing enforces that; a future biome reload/rebuild path silently shows the wrong tint map.

**Suggestion:** Give `BiomeField` a content key like `HeightField::key` (e.g. `mix64(hash of indices) ^ dims`), or key on `(width, height, ptr)` — cheaper still, a generation counter in the field.

---

### [LOW] Finding 6: `fs_sky` ray-shell intersection uses f32 discriminant at planetary magnitude

**File:** `crates/csp-render/src/terrain.wgsl:346–367`

**Problem:** `disc_top = b·b − (dot(pa,pa) − r_top·r_top)` with `|pa| ~ 6.7e5 m` (f32 uniform). `dot(pa,pa)` carries ~1e5 m² absolute f32 error, so the hit/miss boundary (`disc_top <= 0` early-out, line 349) and the exit length `t1` wobble by up to ~±300 m on near-tangent rays; the `disc_g`/`tg` ground-strike boundary (lines 359–367) has the same issue. The module doc claims "no large-magnitude f32 subtraction anywhere" — true for the vertex path, not for this test.

**Impact:** Sub-pixel limb-boundary shimmer where the scattered-sky→raw-space transition flickers ~1 px near the horizon tangent; bounded and mostly invisible, but it is exactly the f32-planet-magnitude class §2.3 is meant to exclude.

**Suggestion:** Compute the discriminant from a pre-shifted origin, or split the quadratic: precompute `dot(pa,pa) − r²` CPU-side in f64 and pass it as an f32 uniform (it is ~±1e10 — representable to ~1 m), or use the stable root form `t = c/(−b + s)`.

---

### [LOW] Finding 7: `get_current_texture` `Validation`/`Timeout` outcomes become a silent permanent Skip

**File:** `crates/csp-render/src/lib.rs:166–167` (`_ => FrameResult::Skip`)

**Problem:** `Timeout`, `Occluded`, and `Validation` all map to `Skip`. If the surface is misconfigured (e.g., after a driver-side format/caps change that a `Lost` didn't precede), every frame takes the `Skip` path forever with no reconfigure attempt and no log — a permanently black window that looks like "nothing to draw".

**Suggestion:** Log-and-reconfigure on `Validation` (it is never transient), or at minimum warn once so a dead-surface loop is diagnosable.

---

## Notes / non-findings (checked, intentionally not findings)

- `GpuBuffer` regrowth renames the debug label to `"vtx-grow"` — cosmetic only.
- `patch_might_hold_sea`'s margin (`0.25·amp`, min 250 m) vs `ocean_datum − R` is conservative in the safe direction for `datum ≥ R` bodies (Terrin: `datum_radius_m = 6.0e5 = R`); `datum < R − margin` would only overdraw ocean under terrain, never punch a hole.
- `uploaded_height_key` (u64 content key) vs `uploaded_biome_key` (pointer) asymmetry is what motivates Finding 5.
- `emit_polyline`/`emit_quad` in `csp-view/src/map.rs` are already atomic per segment/quad — no equivalent of Finding 1 on the map path.
- `queue.write_buffer` called while a `RenderPass` is open (grid lazy-upload, instance upload) is ordered before the submission's recorded commands — legal, no hazard.
