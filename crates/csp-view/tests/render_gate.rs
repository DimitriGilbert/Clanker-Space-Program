//! M10 render gate — the verification the shipped "uniform purple"
//! build was missing.
//!
//! The failure mode it guards: the app booted into Flight mode with no
//! vessel InBubble, so `extract_flight` produced a packet with no
//! terrain and only the fallback sky — which then failed its own
//! reverse-Z depth test (`Greater` against a 0.0-cleared buffer cannot
//! pass geometry at NDC z = 0). Every pixel stayed the clear color and
//! no test could see it: packet-level unit tests can't observe a
//! rasterized framebuffer. Only a real device + pixel readback can.
//!
//! These tests create a wgpu device with **no surface**
//! (`compatible_surface: None` — lavapipe's `libvulkan_lvp.so`
//! provides a real Vulkan adapter on dev/CI boxes), render the flight
//! scene exactly the way `csp-app`'s scene pass does — same clear
//! color, same reverse-Z depth target, terrain-then-vertex draw order —
//! and read the pixels back through `copy_texture_to_buffer` +
//! `map_async`.
//!
//! - [`boot_scene_has_real_geometry`]: the seeded new-game view (what
//!   `csp-app` now constructs) must produce a terrain packet + craft
//!   vertices AND the rendered image must differ from the uniform
//!   clear color — real 3D output, not a void.
//! - [`empty_flight_scene_is_what_the_gate_rejects`]: the unseeded
//!   view — exactly the shipped bug — produces no terrain packet and
//!   only the fallback-sky vertex count, so the positive test's
//!   structural assertions fail on it. It also pins that the repaired
//!   fallback sky actually rasterizes now (the whole-screen gradient,
//!   not the clear color).
//!
//! No adapter on the host (a machine without lavapipe or any Vulkan
//! ICD): the tests print a `SKIP` line and return early rather than
//! fail — the gate exists to catch renderer/content regressions where
//! a GPU exists to run them, not to require one. **Except** in
//! required mode: `CSP_RENDER_GATE_REQUIRED=1` turns a missing
//! adapter into a test failure, so CI (which installs lavapipe and
//! sets the flag) can never degenerate into a permanent green no-op —
//! the defect where the gate silently never ran.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_view::View;

/// Offscreen render size. `W * 4` must be a multiple of wgpu's 256-byte
/// copy-row alignment (320 · 4 = 1280 = 5 · 256).
const W: u32 = 320;
const H: u32 = 240;
/// The app's scene-pass clear color (csp-app `paint`, linear RGBA).
const CLEAR: [f64; 4] = [0.02, 0.02, 0.05, 1.0];
/// Any surface-legal format works — the renderers take it as a param.
/// sRGB like the real surface path.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A surfaceless adapter + device — `None` when no driver exists
/// (the documented skip case). LowPower prefers the CPU adapter
/// (lavapipe) over a real GPU when both exist: the test should run on
/// the weakest conformant implementation, not the fastest.
fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        ..Default::default()
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("render-gate"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .ok()?;
    Some((device, queue))
}

/// Whether the render gate is REQUIRED on this host —
/// `CSP_RENDER_GATE_REQUIRED` set (CI sets it, alongside the lavapipe
/// install step). In required mode a missing adapter is a failure,
/// not a skip: otherwise losing the Vulkan ICD turns the gate into a
/// silent green no-op.
fn render_gate_required() -> bool {
    std::env::var("CSP_RENDER_GATE_REQUIRED").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// The gate's device — or a loud `SKIP` line and `None`. In required
/// mode a missing adapter panics instead of skipping.
fn gate_device(test: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
    match headless_device() {
        Some(dq) => Some(dq),
        None if render_gate_required() => panic!(
            "render-gate({test}): CSP_RENDER_GATE_REQUIRED is set but no headless \
             GPU adapter exists — install a Vulkan ICD (`mesa-vulkan-drivers` ships \
             lavapipe's libvulkan_lvp.so) or unset the flag"
        ),
        None => {
            eprintln!(
                "render-gate: SKIP {test} — no headless GPU adapter (install lavapipe; \
                 CSP_RENDER_GATE_REQUIRED=1 makes this a failure)"
            );
            None
        }
    }
}

/// One frame of the app's §2.5 loop, headlessly — enough for the
/// seeded spawn command to land and the extract to emit a packet.
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
}

/// One presentation frame with NO sim advance — the schedules
/// (Presentation/UiBuild/RenderExtract) run, so the terrain cache
/// still bakes and packets still emit, but the scene clock stands
/// still. `until_real_terrain` polls with these so the capture time
/// cannot drift with the bake's wall-time (the 3b vision-check
/// failure: paired armed/disarmed dumps minutes apart compared
/// different suns — a craft shadow appeared in one capture and not
/// the other).
fn idle_frame(view: &mut View) {
    view.begin_frame();
    view.end_frame();
}

/// Advance the sim to an absolute scene time (seconds) if `DUMP_TIME`
/// is set — the dump-suite determinism knob. Pairs with
/// `until_real_terrain`'s no-sim polling: capture time = spawn time +
/// the test's own fixed stepping + this pinned offset, never wall
/// clock.
fn pin_dump_time(view: &mut View) {
    if let Ok(t) = std::env::var("DUMP_TIME")
        && let Ok(secs) = t.parse::<f64>()
    {
        let target = (secs * csp_core::TICK_HZ as f64).round() as u64;
        let now = view.sim.clock().ticks();
        if target > now {
            frame(view, target - now);
        }
    }
}

/// Advance `view` until the flight packet carries the REAL baked
/// heightfield for the dominant body. On native the cache bakes on a
/// background thread (C12-F5) and, until it lands, the packet is
/// deliberately SKY-ONLY — `heightfield: None` with no terrain or
/// ocean instances (M5 §5.2-R2 arrival polish: the player must never
/// see a flat datum sphere where mountains should be), so a relief
/// dump captured in the first few frames draws no ground at all — a
/// relief-verification dump captured there proves nothing. Polls the
/// packet's heightfield content key against `bake_profiled_key` for
/// the AUTHORED `(seed, profile)` — the same key contract
/// `TerrainCache::field` serves, all public API — and panics if the
/// bake never lands.
fn until_real_terrain(view: &mut View) {
    let want = {
        let cfg = view.world.resource::<csp_view::terrain::TerrainConfig>();
        let t = cfg.bodies.get("terrin").unwrap();
        csp_render::terrain::HeightField::bake_profiled_key(t.seed, t.profile)
    };
    // Bound: a debug-build Mountain bake is single-digit seconds; poll
    // up to ~1.5 min of idle frames+sleeps before calling it a stall.
    // IDLE frames (no sim step): the bake progresses on wall-time while
    // the scene clock stands still — the capture time below is
    // therefore a function of the test's fixed stepping (+ DUMP_TIME)
    // only, never of how long the bake happened to take.
    for _ in 0..12_000 {
        idle_frame(view);
        if let Some(p) = view.take_flight_packet()
            && let Some(t) = &p.terrain
            && let Some(h) = &t.heightfield
            && h.key == want
        {
            // The detection take consumed this frame's packet — emit
            // one more so the caller's own `take_flight_packet` sees
            // a fresh packet (the cache now serves the real field
            // every frame, so the re-emit carries it too).
            idle_frame(view);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!(
        "the real Terrin heightfield never landed in the flight packet — background bake stalled?"
    );
}

/// What the readback measured. `distinct` is a color-diversity count
/// (4-bit-per-channel buckets): a uniform image has 1, a two-stop
/// gradient a few dozen, a lit scene hundreds.
struct Stats {
    /// Pixels differing from the clear color by more than `tol`.
    non_clear: usize,
    /// Occupied 16-per-channel color buckets.
    distinct: usize,
    /// Largest per-channel spread across the image (uniform ⇒ 0).
    max_spread: u8,
    /// Near-white pixels (every channel ≥ 210): the distinguishing
    /// signal for the washed-out bug shape — a plausible scene has
    /// some (sun glints), a blown frame is mostly white.
    near_white: usize,
}

fn analyze(img: &[u8]) -> Stats {
    // Expected byte value of the linear clear color under the sRGB
    // attachment's write conversion — computed, not assumed, so a
    // format change can't silently invalidate the check.
    let clear_byte = |v: f64| -> u8 {
        let s = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    };
    let clear = [
        clear_byte(CLEAR[0]),
        clear_byte(CLEAR[1]),
        clear_byte(CLEAR[2]),
    ];
    let mut buckets = [0u32; 4096];
    let mut lo = [255u8; 3];
    let mut hi = [0u8; 3];
    let mut non_clear = 0usize;
    for px in img.chunks_exact(4) {
        let diff = (0..3).any(|c| px[c].abs_diff(clear[c]) > 8);
        if diff {
            non_clear += 1;
        }
        let b = ((px[0] as usize >> 4) << 8) | ((px[1] as usize >> 4) << 4) | (px[2] as usize >> 4);
        buckets[b] += 1;
        for c in 0..3 {
            lo[c] = lo[c].min(px[c]);
            hi[c] = hi[c].max(px[c]);
        }
    }
    let mut near_white = 0usize;
    for px in img.chunks_exact(4) {
        if px[0] >= 210 && px[1] >= 210 && px[2] >= 210 {
            near_white += 1;
        }
    }
    Stats {
        non_clear,
        distinct: buckets.iter().filter(|n| **n > 0).count(),
        max_spread: (0..3).map(|c| hi[c] - lo[c]).max().unwrap_or(0),
        near_white,
    }
}

/// Rec.709 luma of one RGBA8 readback pixel, in 8-bit levels.
fn luma(px: &[u8]) -> f64 {
    (2126.0 * f64::from(px[0]) + 7152.0 * f64::from(px[1]) + 722.0 * f64::from(px[2])) / 10_000.0
}

/// [`luma`] averaged over the 3×3 block of pixels centred on `(x, y)`, in
/// 8-bit levels.
fn box_luma(img: &[u8], w: usize, x: usize, y: usize) -> f64 {
    let mut sum = 0.0;
    for yy in y - 1..=y + 1 {
        for xx in x - 1..=x + 1 {
            sum += luma(&img[(yy * w + xx) * 4..]);
        }
    }
    sum / 9.0
}

/// Mean |4-neighbour Laplacian| of the luma over the pixels at least two in
/// from the border, in 8-bit levels — the grain metric. The luma is
/// low-passed with a 3×3 box filter in floating point before the kernel
/// runs: `fs_sky`'s output dither is a noise of σ ≈ 0.3 levels, uncorrelated
/// pixel to pixel, and a per-pixel kernel reads it at 0.92 — right at a
/// bound that admits the sky. Pixel-uncorrelated noise of standard
/// deviation S levels measures 0.50·S (3.56·S without the filter), so the
/// bound has room on both sides of the dither; a smooth sky measures far
/// less whether or not it was quantised to 8 bits on the way out. Every box
/// the kernel reads lies inside the frame, so no edge convention is needed.
fn mean_laplacian(img: &[u8], w: usize, h: usize) -> f64 {
    let mut sum = 0.0;
    let mut n = 0u64;
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let c = box_luma(img, w, x, y);
            let around = box_luma(img, w, x - 1, y)
                + box_luma(img, w, x + 1, y)
                + box_luma(img, w, x, y - 1)
                + box_luma(img, w, x, y + 1);
            sum += (4.0 * c - around).abs();
            n += 1;
        }
    }
    sum / n as f64
}

/// The grain bound of [`mean_laplacian`], in 8-bit levels. On the ground
/// zenith frame the jittered march measures 2.45–2.47 against it, the
/// deterministic stratum march with `fs_sky`'s output dither 0.14: 6.2×
/// over and 2.8× under, on lavapipe and RADV alike.
const GRAIN_BOUND: f64 = 0.4;

/// Rec.709 luma averaged over the frame, in 8-bit levels.
fn mean_luma(img: &[u8]) -> f64 {
    img.chunks_exact(4).map(luma).sum::<f64>() / (img.len() / 4) as f64
}

/// How far a star must rise above its surroundings to count: the sky
/// itself is smooth to within one quantisation level, the dimmest
/// resolved star peaks dozens of levels above it.
const STAR_CONTRAST: f64 = 6.0;

/// Star pixels: those whose luma tops the median of their 5×5
/// neighbourhood by more than [`STAR_CONTRAST`] — an isolated bright
/// point on the sky, whatever the sky's own brightness. The 2-pixel
/// border has no full neighbourhood and is not tested.
fn star_pixels(img: &[u8], w: usize, h: usize) -> usize {
    let mut window = [0f64; 25];
    let mut n = 0usize;
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            for (k, v) in window.iter_mut().enumerate() {
                let yy = y + k / 5 - 2;
                let xx = x + k % 5 - 2;
                *v = luma(&img[(yy * w + xx) * 4..]);
            }
            window.sort_by(|a, b| a.total_cmp(b));
            if luma(&img[(y * w + x) * 4..]) - window[12] > STAR_CONTRAST {
                n += 1;
            }
        }
    }
    n
}

/// Render one `FlightPacket` into an offscreen texture the way the
/// app's scene pass does (clear color, reverse-Z depth, terrain then
/// craft stream), and read the RGBA pixels back.
fn render_flight_scene(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    packet: &csp_render::flight::FlightPacket,
) -> Vec<u8> {
    render_flight_scene_sized(device, queue, packet, W, H)
}

/// Sized variant — the 256 B copy-row alignment must divide `w*4`.
/// Dump A/B knobs, both mutating a packet CLONE so the standing gate
/// tests (which never set them) always exercise the extract's real
/// rows: `DUMP_SHADOWS=0` zeroes the packet's cascade row, rendering
/// the SAME frame disarmed for the phase-3b shadow-evidence diff;
/// `DUMP_DEREPETITION=0` raises the terrain uniform's de-repetition
/// kill flag (`uniforms.art.z`), rendering the same frame with the
/// pre-4b plain periodic tiling for the phase-4b tile-periodicity
/// A/B pair.
#[allow(clippy::too_many_lines)] // one render-pass recipe for the gate
fn render_flight_scene_sized(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    packet: &csp_render::flight::FlightPacket,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let disarmed = std::env::var("DUMP_SHADOWS").is_ok_and(|v| v == "0");
    let plain_tiles = std::env::var("DUMP_DEREPETITION").is_ok_and(|v| v == "0");
    let patched;
    let packet = if disarmed || plain_tiles {
        let mut p = packet.clone();
        if disarmed {
            p.shadow = csp_render::shadow::ShadowUniforms::default();
        }
        if plain_tiles && let Some(t) = p.terrain.as_mut() {
            t.uniforms.art[2] = 1.0; // terrain.wgsl's de-repetition kill flag
        }
        patched = p;
        &patched
    } else {
        packet
    };
    // Same target layout as the app: a 4×-multisampled color
    // attachment resolved into the single-sampled readback texture.
    let msaa_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render-gate-msaa"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: csp_render::MSAA_SAMPLES,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let msaa = msaa_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let color_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render-gate-target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target = color_tex.create_view(&wgpu::TextureViewDescriptor::default());
    // Terrain first: the flight renderer borrows the cascade
    // resources (R1/3b) from it, mirroring the app's construction.
    let mut terrain = csp_render::terrain::TerrainRenderer::new(device, FORMAT);
    let mut flight = csp_render::flight::FlightRenderer::new(device, FORMAT, (w, h), &terrain);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render-gate"),
    });
    // The R1/3b cascade phase, exactly where the app records it:
    // BEFORE the scene pass, craft cast staged from the same packet.
    // Disarmed packets (all-zero `SU`) still take the call — it is
    // the disarm write path too.
    if let Some(tp) = &packet.terrain {
        let cast = flight.stage_cascade_cast(device, queue, packet);
        terrain.draw_shadow(&mut encoder, device, queue, tp, &packet.shadow, cast);
    }
    // §2.3's two-pass flight frame, mirroring the app: the FAR pass
    // (sky backdrop + far instances + far flight stream, depth cleared
    // 0.0, no resolve) records first; the NEAR pass below loads the
    // color, clears the depth again, and draws near terrain → craft →
    // near ocean exactly where the app does.
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene-far"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &msaa,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: CLEAR[0],
                        g: CLEAR[1],
                        b: CLEAR[2],
                        a: CLEAR[3],
                    }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: flight.depth_view(),
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    // The near pass clears again before reading — the
                    // far depths are write-only (store discarded, the
                    // app's far pass mirrored exactly).
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(tp) = &packet.terrain {
            terrain.draw_far(&mut pass, device, queue, tp);
        }
        flight.draw_far(&mut pass, device, queue, packet);
    }
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &msaa,
                resolve_target: Some(&target),
                ops: wgpu::Operations {
                    // The far pass owns the frame's clear; this pass
                    // composites over its samples.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            // Same attachment as the app: the flight renderer's shared
            // reverse-Z target, cleared 0.0 (far) again — the §2.3
            // partition (near never tests against far depths).
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: flight.depth_view(),
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // The app's near-pass draw order: near terrain, craft vertex
        // stream, then the depth-test-only near ocean over both. The
        // sky and far instances drew in the far pass above.
        if let Some(tp) = &packet.terrain {
            terrain.draw(&mut pass, device, queue, tp);
        }
        flight.draw(&mut pass, device, queue, packet);
        if let Some(tp) = &packet.terrain {
            terrain.draw_ocean(&mut pass, tp);
        }
    }
    let row_bytes = w as usize * 4;
    debug_assert_eq!(row_bytes % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize, 0);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("render-gate-readback"),
        size: (row_bytes * h as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes as u32),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let (tx, rx) = std::sync::mpsc::channel();
    buf.map_async(wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().expect("map_async failed");
    let img = buf.get_mapped_range(..).unwrap().to_vec();
    buf.unmap();
    img
}

/// The positive gate: the new-game view — `View::new` + the app's
/// `seed_starter_craft` — must produce a packet with real scene content
/// (terrain + craft vertices) AND rasterize to an image that is not
/// the uniform clear color.
#[test]
fn boot_scene_has_real_geometry() {
    let Some((device, queue)) = gate_device("boot_scene_has_real_geometry") else {
        return;
    };

    let mut view = View::new().expect("data/ load");
    // The app's own startup path — `App::new_inner` calls exactly this.
    view.seed_starter_craft().expect("starter seed");
    view.set_flight_viewport(f64::from(W), f64::from(H));
    // The authored launch site is authored where the baked heightfield
    // sits ~at the sim's datum (the old lon-π site was under a ~+57 m
    // plateau and a datum spawn rendered buried): the default 60 m
    // chase camera now frames the craft on flat ground, but a pulled
    // back camera keeps the horizon — lit terrain + sky + pad — in the
    // frame so the pixel-variety assertions measure real scene content.
    view.world
        .resource_mut::<csp_view::flight::FlightInput>()
        .set_distance(300.0);
    for _ in 0..3 {
        frame(&mut view, 1);
    }
    // Wait out the background heightfield bake: the first frames after
    // the seed are deliberately sky-only (M5 §5.2-R2 arrival polish —
    // no flat datum stand-in), and this gate exists to prove TERRAIN
    // content specifically.
    until_real_terrain(&mut view);
    assert_eq!(view.mode(), csp_view::ViewMode::Flight);
    assert!(
        view.flight_focus().is_some(),
        "no focused vessel after the starter seed"
    );

    let packet = view
        .take_flight_packet()
        .expect("RenderExtract produced a flight packet");
    // Structural half of the gate: real content, not the fallback-only
    // packet. `terrain: None` is precisely the shipped bug's shape.
    assert!(
        packet.terrain.is_some(),
        "no terrain packet — an empty flight scene rasterizes to nothing"
    );
    const FALLBACK_SKY_VERTS: usize = 6;
    assert!(
        packet.vertices.len() > FALLBACK_SKY_VERTS,
        "only the {}-vertex fallback sky emitted — no craft geometry",
        FALLBACK_SKY_VERTS
    );

    // Pixel half: the rasterized scene is not the uniform clear color.
    let img = render_flight_scene(&device, &queue, &packet);
    let stats = analyze(&img);
    let total = (W * H) as usize;
    eprintln!(
        "seeded scene: non_clear={}/{} distinct={} spread={} near_white={}",
        stats.non_clear, total, stats.distinct, stats.max_spread, stats.near_white
    );
    assert!(
        stats.non_clear as f64 / total as f64 > 0.5,
        "most of the frame is still the clear color ({}/{}) — the scene pass drew nothing",
        stats.non_clear,
        total
    );
    assert!(
        stats.distinct > 8 && stats.max_spread > 30,
        "image lacks real scene shading ({} buckets, spread {})",
        stats.distinct,
        stats.max_spread
    );
    // Distinguishing assertion (verified-report-13-F1): the old gate
    // passed the historical washed-out bug shape because it only
    // checked non-clear + color buckets. A blown-white frame now
    // fails — the control test below proves this bound rejects it.
    assert!(
        (stats.near_white as f64 / total as f64) < 0.25,
        "a quarter of the frame is near-white — the washed-out bug shape ({}/{})",
        stats.near_white,
        total
    );
}

/// The washed-out control — proves the near-white bound above is not
/// vacuous. The same seeded packet rendered with the Rayleigh
/// inscatter driven 80× hot reproduces the "pale-blown" bug shape:
/// it rasterizes plenty (non_clear passes trivially) but the frame
/// is a white veil the old assertion set would have blessed.
#[test]
fn washed_out_scene_is_what_the_gate_rejects() {
    let Some((device, queue)) = gate_device("washed_out_scene_is_what_the_gate_rejects") else {
        return;
    };

    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    view.set_flight_viewport(f64::from(W), f64::from(H));
    view.world
        .resource_mut::<csp_view::flight::FlightInput>()
        .set_distance(300.0);
    for _ in 0..3 {
        frame(&mut view, 1);
    }
    // Same wait as the positive gate — the control must be the SAME
    // scene shape driven hot, sky-only frames would weaken the A/B.
    until_real_terrain(&mut view);
    let mut packet = view
        .take_flight_packet()
        .expect("RenderExtract produced a flight packet");
    // Drive the inscatter term 80× hot — the same failure shape the
    // washed-out bug produced (everything rasterizes, everything is
    // white).
    if let Some(tp) = &mut packet.terrain {
        for c in tp.uniforms.rayleigh.iter_mut().take(3) {
            *c *= 80.0;
        }
        for c in tp.uniforms.biome_colors.iter_mut().flatten() {
            *c = (*c * 6.0).min(1.0);
        }
    }
    let img = render_flight_scene(&device, &queue, &packet);
    let stats = analyze(&img);
    let total = (W * H) as usize;
    eprintln!(
        "washed control: non_clear={}/{} near_white={}",
        stats.non_clear, total, stats.near_white
    );
    // The frame DID rasterize (the old gate would pass it)…
    assert!(
        stats.non_clear as f64 / total as f64 > 0.5,
        "control failed to rasterize — it must differ from clear, not be absent"
    );
    // …but the distinguishing bound rejects it.
    assert!(
        stats.near_white as f64 / total as f64 >= 0.25,
        "the washed-out control should FAIL the near-white bound ({}/{}) — \
         the gate's distinguishing assertion is vacuous",
        stats.near_white,
        total
    );
}

/// The negative control — proves the gate above is not vacuous. A
/// `View::new` WITHOUT the starter seed is exactly what shipped: the
/// packet carries no terrain and only the fallback sky, so every
/// structural assertion in `boot_scene_has_real_geometry` fails on it.
/// The rasterized gradient additionally pins the depth-compare fix —
/// before `GreaterEqual`, even this 6-vertex sky was culled and the
/// readback was the uniform clear color.
#[test]
fn empty_flight_scene_is_what_the_gate_rejects() {
    let Some((device, queue)) = gate_device("empty_flight_scene_is_what_the_gate_rejects") else {
        return;
    };

    let mut view = View::new().expect("data/ load");
    view.set_flight_viewport(f64::from(W), f64::from(H));
    for _ in 0..3 {
        frame(&mut view, 1);
    }
    let packet = view
        .take_flight_packet()
        .expect("RenderExtract produced a flight packet");
    // The shipped bug's packet shape — the positive gate's
    // `terrain.is_some()` / `vertices > 6` assertions fail on this.
    assert!(
        packet.terrain.is_none(),
        "an empty flight scene must not claim a terrain packet"
    );
    // §2.3: the fallback sky rides the FAR stream now (the far pass
    // owns the sky backdrop) — the near stream stays empty for this
    // craftless, environment-less scene.
    assert_eq!(
        packet.far_vertices.len(),
        6,
        "an empty flight scene emits only the fallback-sky quad (far stream)"
    );
    assert!(
        packet.vertices.is_empty(),
        "an empty flight scene has no near-stream geometry"
    );

    // With the `GreaterEqual` fix the fallback sky rasterizes — the
    // readback is the two-stop gradient, NOT the clear color. Before
    // the fix this image was uniform clear (the reported bug).
    let img = render_flight_scene(&device, &queue, &packet);
    let stats = analyze(&img);
    let total = (W * H) as usize;
    eprintln!(
        "empty scene:  non_clear={}/{} distinct={} spread={}",
        stats.non_clear, total, stats.distinct, stats.max_spread
    );
    assert!(
        stats.non_clear as f64 / total as f64 > 0.9,
        "the fallback sky failed to rasterize — the frame is the clear \
         color again ({}/{})",
        stats.non_clear,
        total
    );
}

/// A zenith-looking sky-only packet on an atmosphere-armed Terrin —
/// the camera sits at `alt_m` over the +x datum looking straight up
/// (every ray crosses the shell, so `fs_sky` runs the gated
/// shell-march path, never the vacuum early-return). `sun_dir` is a
/// unit direction in the body frame; keep it outside the ~45°
/// half-fov around zenith or its disc legitimately whites pixels.
fn zenith_sky_packet(
    alt_m: f64,
    sun_dir: [f32; 4],
    art: &std::sync::Arc<csp_render::terrain::TerrainArt>,
) -> csp_render::terrain::TerrainPacket {
    const R: f64 = 600_000.0;
    const TOP: f64 = 70_000.0;
    const H_SCALE: f64 = 5_600.0;
    const AMP: f64 = 2_400.0;
    let eye = R + alt_m;
    let aspect = f64::from(W) / f64::from(H);
    let t = (35.0f64.to_radians()).tan();
    csp_render::terrain::TerrainPacket {
        sky: true,
        art: Some(art.clone()),
        uniforms: csp_render::terrain::TerrainUniforms {
            // Columns (right, up, −fwd): the centre ray `rd = view_rot
            // · (0,0,−1)` lands on +x — straight up from the eye.
            view_rot: [
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [-1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            eye_rel_body: [eye as f32, 0.0, 0.0, 0.0],
            sun_dir,
            planet: [R as f32, TOP as f32, R as f32, AMP as f32],
            atmo: [H_SCALE as f32, 0.0, alt_m as f32, 0.0],
            misc: [
                (t * aspect) as f32,
                t as f32,
                (eye * eye - (R + TOP) * (R + TOP)) as f32,
                (eye * eye - R * R) as f32,
            ],
            spin: [0.0, 0.0, 1.0, 0.0],
            rayleigh: [5.8e-6, 13.5e-6, 33.1e-6, 0.0],
            art: [1.0, 0.0, 0.0, 0.0],
            class_means: test_means(art),
            ..Default::default()
        },
        atmo: Some(csp_render::terrain::AtmoDesc {
            radius_m: R,
            top_m: TOP,
            scale_height_m: H_SCALE,
        }),
        ..Default::default()
    }
}

/// Star/sky regression gate (C16-F1). The old assertion set passed
/// even with the daylight gate broken (`star_vis = 1` injected): the
/// coarse image stats only grew, so a blue day sky peppered with
/// white stars read as a *better* frame. This test renders the gated
/// path directly — a zenith view on a sun-lit atmosphere — and
/// asserts the distinguishing signal: **zero near-white pixels**,
/// because a day sky is a smooth blue gradient and every star that
/// survives the gate is a regression.
///
/// Two renders pin both halves of the contract so the bound is not
/// vacuous: with the sun ~25° above the horizon behind the view axis
/// the sky is day-lit and `star_vis` must be 0; with the sun ~53°
/// below it the same frame is deep night and stars MUST appear — a
/// starless night sky would mean the starfield itself broke, which
/// the day assertion alone cannot see.
#[test]
fn day_sky_shows_no_stars_night_sky_shows_them() {
    let Some((device, queue)) = gate_device("day_sky_shows_no_stars_night_sky_shows_them") else {
        return;
    };
    let art = csp_view::art::load()
        .0
        .terrain
        .clone()
        .expect("terrain art pack loaded");
    let count_near_white = |img: &[u8]| -> usize {
        img.chunks_exact(4)
            .filter(|p| p[0] >= 210 && p[1] >= 210 && p[2] >= 210)
            .count()
    };

    // Day: sun ~25° up, ~65° off the view axis — lit shell, no disc
    // in frame.
    let day = zenith_sky_packet(5_000.0, [0.42, 0.0, 0.9075, 0.0], &art);
    let img = render_flight_scene(
        &device,
        &queue,
        &csp_render::flight::FlightPacket {
            terrain: Some(day),
            ..Default::default()
        },
    );
    let stats = analyze(&img);
    let day_white = count_near_white(&img);
    eprintln!(
        "day sky:   non_clear={}/{} distinct={} near_white={}",
        stats.non_clear,
        (W * H) as usize,
        stats.distinct,
        day_white
    );
    // Sanity: the sky actually rendered (a broken sky draw would also
    // satisfy the star bound — the gate must not pass on a void).
    assert!(
        stats.non_clear as f64 / (W * H) as f64 > 0.9,
        "day-sky frame did not rasterize ({}/{}) — the star assertion \
         would be vacuous",
        stats.non_clear,
        (W * H) as usize
    );
    assert_eq!(
        day_white, 0,
        "{day_white} near-white pixels in the day sky — the fs_sky \
         daylight gate let stars through (the star_vis = 1 failure shape)"
    );

    // Night: same view, sun 53° below the horizon — inscatter
    // collapses, star_vis → 1, the field must be up there.
    let night = zenith_sky_packet(5_000.0, [-0.8, 0.0, 0.6, 0.0], &art);
    let img = render_flight_scene(
        &device,
        &queue,
        &csp_render::flight::FlightPacket {
            terrain: Some(night),
            ..Default::default()
        },
    );
    let night_white = count_near_white(&img);
    eprintln!("night sky: near_white={night_white}");
    assert!(
        night_white > 20,
        "only {night_white} star pixels in the night zenith sky — the \
         starfield is not rendering at all, so the day assertion above \
         proves nothing"
    );
}

/// The day sky's sun, shared by the two sky gates below: ~25° above the
/// horizon and ~65° off the zenith view axis (the same vector
/// `day_sky_shows_no_stars_night_sky_shows_them` renders its day frame
/// with), so neither the disc nor its glare is in frame.
const DAY_SUN: [f32; 4] = [0.42, 0.0, 0.9075, 0.0];

/// Grain gate — the "looking straight up from the ground the daytime sky
/// is very grainy" complaint. `fs_sky` integrates the shell with one
/// sample per stratum; while those samples were jittered per pixel, every
/// pixel estimated the same integral differently and a smooth blue sky
/// came out as ±20% speckle. No assertion above can see that: grain only
/// grows the colour-bucket count. This renders a ground-level zenith
/// packet and measures the one thing grain produces and a real sky never
/// has — pixel-to-pixel luma curvature, as the mean |4-neighbour
/// Laplacian| of the box-filtered luma in 8-bit levels ([`mean_laplacian`];
/// `fs_sky`'s output dither sits under the bound at 0.14).
///
/// The bound is 0.4. The jittered march measured 2.45 on RADV and 2.47 on
/// lavapipe; the deterministic stratum march 0.14 on both. The bound is
/// read only after the frame has been shown to be the lit day sky it was
/// calibrated on: the app's clear color alone sits at luma 45.6 with no
/// curvature at all, so a frame the sky pass failed to draw would pass a
/// luma band for free — hence a colour-bucket and spread floor (an undrawn
/// frame has one bucket and no spread), then the luma band, then zero
/// near-white pixels (a blown frame).
#[test]
fn zenith_sky_is_grain_free_from_the_ground() {
    let Some((device, queue)) = gate_device("zenith_sky_is_grain_free_from_the_ground") else {
        return;
    };
    let art = csp_view::art::load()
        .0
        .terrain
        .clone()
        .expect("terrain art pack loaded");
    let img = render_flight_scene(
        &device,
        &queue,
        &csp_render::flight::FlightPacket {
            terrain: Some(zenith_sky_packet(0.0, DAY_SUN, &art)),
            ..Default::default()
        },
    );
    let stats = analyze(&img);
    let luma = mean_luma(&img);
    let lap = mean_laplacian(&img, W as usize, H as usize);
    eprintln!(
        "ground zenith sky, sun 65° off-axis: distinct={} spread={} mean_luma={luma:.2} \
         near_white={} laplacian={lap:.3}",
        stats.distinct, stats.max_spread, stats.near_white
    );
    assert!(
        stats.distinct > 8 && stats.max_spread > 30,
        "a flat frame ({} colour buckets, spread {}) — the sky pass drew nothing, and a \
         void has no grain",
        stats.distinct,
        stats.max_spread
    );
    assert!(
        (30.0..60.0).contains(&luma),
        "mean luma {luma:.2} is outside 30–60 — not the lit day sky the grain bound was \
         calibrated on"
    );
    assert_eq!(
        stats.near_white, 0,
        "{} near-white pixels in a day zenith frame with the sun off-axis — the frame is \
         blown, or stars are leaking through the daylight gate",
        stats.near_white
    );
    assert!(
        lap < GRAIN_BOUND,
        "mean |Laplacian| {lap:.3} — the sky is grainy again (bound {GRAIN_BOUND}; the \
         jittered march measured 2.45–2.47, the stratum march 0.14). A per-pixel random \
         offset in fs_sky's march is the usual cause."
    );
}

/// Star gate — the "stars are visible way too early" complaint. The
/// luminance half of `fs_sky`'s gate cannot tell night from a day sky that
/// has merely thinned: on its own it had the stars half on by 14 km in
/// broad daylight. Daylight stars are therefore held back by the
/// observer's altitude — none below 42 km, full by 55 km. Rendered on the
/// zenith packet lifted to each altitude, star pixels must be exactly
/// zero at 10, 30 and 40 km and present at 55 and 60 km: a starless frame
/// up there would mean the starfield or the gate broke, which the zero
/// assertions alone cannot see.
///
/// Dusk keeps the behaviour the altitude gate must not touch, and is
/// pinned with it: from the ground with the sun 9° down the glow gate is
/// half open and the stars have to be up there.
#[test]
fn daytime_stars_wait_for_altitude() {
    let Some((device, queue)) = gate_device("daytime_stars_wait_for_altitude") else {
        return;
    };
    let art = csp_view::art::load()
        .0
        .terrain
        .clone()
        .expect("terrain art pack loaded");
    // The zero assertions are only meaningful on a drawn frame, and
    // `non_clear` cannot tell: the sunward edge of the 10 km frame is
    // lit to within 8 levels of the clear color. An undrawn frame is ONE
    // colour bucket with no spread; the bar cannot go higher, since the
    // 30 km sky is dark and smooth enough to fill only seven.
    let render = |alt_m: f64, sun: [f32; 4]| -> usize {
        let img = render_flight_scene(
            &device,
            &queue,
            &csp_render::flight::FlightPacket {
                terrain: Some(zenith_sky_packet(alt_m, sun, &art)),
                ..Default::default()
            },
        );
        let stats = analyze(&img);
        let stars = star_pixels(&img, W as usize, H as usize);
        eprintln!(
            "zenith sky at {alt_m:.0} m: distinct={} spread={} star_pixels={stars}",
            stats.distinct, stats.max_spread
        );
        assert!(
            stats.distinct > 1 && stats.max_spread > 8,
            "a flat frame at {alt_m:.0} m ({} colour buckets, spread {}) — the sky pass \
             drew nothing, so its star count is vacuous",
            stats.distinct,
            stats.max_spread
        );
        stars
    };
    for alt_m in [10_000.0, 30_000.0, 40_000.0] {
        let stars = render(alt_m, DAY_SUN);
        assert_eq!(
            stars, 0,
            "{stars} star pixels in a DAYLIGHT zenith sky at {alt_m:.0} m — stars are \
             arriving way too early again (the altitude gate is gone or open too low)"
        );
    }
    for alt_m in [55_000.0, 60_000.0] {
        let stars = render(alt_m, DAY_SUN);
        assert!(
            stars > 20,
            "only {stars} star pixels in the daylight zenith sky at {alt_m:.0} m — the \
             altitude gate is still shut where it should be fully open, or the starfield \
             is not rendering"
        );
    }
    let dusk_sun = {
        let e = (-9.0f64).to_radians();
        [e.sin() as f32, 0.0, e.cos() as f32, 0.0]
    };
    let stars = render(0.0, dusk_sun);
    assert!(
        stars > 20,
        "only {stars} star pixels in the zenith sky at dusk from the ground (sun 9° down) \
         — the altitude gate is eating the stars it was meant to leave alone"
    );
}

/// The grain metric's smooth test sky: a linear gradient per channel, as
/// `fs_sky` computes it before its store.
fn gradient_rgb(x: usize, y: usize, w: usize, h: usize) -> [f64; 3] {
    let t = x as f64 / (w - 1) as f64;
    let u = y as f64 / (h - 1) as f64;
    [30.0 + 20.0 * t, 40.0 + 25.0 * u, 100.0 - 15.0 * t]
}

/// [`gradient_rgb`] stored to 8 bits: per pixel `offset` yields a number of
/// code values that is added to every channel alike before rounding.
fn stored_gradient(w: usize, h: usize, mut offset: impl FnMut() -> f64) -> Vec<u8> {
    let mut img = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let d = offset();
            for c in gradient_rgb(x, y, w, h) {
                img.push((c + d + 0.5) as u8);
            }
            img.push(255);
        }
    }
    img
}

/// One step of xorshift32 on a non-zero state — deterministic noise for the
/// tests below (the workspace has no `rand`).
fn xorshift32(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

/// The grain metric without a GPU. A smooth gradient rounded to 8 bits
/// measures far under the gate's bound, and the same gradient plus white
/// noise of ~5 levels standard deviation — what the jittered march left on
/// the sky — measures the 0.50·S that [`mean_laplacian`] documents, several
/// times over it.
#[test]
fn laplacian_metric_passes_quantised_gradient_and_rejects_white_noise() {
    let (w, h) = (W as usize, H as usize);
    let smooth = mean_laplacian(&stored_gradient(w, h, || 0.0), w, h);
    eprintln!("quantised gradient: laplacian {smooth:.4}");
    assert!(
        smooth < GRAIN_BOUND,
        "an 8-bit-quantised smooth gradient measures {smooth:.3} — the grain bound would \
         reject grain-free skies"
    );

    // One draw per pixel, ±9 levels on every channel alike, so the luma
    // noise has standard deviation sqrt(30) = 5.48 levels.
    let mut state = 0x9e37_79b9_u32;
    let noisy = stored_gradient(w, h, || ((xorshift32(&mut state) % 19) as i32 - 9) as f64);
    let grain = mean_laplacian(&noisy, w, h);
    let expected = 0.50 * 30.0f64.sqrt();
    eprintln!("gradient + white noise (σ = 5.48 levels): laplacian {grain:.3}");
    assert!(
        (grain - expected).abs() < 0.1 * expected,
        "white noise of σ = 5.48 levels measured {grain:.3}, not the documented 0.50·σ ≈ \
         {expected:.2} — the grain metric is miscounting"
    );
    assert!(
        grain > GRAIN_BOUND,
        "white noise of σ = 5.48 levels measures {grain:.3} — under the gate's bound of \
         {GRAIN_BOUND}, which would let the jittered march's grain through"
    );
}

/// The grain metric on `fs_sky`'s store. A gradient rounded to 8 bits after
/// a triangular dither of ±half a code value — two uniform draws summed, as
/// `dither_tpdf` — carries noise of σ ≈ 0.3 levels and has to pass: it is
/// what every daytime sky frame now contains.
#[test]
fn laplacian_metric_passes_half_step_dithered_gradient() {
    let (w, h) = (W as usize, H as usize);
    let mut state = 0x85eb_ca6b_u32;
    let mut draw = || f64::from(xorshift32(&mut state) % 4096);
    let dithered = stored_gradient(w, h, || (draw() + draw()) / 8192.0 - 0.5);

    // The fixture is the store it claims. Against plain rounding exactly
    // the samples whose threshold the dither crossed move — a fraction
    // E|d| = 1/6 — and each by one code value.
    let plain = stored_gradient(w, h, || 0.0);
    let moved: Vec<i32> = dithered
        .iter()
        .zip(&plain)
        .filter(|(a, b)| a != b)
        .map(|(a, b)| i32::from(*a) - i32::from(*b))
        .collect();
    let fraction = moved.len() as f64 / (w * h * 3) as f64;
    eprintln!("half-step dithered gradient: {fraction:.4} of samples moved by the dither");
    assert!(
        (0.15..0.185).contains(&fraction) && moved.iter().all(|d| d.abs() == 1),
        "{fraction:.4} of the samples moved ({}/{}) by other than one code value — the \
         fixture is not fs_sky's ±half-code triangular dither",
        moved.iter().filter(|d| d.abs() != 1).count(),
        moved.len()
    );

    let lap = mean_laplacian(&dithered, w, h);
    eprintln!("half-step dithered gradient: laplacian {lap:.3}");
    assert!(
        lap < GRAIN_BOUND,
        "a ±half-code dithered gradient measures {lap:.3} — over the grain bound of \
         {GRAIN_BOUND}, which would reject every sky fs_sky dithers"
    );
}

/// The star count without a GPU: on a smooth sky it counts exactly the
/// isolated bright points planted in it, and nothing else — neither the
/// quantised gradient nor a broad soft glow (sun halo, limb), which lifts
/// a pixel and its neighbourhood together.
#[test]
fn star_count_counts_exactly_the_planted_stars() {
    let (w, h) = (64usize, 48usize);
    let mut sky = vec![0u8; w * h * 4];
    for (y, row) in sky.chunks_exact_mut(4 * w).enumerate() {
        for px in row.chunks_exact_mut(4) {
            let v = (8 + y / 8) as u8;
            px.copy_from_slice(&[v, v + 2, v + 16, 255]);
        }
    }
    assert_eq!(
        star_pixels(&sky, w, h),
        0,
        "a starless quantised gradient counted as stars"
    );
    // 7 one-pixel stars, each ≥ 5 px from every other and from the border.
    const STARS: [(usize, usize); 7] = [
        (7, 7),
        (20, 30),
        (31, 12),
        (40, 40),
        (55, 6),
        (58, 25),
        (33, 24),
    ];
    for (x, y) in STARS {
        sky[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[200, 210, 255, 255]);
    }
    assert_eq!(
        star_pixels(&sky, w, h),
        STARS.len(),
        "the planted stars were not counted one pixel each"
    );
    // A broad soft glow (σ = 12 px, +40 levels at its centre): every
    // pixel rises together with its neighbourhood.
    for y in 0..h {
        for x in 0..w {
            let d2 = (x as f64 - 52.0).powi(2) + (y as f64 - 38.0).powi(2);
            let glow = 40.0 * f64::exp(-d2 / 288.0);
            let px = &mut sky[(y * w + x) * 4..(y * w + x) * 4 + 4];
            for c in &mut px[..3] {
                *c = (u16::from(*c) + glow as u16).min(255) as u8;
            }
        }
    }
    assert_eq!(
        star_pixels(&sky, w, h),
        STARS.len(),
        "a broad soft glow was counted as stars"
    );
}

/// Scratch sky-dome dump — renders the terrain sky pass alone with a
/// vacuum-body packet (fs_sky's unattenuated space path) so the
/// procedural starfield is readable on its own. Three frames: north,
/// equator, south — the shell-lattice hash must give crisp point stars
/// with uniform density at every latitude. Run explicitly:
/// `cargo test -p csp-view --test render_gate dump_sky -- --ignored`.
#[test]
#[ignore]
#[allow(clippy::too_many_lines)] // three render+readback blocks; it is a scratch dump
fn dump_sky() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let art = csp_view::art::load();
    let terrain_art = art.0.terrain.clone().expect("terrain art pack loaded");
    let mut terrain = csp_render::terrain::TerrainRenderer::new(&device, FORMAT);

    let msaa_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sky-dump-msaa"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: csp_render::MSAA_SAMPLES,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let msaa = msaa_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let color_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sky-dump-target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let flight = csp_render::flight::FlightRenderer::new(&device, FORMAT, (W, H), &terrain);
    let aspect = f64::from(W) / f64::from(H);
    let t = (35.0f64.to_radians()).tan(); // ~70° vertical fov
    // Column-major view→world rotations: the sky ray is
    // rd = view_rot * (ndc·tan, -1) — centre pixel looks along −z_view.
    let rot = |x: [f32; 4], y: [f32; 4], z: [f32; 4]| [x, y, z, [0.0, 0.0, 0.0, 1.0]];
    let views = [
        (
            "north",
            rot(
                [-1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, -1.0, 0.0],
            ),
        ),
        (
            "equator",
            rot(
                [0.0, 0.0, -1.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0],
            ),
        ),
        (
            "south",
            rot(
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ),
        ),
    ];
    for (name, view_rot) in views {
        let packet = csp_render::terrain::TerrainPacket {
            sky: true,
            art: Some(terrain_art.clone()),
            uniforms: csp_render::terrain::TerrainUniforms {
                view_rot,
                eye_rel_body: [600_100.0, 0.0, 0.0, 0.0],
                sun_dir: [-0.6, 0.5, 0.62, 0.0],
                planet: [600_000.0, 0.0, 0.0, 2_400.0],
                misc: [(t * aspect) as f32, t as f32, 0.0, 0.0],
                art: [1.0, 0.0, 0.0, 0.0],
                class_means: test_means(&terrain_art),
                ..Default::default()
            },
            ..Default::default()
        };
        let target = color_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sky-dump"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sky"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &msaa,
                    resolve_target: Some(&target),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: flight.depth_view(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            terrain.draw(&mut pass, &device, &queue, &packet);
        }
        let row_bytes = W as usize * 4;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sky-dump-readback"),
            size: (row_bytes * H as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &color_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes as u32),
                    rows_per_image: Some(H),
                },
            },
            wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().expect("map_async failed");
        let img = buf.get_mapped_range(..).unwrap().to_vec();
        buf.unmap();
        let mut out = format!("P6 {W} {H} 255\n").into_bytes();
        for px in img.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
        let path = format!("/tmp/sky_{name}.ppm");
        std::fs::write(&path, &out).unwrap();
        eprintln!("wrote {path}");
    }
}

/// Scratch visual dump — write the seeded frame to /tmp/frame.ppm.
/// Run explicitly: `cargo test -p csp-view --test render_gate dump -- --ignored`.
/// Determinism: `until_real_terrain` polls on IDLE frames (no sim
/// advance) and `DUMP_TIME=<seconds>` optionally pins the capture
/// clock — the scene (hence the sun) is a pure function of the knobs,
/// never of the bake's wall-time. Paired A/B dumps (DUMP_SHADOWS,
/// DUMP_DEREPETITION) run minutes apart with identical lighting.
#[test]
#[ignore]
fn dump_low() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let yaw: f64 = std::env::var("DUMP_YAW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.7);
    let pitch: f64 = std::env::var("DUMP_PITCH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.2);
    let dist: f64 = std::env::var("DUMP_DIST")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(710.0);
    let mut view = View::new().unwrap();
    view.seed_starter_craft().unwrap();
    view.set_flight_viewport(320.0, 240.0);
    {
        let mut input = view.world.resource_mut::<csp_view::flight::FlightInput>();
        input.set_distance(dist);
        input.pitch = pitch;
        input.yaw = yaw;
    }
    // Capture only once the packet carries the REAL heightfield — the
    // first frames are sky-only, no ground drawn (see
    // [`until_real_terrain`]). DUMP_TIME then pins the scene clock:
    // paired A/B dumps can never drift to different suns.
    until_real_terrain(&mut view);
    pin_dump_time(&mut view);
    let packet = view.take_flight_packet().unwrap();
    let img = render_flight_scene(&device, &queue, &packet);
    let name = std::env::var("DUMP_OUT").unwrap_or_else(|_| "/tmp/frame_low.ppm".into());
    let mut out = b"P6 320 240 255\n".to_vec();
    for px in img.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(&name, &out).unwrap();
    // Count near-white pixels for the sweep + report the cascade arm
    // state (3b) — a dawn/night frame honestly renders disarmed.
    let white = img
        .chunks_exact(4)
        .filter(|p| p[0] > 235 && p[1] > 235 && p[2] > 235)
        .count();
    eprintln!(
        "{name}: white={white} shadows={} t={:.1}s",
        if packet.shadow.armed() {
            "armed"
        } else {
            "disarmed"
        },
        view.sim.clock().ticks() as f64 / csp_core::TICK_HZ as f64
    );
}

/// Scratch dump of an actually-ascending craft: seed + throttle 1.0,
/// step until ~700 m, then rasterize the real flight packet — the
/// same VFX state the reported ~710 m screenshot came from.
/// Determinism: `until_real_terrain` polls on IDLE frames and
/// `DUMP_TIME=<seconds>` pins the capture clock on top of the fixed
/// spawn stepping — the sun at capture is knob-controlled, never
/// bake-wall-time (the 3b vision-check failure mode). The flagship
/// craft-shadow pair intentionally shares the pinned camera
/// (`flight_craftshadow` and `flight` render the same pinned frame —
/// the duplication is the determinism contract, not a bug).
/// `cargo test -p csp-view --test render_gate dump_flight -- --ignored`.
#[test]
#[ignore]
fn dump_flight() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let mut view = View::new().unwrap();
    view.seed_starter_craft().unwrap();
    view.set_flight_viewport(320.0, 240.0);
    {
        let mut input = view.world.resource_mut::<csp_view::flight::FlightInput>();
        input.yaw = env_f("DUMP_YAW", 0.7);
        input.pitch = env_f("DUMP_PITCH", 0.25);
        input.set_distance(env_f("DUMP_DIST", 60.0));
    }
    // Wait for the REAL heightfield BEFORE lighting the engine — the
    // ascent loop below runs only seconds of wall time and the
    // background bake (single-digit seconds in debug) would otherwise
    // still be drawing sky-only at capture (see [`until_real_terrain`]).
    until_real_terrain(&mut view);
    pin_dump_time(&mut view);
    frame(&mut view, 1); // the focus resolves in flight_present
    let vessel = view.flight_focus().expect("starter vessel");
    view.enqueue_command(
        csp_view::SourceClass::Ui,
        0,
        csp_view::SimCommand::SetThrottle {
            vessel,
            throttle: 1.0,
        },
    );
    let alt = env_f("DUMP_ALT", 710.0);
    // Step until the HUD's staged altitude passes the target (or ~60 s).
    for _ in 0..400 {
        frame(&mut view, 30);
        let a = view
            .world
            .resource::<csp_view::flight::FlightUiData>()
            .alt_m;
        if a > alt {
            break;
        }
    }
    let a = view
        .world
        .resource::<csp_view::flight::FlightUiData>()
        .alt_m;
    let packet = view.take_flight_packet().unwrap();
    let img = render_flight_scene(&device, &queue, &packet);
    let name = std::env::var("DUMP_OUT").unwrap_or_else(|_| "/tmp/flight.ppm".into());
    let mut out = format!("P6 {W} {H} 255\n").into_bytes();
    for px in img.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(&name, &out).unwrap();
    let white = img
        .chunks_exact(4)
        .filter(|p| p[0] > 235 && p[1] > 215)
        .count();
    eprintln!(
        "{name}: alt={a:.0} white={white} shadows={} t={:.1}s",
        if packet.shadow.armed() {
            "armed"
        } else {
            "disarmed"
        },
        view.sim.clock().ticks() as f64 / csp_core::TICK_HZ as f64
    );
}

/// The pack's per-class means → the `class_means` uniform, mirroring
/// `csp_view::terrain::build_packet`'s sRGB→linear fill.
fn test_means(art: &csp_render::terrain::TerrainArt) -> [[f32; 4]; 4] {
    let lin = |u: u8| {
        let v = f32::from(u) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let mut m = [[0.0f32; 4]; 4];
    for (d, s) in m.iter_mut().zip(art.albedo.means.iter()) {
        *d = [lin(s[0]), lin(s[1]), lin(s[2]), f32::from(s[3]) / 255.0];
    }
    m
}

fn env_f(k: &str, d: f64) -> f64 {
    std::env::var(k)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(d)
}

/// §2.3 routing for `dump_surface`'s hand-built scene, mirroring
/// `build_packet`'s layout and rule: a patch routes far only when its
/// nearest point ALONG THE VIEW AXIS (`dot(centre − eye, fwd) −
/// patch_extent`) clears the split — straddlers (radial or angular)
/// stay near, whole. Returns (near terrain, terrain far start, near
/// ocean, ocean far start), near-then-far per list.
fn routed_surface_instances(
    leaves: &[csp_render::terrain::PatchId],
    eye_rel: csp_core::DVec3,
    fwd: csp_core::DVec3,
    r_body: f64,
    amplitude_m: f64,
    field: &csp_render::terrain::HeightField,
) -> (
    Vec<csp_render::terrain::TerrainInstance>,
    u32,
    Vec<csp_render::terrain::TerrainInstance>,
    u32,
) {
    let split = csp_render::flight::FAR_SPLIT_M;
    let routed_far = |inst: &csp_render::terrain::TerrainInstance, extent: f64| {
        let o = inst.origin_rel_eye;
        f64::from(o[0]) * fwd.x + f64::from(o[1]) * fwd.y + f64::from(o[2]) * fwd.z - extent
            >= split
    };
    let mut terrain_inst = Vec::new();
    let mut ocean_inst = Vec::new();
    let mut terrain_far = Vec::new();
    let mut ocean_far = Vec::new();
    for &id in leaves {
        let extent = csp_render::terrain::patch_extent_m(id, r_body, amplitude_m);
        let inst = csp_render::terrain::patch_instance(id, 0.0, eye_rel, r_body);
        if routed_far(&inst, extent) {
            terrain_far.push(inst);
        } else {
            terrain_inst.push(inst);
        }
        // replicate patch_might_hold_sea's 5x5 sample rule
        let (corner, du, dv) = csp_render::terrain::patch_cube(id);
        let mut lo = f64::INFINITY;
        for j in 0..5 {
            for i in 0..5 {
                let c = corner + du * (i as f64 / 4.0) + dv * (j as f64 / 4.0);
                lo = lo.min(field.sample_dir(c / c.norm()));
            }
        }
        if lo * amplitude_m <= 600.0 {
            let extent = csp_render::terrain::patch_extent_m(id, r_body, amplitude_m);
            let inst = csp_render::terrain::patch_instance(id, 1.0, eye_rel, r_body);
            if routed_far(&inst, extent) {
                ocean_far.push(inst);
            } else {
                ocean_inst.push(inst);
            }
        }
    }
    let terrain_far_start = terrain_inst.len() as u32;
    let ocean_far_start = ocean_inst.len() as u32;
    terrain_inst.extend(terrain_far);
    ocean_inst.extend(ocean_far);
    (terrain_inst, terrain_far_start, ocean_inst, ocean_far_start)
}

/// The authored Terrin scene assets `dump_surface` and the §2.3
/// mixed-altitude gate share: the profiled heightfield bake, the
/// authored amplitude, the biome map, and the art pack — read from
/// the view's config so a re-authored terrain block flows through
/// without editing the probes.
fn terrin_scene_assets() -> (
    csp_render::terrain::HeightField,
    f64,
    Option<std::sync::Arc<csp_render::terrain::BiomeField>>,
    std::sync::Arc<csp_render::terrain::TerrainArt>,
) {
    let view = View::new().unwrap();
    let cfg = view.world.resource::<csp_view::terrain::TerrainConfig>();
    let t = cfg.bodies.get("terrin").unwrap();
    let field = csp_render::terrain::HeightField::bake_profiled(t.seed, t.profile);
    let biome = t.biome.clone();
    let amplitude = t.amplitude_m;
    let art = csp_view::art::load()
        .0
        .terrain
        .clone()
        .expect("terrain art pack");
    (field, amplitude, biome, art)
}

/// `dump_surface`'s scene core, parameterized: hand-builds the
/// TERRAIN-ONLY surface `FlightPacket` at an arbitrary vantage
/// (lat/lon deg, alt m, pitch rad below the local horizon, heading
/// rad) with the same public builders the extract uses — §2.3's
/// near/far routing included. No craft, no launch complex (the
/// hand-built path never runs `emit_launch_complex`).
#[allow(clippy::too_many_lines)]
// one scene recipe shared by the dump and the gate test
// One scene recipe shared by the dump and the gate — the parameter
// list IS the scene; a config struct would just rename it.
#[allow(clippy::too_many_arguments)]
fn surface_probe_packet(
    lat_deg: f64,
    lon_deg: f64,
    alt_m: f64,
    pitch_down: f64,
    heading_rad: f64,
    amplitude_m: f64,
    field: csp_render::terrain::HeightField,
    biome: Option<std::sync::Arc<csp_render::terrain::BiomeField>>,
    art: std::sync::Arc<csp_render::terrain::TerrainArt>,
) -> csp_render::flight::FlightPacket {
    use csp_core::DVec3;
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();
    let r_body = 600_000.0;
    // Surface direction under the eye.
    let (slat, clat) = lat.sin_cos();
    let (slon, clon) = lon.sin_cos();
    let dir = DVec3::new(clat * clon, clat * slon, slat);
    let h_here = field.sample_dir(dir) * amplitude_m;
    let eye_rel = dir * (r_body + h_here.max(0.0) + alt_m);

    // Camera basis: look `pitch_down` below the local horizon toward
    // `heading` (0 = north, clockwise).
    let up = dir;
    let east0 = DVec3::Z.cross(up);
    let east = if east0.norm_squared() > 1e-12 {
        east0 / east0.norm()
    } else {
        DVec3::X
    };
    let north = up.cross(east);
    let fwd_h = north * heading_rad.cos() + east * heading_rad.sin();
    let fwd = (up * (-pitch_down.sin()) + fwd_h * pitch_down.cos()) / 1.0;
    let z_out = -fwd;
    let right0 = up.cross(z_out);
    let right = if right0.norm_squared() > 1e-12 {
        right0 / right0.norm()
    } else {
        east
    };
    let upv = z_out.cross(right);
    let view = [
        [right.x, right.y, right.z, 0.0],
        [upv.x, upv.y, upv.z, 0.0],
        [z_out.x, z_out.y, z_out.z, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let aspect = f64::from(W) / f64::from(H);
    let fovy = 0.9f64;
    let f = 1.0 / (fovy * 0.5).tan();
    let proj = [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.1],
        [0.0, 0.0, -1.0, 0.0],
    ];
    let mut view_proj = [[0.0f64; 4]; 4];
    for r in 0..4 {
        for c in 0..4 {
            view_proj[r][c] = proj[r][0] * view[0][c]
                + proj[r][1] * view[1][c]
                + proj[r][2] * view[2][c]
                + proj[r][3] * view[3][c];
        }
    }
    // Rebuild the packet through the same builder extract_flight uses —
    // but it is crate-private, so replicate its uniform/instance steps
    // with the public pieces.
    let leaves = csp_render::terrain::select_patches(
        eye_rel,
        r_body,
        amplitude_m,
        &csp_render::terrain::LodConfig::default(),
    );
    let (terrain_inst, terrain_far_start, ocean_inst, ocean_far_start) =
        routed_surface_instances(&leaves, eye_rel, fwd, r_body, amplitude_m, &field);
    // view_rot columns = camera axes (sky_view_rot convention).
    let mut view_rot = [[0.0f32; 4]; 4];
    for c in 0..3 {
        view_rot[0][c] = view[0][c] as f32;
        view_rot[1][c] = view[1][c] as f32;
        view_rot[2][c] = view[2][c] as f32;
    }
    let tan_y = (fovy * 0.5).tan();
    let eye_r2 = eye_rel.norm_squared();
    let r_top = r_body + 70_000.0;
    let mut biome_colors = [[0.0f32; 4]; 16];
    if let Some(b) = &biome {
        biome_colors = b.colors;
    }
    let near_uniforms = csp_render::terrain::TerrainUniforms {
        view_proj: {
            let mut o = [[0.0f32; 4]; 4];
            for r in 0..4 {
                for c in 0..4 {
                    o[c][r] = view_proj[r][c] as f32;
                }
            }
            o
        },
        view_rot,
        eye_rel_body: [eye_rel.x as f32, eye_rel.y as f32, eye_rel.z as f32, 0.0],
        sun_dir: [-1.0, 0.0, 0.0, 0.0],
        planet: [r_body as f32, 70_000.0, r_body as f32, amplitude_m as f32],
        atmo: [
            5_600.0,
            if biome.is_some() { 1.0 } else { 0.0 },
            alt_m as f32,
            0.0,
        ],
        misc: [
            (tan_y * aspect) as f32,
            tan_y as f32,
            (eye_r2 - r_top * r_top) as f32,
            (eye_r2 - r_body * r_body) as f32,
        ],
        spin: [0.0, 0.0, 1.0, 0.0],
        rayleigh: [5.8e-6, 13.5e-6, 33.1e-6, 0.0],
        biome_colors,
        shadow: [0.0; 4],
        art: [1.0, 0.0, 0.0, 0.0],
        class_means: test_means(&art),
    };
    let packet = csp_render::terrain::TerrainPacket {
        uniforms: near_uniforms,
        // §2.3 far row — the same FULL-CLONE + one-row swap
        // `build_packet` applies: every other uniform field must ride
        // along verbatim (an earlier version built this from
        // `Default::default()` and zeroed `planet.x` — the far pass's
        // displacement collapsed to degenerate triangles and painted
        // nothing; the full clone is the load-bearing part, pinned by
        // `far_uniforms_are_exactly_the_clip_z_row_swap`).
        uniforms_far: {
            let mut far_vp = view_proj;
            far_vp[2] = [0.0, 0.0, 0.0, csp_render::flight::FAR_SPLIT_M];
            let mut o = [[0.0f32; 4]; 4];
            for r in 0..4 {
                for c in 0..4 {
                    o[c][r] = far_vp[r][c] as f32;
                }
            }
            let mut u = near_uniforms;
            u.view_proj = o;
            u
        },
        heightfield: Some(std::sync::Arc::new(field)),
        biome,
        atmo: Some(csp_render::terrain::AtmoDesc {
            radius_m: r_body,
            top_m: 70_000.0,
            scale_height_m: 5_600.0,
        }),
        terrain: terrain_inst,
        terrain_far_start,
        ocean: ocean_inst,
        ocean_far_start,
        sky: true,
        art: Some(art),
    };
    csp_render::flight::FlightPacket {
        terrain: Some(packet),
        // 3b: solve the cascades for this probe's hand-built camera —
        // the same arm predicate + solve the real extract applies,
        // reconstructed here because the probe bypasses `extract_flight`
        // (basis: the `fwd`/`dir` it already built; sun [−1,0,0]).
        shadow: {
            let sun = DVec3::new(-1.0, 0.0, 0.0);
            if csp_view::flight::cascades_armed(sun, dir) {
                csp_render::shadow::solve_cascades(
                    &csp_render::shadow::ShadowCamera {
                        sun_dir: sun,
                        forward: fwd,
                        up: dir,
                        fov_y: fovy,
                        aspect,
                        near_m: 0.1,
                    },
                    &csp_render::shadow::CascadeConfig::default(),
                )
                .uniforms
            } else {
                csp_render::shadow::ShadowUniforms::default()
            }
        },
        ..Default::default()
    }
}

/// §2.3's split-ring regression gate (the routing blocker's pin): an
/// 800 km vantage looks across the split ring — the disc's terrain
/// spans ~800 km..~1.24 Mm of eye distance, so BOTH passes carry
/// geometry and straddling patches exist. Both routing bugs this gate
/// hunted sent coverable surface to the far pass, whose `split ≤ w`
/// clip trimmed it out of both passes — sky through the planet: the
/// original patch-CENTRE rule (validator: 6,344 px) and the
/// radial-nearest rule (147 px more — the far clip measures FORWARD
/// depth, `radial·cos(off-axis)`, short of the radial distance at the
/// frame's side reaches). Pins, in strengthening order:
///
/// - **Routing** (CPU, exact): straddlers — radial OR angular —
///   route NEAR (whole quads); every far-routed patch's
///   forward-nearest point clears `split + patch_extent`; both bands
///   non-empty (the case is live).
/// - **No sky through the disc**: across the WHOLE frame, no pixel
///   may be exactly the sky-only render where HEAD paints surface —
///   the hole signature itself (a rectangle over the ring also stays
///   under ≤ 32 px of inherent limb-haze coincidence; measured 3).
/// - **HEAD equivalence**: with straddlers whole in the near pass,
///   the composed frame must match the same scene with EVERY patch
///   forced near — the pre-split render (the pinned dumps prove the
///   all-near two-pass frame is bit-identical to HEAD's single pass) —
///   within ≤ 8 px: the residual is the documented cross-pass
///   approximation (a near-routed straddler's beyond-split fringe
///   behind far-routed content along a grazing ray; measured 3). The
///   bug classes this caught measured 575–6,344 px.
#[test]
#[allow(clippy::too_many_lines)] // three pin families over one rendered scene; splitting hides the A/B/C
fn split_ring_vantage_keeps_the_disc_whole() {
    let Some((device, queue)) = gate_device("split_ring_vantage_keeps_the_disc_whole") else {
        return;
    };
    let (field, amplitude, biome, art) = terrin_scene_assets();
    let fp = surface_probe_packet(
        -5.2712,
        167.0745,
        800_000.0,
        1.05, // rad below the local horizon — frames the split ring
        0.0,
        amplitude,
        field,
        biome.clone(),
        art.clone(),
    );
    let tp = fp.terrain.as_ref().unwrap();

    // --- routing ------------------------------------------------------
    let split = csp_render::flight::FAR_SPLIT_M;
    let dist2 = |i: &csp_render::terrain::TerrainInstance| {
        let o = i.origin_rel_eye;
        f64::from(o[0]) * f64::from(o[0])
            + f64::from(o[1]) * f64::from(o[1])
            + f64::from(o[2]) * f64::from(o[2])
    };
    // The extent rule, reconstructed from each instance's own rows
    // (`rt::patch_extent_m`'s body; this scene's body radius 600 km).
    let extent_of = |i: &csp_render::terrain::TerrainInstance| {
        use csp_core::DVec3;
        let d = |v: [f32; 4]| DVec3::new(f64::from(v[0]), f64::from(v[1]), f64::from(v[2]));
        (d(i.du) + d(i.dv)).norm() * 0.5 * 600_000.0 + amplitude
    };
    // The camera forward, recovered from the packet's column-major
    // uniforms (math row 3 = the [0,0,−1,0]·V row = the forward).
    let fwd = {
        use csp_core::DVec3;
        let u = &tp.uniforms;
        let v = DVec3::new(
            f64::from(u.view_proj[0][3]),
            f64::from(u.view_proj[1][3]),
            f64::from(u.view_proj[2][3]),
        );
        let n = v.norm();
        if n > 1e-9 {
            DVec3::new(v.x / n, v.y / n, v.z / n)
        } else {
            DVec3::ZERO
        }
    };
    let forward_of = |i: &csp_render::terrain::TerrainInstance| {
        let o = i.origin_rel_eye;
        f64::from(o[0]) * fwd.x + f64::from(o[1]) * fwd.y + f64::from(o[2]) * fwd.z
    };
    let near_n = tp.terrain_far_start as usize;
    let far_n = tp.terrain.len() - near_n;
    assert!(near_n > 0, "the 800 km disc's near band is non-empty");
    assert!(far_n > 0, "the 800 km disc's far band is non-empty");
    let straddlers = tp.terrain[..near_n]
        .iter()
        .filter(|i| dist2(i) >= split * split)
        .count();
    assert!(
        straddlers > 0,
        "the 800 km vantage must exercise straddlers (centre ≥ split, routed near)"
    );
    // The far list's forward-nearest bound clears the split — the
    // exact quantity the far pass's clip test measures.
    for i in &tp.terrain[near_n..] {
        let nearest_fwd = forward_of(i) - extent_of(i);
        assert!(
            nearest_fwd >= split,
            "far-routed patch forward-nearest point {nearest_fwd:.0} m < split"
        );
    }

    // --- composed frame vs the sky-only and all-near renders ---------
    let composed = render_flight_scene(&device, &queue, &fp);
    // Sky-only: identical packet with empty instance lists — what a
    // hole reveals wherever the composed frame matches it.
    let mut sky_fp = fp.clone();
    let tp_sky = sky_fp.terrain.as_mut().unwrap();
    tp_sky.terrain.clear();
    tp_sky.terrain_far_start = 0;
    tp_sky.ocean.clear();
    tp_sky.ocean_far_start = 0;
    let sky = render_flight_scene(&device, &queue, &sky_fp);
    // All-near: HEAD's render through the same pipelines (the pinned
    // dumps' proven equivalence).
    let mut head_fp = fp.clone();
    let tp_head = head_fp.terrain.as_mut().unwrap();
    tp_head.terrain_far_start = tp_head.terrain.len() as u32;
    tp_head.ocean_far_start = tp_head.ocean.len() as u32;
    let head = render_flight_scene(&device, &queue, &head_fp);

    // The interior rectangle straddling the split ring, safely inside
    // the silhouette: rows 66–82 %, cols 40–60 % of the frame.
    let rows = (0.66 * H as f32) as usize..(0.82 * H as f32) as usize;
    let cols = (0.40 * W as f32) as usize..(0.60 * W as f32) as usize;
    let px = |img: &[u8], r: usize, c: usize| -> [u8; 4] {
        let o = (r * W as usize + c) * 4;
        [img[o], img[o + 1], img[o + 2], img[o + 3]]
    };
    let mut sky_through = 0usize;
    let mut rect_n = 0usize;
    for r in rows {
        for c in cols.clone() {
            rect_n += 1;
            let a = px(&composed, r, c);
            let s = px(&sky, r, c);
            if (0..3).all(|k| a[k].abs_diff(s[k]) <= 8) {
                sky_through += 1;
            }
        }
    }
    // The airtight check — over the WHOLE frame: the composed render
    // vs the all-near (HEAD) render. Any difference must be confined
    // to the split band (the documented cross-pass approximation):
    // report the locus precisely.
    let mut head_diff = 0usize;
    let (mut rmin, mut rmax, mut cmin, mut cmax) = (usize::MAX, 0, usize::MAX, 0);
    for (i, (a, h)) in composed
        .chunks_exact(4)
        .zip(head.chunks_exact(4))
        .enumerate()
    {
        if (0..3).any(|k| a[k].abs_diff(h[k]) > 8) {
            head_diff += 1;
            let (r, c) = (i / W as usize, i % W as usize);
            rmin = rmin.min(r);
            rmax = rmax.max(r);
            cmin = cmin.min(c);
            cmax = cmax.max(c);
        }
    }
    // Exact-hole count over the WHOLE frame: pixels where the
    // composed frame is bit-for-bit the sky render while HEAD paints
    // surface — the sky-through-the-planet signature itself.
    let mut exact_holes = 0usize;
    for (i, ((a, h), sk)) in composed
        .chunks_exact(4)
        .zip(head.chunks_exact(4))
        .zip(sky.chunks_exact(4))
        .enumerate()
    {
        let is_sky = (0..3).all(|k| a[k] == sk[k]);
        let head_paints = (0..3).any(|k| a[k].abs_diff(h[k]) > 8);
        if is_sky && head_paints {
            exact_holes += 1;
            if exact_holes <= 3 {
                let _ = i; // locus printable on demand
            }
        }
    }
    eprintln!(
        "split-ring vantage: rect={rect_n} sky_through={sky_through} head_diff={head_diff} \
         exact_holes={exact_holes} near={near_n} far={far_n} straddlers={straddlers}"
    );
    // Scratch frames for offline inspection (never asserted on).
    if std::env::var("SPLIT_RING_DUMP").is_ok() {
        let write = |name: &str, img: &[u8]| {
            let mut out = format!("P6 {W} {H} 255\n").into_bytes();
            for p in img.chunks_exact(4) {
                out.extend_from_slice(&p[..3]);
            }
            std::fs::write(name, &out).unwrap();
        };
        write("/tmp/split-composed.ppm", &composed);
        write("/tmp/split-head.ppm", &head);
        write("/tmp/split-sky.ppm", &sky);
    }
    assert_eq!(
        exact_holes, 0,
        "sky shows through the disc ({exact_holes} px are exactly the sky render where HEAD \
         paints surface) — the far pass is clipping visible surface away"
    );
    // A handful of interior pixels legitimately coincide with the sky
    // render (limb haze + MSAA blends — measured 3/2432); the hole
    // signature is the exact_holes==0 pin above. The bound sits an
    // order under any real hole and an order over the coincidence
    // floor.
    assert!(
        sky_through <= 32,
        "sky shows through the disc interior ({sky_through}/{rect_n} px match the sky-only \
         render) — the far pass is clipping visible surface away"
    );
    // The composed frame vs the all-near (HEAD) render: measured 3 px
    // of the documented cross-pass approximation — a near-routed
    // straddler's beyond-split fringe sitting behind far-routed
    // content along a grazing ray, where the near pass's
    // unconditional-over policy picks the farther surface that HEAD's
    // single-pass depth test resolves the other way (§7.2.1). Stable,
    // sub-0.01 % of the frame, zero at gameplay vantages (the pinned
    // dumps are byte-identical); the bound catches any regression
    // class that drops or reorders far content wholesale (the bugs
    // this test hunted measured 575–6,344 px).
    assert!(
        head_diff <= 8,
        "the composed 800 km frame differs from the all-near HEAD render in {head_diff} px — \
         the far pass changed visible output"
    );
}

/// Scratch terrain dump at an arbitrary surface point/heading —
/// hand-builds the `TerrainPacket` (same builders `extract_flight`
/// uses) so any location/altitude/pitch can be inspected without
/// flying the sim. TERRAIN-ONLY by design: the packet is hand-built
/// with an empty flight-vertex stream, so the launch complex (pad,
/// gantry, VAB — `structures::emit_launch_complex` is crate-private
/// and only runs inside the real extract) does NOT appear here; judge
/// pad-ground alignment from `dump_low`/`dump_flight`, which render
/// the full extract — structures and craft anchored on the same
/// heightfield this probe bakes. The field itself IS the real authored
/// bake (profiled, synchronous — no cache stand-in race here).
/// Env knobs: DUMP_LAT, DUMP_LON (deg), DUMP_HEADING (deg), DUMP_ALT
/// (m), DUMP_PITCH (rad, look-down), DUMP_AMP (m, displacement-scale
/// override — 0 bakes the flat datum for the A/B pair), DUMP_BODY
/// (DUMP_SCAN survey body; render stays Terrin), DUMP_OUT.
/// `cargo test -p csp-view --test render_gate dump_surface -- --ignored`.
#[test]
#[ignore]
#[allow(clippy::too_many_lines)]
fn dump_surface() {
    use csp_core::DVec3;
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let env = |k: &str, d: f64| {
        std::env::var(k)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(d)
    };
    // Defaults: the terrin-launch pad, in DEGREES (lat −0.092 rad,
    // lon 2.916 rad — the units this probe's knobs take).
    let lat = env("DUMP_LAT", -5.2712);
    let lon = env("DUMP_LON", 167.0745);
    let alt = env("DUMP_ALT", 710.0);
    let pitch_down = env("DUMP_PITCH", 0.55); // rad below the local horizon
    let heading = env("DUMP_HEADING", 0.0).to_radians();

    if std::env::var("DUMP_SCAN").is_ok() {
        // Coarse normalized-height survey: + land, . shallow sea, ~ deep
        // sea. `DUMP_BODY` (default terrin) picks whose AUTHORED field
        // the survey samples — the land-fraction measurement a relief
        // re-authoring wants per body — scan-only: the render path
        // below stays Terrin (radius, biome map).
        let view = View::new().unwrap();
        let cfg = view.world.resource::<csp_view::terrain::TerrainConfig>();
        let name = std::env::var("DUMP_BODY").unwrap_or_else(|_| "terrin".into());
        let t = cfg
            .bodies
            .get(&name)
            .unwrap_or_else(|| panic!("no terrain config for {name}"));
        let field = csp_render::terrain::HeightField::bake_profiled(t.seed, t.profile);
        for la in (-80..=80i32).step_by(8) {
            let mut row = String::new();
            for lo in (0..360i32).step_by(6) {
                let (slat, clat) = (la as f64).to_radians().sin_cos();
                let (slon, clon) = (lo as f64).to_radians().sin_cos();
                let d = DVec3::new(clat * clon, clat * slon, slat);
                let h = field.sample_dir(d);
                row.push(if h > 0.45 {
                    '#'
                } else if h > 0.1 {
                    '+'
                } else if h > 0.0 {
                    '.'
                } else if h > -0.15 {
                    '-'
                } else {
                    '~'
                });
            }
            eprintln!("{la:4} {row}");
        }
        return;
    }
    // The AUTHORED Terrin field + amplitude (M5 §5.2-R2: Mountain,
    // 0.006·R = 3 600 m) — the same profiled bake and scale the
    // terrain pass draws, read from the view's config so a re-authored
    // terrain block flows here without editing this probe.
    let (field, authored_amplitude, biome, art) = terrin_scene_assets();
    // A/B knob: force the displacement scale (0 = flat datum) to prove
    // the mesh displacement is live — same scene, two amplitudes, diff.
    let amplitude_m = env("DUMP_AMP", authored_amplitude);
    let h_here = {
        let dir = {
            let (slat, clat) = lat.to_radians().sin_cos();
            let (slon, clon) = lon.to_radians().sin_cos();
            DVec3::new(clat * clon, clat * slon, slat)
        };
        field.sample_dir(dir) * amplitude_m
    };
    let fp = surface_probe_packet(
        lat,
        lon,
        alt,
        pitch_down,
        heading,
        amplitude_m,
        field,
        biome,
        art,
    );
    let img = render_flight_scene(&device, &queue, &fp);
    let name = std::env::var("DUMP_OUT").unwrap_or_else(|_| "/tmp/surf.ppm".into());
    let mut out = format!("P6 {W} {H} 255\n").into_bytes();
    for px in img.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(&name, &out).unwrap();
    let white = img
        .chunks_exact(4)
        .filter(|p| p[0] > 235 && p[1] > 215)
        .count();
    eprintln!(
        "{name}: h_here={h_here:.0} white={white} shadows={}",
        if fp.shadow.armed() {
            "armed"
        } else {
            "disarmed"
        }
    );
}

/// Scratch visual dump — write the seeded frame to /tmp/frame.ppm.
/// Run explicitly: `cargo test -p csp-view --test render_gate dump -- --ignored`.
#[test]
#[ignore]
fn dump() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let mut view = View::new().unwrap();
    view.seed_starter_craft().unwrap();
    view.set_flight_viewport(320.0, 240.0);
    view.world
        .resource_mut::<csp_view::flight::FlightInput>()
        .set_distance(25.0);
    for _ in 0..3 {
        frame(&mut view, 1);
    }
    // Sky-only until the bake lands — wait so the scratch dump shows
    // the ground it exists to eyeball.
    until_real_terrain(&mut view);
    let packet = view.take_flight_packet().unwrap();
    let img = render_flight_scene(&device, &queue, &packet);
    let mut out = b"P6 320 240 255\n".to_vec();
    for px in img.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write("/tmp/frame.ppm", &out).unwrap();
}

/// C17-F1 device-side check — the pick pass must emit no 0-tagged
/// geometry. `VabPacket.vertices` interleaves pick-0 decoration
/// (selection shells, ghosts, markers) with the real part triangles;
/// under `Greater`-wins reverse-Z an inflated shell encloses its
/// part and beats it at every covered pixel, so the pre-fix pass
/// decoded real parts as background (the click-to-place killer).
/// This packet puts pick-0 triangles strictly NEARER than two part
/// triangles over their pixels — the readback must still decode the
/// part ids through the real `encode_pick`/`map_pick` path.
#[test]
fn vab_pick_pass_emits_no_pick_zero_geometry() {
    let Some((device, queue)) = gate_device("vab_pick_pass_emits_no_pick_zero_geometry") else {
        return;
    };
    let mut vab = csp_render::vab::VabRenderer::new(&device, FORMAT, (W, H), (W, H));
    let v = |x: f32, y: f32, z: f32, pick: u32| csp_render::vab::VabVertex {
        pos: [x, y, z],
        world: [0.0; 3],
        nrm: [0.0; 3],
        color: [1.0; 4],
        mat: [0.0; 3],
        pick,
    };
    let packet = csp_render::vab::VabPacket {
        vertices: vec![
            // A full-frame pick-0 "selection shell" — z 0.9 is NEARER
            // than the parts' 0.5 under reverse-Z, so rasterizing it
            // writes 0 (and depth) over every pixel below.
            v(-1.5, -1.5, 0.9, 0),
            v(1.5, -1.5, 0.9, 0),
            v(1.5, 1.5, 0.9, 0),
            v(-1.5, -1.5, 0.9, 0),
            v(1.5, 1.5, 0.9, 0),
            v(-1.5, 1.5, 0.9, 0),
            // Part A — pick id 1, centered on NDC (0, 0).
            v(-0.2, -0.2, 0.5, 1),
            v(0.2, -0.2, 0.5, 1),
            v(0.0, 0.2, 0.5, 1),
            // A pick-0 marker covering the same pixel.
            v(0.0, 0.0, 0.9, 0),
            v(0.1, 0.0, 0.9, 0),
            v(0.0, 0.1, 0.9, 0),
            // Part B — pick id 2, centered on NDC (0.5, −0.5); a
            // second pickable run so the multi-draw path is hit.
            v(0.4, -0.6, 0.5, 2),
            v(0.6, -0.6, 0.5, 2),
            v(0.5, -0.4, 0.5, 2),
            // A trailing pick-0 ghost over the center again.
            v(-0.05, -0.05, 0.9, 0),
            v(0.05, -0.05, 0.9, 0),
            v(0.0, 0.05, 0.9, 0),
        ],
        pick_ids: vec![11, 22],
        ..Default::default()
    };
    let pick_at = |vab: &mut csp_render::vab::VabRenderer, x: u32, y: u32| -> u32 {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render-gate-pick"),
        });
        assert!(vab.encode_pick(&mut encoder, &device, &queue, &packet, x, y));
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        vab.map_pick(tx);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap()
    };
    // Pixel (160, 120) = NDC (0, 0) — inside part A AND covered by
    // three pick-0 triangles. The pre-fix pass decoded 0 here.
    assert_eq!(
        pick_at(&mut vab, W / 2, H / 2),
        1,
        "pick-0 decoration shadowed the part"
    );
    // Pixel (240, 180) = NDC (0.5, −0.5) — inside part B.
    assert_eq!(
        pick_at(&mut vab, 240, 180),
        2,
        "the second pickable run did not rasterize"
    );
    // Outside every part the pixel still decodes background.
    assert_eq!(pick_at(&mut vab, 5, 5), 0);
}
