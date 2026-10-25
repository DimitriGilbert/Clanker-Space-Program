# Ruthless Vision QA Report — Clanker Space Program
Session: ruthless-20261025 · Judge: vision QA (rendered-pixels audit) · Gate resolution 320x240 (not judged as a defect; per audit rules)
Evidence basis: full-frame reads plus 5–10x pixel crops and per-pixel sampling (PIL) of every image in `screenshots/`.

---

## 1. flight_scene.png — FAIL
One-line: A readable rocket-on-grass scene whose believability collapses — stars in a daylight sky, a rocket with zero ground contact, and no launch pad anywhere in frame.

1. **CRITICAL — Starfield and Milky Way visible in the daytime sky.** Location: entire sky band, y=0–58, brightest cluster upper-right. White star points and a diagonal brightening (Milky Way glow) show straight through the launch-day sky. Sampled column x=60 shows the "gradient" is noisy ±10 RGB steps — it is the NASA star map under a blue tint, not an atmosphere. Symptom: the very first scene a player sees reads as "space compositing fail"; the planet has no atmosphere at all.
2. **HIGH — Rocket has no contact shadow and no launch pad.** Location: ground directly beneath the engine, x≈145–175, y≈134–150. Sampled pixels there are (133–145, 157–174, 98–122) — identical to surrounding grass; no darkening, no AO blob. Brief says "on launch pad"; there is no pad, tower, or concrete anywhere in the 320px frame. Symptom: the rocket reads as a floating decal hovering over grass; nothing anchors it to the world.
3. **HIGH — No atmospheric depth at the horizon.** Location: horizon line at y≈58, full width. Sky (85,112,129) cuts to terrain (163,174,168)→(210,211,179) in a hard 1px step; terrain brightness at y=58 (182,197,146) equals terrain at y=94 (172,198,134) — zero distance fog/falloff. Symptom: the world looks like a flat cardboard backdrop pasted against the sky instead of 3km of air.
4. **MEDIUM — Terrain texture reads as moldy blotches, washed-out pastel.** Location: all ground area; worst lower-center and left. Large soft grey/white splotches over mint-cream green, blurry while the rocket is crisp — the ground looks out of focus. Symptom: ground looks dirty/stained rather than varied; palette is over-bright and low-contrast, killing material read.
5. **MEDIUM — Shoreline is a straight, jagged, unblended diagonal.** Location: lower-left, teal region running corner-to-corner. The water edge is a perfectly straight line with stair-stepped pixels and zero transition (no beach, no wet-sand band, no foam). Symptom: reads as a rectangular water plane clipping through terrain, not a coastline.
6. **LOW — Water is flat unlit teal.** Location: the teal band, lower-left. Uniform pastel with faint streak noise; no sun glint, no depth gradient, no shading. Symptom: water has no material identity.
7. **LOW (at this gate) — Missing AA only marginally visible on the rocket silhouette.** At ~16px wide the jaggies are sub-perceptual here; the gate resolution is masking the missing-AA defect, which will be plainly visible at 1080p. Flagged as QA risk, not a visible artifact at this resolution.
Credit: rocket model is well-proportioned with clean two-tone paint and a readable faceted nose cone; camera framing centers the subject with a sane horizon; the scene parses instantly as "rocket on grass".

## 2. map_scene.png — MIXED (best image of the set)
One-line: Instantly parseable orbit diagram with good color-coded bodies, undermined by harshly aliased orbit lines and sloppy background stars.

1. **HIGH — Orbit lines are stair-stepped pixel staircases.** Location: both ellipse arcs, most visible top of the blue orbit (crop y≈40–90). The 1px curves show blocky aliasing steps on every curve — the clearest in-set evidence of the missing anti-aliasing. Symptom: the flagship map view looks unpolished/engine-demo-grade; on scaled-up displays it will crawl when the view moves.
2. **MEDIUM — Background stars are inconsistent oversized blobs.** Location: scattered; e.g. cluster of 4–6px fuzzy grey puffs upper-center-left. Sizes range from 1px dots to soft blobs with no logic. Symptom: reads as noise puffs rather than stars; distracts from the diagram.
3. **LOW — Both planets sit at nearly the same orbital angle.** Location: ~3–4 o'clock on both ellipses. Symptom: system looks statically aligned/posed.
Credit: composition is genuinely good — sun centered, orbits concentric, planet color-matched to its orbit line (orange-on-orange, cyan-on-blue), soft sun glow halo, dark-but-not-black background (39,39,63). This image does its one job: you understand the system in one second.

## 3. vab_scene.png — MIXED
One-line: A nicely lit, centered craft hanging in a pitch-black void with a flat mint-green disc glued to its tail.

1. **HIGH — No environment at all: craft floats in a uniform (39,39,63) void.** Location: entire background, all four corners to center. No floor, no grid, no walls, no horizon line — 70% of the frame is empty flat color. Symptom: an "assembly building" with no building and no floor reads as a model viewer placeholder; there is no spatial anchor for placement/rotation.
2. **MEDIUM — Flat, unshaded mint-green disc at the engine base.** Location: under the engine, rows ≈142–151, pixels up to (123,231,171) — saturated pastel green with zero shading, hard-clipped to background on row 152. Symptom: reads as an untextured placeholder ellipse (wrong color for any plume or pad light), and its hard edge looks like a clipping bug.
3. **MEDIUM — Grey body shading is patchy and dented-looking.** Location: mid-body below the orange band. Flat-shaded triangular facets with abrupt, non-planar shade steps — reads as dented metal rather than deliberate low-poly style, and clashes with the smooth-shaded nose dome above it. Symptom: the hull looks damaged/unclean.
Credit: lighting on the craft itself is the best in the set — believable key from upper-left, soft white dome highlight, orange band reads clearly; framing is centered and generous.

## 4. sky_north.png — FAIL
One-line: Real-star-map density ruined by a pole pinch that smears the zenith into a warp-speed starburst dead-center of frame.

1. **HIGH — Dome-pole UV pinch: stars stretch into radial streaks converging on a central point.** Location: center of frame (the pole). The star field dissolves into a dark radial starburst; individual stars near the pole are smeared into spokes. Symptom: pitch the camera up in flight and the sky turns into a hyperspace effect; the NASA map's precision destroys itself exactly where astronomy fans will look.
2. **LOW — Overall exposure is murky.** The frame is dominated by near-black browns; the Milky Way band present in the texture barely registers. Symptom: looks underexposed rather than "night sky".

## 5. sky_equator.png — PASS
One-line: Clean, believable starfield — sharp round stars, sensible density, and a soft galactic glow that reads as the Milky Way core, not an artifact.

1. **LOW — The bright glow in the upper-left corner is clipped by the frame edge.** Location: top-left corner gradient. It fades smoothly with no banding (verified), so it is plausible texture content, but its corner placement makes it read as a light leak at first glance.
Credit: this is what the dome should look like everywhere. If the poles rendered like this, the sky would be a feature, not a defect.

## 6. sky_south.png — FAIL
One-line: Same defect as north — the south celestial pole renders as a radial smear front and center.

1. **HIGH — Identical dome-pole pinch at frame center.** Location: center (≈x=160,y=115). Radial streaking converges on the pole point; stars within ~30px of center are unreadable smears. Symptom: same "warp effect" as north; both poles of the game's headline star map are visually broken.
2. **LOW — Same murky exposure as sky_north.**
Credit: the surrounding field has lovely star variety and the two poles do show genuinely different sky content — the underlying data work is real.

## 7. webdemo.png — FAIL
One-line: A total black void — confirmed — with nothing but a single 2px white hairline; no UI, no fallback, no error, no loading text of any kind.

1. **CRITICAL — Boot state is a black rectangle with zero user communication.** Verified by pixel scan of the full 1280x800: canvas is uniformly (5,6,10); the only non-black content in the entire page is a 2px light-grey/white horizontal line at y=720–721 spanning full width (edge pixels (178–207), center (255,255,255)). There is no title, no "WebGPU not available" message, no loading spinner, no fallback renderer prompt. Symptom: every user without a WebGPU adapter — and every user during load — sees what looks like a crashed page; the lone hairline makes it look broken rather than merely empty.
2. **MEDIUM — The hairline itself is an unexplained artifact.** Location: y≈720, full width. A 1–2px bright rule with nothing above or below it — presumably the border of an unstyled footer/container. Symptom: signals half-initialized layout; even in a failure state it looks accidental.

---

# Overall verdict

**The owner's claim is PARTIALLY JUSTIFIED — the graphics are not uniformly "shit", but every headline scene ships at least one plausibility-breaking defect, and two screens (web boot, VAB) currently read as broken placeholders.** The raw material is better than the owner's rhetoric: the craft model has real paint and shading work, the orbit map is honestly well-composed, and the NASA star map at the equator heading is genuinely good. What makes the output look shit is integration, not assets: the sky dome is blended into the day scene with no atmosphere at all (stars in daylight, no horizon haze, no fog), nothing in the world casts or receives a shadow, curved lines have no anti-aliasing, and the web entry point communicates nothing when WebGPU is absent. Each individual defect is a one-line shader/material fix; together they make every scene feel like a tech demo from 2001 rather than a styled game.

**Top 5 visual defects by player impact:**
1. **Web demo boots to a black void with no fallback/error UI** (CRITICAL) — the first thing any web player experiences is a screen that looks crashed; the product fails before its graphics are even seen.
2. **Stars and the Milky Way visible in the daytime launch sky** (CRITICAL) — the boot scene, the game's first impression, violates the most basic sky plausibility rule in the genre.
3. **Sky dome pole pinch: radial warp-speed smear at both celestial poles** (HIGH) — the headline NASA star map self-destructs exactly where players will aim the camera to admire it.
4. **Rocket has no contact shadow, no exhaust, and no launch pad** (HIGH) — the hero object floats over moldy pastel grass with zero physical grounding.
5. **VAB is a craft in a black void with a flat mint placeholder disc** (HIGH) — the primary craft-editing space has no floor, no environment, and an unshaded green ellipse that reads as a bug.

(Just missing the cut: hard un-fogged horizon line and straight aliased shoreline in the flight scene; stair-stepped orbit lines in map view.)
