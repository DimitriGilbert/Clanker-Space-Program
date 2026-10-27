# Review Report C16 — Renderer + render-gate tests (session-full-20261026)

Scope: `csp-render` (lib, terrain.rs, terrain.wgsl, map.rs+wgsl, flight.rs+wgsl) + `csp-view` tests (render_gate.rs, render_dump_extra.rs). Reviewed the current working tree (HEAD 7dd4b86; star fix landed in ff897d4). All WGSL read line-by-line; every finding verified empirically where possible (csp-render unit tests green, render gate green on lavapipe, gate math re-implemented on CPU, regression-injection experiment run and reverted).

## Verdict on complaint #3 (grainy sky / stars too early): FIXED

Both root causes addressed in ff897d4, verified:

1. **Hard smoothstep daylight gate** — `terrain.wgsl:1103-1105`: `star_vis = 1.0 - smoothstep(6e-4, 4e-3, ins_lum)`. Units: `ins_lum` is the Rec.709 luminance of the post-gain inscatter (`ins = acc*4.2 + acc_m*1.5`) — exactly the quantity added to the pixel, so thresholds are in final additive-luminance units. CPU re-implementation of the LUT + zenith march with Terrin's real params (R=600 km, top 70 km, H=5.6 km, RAYLEIGH_BETA, sun at zenith):
   | alt | ins_lum | star_vis |
   |---|---|---|
   | 0 m | 0.049 | 0.000 |
   | 10 km | 0.0063 | 0.000 |
   | 15 km | 0.0026 | 0.39 |
   | 20 km | 0.0010 | 0.95 |
   | 25 km | 0.0004 | 1.000 |
   Day-zenith margin is ~12x above the kill threshold (no speckle possible on any blue sky); star return band 15-25 km is physically sensible for a 1/10-scale body with an Earth-like 70 km atmosphere (cf. KSP). Gate applies inside the only branch where scattering exists; vacuum bodies (:1023) and shell-miss rays (:1038) correctly bypass it; ground-strike rays kill stars via `bg_t = 0` (:1084-1087) so stars never show below the horizon.
2. **Sparse resolved starfield** — `star_field` (:966-988): ~750 bright (occ 0.022) + ~700 faint (occ 0.006-0.024 in the Milky-Way band) stars vs the old 11-23% occupancy grain layer; sigma floored at 0.55-0.65 px so no star is sub-pixel.
3. **March jitter de-grained** — single per-pixel phase (old ±8% day-sky grain) replaced by per-step stratified jitter (:1069-1071, view_inscatter :294-296); residual quadrature noise ~1-2% of ins — sub-visible. No dither anywhere upstream; LUT bilinear convention is seam-locked by `transmittance_lut_is_sane` (endpoint-centered grid-point check); tonemap+8-bit leaves only ordinary quantization banding.

## Findings

### [MEDIUM] Finding 1: The render gate cannot see star/sky-quality regressions — complaint #3's class is regression-invisible
**File**: crates/csp-view/tests/render_gate.rs:370-399 (assertions), :516-519 (`dump_sky`)
**Problem**: Empirically proven: I patched `star_vis = 1.0` (stars at full gain across the entire daylight blue sky — complaint #3 at its worst) and `boot_scene_has_real_geometry` still PASSED (then reverted; tree clean). The active assertions (`non_clear > 50%`, `distinct > 8`, `spread > 30`, `near_white < 25%`) are coarse image statistics — a speckled day sky *increases* distinct color buckets. The only starfield exercise, `dump_sky`, is `#[ignore]`d, has zero assertions, and renders a vacuum packet (`planet: [600_000.0, 0.0, ...]` :596) so `fs_sky` early-returns at :1023 before the gated path ever executes.
**Impact**: The exact regression class that shipped once (star visibility/gate math) can ship again undetected; the fix ff897d4 itself was verified only by eye.
**Suggestion**: One cheap assertion closes it: on the existing seeded day scene, crop the upper-third sky region and count speckle pixels (a pixel >Δ brighter than its 4-neighbourhood mean); currently ~0, with the gate disabled it is hundreds. Alternatively/also a CPU-mirror unit test of the gate sweep (same pattern as the `lut_trans` mirror) asserting star_vis = 0 at ground-day and > 0.9 at 30 km against Terrin's params.

### [LOW] Finding 2: `render_flight_scene_sized` mixes params and module consts — the "sized" variant silently isn't
**File**: crates/csp-view/tests/render_gate.rs:284, :303-307
**Problem**: Buffer size is `row_bytes * H as usize` (param `w` for row bytes, const `H` for height); the copy extent uses `width: W` (const) instead of `w`. A future caller with a larger height or any width != 320 gets a wgpu copy-validation panic (or, for w > W, a silently wrong crop).
**Impact**: Latent today — the only caller chain passes `(W, H)` (:189) — but the function exists precisely to support other sizes.
**Suggestion**: Use `w`/`h` in both the buffer size and the copy extent; drop the consts inside the sized function.

### [LOW] Finding 3: ACES `tone_map` hand-duplicated across three shaders
**File**: crates/csp-render/src/terrain.wgsl:196-207, flight.wgsl:32-43, vab.wgsl:52
**Problem**: PLAN §5.1: "Shaders are authored once with preprocessor branches ... never duplicated by hand" (WESL/naga_oil named as the mechanism). The tonemap curve is now a shared presentation contract (the lit hull must roll off like the ground — flight.wgsl's own comment says so) maintained in three places; a re-tune of one silently diverges the others.
**Impact**: Divergence risk on any future tone-curve change; today the three copies are identical.
**Suggestion**: Build-time include — an xtask step prepending a `common/tonemap.wgsl` (same pattern as `bake_art`), or naga_oil imports if a dependency is acceptable.

### [LOW] Finding 4: Full star field computed for every sky pixel, then multiplied to zero all day
**File**: crates/csp-render/src/terrain.wgsl:1014-1016 vs :1103-1105
**Problem**: `star_field(rd, pr)` runs both 3x3x3 cell walks (~54 lattice hashes + ~6 hashes per occupied cell per pixel) before `star_vis` zeroes the result across the entire day side; on the night side it is needed, on the day side it is pure waste.
**Impact**: ~0.5-1 ms/frame of integer-hash work at 1080p on iGPU-class parts — eats into the §5.3 "200-part vessel, 144 fps" budget for zero output.
**Suggestion**: Compute the march and `star_vis` first; evaluate `star_field` only when `star_vis > 0` (the vacuum/shell-miss paths at :1024/:1038 keep their unconditional evaluation).

### [LOW] Finding 5: §5.4's depth-precision measurement was never recorded
**File**: docs/perf/perf-log-devbox-release.ron (absent row)
**Problem**: §5.4: "render the 10^7 m test scene with a 1 m reference object at 10^7 m; if depth-fighting artefacts exceed 1 pixel at 1080p, add the far-field pass. **Recorded in the perf log.**" The perf log carries the fps/ms rows but no depth artefact row; no test or measurement exists anywhere in the tree.
**Impact**: The single-frustum reverse-Z decision (§2.3) stands on assumption, not the PRD's own measurable trigger.
**Suggestion**: One lavapipe-runnable variant of the render gate (1 m object beside a surface at 10^7 m range, count depth-fight pixels) + one perf-log row.

## Verified-clean (checked, not flagged)
- `misc.z/w` shell constants baked in f64 CPU-side (csp-view/src/terrain.rs:628-631) — the |pa|^2 - r^2 cancellation is correct; residual b^2 rounding ~centimetres at the limb.
- `trans_between` descending-segment fix (ratio along the up-going continuation, :255-263) — correct; the old both-ends-0 blackout is gone.
- Resize/state paths: `Gpu::resize` reconfigures + `MsaaColor::ensure`; `FlightRenderer::resize` recreates depth; `TerrainRenderer` keyed re-uploads (content-hash keys, no ABA; rebind only on change) — no leaks, no stale-bind-group paths found. `write_art_tex` mip repadding meets the 256 B row alignment.
- LOD: horizon cull credits displaced relief via the two-observer rule (P08-F1); u16 index wrap guarded by `max_level.min(16)`; budget saturation emits coarse coverage first.
- LUT bake/shader convention parity locked by test (`transmittance_lut_is_sane`, grid-point exactness).
- Ocean-after-flight depth-test-only ordering (C11-F3) and the flight `GreaterEqual` vs terrain `Greater` far-plane handling are mutually consistent with the pass clear order.
- CI installs lavapipe and sets `CSP_RENDER_GATE_REQUIRED=1` (ci.yml:27-39) — the gate cannot silently degrade to green-skip. wgpu pinned "30" (lock 30.0.1) per §5.1.

## PRD coverage table (§5.1 Baseline + §5.2 R2 + §5.4)

| Requirement | Status | Evidence |
|---|---|---|
| §5.1 per-draw bind groups, CPU-culled instanced draws | WIRED | terrain.rs:158 `select_patches` (horizon+budget), single instanced draw :1332-1338 |
| §5.1 texture atlases + fixed slot sets | WIRED | D2Array albedo/normal bindings 4-5 (:716-725); fixed 8-slot bind group |
| §5.1 CPU quadtree LOD, pre-baked tiles, reduced draw distance | WIRED | select_patches + LodConfig (max_patches 2048); HeightField bake; `lod_subdivides_with_distance` |
| §5.1 vertex pipeline only | WIRED | `Features::empty()` lib.rs:227; no mesh/task path |
| §5.1 simple forward, few lights | WIRED | `PartLight` one sun + hemi ambient (lib.rs:33), flight.wgsl fs_main |
| §5.1 post FXAA/none | WIRED (+4x MSAA — not post-processing; strictly better than "none") | lib.rs:65 |
| §5.1 atmosphere: precomputed LUTs, analytic single-scatter | WIRED | `bake_transmittance` terrain.rs:568; fs_sky march + `view_inscatter` |
| §5.1 `RenderTier { Baseline, Advanced }` chosen at init | ABSENT — sanctioned: §6.4 cut #1 "R3 Advanced render tier -> post-v1 ... pure addition on top of a working Baseline". No code claims a tier it cannot deliver (honest omission, not a degraded claim) | no RenderTier symbol in csp-render |
| §5.1 tier-parity (zero-silhouette, dE2000 <= 2%) screenshot harness | MOOT with one tier; the harness that exists is coarse-statistics (render_gate), not dE2000/coverage masks | render_gate.rs `analyze()` |
| §5.1 shaders authored once (WESL/naga_oil), never hand-duplicated | PARTIAL | Finding 3 (tone_map x3); no binding-model duplication (single tier) |
| §5.1 naga/WGSL validation in CI | WIRED | lib.rs `shaders_parse_and_validate` (all 4 shaders, full validation); CI runs cargo test |
| §5.1 wgpu pinned 30.0.x | WIRED | workspace `wgpu = "30"`, Cargo.lock 30.0.1 |
| §5.1 no `wgpu::hal` passthrough | WIRED | `#![forbid(unsafe_code)]` lib.rs:1 |
| §5.2 R2: quadtree + per-patch f64 origins + LUT scattering + ocean + planet-scale LOD | WIRED | `patch_instance` bake (test `patch_origin_bake_preserves_precision`, <0.1 m); ocean pipeline + shoreline culling; horizon cull |
| §5.4 depth test at 10^7 m recorded in perf log | MISSING | Finding 5 |
| §3.7 presentation floor (plume/plasma/dust visuals ride this pass) | WIRED (unlit passthrough stream, flight.wgsl:74-77) | C19 owns the emitters |

## Top 3 issues
1. [MEDIUM] Render gate blind to star/sky regressions — proven by injection experiment (Finding 1).
2. [LOW] `render_flight_scene_sized` const/param mismatch — latent size trap in the gate harness (Finding 2).
3. [LOW] `tone_map` triplicated across WGSL files against §5.1's authored-once rule (Finding 3).
