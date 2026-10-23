# Verification Report — Cluster C16 (`csp-render` wgpu passes + WGSL)

**Report under review:** `.review/session-deep1/review-report-16.md`
**Source verified against:** `crates/csp-render/src/{lib,flight,terrain}.rs`, `terrain.wgsl`, `crates/csp-view/src/{flight,map,vab,terrain,lib}.rs`, `crates/csp-app/src/lib.rs`, `crates/csp-view/tests/render_gate.rs`
**Method:** read every cited line range in full; traced `project_into`/`emit` call chains, the `Resized`/`finish_gpu_init` event ordering, the LUT bake↔shader coordinate conventions, and the caller-side `FrameResult` handling.

---

## Verdicts

| # | Severity | Finding | Verdict |
|---|----------|---------|---------|
| 1 | HIGH | Plume/plasma/dust `emit` drops single vertices → misaligned triangle stream | **CONFIRMED** |
| 2 | MEDIUM | Flight viewport only set on `Resized` → aspect stuck at 1.0 on wasm | **CONFIRMED** |
| 3 | MEDIUM | Transmittance LUT endpoint-baked vs interior-sampled → ≤½-texel bias | **CONFIRMED** (minor quant nit in μ estimate) |
| 4 | LOW | `max_level > 16` guard is `debug_assert!` only → release `u16` wrap | **CONFIRMED** |
| 5 | LOW | Biome re-upload keys on `Arc::as_ptr` → latent ABA | **CONFIRMED** |
| 6 | LOW | `fs_sky` shell test uses f32 discriminant at planetary magnitude | **CONFIRMED** |
| 7 | LOW | `Validation`/`Timeout`/`Occluded` → silent permanent `Skip` | **CONFIRMED** |

**7/7 findings confirmed. 0 dismissed.**

---

## Finding 1 — CONFIRMED (HIGH)

**Verified chain:**

- `crates/csp-view/src/flight.rs:813–817` — `emit` closure is exactly as reported: pushes a `FlightVertex` only when `project_into` returns `Some`; a `None` pushes nothing (no placeholder, no triangle drop).
- `crates/csp-view/src/map.rs:961–989` — `project_into` returns `None` on `clip[3] <= CLIP_EPS` (line 977; `clip.w = −z_view`, i.e. at/behind the eye plane) or any non-finite component.
- Emit sites confirmed: plume fan `emit(v, tip, …); emit(v, q0, …); emit(v, q1, …)` (flight.rs:902–904), plasma sheath six `emit`s per face (945–950), dust fan three `emit`s per wedge (1015–1017). All write into the shared `v = &mut packet.vertices` (line 788) across *all* vessels.
- `crates/csp-render/src/flight.rs:102` — `PrimitiveTopology::TriangleList`; line 186 `pass.draw(0..n, 0..1)` with `n` = vertex count from `GpuBuffer::upload` — an **unindexed** triangle list. Dropping any single vertex shifts the mod-3 boundary for every later vertex in the packet.
- Contrast confirmed: the craft-part loop (flight.rs:854–868) uses the all-or-nothing `let [Some(c0), Some(c1), Some(c2)]` pattern with the quoted comment at 855–857; VAB `emit_part` (vab.rs:372–385) uses the identical pattern; map `emit_quad`/`emit_polyline` (map.rs:992–1063) push 6 verts atomically per quad/segment.
- **Reachability confirmed:** `FlightEvent::Zoom` clamps `distance_m` to `[5.0, 1e6]` (flight.rs:549); plume `len = 6.0 + 30.0·throttle` (line 879) extends up to 36 m behind the nozzle; dust `base` sits on the surface below the craft (line 993); plasma corners are the hull AABB ± margin (927–950). At min zoom and/or with a >5 m hull, orbiting places fan corners behind the eye plane — one `None` corrupts the tail of the stream (all later triangles of this and subsequent vessels, including the otherwise-safe craft parts of the next vessel).
- `render_gate.rs` only exercises a healthy camera (test sets viewport + starter craft at lines 256–262); nothing covers a behind-eye vertex — the failure is indeed test-invisible.

**Verdict: CONFIRMED.** Mechanism, reachability, blast radius, and the asymmetry with the part-mesh emitter all verified. The suggested fix (project all corners, skip triangle whole) is correct and matches the established convention.

## Finding 2 — CONFIRMED (MEDIUM)

**Verified chain:**

- `crates/csp-app/src/lib.rs:1248–1252` — the *only* production `set_flight_viewport` call, inside `WindowEvent::Resized`, gated on `(Some(g), Some(f)) = (&self.gpu, &mut self.flight_renderer)`. A `Resized` arriving while `self.gpu.is_none()` is silently swallowed — there is no later re-feed.
- `crates/csp-view/src/flight.rs:84` — `FlightInput::default().viewport_px = (1.0, 1.0)`; `solve_orbit_camera` computes `aspect = (w / h.max(1)).max(1e-6)` (map.rs:939) → aspect 1.0 forever.
- `crates/csp-app/src/lib.rs:688–693` — `set_map_viewport` and `set_vab_viewport` are re-fed every frame from `gpu.config` (whose initial values come from `Gpu::new(window.inner_size())` at line 498/510, so map/vab are correct from the first frame). Flight was left off the list — verbatim as reported.
- **wasm path confirmed:** `init_graphics` (lines 504–513) spawns `Gpu::new` via `wasm_bindgen_futures::spawn_local` + `mpsc` channel; `self.gpu` stays `None` until `poll_gpu_init` lands it (519–539). Any `Resized` delivered while the GPU promise is pending — including the initial one winit emits — is dropped by the guard. Self-healing then depends entirely on whether the browser/canvas ever emits another `Resized`; on a fixed-size canvas it never does → aspect stuck at 1.0 for the session. Native can hit the same window if a backend delivers no `Resized` between `finish_gpu_init` and the first `RedrawRequested`.
- `render_gate.rs:259` and `:322` call `view.set_flight_viewport(W, H)` manually — production has no equivalent call site (grep over the tree confirms: only `csp-app/src/lib.rs:1251`, the method itself at `csp-view/src/lib.rs:1232`, and the two test sites).
- `misc` uniform in `build_packet` (csp-view/src/terrain.rs:301–302, 332) shares the same `viewport_px`, so sky-ray aspect and camera aspect are consistently wrong together — the report's "self-consistent but wrong" characterization is accurate.

**Verdict: CONFIRMED.** Fix suggestion (per-frame re-feed alongside map/vab, or a call in `finish_gpu_init` after line 576) is correct.

## Finding 3 — CONFIRMED (MEDIUM)

**Verified chain:**

- Bake is endpoint-centered: `mu = -1.0 + 2.0·col/(LUT_W-1)` and `h = top_m·row/(LUT_H-1)` (terrain.rs:539–541; `LUT_W=64`, `LUT_H=32` at 521–523).
- Shader `trans()` uses the interior-texel-center convention: `uu = (mu·0.5+0.5)·f32(d.x) − 0.5`, `vv = clamp(h/u.planet.y)·f32(d.y) − 0.5` (terrain.wgsl:79–80) with clamped taps (90–93).
- Correct texel coordinate for the baked grid is `c(mu) = (mu+1)/2·(W−1)` and `r(h) = (h/top)·(H−1)`. Error in texel units: `uu − c = mu/2` and `vv − r = h/top − 0.5` — linear ramp, 0 at the center, **±0.5 texel max**, clamped to the endpoint texel exactly at `mu = ±1` / `h ∈ {0, top}` (verified: at mu=1, uu=63.5 → both taps clamp to texel 63 → exact; the largest unclamped error sits just inside the endpoints). The report's qualitative claim is exact.
- Contrast confirmed: `HeightField::bake` uses `(i+0.5)/w`, `(j+0.5)/h` (terrain.rs:169–174), so `·d − 0.5` in `height_at`/`sample_dir` (wgsl:47–48, terrain.rs:199–213) is correct there — the two bakes genuinely use different conventions while the shader uses one formula.
- `transmittance_lut_is_sane` (terrain.rs:1189–1208) validates only the bake against the closed form; the shader mapping is never exercised — test-invisible, as claimed.
- Fix check: `·f32(d.x - 1)` / `·f32(d.y - 1)` reproduces the bake's texel indexing exactly; alternatively interior-centering the bake at `(col+0.5)/LUT_W` matches the shader. Both suggestions are valid.

**Quant nit (does not change the verdict):** the report's "in μ up to ~0.008" understates by ~2× — the maximum unclamped μ error is `|mu|/63 ≈ 0.016` (0.5 texel × `2/(W−1)` spacing). The altitude figure `top/(2·(LUT_H−1)) ≈ 1.13 km` for a 70 km top is correct (≈ 0.2 scale heights at H = 5.6 km → up to ~20 % multiplicative error in T mid-atmosphere, direction and magnitude as claimed).

**Verdict: CONFIRMED.** Systematic endpoint-vs-center bias of up to ½ texel per axis; deterministic, bounded, real.

## Finding 4 — CONFIRMED (LOW)

- `debug_assert!(cfg.max_level <= 16, …)` at terrain.rs:302–305 with the explanatory comment at 299–301; `PatchId.{x,y}` are `u16` (223–231); child emission `x: id.x * 2 + dx` (344–345) — a level-16 node splitting computes `2·65535+dx`, which in release wraps `u16` (in debug the arithmetic overflow itself panics, making the `debug_assert!` nearly redundant there — in release it wraps silently to wrong cells).
- `LodConfig::default().max_level = 12` (262–269) and the only construction site is `config_from_bodies` (`rt::LodConfig::default()` — csp-view/src/terrain.rs:119) — no code path tunes it up today; purely latent, as the report states.

**Verdict: CONFIRMED.** `assert!`/clamp/`u32` widening all adequate; severity LOW is fair (runs once per `select_patches` call — actually once per frame in flight mode, still negligible).

## Finding 5 — CONFIRMED (LOW)

- `biome_key = packet.biome.as_ref().map(|b| Arc::as_ptr(b) as usize)` at terrain.rs:1032; compared against `self.uploaded_biome_key` at 1033/1049 to skip re-upload.
- `TerrainConfig.bodies` is `BTreeMap<String, TerrainBody>` holding `Option<Arc<BiomeField>>` (csp-view/src/terrain.rs:56, 66); `config_from_bodies` is installed once at `View::new` (csp-view/src/lib.rs:722) — no biome reload/rebuild path exists, so addresses cannot currently recycle while referenced. The packet clones the `Arc` (csp-view/src/terrain.rs:348), keeping it alive through draw. **Today benign; the ABA hazard is real but latent** — exactly as the report frames it.
- Asymmetry with `uploaded_height_key` (content hash `hf.key`, terrain.rs:988–1003) confirmed.

**Verdict: CONFIRMED** as a latent design hazard; suggestion (content key / generation counter) is appropriate.

## Finding 6 — CONFIRMED (LOW)

- terrain.wgsl:346–367: `pa = u.eye_rel_body.xyz` (f32 uniform, magnitude ~`R + alt` ≈ 6×10⁵ m — see uniform fill at csp-view/src/terrain.rs:318). `dot(pa,pa)` ≈ 3.6×10¹¹ m²; f32 mantissa gives absolute error on the order of 10⁴–10⁵ m² (the report's ~1e5 m² is within order), so `√disc` — and therefore the `disc_top <= 0` hit/miss boundary (line 349), `t1` (357), and the `disc_g`/`tg` ground-strike branch (359–367) — wobbles by roughly ±100–300 m near tangent rays. Order of magnitude confirmed.
- The vertex path does honor the "no large-magnitude f32 subtraction" rule (`wrel` is patch-local, line 179); this shell test is indeed the exception.

**Verdict: CONFIRMED** — bounded sub-pixel limb shimmer; LOW severity is fair. The CPU-side-f64-precompute suggestion is the clean fix (the discriminant constant `dot(pa,pa) − r²` is ~±10¹¹ but representable to ~8 m in f32 — close to the claimed ~1 m, same conclusion).

## Finding 7 — CONFIRMED (LOW)

- `crates/csp-render/src/lib.rs:160–167`: `Success|Suboptimal` → texture; `Lost|Outdated` → `Reconfigure`; everything else (`Timeout`, `Occluded`, `Validation`) → `Skip` with no log.
- Caller: `csp-app/src/lib.rs:951–957` — `Reconfigure` calls `gpu.resize(w, h)` and returns; `Skip` returns with no reconfigure attempt and no diagnostic. A persistent `Validation` (never a transient state) → permanently black window, indistinguishable from "nothing to draw". Confirmed verbatim.

**Verdict: CONFIRMED.** A `warn!`-once on `Validation` (or routing it to reconfigure) is the right fix; LOW is fair — no known trigger path, purely a diagnosability gap.

---

## Spot-checks of the report's "verified clean" section

- Depth-state pairing: flight pipeline declares `Depth32Float` + `GreaterEqual` (flight.rs:71–106) — matches the report.
- `GpuBuffer::upload` regrowth before draw (`vtx-grow` label, lib.rs:58) — matches.
- `TerrainPacket.biome: Option<Arc<BiomeField>>` at terrain.rs:466 — matches.
- `emit_polyline`/`emit_quad` atomicity in map.rs (verified above) — matches; no map-side equivalent of Finding 1.
- `VabVertex` pick channel `@interpolate(flat)` and VAB all-or-nothing emit (vab.rs:372–385) — matches.

No discrepancies found in the report's no-finding claims sampled.

## Summary

All seven findings reproduce exactly at the cited lines:

- **Finding 1 (HIGH)** — real defect: per-vertex `emit` in an unindexed `TriangleList` misaligns every subsequent triangle when `project_into` drops a behind-eye vertex; reachable at min zoom / large hulls.
- **Finding 2 (MEDIUM)** — real defect: flight camera aspect can be stuck at 1.0 for the session on wasm (deferred GPU init swallows the only `Resized`-gated feed); map/VAB self-heal per-frame, flight doesn't.
- **Finding 3 (MEDIUM)** — real convention mismatch: LUT baked at endpoints, sampled at texel centers; up to ½-texel systematic bias (~1.1 km altitude / ~0.016 μ at the extreme; the report's μ figure is ~2× low but the finding stands).
- **Findings 4–7 (LOW)** — confirmed as latent/defensive-hardening issues: release-build `u16` wrap, `Arc` pointer ABA, f32 planetary discriminant, silent `Skip` on `Validation`.

Nothing in the report was found to be overstated into dismissal; the only inaccuracy is a minor quantitative underestimate in Finding 3's μ error bound.
