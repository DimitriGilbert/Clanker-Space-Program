# P08 — Terrain & atmosphere (cluster review)

**Files reviewed:** `crates/csp-view/src/terrain.rs` (547), `crates/csp-render/src/terrain.rs` (1298),
`crates/csp-render/src/terrain.wgsl` (422), `crates/csp-view/src/flight.rs` (FlightEnv ~98–120,
staging ~626–649, chase-camera floor ~992–1082, extract ~1545–1600).

**Verdict:** the high-focus items are all clean — the `sky_view_rot` fix is *correct* (columns
are the camera axes; verified against `perspective_revz`'s exact row structure and the
end-to-end roundtrip test), the two texel-convention samplers are properly separated, the
`u.spin` phase is bit-consistent with the sim's §2.6.4 `surface_pose_*` convention, and the
quadtree is deterministic + budget-bounded. Findings below are real but mostly LOW/MEDIUM
cosmetic or latency issues.

---

## Findings

### F1 — MEDIUM: horizon cull uses the bare-datum horizon — displaced terrain at the limb is culled away

- **File:line:** `crates/csp-render/src/terrain.rs:309-333` (`select_patches`)
- **Problem:** the cull rejects a patch when `acos(sub_dir·dir0) > horizon + ang_radius`, where
  `horizon = acos(R / (R + alt))` is the geometric horizon of the *datum sphere* and
  `ang_radius = atan(cube_size · 0.75)` covers only the patch's own geometric angular radius
  (0.75 ≥ the true half-diagonal factor √2/2 ≈ 0.707 — correct direction, but only ~6% slack).
  Nothing accounts for the displaced surface: terrain rises up to `amplitude_m` above datum
  (`AMPLITUDE_FRAC = 0.004` → 2400 m on Terrin).
- **Evidence:** a peak of height `amp` pokes over the datum horizon out to angular distance
  `horizon + sqrt(2·amp/R)` ≈ `horizon + 0.089 rad` ≈ **+5.1°** (Terrin numbers, verified
  numerically). A culled patch is dropped whole (no children either), so a peak sitting in the
  near-rim band of a just-culled patch — which IS visible above the limb — never draws. Because
  the cull boundary moves with the camera, peaks at the limb will also pop in/out as the
  camera translates (patch-centre test, not conservative-per-feature).
- **Impact:** within ~5° of arc past the geometric horizon, elevated terrain is missing —
  distant mountain ranges that should silhouette over the horizon from a landed/low flight
  camera are absent, and the planet limb reads as a smooth datum arc instead of the baked
  relief. Cosmetic (a few km at a 600 km limb), but systematic, and the frame-to-frame popping
  at the boundary is the kind of artifact that reads as a rendering bug.
- **Suggestion:** add `sqrt(2·amplitude_m/radius_m)` (or the closed-form elevated-horizon
  angle) to the cull bound — `amplitude_m` isn't currently passed into `select_patches`, so
  it needs to become a parameter (or ride `LodConfig`). One extra term; the BFS/budget logic
  is untouched.

### F2 — MEDIUM (cross-cluster flag): sim contact surface is the bare datum sphere; rendered ground is ±2.4 km displaced

- **File:line:** `crates/csp-view/src/terrain.rs:35` (`AMPLITUDE_FRAC = 0.004` → ±2400 m at
  Terrin) vs `csp-phys` `simple.rs` doc ("the environment's ground is a sphere of `radius_m`
  centred at `body_center`").
- **Problem:** the R2 pass makes the mismatch *visible*: the sim collides/latches/settles
  against the datum sphere while the drawn ground is the displaced heightfield. The pad site
  is pinned to the rendered surface (`pad_spawn.rs` `launch_site_sits_on_rendered_ground`,
  ±5 m), but any off-pad landing/touchdown contacts the invisible datum: over positive relief
  the craft visibly embeds up to ~2.4 km into the hill before contact fires; over negative
  relief it rests up to ~2.4 km above the drawn ground.
- **Evidence:** `height_normalized` returns [−1,1] × `amplitude_m`; the only alignment
  guarantee is the authored site. This is presumably the accepted R2 scope ("terrain is
  decoration" per the pad-test comment), but the playability consequence lands squarely in
  the flight view this cluster renders.
- **Impact:** landing anywhere except the pad produces an obvious sink-into/float-over
  ground bug class. The chase-camera floor (`surface_floor_radius`) already proves the view
  CAN sample the rendered surface — the sim just doesn't.
- **Suggestion:** at minimum document the bound next to `AMPLITUDE_FRAC` (e.g. cap relief or
  note "visual-only ground"); the real fix is a terrain-aware contact altitude fed back to
  the sim — out of scope for R2, but worth an explicit follow-up since the pad test hides it.

### F3 — LOW: dust puff anchored to the datum sphere, buried under displaced terrain

- **File:line:** `crates/csp-view/src/flight.rs:1763`
- **Problem:** the contact dust ring is emitted at
  `base = env.body_center + dir · (env.radius_m + 0.3)` — the bare datum. On any ground where
  the heightfield sits above `datum + 0.3 m` (most land), the ring is inside the rendered
  hill and invisible.
- **Evidence:** `crate::terrain::surface_floor_radius` already exists to answer "where is the
  rendered surface along this direction" and is used for the camera floor at L1042 — the
  dust base just doesn't use it.
- **Impact:** touchdown dust won't show on landings over above-datum terrain — the cue that
  sells contact silently disappears exactly where the camera is closest.
- **Suggestion:** sample `surface_floor_radius(cfg, cache, env, dir)` for the dust base (it's
  already threaded into `extract_flight`'s scope via `terrain_cfg`/`terrain_cache`), or at
  least `max(datum, floor)` + 0.3.

### F4 — LOW: biome legend >16 entries silently truncates to the height ramp

- **File:line:** `crates/csp-view/src/terrain.rs:164` (`loaded.legend.entries.iter().take(16)`)
- **Problem:** palette slots are capped at 16 (`biome_colors[16]`), but nothing in the data
  validator bounds `BiomeLegend.entries` to 16 (`check_biomes` only checks index/id
  uniqueness + palette/PNG coverage). Entries ≥17 leave `slot_of[their PNG index] = 255` →
  their pixels fall through to the ramp palette.
- **Evidence:** the shader reads `biome_colors[slot]` only for `slot < 255u` (terrain.wgsl:260);
  a 17th+ authored biome renders as generic height-ramp terrain with no warning emitted (the
  decode `Err` path is the only `log::warn` in `biome_field`).
- **Impact:** silent data-driven tint loss for over-limit legends — an authored biome simply
  doesn't appear and nobody is told.
- **Suggestion:** `log::warn!` (or validation error) when `entries.len() > 16`.

### F5 — LOW: ocean shoreline test can miss sub-grid basins → dry holes below the sea datum

- **File:line:** `crates/csp-view/src/terrain.rs:224-242` (`patch_might_hold_sea`)
- **Problem:** emission is decided by the minimum of a 5×5 direction grid + a
  `max(amp·0.25, 250 m)` depth margin. On coarse leaf patches (a level-0 face spans ~78°;
  grid spacing ~20° ≈ 120 km on Terrin) a basin narrower than the grid spacing and deeper
  than the margin between samples emits no ocean — the terrain patch still draws, so the
  region renders as dry ground below the water datum.
- **Evidence:** the test is `lo·amp ≤ (datum − radius) + margin` — the sign is correct
  (conservative toward emitting), and the doc states the bound openly ("margin bounds the
  miss on dips narrower than a fifth of the patch"), but a genuinely missed basin produces a
  real hole in the sea, not just a shoreline error.
- **Impact:** rare with the authored continents-scale FBM (features are wide), but possible —
  a small deep basin ringed by highland draws dry.
- **Suggestion:** cheap insurance: also evaluate the CPU `height_normalized` (not the baked
  bilinear field) at the 5×5 points, or increase margin for coarse levels (scale margin with
  patch size). Low priority — heuristic bound is documented.

### F6 — LOW (perf note): `TerrainCache` is single-slot — SOI switch re-bakes 524k texels; first bake lands on the first staged frame regardless of mode

- **File:line:** `crates/csp-view/src/terrain.rs:180-207` (`TerrainCache::field`),
  `crates/csp-view/src/flight.rs:1096-1147` (`flight_present` runs every frame in every mode)
- **Problem:** the cache keys on one `(body_id, seed)` — switching dominant bodies (SOI
  transfer) drops the previous body entirely, so an A→B→A hop re-bakes the 1024×512 field
  (~524k `height_normalized` evals, tens-to-hundreds of ms) each time on the camera-solve or
  extract path — a frame hitch, not amortized. `flight_present` solves the chase camera every
  frame in every mode (not gated on `MapMode::Flight`), so the first bake happens on the
  first staged frame even while in Map/VAB — a startup stall that's at least predictable.
- **Impact:** a visible hitch on SOI transitions / first-flight entry. Bounded and rare.
- **Suggestion:** keep a small map keyed by `(body_id, seed)` (bodies per system ≤ a handful)
  instead of one slot, or pre-bake the dominant body at `View::new`/`capture_sync_input`.

---

## Items verified clean (the focus-list concerns)

- **`sky_view_rot` transpose fix — CORRECT.** `perspective_revz` produces exactly the assumed
  `P = [[f/a,0,0,0],[0,f,0,0],[0,0,0,near],[0,0,-1,0]]` (map.rs:1365-1373) and the chase view
  rows are `[right; upv; z_out]` (flight.rs:1068-1073), so `vp`'s rows 0/1/3 recover the axes;
  `sky_view_rot` stores them as WGSL *columns* (`view_rot[0]=right`, `[1]=upv`, `[2]=z_out`),
  making `view_rot·v` expand the view ray over the camera basis — `fs_sky`'s
  `(ndc.x·tan·aspect, ndc.y·tan, −1)` matches the projection. TerrainUniforms declares the
  matrices column-major and `m4_wgsl` transposes row-major math → `m[col][row]` consistently.
  The two new tests (`view_rot_columns_are_the_camera_axes`, `sky_ray_roundtrips_through_view_proj`)
  pin both the column convention and the end-to-end ray.
- **The two texel conventions are not shared.** `height_at` (wgsl:44-59) uses texel-centre
  `·d − 0.5`, `trans` (wgsl:80-104) uses endpoint `·(d−1)` — matching `HeightField::bake`'s
  `(i+0.5)/w` centres vs `bake_transmittance`'s `col/(W−1)`, `row/(H−1)`. CPU mirrors agree:
  `HeightField::sample_dir` ≡ `height_at` (wrap-u/clamp-v), `lut_trans` ≡ `trans`, and
  `transmittance_lut_is_sane` pins sampling-at-grid-points = exact texels (the half-texel-bias
  guard). The biome sampler is a third *documented* convention (nearest, north-first rows) that
  correctly matches `decode_indexed_png`'s PNG scan order.
- **`u.spin` phase ≡ §2.6.4 exactly.** `capture_sync_input` (flight.rs:632-637):
  `axis = body_spin/ω`, `phase = ω·clock().seconds()`; `SimTime::seconds() = ticks/TICK_HZ` =
  the `tick.ticks()/TICK_HZ` in `bubble.rs::surface_pose_capture/restore` (L183-191, 241-253).
  Fixed→inertial is `R(axis,+phase)` in the sim; the shader samples at `R(axis,−phase)·dir`
  (wgsl:185), `patch_might_hold_sea`/`surface_floor_radius` use the same `R(axis,−phase)`
  quaternion (terrain.rs:275, 369-373). `rem_euclid(2π)` in f64 before the f32 cast keeps long
  clocks stable, and the phase is recomputed closed-form each frame — no accumulation path
  exists, so the "ground slides under parked craft" drift cannot arise. Normals rotate back
  by `+phase` (wgsl:221). The latched-craft restore (`stage_latched`) uses the same
  `sim.clock()` → rendered craft and rendered ground co-rotate identically.
- **Quadtree determinism + budget.** `select_patches` is a FIFO BFS with fixed face order
  (0..6) and fixed child order; `out.len()+queue.len()` is non-decreasing and a split adds ≤3
  net, so emitted leaves ≤ `max_patches+3` — bounded; the level-16 `u16` overflow is clamped,
  not asserted.
- **f64-origin bake.** `patch_instance` computes `dir0·R − eye` in f64 then casts to f32;
  the shader builds only `radius·(dir−dir0) + dir·h` — the residual is the f32 quantization of
  `origin−eye` plus unit-space delta error, ~cm-scale at 600 km — consistent with the ~5 cm
  claim (test asserts <0.1 m).
- **Ocean emission gating.** `patch_might_hold_sea` correctly compares *minimum* sampled
  height against `datum−radius + margin` (conservative toward emitting; sign verified for
  datum above, equal, and below `radius_m`), samples through the same `unspin` conjugate the
  shader uses, and ocean instances bake origins at the ocean datum (`planet.z`) consistent
  with `vs_terrain`'s `radius = select(planet.x, planet.z, ocean)`.
- **`seed_of` fallback.** FNV over the body id — deterministic and identical in both callers
  (`surface_floor_radius`, `build_packet` via `TerrainBody::synthesized`); authored bodies
  always take `TerrainDef::noise_seed` (with a warn when `heightmap` is set), so the fallback
  can't shadow an authored seed — only `with_sim`/missing-config worlds use it.
- **Uniform layout / draw order.** `TerrainUniforms` field order matches WGSL `struct U`
  (every member 16-byte). Draw order is sky (`Always`, no depth write) → terrain+ocean
  instanced (`Greater`, depth write) → flight verts (`GreaterEqual`) — craft correctly
  occludes behind terrain and blends plume/sheath over it; the app calls `terrain.draw`
  before `flight.draw` (app/lib.rs:1218-1221).
- **Not a bug worth filing:** wgpu-30 `queue.write_texture` internally repitches
  misaligned `bytes_per_row` (verified in `wgpu-core-30.0.1/src/device/queue.rs:984-1030`
  "copy chunked" path), so the authored 64×32 biome maps (`bytes_per_row = 64`, not a
  multiple of 256) upload safely. The 4096-instance buffer grows via `GpuBuffer::upload`.
  `atmo.x` raw-vs-clamped scale-height is consistent (`max(·,1.0)` on both sides).

**Tests run:** `cargo test -p csp-render terrain` — 4/4 pass (noise determinism, LOD split
depth + horizon bound, f64-origin precision, LUT bake↔shader convention).
