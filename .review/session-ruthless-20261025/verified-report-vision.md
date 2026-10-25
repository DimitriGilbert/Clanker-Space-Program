# Verified Vision Report — pixel re-check
Verifier: vision verification agent, 2026-10-25. Every finding in `vision-report.md` re-checked against the PNGs in `screenshots/` (full-frame reads + 3–7x NEAREST crops + per-pixel PIL sampling).

Timeline (git log vs PNG mtimes):
- `flight_scene.png` 13:01:43, `sky_*.png` 13:01:58 — captured BEFORE `915e356` (13:02:43, terrain washout/tonemap) and BEFORE `1e8c6da` (14:08:04, procedural sky + authored parts).
- `map_scene.png`, `vab_scene.png` 13:06:43, `webdemo.png` 13:10:28 — after `915e356`, before `1e8c6da`.
- `915e356` committed the flight/sky PNGs itself and its message says "The screenshot showed pale-blown terrain + white scribbly streaks" — the fixer was fixing this exact capture.
- `1e8c6da` deleted `assets/src/starmap_4k.jpg` entirely and replaced the equirect dome with a procedural point-starfield ("no equirect smear — there is no image to blur", "uniform density at every latitude incl poles", "dimmed near sun, occluded by planet"), and re-authored all rocket parts (Kenney kit GLBs deleted, `parts.csar` 156KB→307KB). `vab.rs` change in that commit is comment-only.

---

## flight_scene.png

### Finding: Starfield and Milky Way in daytime sky — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: White star points + Milky Way glow visible through the launch-day sky; NASA map under blue tint, not an atmosphere.
**Verification**: CONFIRMED in the captured pixels. Full sky band (y=0–58) shows dozens of star points in the 3x crop; 11 near-white pixels (sum>600) sampled, clustered upper-right (x=224–283, y=6–16) exactly as reported; column x=60 measures 75→69→73→72→81→72→83→77→89→96 — the noisy ±10-step "gradient" the report describes. STALE vs current code: `1e8c6da` deleted the starmap texture and its starfield is dimmed by atmosphere transmittance and near-sun falloff, so this capture no longer represents HEAD. Whether the new sky is fully star-free at day was not re-rendered — flagged for a fresh capture.

### Finding: No contact shadow / no launch pad — CONFIRMED (current)
**Original**: Ground under engine is identical grass; no AO blob, no pad, tower, or concrete anywhere in frame.
**Verification**: CONFIRMED. 6x crop of the rocket base shows landing legs meeting unmodified grass; samples at x=145–175, y=134–150 are grass tones (113–145, 141–174, 98–136) matching the report's numbers; only "dark" pixels in the region are the engine bell itself (y≤133). No pad structure anywhere in the 320px frame. No commit after capture adds shadows or a pad — still current at HEAD.

### Finding: No atmospheric depth at horizon — CONFIRMED (current)
**Original**: Sky (85,112,129) cuts to terrain in a hard 1px step; zero fog falloff near-vs-far.
**Verification**: CONFIRMED. Resampled exactly: y=57 x=280 = (85,112,129), y=58 x=280 = (163,174,168) — the report's numbers reproduce to the pixel. Near terrain y=58 (172,194,136) ≈ far terrain y=94 (172,198,134): no distance falloff. `915e356` added only a tonemap, no fog — current.

### Finding: Terrain washout / moldy blotches — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: Soft grey/white splotches over mint-cream green, blurry ground vs crisp rocket.
**Verification**: CONFIRMED in the captured pixels (visible throughout the frame and in the shore crop; fixer's own commit message cites "pale-blown terrain"). STALE vs current code: `915e356` (1 min after capture) added ACES tonemap to fs_terrain/fs_sky/lit path and re-floored rock albedo precisely for this. Needs fresh capture to confirm the fix.

### Finding: Shoreline straight, jagged, unblended — CONFIRMED (current)
**Original**: Perfectly straight stair-stepped water edge, no beach/wet-sand/foam transition.
**Verification**: CONFIRMED. 4x crop of lower-left shows the teal/green boundary as a single straight diagonal with clearly visible pixel staircase and zero transition band. No commit addresses shoreline geometry — current.

### Finding: Water flat unlit teal — CONFIRMED (current, with note)
**Original**: Uniform pastel, faint streak noise, no glint/depth/shading.
**Verification**: CONFIRMED in pixels (flat teal field, faint streaks, 4x crop). Note: the "faint streaks" were the clipped ocean-glint worms that `915e356` explicitly reduced ("glint texels clipped to warm-white worms... Reduced"), so post-fix water is likely even flatter — the material-identity complaint stands directionally.

### Finding: Missing AA only marginally visible on rocket — CONFIRMED (as stated)
**Original**: Sub-perceptual at 320px; flagged as QA risk for 1080p, not a visible artifact here.
**Verification**: CONSISTENT WITH PIXELS. No visible jaggies on the ~16px-wide rocket at gate resolution, which is exactly what the finding claims. It is a risk note, not a defect claim.

## map_scene.png

### Finding: Orbit lines stair-stepped — CONFIRMED (current)
**Original**: 1px ellipse arcs show blocky aliasing steps, clearest on the blue orbit top.
**Verification**: CONFIRMED. 4x crop of both arcs shows unambiguous pixel staircase along every curve. No commit after capture touches the map renderer — current at HEAD.

### Finding: Background stars inconsistent oversized blobs — CONFIRMED (current)
**Original**: Sizes range from 1px dots to soft 4–6px fuzzy grey puffs with no logic.
**Verification**: CONFIRMED. The orbit crop shows both sharp 1px dots and several soft grey puffs (e.g. upper-center-left cluster) with no size logic. Current — no map-render changes landed.

### Finding: Both planets at nearly same orbital angle — CONFIRMED (LOW, current)
**Original**: Both bodies sit ~3–4 o'clock on their ellipses; system looks posed.
**Verification**: CONFIRMED. Full frame shows orange and cyan planets nearly radially aligned on the right side of their orbits. This is simulation state, not a render defect — trivially transient in live play.

## vab_scene.png

### Finding: Craft floats in uniform (39,39,63) void — CONFIRMED (current)
**Original**: No floor, grid, walls, or horizon; ~70% of frame empty flat color.
**Verification**: CONFIRMED and worse than reported: pixel count shows (39,39,63) covers 75,081 of 76,800 pixels = 97.8% of the frame. Zero environment content. The only `vab.rs` change in `1e8c6da` is a comment — still current at HEAD.

### Finding: Flat unshaded mint disc at engine base — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: Saturated pastel green ellipse, zero shading, hard-clipped to background.
**Verification**: CONFIRMED in the captured pixels. Mint pixels span y=136–151 (report's 142–151 is the unoccluded lower rows), brightest sampled (134,248,184) vs report's (123,231,171) — same saturated pastel family; row 151 still mint (105,200,149), row 152 pure (39,39,63) — the hard clip at row 152 reproduces exactly. STALE vs current code: the disc is Kenney part geometry, and `1e8c6da` deleted the Kenney kit and re-authored all parts (no disc-drawing code exists in `vab.rs`). Needs fresh capture.

### Finding: Grey body shading patchy/dented — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: Abrupt non-planar facet steps below the orange band vs smooth nose dome; hull reads damaged.
**Verification**: CONFIRMED in the captured pixels. 7x body crop shows the smooth-shaded dome up top, then visibly discordant flat facets with abrupt shade discontinuities below the orange band. STALE vs current code: same `1e8c6da` part re-authoring replaced this geometry entirely.

## sky_north.png

### Finding: Dome-pole UV pinch — radial starburst at center — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: Stars smear into radial spokes converging on the pole point dead-center.
**Verification**: CONFIRMED in the captured pixels. Full frame and 4x center crop show unmistakable radial streaks/spokes converging on a central point; stars near center are unreadable smears. STALE vs current code: `1e8c6da` deleted the equirect texture and its replacement is a 3D point lattice with explicitly "no equirect smear — there is no image to blur", uniform density at poles. Needs fresh capture to confirm.

### Finding: Murky overall exposure — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: Near-black browns dominate; Milky Way barely registers.
**Verification**: CONFIRMED in the captured pixels (frame is dominated by near-black brown). STALE: the dome this judged was deleted in `1e8c6da`; `915e356` also added a tonemap to fs_sky after capture.

## sky_equator.png

### Finding: Corner glow clipped by frame edge (LOW) — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: Upper-left glow fades smoothly (no banding) but corner placement reads as light leak.
**Verification**: CONFIRMED in the captured pixels. 4x corner crop: smooth gradient, clipped by both top and left edges, no banding — the report's own description. The frame-level PASS verdict was fair at capture. STALE vs current code: the textured dome is gone (`1e8c6da`).

## sky_south.png

### Finding: Identical pole pinch at frame center — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: Radial streaking converges at ≈(160,115); stars within ~30px unreadable smears.
**Verification**: CONFIRMED in the captured pixels. 4x center crop shows the same convergence spokes as north; both poles of the dome were broken exactly as reported. STALE vs current code: same `1e8c6da` procedural replacement.

### Finding: Same murky exposure as sky_north — STALE-BY-CONCURRENT-FIX (confirmed true at capture)
**Original**: Near-black dominance, underexposed read.
**Verification**: CONFIRMED in the captured pixels. STALE for the same reasons as sky_north.

## webdemo.png

### Finding: Black rectangle, zero user communication — CONFIRMED (current)
**Original**: Canvas uniformly (5,6,10); only content a 2px hairline at y=720–721; no title, no WebGPU message, no spinner, no fallback.
**Verification**: CONFIRMED by exhaustive pixel scan of all 1280x800. Dominant color (5,6,10) everywhere; the only content is the hairline structure at rows 719–721 plus a subtle 1px (16,16,16) border at row 0 — refinement, not contradiction: row 719–720 are (16,16,16) (with (21,21,21) corners), row 721 is full-width (255,255,255) with (178,178,180) edge pixels — matching the report's "edge (178–207), center (255)". Zero text/UI anywhere. Neither `915e356` nor `1e8c6da` adds any web fallback UI — current at HEAD.

### Finding: Hairline is an unexplained artifact — CONFIRMED (current)
**Original**: 1–2px bright rule with nothing above or below; signals half-initialized layout.
**Verification**: CONFIRMED. 3x crop shows the lone white rule in a black field; precise scan shows it is a container border structure (subtle 16-grey border rows + white bottom edge, plus a matching subtle border at row 0). No commit removes it — current.

---

# Summary

**11 CONFIRMED (still current at HEAD), 9 STALE-BY-CONCURRENT-FIX (all verified true in the captured pixels, code has since changed), 0 DISMISSED.**

- Zero findings were killed — every claim in the vision report is supported by the pixels, and its quoted sample values reproduce to the pixel in every case checked (horizon step, under-rocket grass, mint-disc clip row, webdemo hairline values).
- Stale set (needs fresh captures before judging fixed): flight daytime stars + terrain washout (1.1, 1.4 — fixed by `915e356`/`1e8c6da` sky+tonemap work), all five sky-dome findings (4.1, 4.2, 5.1, 6.1, 6.2 — the entire NASA dome was deleted in `1e8c6da`), and both VAB part-geometry findings (3.2, 3.3 — Kenney kit replaced by authored parts in `1e8c6da`).
- Still-live defects at HEAD: web boot black void with lone hairline (7.1, 7.2), VAB 97.8%-black void with no environment (3.1), rocket with no contact shadow/pad (1.2), hard unfogged horizon (1.3), straight aliased shoreline (1.5), flat water (1.6), aliased map orbit lines (2.1), blob map stars (2.2).
