# Ruthless-audit visual capture log — 2026-10-25, 13:00–13:15 local

All commands run with cwd = repo root. GPU: surfaceless wgpu device via
lavapipe/RADV (no DISPLAY). Cargo invocations serialized.

## Git state during capture

Start of capture (13:00):

```
 M crates/csp-render/src/terrain.wgsl
 M crates/csp-view/Cargo.toml
 M crates/csp-view/tests/render_gate.rs
?? .review/session-ruthless-20261025/

crates/csp-render/src/terrain.wgsl   |  49 +++++-
crates/csp-view/Cargo.toml           |   3 +
crates/csp-view/tests/render_gate.rs | 327 +++++++++++++++++++++++++++++++++++
```

The concurrent fixing agent was mid-edit (terrain.wgsl + the render gate
test were its in-flight work). **Everything compiled on the first
attempt** — no retry loop was needed. One `cargo` invocation briefly
blocked on the build-directory lock ("Blocking waiting for file lock on
build directory") and then proceeded; that is the fixing agent's
overlap, not a breakage.

Mid-capture (13:05) the fixing agent landed commit `915e356`
"fix(render): terrain washout + white-worm VFX artifacts" — the three
dirty files above went clean, and that commit also swept the first four
screenshots of this session (flight_scene, sky_north/equator/south) and
`exploration-map.md` into its tree. Screenshots captured after 13:02
(map_scene, vab_scene, webdemo) are untracked working-tree files.

Timeline of the fixing agent's recent commits for context:
`ac724a6` feat(app), `b644a73` feat(art), `20660df` fix sky, `396e12c`
fix app+render, `915e356` fix terrain washout (13:02:43, during this
capture).

## Step 2 — render gate

```
cargo test -p csp-view --test render_gate -- --nocapture
```

Result: **2 passed; 0 failed; 5 ignored** (the 5 `dump_*` scratches).

Verbatim pixel-stat output (gold — real GPU readback):

```
empty scene:  non_clear=76800/76800 distinct=4 spread=19
test empty_flight_scene_is_what_the_gate_rejects ... ok
seeded scene: non_clear=76800/76800 distinct=148 spread=177
test boot_scene_has_real_geometry ... ok
```

Reading: the negative control rasterizes a 4-bucket gradient (the
fallback sky only), the seeded boot scene reaches 148 color buckets /
177 channel spread — real lit terrain + craft, not a void.

## Step 3 — flight scene PNG

```
cargo test -p csp-view --test render_gate dump -- --ignored --nocapture
magick /tmp/frame.ppm .review/session-ruthless-20261025/screenshots/flight_scene.png
```

The `dump` substring filter also ran `dump_low`, `dump_surface`,
`dump_flight` — all ok. Verbatim:

```
/tmp/frame_low.ppm: white=0
/tmp/surf.ppm: h_here=-145 white=0
/tmp/flight.ppm: alt=715 white=74
```

`white=74` at 715 m altitude post-washout-fix (the fix agent's commit
claims the white-worm VFX streaks are gone; 74 near-white px of 76800
is residual sky/bright pixels, not scribbles — see stats below).

## Step 4 — sky PNGs

```
cargo test -p csp-view --test render_gate dump_sky -- --ignored --nocapture
wrote /tmp/sky_north.ppm
wrote /tmp/sky_equator.ppm
wrote /tmp/sky_south.ppm
```

All three converted to `screenshots/sky_{north,equator,south}.png`.
These render the NASA starfield dome alone (commit `20660df` replaced
the landscape-photo sky).

## Step 5 — NEW dumper: orbit map + VAB 3D scene

Created `crates/csp-view/tests/render_dump_extra.rs` (new file, no
conflict with the fixing agent) cloning render_gate.rs machinery:
surfaceless LowPower device, 320x240 Rgba8UnormSrgb target, clear
`[0.02, 0.02, 0.05, 1]`, copy_texture_to_buffer + map_async readback,
P6 PPM. Map pass attaches no depth (map pipelines declare none — same
as csp-app `paint`); VAB pass attaches `VabRenderer::depth_view()`
cleared 0.0 (reverse-Z).

```
cargo test -p csp-view --test render_dump_extra -- --ignored --nocapture

map packet: bg=2178 lines=6168 markers=66
wrote /tmp/map.ppm
test dump_map ... ok
vab packet: vertices=12633 pick_ids=3 epoch=0
wrote /tmp/vab.ppm
test dump_vab ... ok

test result: ok. 2 passed; 0 failed; 0 ignored
```

Both scenes extracted real content: the map carries a 2178-vertex
starfield backdrop, 6168 orbit-line vertices and 66 marker vertices;
the VAB carries a 12633-vertex 3-part kitbash craft (starter "Stock
Hopper"). Converted to `screenshots/map_scene.png`,
`screenshots/vab_scene.png`.

## Step 6 — full workspace tests

```
cargo test --workspace
```

Exit code 0. **908 passed; 0 failed; 8 ignored** across 55 test-result
blocks. Second confirming run (warm cache): identical, EXIT=0.
Slowest: `csp_sim` lib tests 424.64s (337 tests).

Notable per-target counts (non-empty targets):

| target | result |
|---|---|
| csp_sim lib | 337 passed (424.6 s) |
| csp_phys lib | 146 passed |
| csp_server e2e | 22 passed |
| csp_app lib | 38 passed |
| csp_data lib | 64 passed |
| csp_server lib | 28 passed |
| csp_ui lib | 34 passed |
| csp_script lib | 11 passed |
| xtask lint tests | 11 passed |
| eval_suite | 11 passed |
| render_gate | 2 passed, 5 ignored (scratch dumps) |
| render_dump_extra | 2 ignored (this session's dumpers) |

8 ignored total = 5 render_gate dumps + 2 render_dump_extra dumps + 1
csp_core doctest. **No crate failed.**

## Step 7 — webdemo screenshot (best effort, time-boxed)

`apps/demo/dist/` exists (index.html, csp_app.js, csp_app_bg.wasm).

Attempt 1 — `file://` direct: blocked by CORS on the module script:
```
"Access to script at 'file://.../csp_app.js' from origin 'null' has
been blocked by CORS policy: Cross origin requests are only supported
for protocol schemes: chrome, chrome-extension, data, http, https..."
```

Attempt 2 — served via `python3 -m http.server` on 127.0.0.1:8791,
`--enable-unsafe-swiftshader`: page boots, wasm loads, then:
```
CONSOLE: "No available adapters."
CONSOLE: "No font provider has any font for FontFamily::Proportional.
No text will be visible. Enable the `default_fonts` feature of
`epaint`/`egui` to bundle fonts, or install a `FontProvider` of your own."
CONSOLE: "GPU init failed: adapter: No suitable graphics adapter found;
noop not requested, vulkan support not compiled in, metal support not
compiled in, dx12 support not compiled in, gl not requested, webgpu
found no adapters"
```

Attempt 3 — added `--use-angle=swiftshader --enable-features=Vulkan
--use-vulkan --disable-gpu-sandbox`:
```
CONSOLE: "Failed to create WebGPU Context Provider"
(same GPU-init failure, same font warnings)
```

`screenshots/webdemo.png` (1280x800) was captured but is the failed-boot
black page: 11 unique colors, mean ≈ 7/255. Two findings independent of
this box's lack of a headless-WebGPU adapter:

1. **The shipped wasm build bundles no fonts** (`default_fonts` feature
   off, no FontProvider) — on any machine without fontconfig fonts the
   entire UI is textless by design. Console says so verbatim.
2. The app has no visible fallback UI for the no-WebGPU case — the
   failure lives only in the console; the page stays black.

## Screenshot inventory + stats

`magick identify` + `%[mean]`/`%[standard-deviation]` (quantum 0–65535)
+ `%k` unique colors, per produced PNG:

| file | size | mean | stddev | unique colors | verdict |
|---|---|---|---|---|---|
| flight_scene.png | 320x240 | 40065.6 | 7994.35 | 33607 | rich lit scene, no washout (mean 61%, high color diversity) |
| map_scene.png | 320x240 | 13133.9 | 5450.33 | 1042 | starfield + orbit lines + markers on dark clear |
| vab_scene.png | 320x240 | 12636.1 | 4233.75 | 1165 | 3-part lit craft on dark editor background |
| sky_north.png | 320x240 | 10445.4 | 4528.66 | 6457 | starfield dome, dark |
| sky_equator.png | 320x240 | 8280.47 | 4353.42 | 4330 | starfield dome, darkest view |
| sky_south.png | 320x240 | 10285.2 | 4866.26 | 7400 | starfield dome |
| webdemo.png | 1280x800 | 1890.69 | 2258.35 | 11 | FAILED boot — black page (see step 7) |

No screenshot is a uniform void; the flight scene in particular shows
33.6k distinct colors (pre-fix screenshots of the washout build were
pale two-tone; current mean 61% with 8k stddev indicates shading
variety survived the ACES tonemap).

## Compilation verdict

The working tree compiled cleanly at every point it was touched during
this capture (13:00–13:15), including the moment the fixing agent held
uncommitted terrain.wgsl edits. The only observed contention was one
transient build-lock wait. No "working tree does not compile" finding.
