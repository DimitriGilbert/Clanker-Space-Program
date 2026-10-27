# Verified Report C16 (session-full-20261026)

Verifier: independent re-verification of review-report-C16.md against the working tree (HEAD 7dd4b86). No file edited; tests executed read-only. Result: **5 confirmed, 0 dismissed** (Finding 2's impact detail corrected, defect itself confirmed).

Empirical baseline reproduced by verifier: `cargo test -p csp-render` green (5 passed); `cargo test -p csp-view --test render_gate` green (3 passed, 5 dumps ignored — dump_sky among them); lavapipe ICD present on host; `git status` shows the reviewer's injection patch fully reverted (no modifications under crates/).

---

## Verdict on the complaint-#3 "FIXED" claim — CONFIRMED (trusted)

The three pillars were each verified against source, and the CPU model was independently re-derived by the verifier (the reviewer's implementation is not in the repo).

1. **Gate math and units.** `terrain.wgsl:1103-1105`: `ins_lum = dot(ins, (0.2126, 0.7152, 0.0722))`; `star_vis = 1.0 - smoothstep(6.0e-4, 4.0e-3, ins_lum)`; `ins = acc*4.2 + acc_m*1.5` (:1088) is added directly to the pixel before `tone_map` (:1105, :1107) — so thresholds are in final additive-linear-luminance units, exactly as claimed.
2. **CPU re-derivation (verifier's own, from the repo's real constants).** Constants used: `RAYLEIGH_BETA = [5.8e-6, 13.5e-6, 33.1e-6] 1/m` (crates/csp-render/src/terrain.rs:479, wired to the uniform at crates/csp-view/src/terrain.rs:658-660); `MIE_BETA = 2.9e-6` (terrain.wgsl:188); gains 4.2/1.5; phase 3/(16pi)(1+mu^2); Mie g=0.72, Hm=0.30H; Terrin R=6.000e5 m, scale height 5600 m, top 70000 m (data/systems/home/terrin/body.ron:16,34,35). For a zenith view ray with sun at zenith, `t_ei * t_is` telescopes to `trans(h0, 1)` (trans_between ratio times trans(alt_i)), so ins has a closed form: `4.2·beta_r·P·T(h0)·H·e^{-h/H} + 1.5·beta_m·P_m·T(h0)·Hm·e^{-h/Hm}`. Recomputed LUT transmittance (endpoint-centered bake, terrain.rs:568-580, is exact at the grid points used) and compared:

   | alt | verifier ins_lum | report ins_lum | star_vis (both) |
   |---|---|---|---|
   | 0 m | 0.046 | 0.049 | 0.000 |
   | 10 km | 0.0062 | 0.0063 | 0.000 |
   | 15 km | 0.0025 | 0.0026 | 0.39 |
   | 20 km | 0.0010 | 0.0010 | 0.95 |
   | 25 km | 0.00043 | 0.0004 | 1.000 |

   Agreement to ~6% at ground (within 32-step march + jitter vs closed form) and ~3 digits over the fade band; the smoothstep arithmetic reproduces star_vis exactly (t=0.588 -> 0.37≈0.39; t=0.118 -> 0.95). Day-zenith margin 0.046/0.004 ≈ 11.6x — report's "~12x" is fair. Zenith is the conservative (darkest) day-sky geometry, so the "no speckle on any blue sky" conclusion holds a fortiori for horizon/limb rays (longer path -> more inscatter).
3. **Gate placement.** Verified in fs_sky: vacuum bodies early-return at :1023 (`u.planet.y <= 0.0`), shell-miss rays at :1034-1038 — both bypass the gate as claimed (no scattering exists there, correct to show full stars); ground-strike rays set `bg_t = 0` at :1084-1087 so stars cannot appear below the horizon; the gate applies only in the shell-crossing branch (:1105).
4. **Sparse starfield.** terrain.wgsl:966-988: bright layer occ 0.022 at s=52 -> 4*pi*52^2*0.022 ≈ 748 all-sky stars; faint layer occ 0.006+0.018*mw_band at s=96 -> ~694 outside the band. sigma = max(0.65·pr, 3.5e-4) / max(0.55·pr, 2.5e-4) where pr is the per-pixel footprint (:962-965, :1014) — i.e. sigma ≥ 0.65/0.55 px by construction, so no star is sub-pixel. Matches "~750 + ~700, sigma floored 0.55-0.65 px".
5. **Jitter.** Per-step stratified jitter confirmed at :1069-1071 (fs_sky) and :294-296 (view_inscatter): each tap draws its own `screen_hash(px + (s*17.13, s*29.71))` offset. The "~1-2% residual" figure is the reviewer's estimate (not independently measured) but is not load-bearing. "No dither anywhere upstream" holds for the sky path — the only `dither` in csp-render (terrain.wgsl:477-480) is ground-albedo biome-band raggeding, not in fs_sky. `transmittance_lut_is_sane` exists (terrain.rs:1505) with the `lut_trans` CPU mirror (:1476).

Caveat noted: at 10 km the margin above the kill threshold is only ~1.5x, so the exact 10-km boundary is LUT/march-sensitive — but every number and the overall conclusion (stars fully off at ground/day, fade band 15-25 km, full above ~25 km) is sound.

## PRD coverage table — CONFIRMED (accurate on every row checked)

- **§5.4 depth row MISSING — confirmed.** PLAN.md:1459-1460 verbatim: "render the 10^7 m test scene with a 1 m reference object at 10^7 m; if depth-fighting artefacts exceed 1 pixel at 1080p, add the far-field pass. **Recorded in the perf log.**" docs/perf/perf-log-devbox-release.ron contains exactly 17 entries (sim/warp/weld/telemetry/fps ids) — grep for depth/far-field/z-fight/1e7 finds nothing, and a tree-wide grep over crates/, docs/, xtask/ finds no depth-fight measurement or test (the only "far-field" hit is an unrelated patched-conics comment in csp-sim/src/transfer.rs:2377).
- **RenderTier ABSENT sanctioned by §6.4 — confirmed.** `RenderTier`/`render_tier` appears nowhere in crates/ (grep). PLAN.md:1547 (§6.4 cut #1): "R3 Advanced render tier -> post-v1. Pure addition on top of a working Baseline" — and §5.2 maps R3 to M9+, i.e. beyond v1 milestones regardless. The report's "honest omission" framing is grounded; no code claims a tier it cannot deliver.
- Spot-checked the WIRED rows: `select_patches` at terrain.rs:~158 with the two-observer horizon rule; single instanced `draw_indexed(0..grid_index_count, 0, 0..n_terrain)` at terrain.rs:1332-1338; D2Array texture entries in the terrain bind group layout (~:716-725); `Features::empty()` at lib.rs request_device and `#![forbid(unsafe_code)]` at lib.rs:1; `MSAA_SAMPLES = 4` at lib.rs:65 (resolution AA, not post — the "+MSAA" annotation is fair); `shaders_parse_and_validate` test at lib.rs:476; wgpu = "30" in Cargo.toml:77 with Cargo.lock 30.0.1; unlit passthrough `if nn < 0.25 { return i.color; }` at flight.wgsl:74-76; CI installs mesa-vulkan-drivers and sets CSP_RENDER_GATE_REQUIRED=1 (ci.yml:27-39). All match.

---

### Finding 1: Render gate blind to star/sky regressions — CONFIRMED
Original: coarse image-statistic assertions pass even with `star_vis = 1` (injection experiment); the only starfield test is `#[ignore]`d, assertion-free, and renders a vacuum packet that bypasses the gated path.
Verification: every factual leg checked in crates/csp-view/tests/render_gate.rs. Active assertions are exactly `non_clear > 50%` (:378-383), `distinct > 8 && spread > 30` (:384-389), `near_white < 25%` (:394-399). `dump_sky` is `#[ignore]`d (:517), contains zero `assert!` (writes /tmp PPMs, :671-677), and builds a packet with `planet: [600_000.0, 0.0, 0.0, 2_400.0]` (:596) — `u.planet.y = 0` makes fs_sky return at :1023 before the gate at :1104 ever runs. render_dump_extra.rs (dump_map/dump_parts/dump_vab) has no star/sky assertion; grep finds no other star test. The injection experiment could not be re-run (would require editing files), but its conclusion is analytically sound: with star_vis=1 on the day sky, ~60-130 visible stars at gain 2.1 tonemap to ~0.92 linear (~249 sRGB) — roughly 0.5-1% of pixels near-white, far under the 25% bound; `distinct` and `non_clear` only increase; `spread` unchanged. The gate passes. MEDIUM severity appropriate — this is the one empirically-shipped regression class in the renderer.
### Finding 2: `render_flight_scene_sized` mixes params and module consts — CONFIRMED (impact detail corrected)
Original: buffer size uses param `w` but const `H`; copy extent uses consts `W`/`H`; non-(W,H) callers hit copy-validation panic or silent crop.
Verification: code confirmed at render_gate.rs:280 (`row_bytes = w*4`), :284 (`size: row_bytes * H as usize`), :298-307 (`bytes_per_row: row_bytes` i.e. w-derived, extent `width: W, height: H`), sole caller :189 passes `(W, H)`. Correction to the impact sentence: with **h > H** the extent (const H) is in-bounds and the required buffer (H-1)*bytes_per_row + W*4 fits exactly, so the result is a **silent top-crop, not a panic**; the panic cases are **h < H** (extent out of texture bounds) and **w < W** (bytes_per_row < extent width requirement); **w > W** is the silent left-crop as the report says. The defect and LOW severity stand: the "sized" variant exists to support other sizes and silently doesn't.
### Finding 3: ACES `tone_map` hand-duplicated across three shaders — CONFIRMED
Original: §5.1 "authored once ... never duplicated by hand"; the shared tonemap contract lives in three WGSL files.
Verification: byte-identical Narkowicz fits (a=2.51, b=0.03, d=2.43, e=0.59, f=0.14) at terrain.wgsl:196-207, flight.wgsl:32-43, vab.wgsl:52-63; map.wgsl has none. PLAN.md:1402-1403 verbatim: "Shaders are authored once with preprocessor branches for the binding model (WESL/naga_oil), never duplicated by hand." The rule's letter targets binding-model duplication (none exists — single tier), so the report's PARTIAL classification rather than a violation-of-letter is the fair reading; the cross-file presentation-contract divergence risk (flight.wgsl's own comment says the hull must match the ground) is real. LOW, confirmed.
### Finding 4: Full star field computed for every sky pixel, then zeroed all day — CONFIRMED
Original: `star_field` runs both 3x3x3 cell walks before `star_vis` multiplies the result away across the day side; ~0.5-1 ms/frame at 1080p on iGPU.
Verification: terrain.wgsl:1014-1016 computes `stars = star_field(rd, pr)` unconditionally at the top of fs_sky; the gated branch then uses `stars * star_vis` (:1105) and the day side has star_vis = 0 (per the confirmed CPU table), so both 27-cell layer walks (~54 lattice hashes + occupied-cell work) are pure waste on every day-side sky fragment. The ms/frame figure is the reviewer's estimate (not measured by verifier) — order-of-magnitude plausible for ~800k sky fragments x ~60 hashes on an iGPU, and the finding does not depend on it. The suggested restructure is sound: computing `ins`/`star_vis` first and guarding `star_field` never double-evaluates, because the vacuum (:1023) and shell-miss (:1038) early returns take their unconditional `space` before the branch. LOW, confirmed.
### Finding 5: §5.4 depth-precision measurement never recorded — CONFIRMED
Original: PRD requires the 10^7 m / 1 m-object depth-fight measurement "Recorded in the perf log"; no row, no test, no measurement exists.
Verification: see the PRD-table section above — PLAN.md:1459-1460 quote exact; perf log exhaustively inventoried (17 rows, none depth); tree-wide grep negative. The single-frustum reverse-Z decision (§2.3, and PLAN.md:277's "add a second far-field pass" trigger) therefore stands unmeasured. LOW, confirmed.

---

## Summary
- Findings: 5 confirmed, 0 dismissed (Finding 2 impact detail corrected; defect unchanged).
- Complaint #3 FIXED claim: CONFIRMED — gate code, starfield sparsity, and jitter verified in source; the CPU model's numbers independently reproduced from the repo's actual atmosphere constants.
- PRD coverage table: CONFIRMED — including §5.4 depth row genuinely MISSING and RenderTier absence genuinely sanctioned by §6.4 cut #1.
