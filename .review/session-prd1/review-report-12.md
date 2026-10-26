# Review Report — PRD-12: Renderer (csp-render, wgpu + WGSL)

**Cluster files:** `crates/csp-render/src/{lib.rs, terrain.rs, terrain.wgsl, map.rs, map.wgsl, flight.rs, flight.wgsl}` (+ `vab.rs`/`vab.wgsl` read for depth-convention cross-check; scored under PRD-11).
**Spec:** PLAN.md §5.1 (tier contract), §5.2 (R0–R4 phases), §5.3 (budgets), §5.4 (measurable render decisions).
**Verdict:** The Baseline tier is genuinely implemented and, in several places, *better than the spec requires* (MSAA4 > "FXAA/none"; transmittance LUT + single-scatter; f64→f32 patch-origin bake with tests; shared height source with physics). The honest gaps vs §5.1's Baseline column are narrow: no `RenderTier` type at all, no instancing on the part/structure streams, no real "Forward HDR" target, and the R1 "shadow cascades" line item substituted by a heightfield ray-march + a single blob. The largest contributor to "graphics are shit" that is *within Baseline scope* is that the flight stream gets **zero atmosphere** — no view-ray extinction/inscatter on craft, structures, plumes — while the terrain behind them is fully aerial-perspectived.

---

## A. Baseline-column compliance audit (PLAN §5.1)

| Baseline requirement | Status | Evidence |
|---|---|---|
| Per-draw bind groups | **Met** | terrain BG (bindings 0–7), PartLight uniform BGs for flight/vab; no bindless anywhere |
| CPU-culled instanced draws | **Partial** | terrain: one instanced draw for all patches + one for ocean (`terrain.rs:1332-1338`, `1354-1360`). Flight/VAB/structures: **not instanced** — every part's triangles are CPU-expanded into a monolithic vertex stream each frame (`csp-view/src/flight.rs:2665+`, `emit_model_part`/`emit_smooth`); see G2 |
| Texture atlases + fixed slot sets | **Met** | terrain bindings 0–7 fixed, 1×1 placeholders keep the single BG valid (`terrain.rs:698-768`); class albedo/normal as `D2Array` layer stacks = atlas-equivalent |
| CPU quadtree LOD, pre-baked tiles, reduced draw distance | **Met** | `select_patches` BFS quadtree (`terrain.rs:158-221`), `HeightField::bake` equirect 1024×512, horizon cull + `max_patches` budget |
| Vertex pipeline only | **Met** | no mesh/task/compute shaders anywhere |
| Simple forward, few lights | **Met** | one directional sun + hemisphere ambient + rim (`flight.wgsl:73-103`, `terrain.wgsl:832-845`) |
| Post: FXAA/none | **Exceeded** | MSAA4 on all surface pipelines (`lib.rs:60-73`) |
| Precomputed LUTs + analytic single-scatter | **Met** | `bake_transmittance` 64×32 LUT (`terrain.rs:568-580`), `view_inscatter`/`fs_sky` analytic march (`terrain.wgsl:275-305`, `1052-1074`) |
| `RenderTier { Baseline, Advanced }` chosen at init from queried Features/Limits | **Missing** | no `RenderTier` type exists anywhere (`lib.rs` exposes only `Gpu`); `required_features: empty()` (lib.rs:227), no feature/limits inspection — see G1 |
| Both binding models compile in CI (naga) | **Partial/n-a** | naga parse+validate gate exists for all 4 shaders (`lib.rs:475-493`) — but only one binding model exists to compile; acceptable while Advanced is cut |
| Reverse-Z | **Met** | infinite-far reverse-Z everywhere; `perspective_revz` `ndc.z = near/(−z_view)` (`csp-view/map.rs:1551-1560`); Depth32Float, clear 0.0, `Greater`/`GreaterEqual` |
| Camera-relative f32 / §2.3 | **Met** | per-patch f64 origins baked `origin − eye` CPU-side (`terrain.rs:449-470`), clip-space packets for map/vab/flight |

---

## B. Spec gaps

### G1 — `RenderTier` does not exist — **MEDIUM**
- **Where:** `crates/csp-render/src/lib.rs` (whole file); `lib.rs:227` (`required_features: wgpu::Features::empty()`).
- **Problem:** §5.1 rules state "`csp-render` exposes `RenderTier { Baseline, Advanced }`, chosen at init from queried `wgpu::Features`/`Limits`." There is no tier enum, no feature query, no selection point. The code is Baseline-*only*, which the cut list allows — but the *contract surface* the spec mandates is absent.
- **Impact:** when Advanced-tier work begins it bolts on ad hoc rather than behind the spec'd selection point; nothing records *why* the device is Baseline (log of queried features/limits).
- **Suggestion:** add `pub enum RenderTier { Baseline, Advanced }`, decide it in `Gpu::new` from `adapter.features()`/`adapter.limits()` (always `Baseline` today), log the decision once. Cheap, makes the contract real.

### G2 — Part/structure draws are not instanced — **MEDIUM**
- **Where:** `crates/csp-render/src/flight.rs:216-232`, `vab.rs:281-297`; producer side `csp-view/src/flight.rs` (`emit_model_part`/`emit_smooth` per part per frame).
- **Problem:** the Baseline column's "CPU-culled instanced draws" is honored by terrain only. Craft/structures/plumes are expanded to raw triangles CPU-side and re-uploaded whole every frame. `FlightVertex` is 64 B; a 1000-part station at ~200–500 verts/part is 13–32 MB of `queue.write_buffer` per frame plus the CPU emit cost.
- **Impact:** the §5.3 "1000-part station in orbit — 60 fps web" row is the most exposed budget; this is the largest remaining Baseline-conformance deviation with a concrete perf cost. (CPU *tri-level* culling exists — `project_into` drops — but that is culling, not instancing.)
- **Suggestion:** instance repeated part meshes: one GPU mesh per part shape + per-instance transform/material rows (mirrors `TerrainInstance`); keep the unlit vertex-colored streams (plumes/sheath/dust/sky) on the existing stream path.

### G3 — "Forward HDR" (R0) is not literally implemented — **LOW**
- **Where:** `terrain.rs:808-812`, `flight.rs:122-126`, `vab.rs:182-187` — all targets are the sRGB surface format; `tone_map` runs per-shader into LDR.
- **Problem:** §5.2 R0 says "Forward HDR". There is no HDR intermediate; ACES is applied per-pass *before* alpha blending, so the ocean/foam/plume blends composite in post-tonemap sRGB space rather than linear HDR.
- **Impact:** mostly cosmetic-correctness (blend results differ slightly from HDR-composited); consistent today because every pass applies the same curve.
- **Suggestion:** either accept and document "LDR + per-pass ACES" as the Baseline choice, or add an `Rgba16Float` scene target + composite pass — that's a real pipeline change; defer with a comment.

### G4 — §5.4 10⁷ m depth-precision validation: not recorded — **LOW**
- **Where:** `docs/perf/perf-log-devbox-release.ron` — contains only sim-CPU rows; zero render entries. §5.4 requires the 10⁷ m test scene ("1 m reference object at 10⁷ m", ≤1 px depth-fighting at 1080p) recorded in the perf log.
- **Problem:** the measurement has never been logged. Analytically the infinite-far reverse-Z Depth32Float gives ~0.6 m depth resolution at 10⁷ m (`ndc.z = 0.1/z`), i.e. borderline-pass — but the spec wants the empirical check.
- **Suggestion:** a lavapipe-rendered variant of `render_gate.rs` could run this headlessly (the gate already proves the pattern); record outcome in the perf log.

### G5 — "Shadow cascades" (§5.2 R1) substituted, scope-limited — **INFO/PARTIAL**
- **Where:** `terrain.wgsl:426-451` (`sun_occlusion`, 5 taps, max ~5 km), `terrain.wgsl:457-470` (`blob_shadow`, focus vessel only).
- **Problem:** no cascades — a documented substitute: heightfield march gives terrain→terrain self-shadowing, the Gaussian blob gives focus-vessel→terrain contact shadow. Missing coverage: craft does not *receive* terrain shadow, structures/other vessels/debris cast nothing, parts don't shadow parts, shadow length capped ~5 km so terminator long-shadows are truncated.
- **Impact:** acceptable Baseline interpretation; the look cost is the lit-through-mountains craft and the lonely blob. Folded into the "graphics" recommendations (§E).

### G6 — Two binding models in CI — **N/A** (single model exists; naga gate present and correct).

---

## C. Defects / issues

### D1 — No aerial perspective on the flight/VAB vertex streams — **MEDIUM** *(top "graphics are shit" item)*
- **Where:** `crates/csp-render/src/flight.wgsl:73-103` — the fragment shader has *no* atmosphere terms at all. `fs_terrain` applies `t_view = trans_between(...)` + `view_inscatter(...)` (`terrain.wgsl:848-856`); craft/structures/plumes/sheath/dust skip both.
- **Problem:** a vessel 20 km away at the bubble boundary, or the launch complex seen from altitude, renders at full local contrast while the terrain behind it is haze-dimmed — geometry "pops" against the scene and reads as a cut-out. The staged `sun_col` is correctly atmosphere-tinted (`csp-view/flight.rs:2636-2658`, C11-F2) so lighting direction is honest; the *view-path* attenuation is missing.
- **Evidence:** `flight.wgsl` `fs_main` returns `tone_map(rgb)` with no `trans`/`inscatter`; `PartLight` (`lib.rs:31-58`) carries no fog fields.
- **Impact:** the single largest honesty/look gap inside Baseline scope — at altitude every rendered object floats visually in front of the atmosphere it is inside.
- **Suggestion:** bind the terrain bind group (or a shared atmosphere uniform + LUT) in the flight pipeline and apply `col·t_view + inscatter` for lit fragments; a cheaper v1 is CPU-staged extinction+inscatter color on `PartLight` applied as a distance fog in `fs_main`.

### D2 — Lit/unlit discriminator is `dot(n,n) < 0.25` on the *interpolated* normal — **LOW**
- **Where:** `flight.wgsl:74-77`, `vab.wgsl:80-82`.
- **Problem:** normal length doubles as the unlit-stream flag. A legit lit triangle whose face normals diverge >~120° interpolates below the 0.5 threshold mid-triangle → those pixels flash raw albedo (bright pop on hard creases). Kitbash emit probably keeps face-normal deltas small, but the encoding is fragile — it's a data-dependent render-mode switch hiding in a varying.
- **Suggestion:** carry an explicit flag (`mat`'s packed field, or a spare vertex channel e.g. `pick`-style flat attr) instead of inferring from ‖n‖.

### D3 — Biome tint sampled nearest-neighbor — **LOW**
- **Where:** `terrain.wgsl:613-619` — `textureLoad(biomemap, floor(...))`, no interpolation.
- **Problem:** biome maps are 64×32 px (`data/systems/home/*/biomes.png`) ≈ 59 km/texel on Terrin. Boundaries are hard texel steps with none of the dithered blending the procedural ramp gets (`ramp_albedo`'s `dither` doesn't cross biome slots). From low orbit/entry the authored regions meet in staircase lines.
- **Suggestion:** bilinear-blend adjacent slot colors over a fractional-texel border (or bake a transition in `BiomeField`), or jitter the texel coordinate with the existing `mottle` noise to ragged the edges.

### D4 — Depth-compare asymmetry across passes sharing one buffer — **LOW (documented, keep watching)**
- **Where:** `flight.rs:98-108` (`GreaterEqual`, justified: fallback sky writes NDC z=0); `terrain.rs:872-878` (`Greater`); `vab.rs:141-147` (`Greater`); sky `Always`/no-write (`terrain.rs:849-853`).
- **Problem:** the flight depth buffer is compared by two different functions (terrain `Greater`, flight stream `GreaterEqual`). Today it's benign: with the infinite-far projection `z=0` is only reachable by literally-emitted z=0 vertices, which only the fallback sky writes — and that stream only exists when `packet.terrain` is `None` (`csp-view/flight.rs:2545-2561`), so the two conventions never collide on the same fragments.
- **Impact:** a future vertex stream emitting z=0 geometry into a terrain-drawn frame would be silently culled; convention is undocumented at the terrain site.
- **Suggestion:** unify on `GreaterEqual` for every surface-destined pipeline (costless, removes the coupling), or at minimum mirror the flight.rs:101-104 rationale comment into `terrain.rs`.

### D5 — Craft receives no terrain shadow; structures fake it — **LOW**
- **Where:** `sun_occlusion` is applied only in `fs_terrain` (`terrain.wgsl:833`); the flight stream never evaluates it. Launch-complex contact shadows are authored "unlit contact-shadow quads" (`csp-view/flight.rs:2574-2583`), and the blob covers only the *focus* vessel (`u.shadow` single anchor, `terrain.rs:298-306`).
- **Impact:** a craft in a mountain's shadow stays full-sun; at terminator the scene's lighting honesty ends at the ground. Contributes to the flat/unlit read.
- **Suggestion:** evaluate the same `sun_occlusion` march in `flight.wgsl` for lit fragments (needs the heightfield + planet uniforms bound — same inputs as the terrain group); or accept and document the limit.

### D6 — Unguarded capability indexing; device-lost degrades to a churn loop — **TRIVIAL**
- **Where:** `lib.rs:270-281` (`caps.formats[0]` fallback index), `lib.rs:291` (`caps.alpha_modes[0]`); `lib.rs:346-356` (`device_lost` → `Reconfigure` every frame forever).
- **Problem:** both index `[0]` on adapter-reported lists (in practice non-empty — wgpu guarantees ≥1 format — but an unchecked panic path in the Cosmetic-class layer). Device-lost recovery reconfigures a dead device per frame; the flag is never cleared, so there is no true recovery path, only graceful non-panic degradation.
- **Suggestion:** `first().copied()` with a logged fallback for both; treat device-lost as terminal (stop rendering, surface the state to the app) rather than reconfiguring forever.

### D7 — Terrain pipelines draw backfaces; per-frame `Vec` churn — **TRIVIAL**
- **Where:** `terrain.rs:801` (`PrimitiveState::default()` → `cull_mode: None`); `terrain.rs:1323-1325` (`Vec::with_capacity` + extend per frame).
- **Problem:** skirt walls make culling risky, but face-interior backfaces are depth-resolved work; the instance concat Vec is small garbage per frame.
- **Suggestion:** optional `cull_mode: Back` if skirt winding is made consistent (verify on all six faces first); keep a scratch `Vec` on the renderer instead of reallocating.

### D8 — Ocean shoreline alpha floors at 0.62; `art.y` wave-scroll f32 drift — **TRIVIAL**
- **Where:** `terrain.wgsl:747` (`alpha = clamp(0.62 + 0.38·(depth/25), 0.62, 1.0)`); `terrain.rs:308-312`/`terrain.wgsl:681-696` (scroll driven by sim-clock seconds cast to f32).
- **Problem:** waterline never reaches transparent at depth≈0 (foam mostly covers it); at very long session times `art.y` in f32 loses sub-integer resolution → scroll stutter (cosmetic, deep-warp only).
- **Suggestion:** let shoreline alpha approach ~0.35; or send `time mod period` for the scroll term.

### D9 — Doc comment misplacement — **TRIVIAL**
- **Where:** `lib.rs:8-10` — the doc block describing *the map pass* sits on `pub mod flight;` (map's own `pub mod` at line 11 carries no doc). rustdoc attributes the map description to `flight`.
- **Suggestion:** move the comment to `pub mod map;`.

### D10 — Zero-length spin axis hazard in `rot_axis` — **TRIVIAL (latent)**
- **Where:** `terrain.wgsl:81-85` — `rot_axis(axis, angle, v)` returns `v·cos(a)` when `axis = 0`; if `u.spin.xyz` were ever non-unit/zero with nonzero phase, heightfield/biome lookups scale `dir` and corrupt sampling. `csp-view` always fills a body-def axis (`terrain.rs:651-656`) so this is latent, not live.
- **Suggestion:** cheap guard `if (dot(axis,axis) < 1e-6) { return v; }`.

---

## D. Honesty check — does the render reflect the sim?

Mostly **yes, unusually well** for this codebase:
- **Shared height source:** physics contact terrain and the shader sample the *same* `csp_core::terrain` bake (`terrain.rs:53-63`; `csp-phys/simple.rs:282-306`) — the drawn surface is the surface the sim collides with, bit-identically.
- **Spin:** `u.spin` carries the §2.6.4 `R(axis, ω·t)` phase; shader samples fixed-frame data at `R(axis,−phase)·dir` (`terrain.wgsl:74-85`) — rendered ground co-rotates with the sim's latched surface.
- **Atmospheric honesty:** LUT bake (`bake_transmittance`) is endpoint-pinned to the shader's `trans` by a dedicated test (`terrain.rs:1504-1552`); craft sun color staged through the same `transmittance_to_space` (C11-F2).
- **Determinism:** all noise is lattice-hash on body-fixed direction, camera-independent, with documented f32-stability bounds (`terrain.wgsl:87-107`); wave scroll rides the sim tick clock, not wall time.
- **The honesty gaps** are D1 (craft unattenuated) and D5 (craft unreceives shadow) — the two places the rendered light stops matching the physics the player is flying through.

## E. "Graphics are shit" — ranked Baseline-scope fixes

Within the Baseline column (no bindless/indirect/Forward+/TAA/volumetrics), ordered by visual payoff per effort:

1. **Aerial perspective on the flight stream (D1)** — binds the existing LUT/uniforms; makes craft, structures, and plumes sit *in* the atmosphere. Biggest single win.
2. **Part surface detail** — parts are pure flat albedo + Blinn spec (`flight.wgsl`, `part_mesh` classifier). §5.1 Baseline explicitly allows "texture atlases + fixed slot sets": a small panel-line/wear/normal atlas on tanks/capsules would kill the flat-plastic read far more than any post effect.
3. **Terrain shadow received by craft/structures (D5)** — reuse the `sun_occlusion` march (heightfield + planet uniforms already exist).
4. **Biome edge blending (D3)** — removes the visible 64×32 texel boundaries from orbit/entry.
5. **LOD split morphing** — `select_patches` splits are instantaneous (no geomorph); new verts pop in on subdivision. Skirts hide cracks but not the detail pop; a short fade-in per new patch is the cheap Baseline version.
6. **Shadow completeness** — extend blob anchor to structures/secondary vessels (more `u.shadow` rows) rather than cascades; consistent with the current substitute.
7. **Long-shadow reach** — extend `sun_occlusion` beyond ~5 km at terminator (one more tap octave); cheap.

## F. Verified non-issues (checked, do not re-flag)

- `queue.write_texture` row alignment for the 64-px R8Uint biome upload: wgpu 30 repads internally (`wgpu-core-30.0.1/device/queue.rs:981-1043`) — `bytes_per_row=64` is legal for queue writes. Heightfield 4096 B and LUT 1024 B rows are aligned anyway.
- `TerrainUniforms` Rust↔WGSL member order matches (verified field-by-field, both end `class_means[4]`).
- `patch_instance` ocean uses the *ocean* datum for `origin_rel_eye` (`csp-view/terrain.rs:588`) — consistent with shader `radius = planet.z`.
- Extra vertex attr `pad` (location 2) unread by `Gv` — legal.
- Ocean drawn after the flight stream with test-only depth — correct submerged-hull handling (C11-F3, `terrain.rs:821-837`, app order `csp-app/lib.rs:1371-1377`).
- `spin`/`unspin` quaternion conjugate convention matches between CPU cull (`patch_might_hold_sea`) and shader.
- wgpu pinned 30.0.1 (Cargo.lock:4453-4454) matching §5.1's "30.0.x" pin.
- `Gpu::resize`/`ensure` recreate the MSAA target on size/format change; flight/vab renderers own their own MSAA depth targets; map correctly carries no depth (pass attaches none — `csp-app/lib.rs:1337-1349`).
- Naga CI gate validates all four WGSL modules eagerly (`lib.rs:475-493`).
