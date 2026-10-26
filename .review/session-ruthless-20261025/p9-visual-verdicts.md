# Phase 9 — Stale-visual backlog verdicts (verify-then-close)
Verifier: P9 visual verification agent, 2026-10-26 04:14–04:25 local.
Method: FRESH captures at HEAD (`fd1e76d`) via
`cargo test -p csp-view --test render_gate -- --ignored --nocapture`
(dump, dump_low, dump_sky, dump_flight, dump_surface — all ok) and
`cargo test -p csp-view --test render_dump_extra -- --ignored`
(dump_map, dump_vab, dump_parts — all ok; vab packet now **42090** vertices,
was 12633 at original capture). PPMs → PNG under
`.review/session-ruthless-20261025/screenshots-verify2/`. Web demo
re-captured via headless chromium against `apps/demo/dist` on
127.0.0.1:8793. All verdicts from per-pixel PIL sampling + ASCII
visualization, same procedure as the original verifier.

| screenshot | source |
|---|---|
| screenshots-verify2/flight_scene.png | /tmp/frame.ppm (`dump`, seeded starter craft, dist 25 m) |
| screenshots-verify2/sky_{north,equator,south}.png | /tmp/sky_*.ppm (`dump_sky`, vacuum-body sky pass) |
| screenshots-verify2/vab_scene.png | /tmp/vab.ppm (`dump_vab`) |
| screenshots-verify2/map_scene.png | /tmp/map.ppm (`dump_map`, now with 4xMSAA resolve) |
| screenshots-verify2/parts_sheet.png | /tmp/parts.ppm (`dump_parts`, authored-part lineup) |
| screenshots-verify2/flight_alt715.png | /tmp/flight.ppm (`dump_flight`, alt=715 m, white=48) |
| screenshots-verify2/low_orbit.png | /tmp/frame_low.ppm (`dump_low`, white=0) |
| screenshots-verify2/surface.png | /tmp/surf.ppm (`dump_surface`, h_here=-145, white=0) |
| /tmp/webdemo2.png | headless chromium, apps/demo/dist |

---

## The 9 STALE-BY-CONCURRENT-FIX items — verdicts

### 1. flight 1.1 — Daylight stars / Milky Way in day sky — **CLOSED**
Expectation: stars gated by inscatter luminance, gone in day shots.
Evidence (flight_scene.png): the sky band (y<58) contains **0 star-like
local maxima** (no pixel >18 lum above all 8 neighbours with lum>120 —
the old frame had dozens of near-white points clustered upper-right).
Zero isolated near-white pixels anywhere in the sky; the only 43
sum>600 pixels below y=100 form one contiguous horizontal band at
y=72–73, x=149–176 — a sunlit launch-complex roofline (210,206,196),
not points. Sky is a clean blue atmosphere gradient
(43,74,109)→(90,131,160) with ±10-lum dither noise, no discrete stars.
Code: `terrain.wgsl:949-956` keeps the procedural starfield in `space`,
attenuated by transmittance and buried under `view_inscatter` on the
day side, plus near-sun falloff — gating is real.

### 2. flight 1.4 — Terrain washout / moldy pale blotches — **CLOSED**
Expectation: ACES tonemap + real textures — textured, not pale.
Evidence (flight_scene.png): ground is now strongly coloured and
varied — grass (144–186, 166–202, 96–147), dirt (102,64,27), rock
(97,96,93), sunlit ridge (210,206,196), deep shadow (27,25,22);
16,164 unique colours, mean lum 138.7 vs the old pale two-tone mint.
No soft grey-white splotch field remains. (New flat-shaded structure
faces are a different, stylistic matter — see "new observations".)

### 3. sky_north 4.1 — Dome-pole UV pinch — **CLOSED**
Expectation: procedural point stars, uniform density at poles.
Evidence (sky_north.png): bright-pixel fraction in annuli around frame
centre is flat — r<15: 0.72 %, r15–30: 1.28 %, r30–45: 1.36 %,
r45–60: 1.42 %, r60–90: 1.30 % (the 13.5 % outer ring is the smooth
sun/atmosphere glow at frame right, not stars). All 99 bright
connected components within r<60 of centre are **≤2 px** — crisp
point stars, zero radial spokes, zero smear convergence. The equirect
texture that pinched was deleted in `1e8c6da`; the shell-lattice hash
(`terrain.wgsl:858-928`) shows no latitude singularity.

### 4. sky_south 6.1 — Same pole pinch — **CLOSED**
Evidence (sky_south.png): density r<15: 1.15 % vs 1.12–1.44 % in all
outer rings — uniform. All 88 bright components within r<60 are ≤2 px.
No starburst, no convergence at (160,115).

### 5. sky_north 4.2 — Murky underexposed dome — **CLOSED**
Evidence (sky_north.png): the murky-brown NASA texture is gone;
background is deep blue-black (4,6,18) with 369 crisp star cores
>150 lum plus a smooth sun/atmosphere glow gradient on the right third.
This is a physically-lit night dome, not an underexposed photograph.

### 6. sky_south 6.2 — Same murky exposure — **CLOSED**
Evidence (sky_south.png): mean 11.3 is low but correct — the
anti-sun view has no scatter glow; 380 star cores >150 (comparable to
north's 369) on clean (4,6,18). Darkness here is physics, not
underexposure.

### 7. sky_equator 5.1 — Corner glow clipped by frame edge (LOW) — **CLOSED with note**
Evidence (sky_equator.png): the textured dome's ambiguous blob is
gone. A smooth sun-scatter gradient remains in the top-left
(corner 40×40 mean 86.7 vs ~11 elsewhere; peak pixel (97,95,94) at
(0,0) falling smoothly to background by x≈100, zero banding). This is
the staged sun's forward-scatter lobe — source-attributable and
physically sensible, unlike the texture clip it replaces. Residual
note: the lobe still peaks exactly at the frame corner, which can
still read as a light-leak at first glance — cosmetic only.

### 8. vab 3.2 — Flat unshaded mint disc at engine base — **CLOSED with note**
Evidence (vab_scene.png): the saturated (123,231,171)-family ellipse
hard-clipped at row 152 is gone. What remains is a ~25-px mint-tinted
cluster at the engine base (x≈164–173, y≈139–151): shaded
(115→157 R, 169→229 G), interleaved with the dark engine bell
(28–68 lum), no flat plateau, no hard clip row. It reads as an
engine-accent/attach-node tint on the authored part, not a floating
placeholder ellipse. Note: if unintended, the engine-base green tint
is the only remnant — trivial polish, not the reported defect.

### 9. vab 3.3 — Grey body shading patchy/dented — **CLOSED**
Evidence (vab_scene.png): the kitbash is now subdivided lit geometry
(42,090 verts vs 12,633) drawn through a per-pixel lit shader
(`vab.wgsl` — hemisphere ambient × baked AO + directional key +
Blinn spec + fresnel rim + ACES, comment cites the raw-RGB
over-bright issue). Row profiles show smooth ramps — the nose dome
gradients 126→196→127 over ~15 px (y=88), the tank ramps
118→199→213 (y=115) with coherent edges. The dark region at
y=99–113 is a single coherent dark part (engine shroud) bounded by
clean edges — not scattered facet dents. No flat-plateau facet runs
inside the hull.

**Score: 9/9 stale items verified CLOSED** (2 with minor cosmetic
residual notes — #7 sun-glare corner, #8 engine tint).

---

## Re-checks of the still-live findings (bonus closures + still-live)

### NOW FIXED (verified in fresh captures)
- **flight 1.2 — no contact shadow / no launch pad → FIXED.**
  `structures.rs::emit_launch_complex` emits apron/mount/gantry/
  crawlerway/VAB/farm + "unlit contact-shadow quads"
  (`flight.rs:2574-2594`). The fresh frame shows the whole complex:
  flat lit aprons (191,187,177)/(189,189,190) and adjacent
  near-black contact shadows (27,25,22); the craft stands among the
  pad structures instead of bare grass.
- **flight 1.3 — hard unfogged horizon → FIXED (improved).**
  The 1-px step is now a 2-px haze blend (x=280: (73,108,128) →
  (134,152,109) → (186,195,149)) and there is real distance haze —
  terrain lum near-horizon ≈160 vs ≈145 close (hazier/brighter with
  distance = scatter). Thin but present; genre-plausible.
- **map 2.1 — stair-stepped orbit lines → FIXED.**
  4×MSAA is global (`csp-render/src/lib.rs:65 MSAA_SAMPLES=4`,
  `map.rs:119 multisample: msaa_state()`). Fresh orbit arcs are
  smooth ~4-px strokes with soft coverage edges — no staircase.
- **vab 3.1 — craft in 97.8% (39,39,63) void → FIXED.**
  `vab.rs:774-975 emit_environment` (backdrop gradient + lit floor
  disc + contact blob + grid; comment cites vision-report 3.1).
  (39,39,63) coverage is now **0.0 %** — radial-gradient hangar
  backdrop + floor fills the frame.

### STILL LIVE
- **map 2.2 — inconsistent oversized star blobs (LOW): still live.**
  64 background components remain soft grey puffs of 8–38 px,
  luminance 68–99, no size logic. Cosmetic; needs a consistent
  point-sprite pass. → follow-up candidate.
- **web 7.1/7.2 — black boot void + hairline: STILL LIVE (re-verified
  with fresh pixels).** Fresh 1280×800 capture is identical to the
  original: uniform (5,6,10) over 99.4 % of frame, same hairline
  structure at y=720–721 (row 720 edge (185,185,185), row 721
  full-width (255,255,255)). The new `apps/demo/index.html` adds
  `#loading` + `#fallback` — but it only fires for `!navigator.gpu`
  or an `init()` exception. The dominant failure mode (WebGPU API
  present, no usable adapter — this box: "webgpu found no adapters")
  fails **inside** the wasm: `csp-app/src/lib.rs:714-716
  poll_gpu_init` only `log::error!`s and the comment says it "stays
  black". `init()` resolves, `#loading` is removed, `#fallback` never
  unhides. → **follow-up fix needed**: signal GPU-init failure out of
  the wasm (JS event/callback or `init()` rejection) so the shell can
  unhide #fallback; or a JS watchdog that shows #fallback if no frame
  is presented within N seconds. Also still true: the shipped wasm
  bundles no fonts ("No font provider has any font … no text will be
  visible") — egui text is invisible on font-less systems.
  (dist/index.html is an older build than the fallback edit; rebuilt
  needed before retest — but the code path above fails regardless.)

### NOT RE-VERIFIABLE in the fresh scene (world changed)
- **flight 1.5 — straight aliased shoreline** and **1.6 — flat teal
  water**: the spawn/launch site moved — the fresh frame contains no
  water body at all (launch complex + mountain valley). The specific
  straight teal diagonal isn't present in any fresh capture. Marked
  unverifiable, not closed — would need a coastal capture.

## New observations in fresh captures (not original findings)
- **Launch-complex faces are flat untextured quads.** ~50 % of the
  lower flight frame is two flat greys ((191,187,177),(189,189,190))
  with zero texture and very hard-edged black shadow quads — reads as
  untextured grey boxes at 320 px. Functional and lit, but the
  flat-geometry look is the weakest part of the new scene. LOW.
- **surface.png is near-black** (mean 22.7, `h_here=-145` — camera
  ~145 m under the local datum / night side). Edge-case dump pose;
  flagging so it isn't mistaken for a regression.
- **Flight scene is now a mountain-valley + launch complex shot** —
  completely different composition than the captured "rocket on flat
  grass". All scene-level judgments above account for this.

## Summary
- 9/9 stale vision findings verified CLOSED with pixel evidence
  (daylight stars, pole pinch ×2, murky ×2, equator glow, terrain
  washout, mint disc, dented shading).
- 4 previously-live findings additionally verified FIXED since the
  verification report (pad+contact shadows, horizon haze, orbit-line
  AA, VAB void).
- Still live: map blob stars (LOW), **web boot black void + hairline
  on the adapter-failure path** (the fallback UI exists but the wasm
  never tells it to show — re-file, HIGH), bundled-fonts gap (MED).
- Unverifiable this pass: shoreline/water (site moved).
- 1 new LOW: flat untextured launch-complex faces + hard shadow quads.
