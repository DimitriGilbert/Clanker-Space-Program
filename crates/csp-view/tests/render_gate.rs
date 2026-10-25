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
//! ICD): the tests print a note and return early rather than fail —
//! the gate exists to catch renderer/content regressions where a GPU
//! exists to run them, not to require one.

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

/// One frame of the app's §2.5 loop, headlessly — enough for the
/// seeded spawn command to land and the extract to emit a packet.
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
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
#[allow(clippy::too_many_lines)] // one render-pass recipe for the gate
fn render_flight_scene_sized(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    packet: &csp_render::flight::FlightPacket,
    w: u32,
    h: u32,
) -> Vec<u8> {
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
    let mut flight = csp_render::flight::FlightRenderer::new(device, FORMAT, (w, h));
    let mut terrain = csp_render::terrain::TerrainRenderer::new(device, FORMAT);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render-gate"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &msaa,
                resolve_target: Some(&target),
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
            // Same attachment as the app: the flight renderer's shared
            // reverse-Z target, cleared 0.0 (far).
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
        // The app's draw order: surface pass (sky backdrop + opaque
        // terrain), craft vertex stream, then the depth-test-only
        // ocean over both.
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
                rows_per_image: Some(h),
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
    img
}

/// The positive gate: the new-game view — `View::new` + the app's
/// `seed_starter_craft` — must produce a packet with real scene content
/// (terrain + craft vertices) AND rasterize to an image that is not
/// the uniform clear color.
#[test]
fn boot_scene_has_real_geometry() {
    let Some((device, queue)) = headless_device() else {
        eprintln!("render-gate: no headless GPU adapter (install lavapipe) — skipping");
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
        .distance_m = 300.0;
    for _ in 0..3 {
        frame(&mut view, 1);
    }
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
    let Some((device, queue)) = headless_device() else {
        eprintln!("render-gate: no headless GPU adapter (install lavapipe) — skipping");
        return;
    };

    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    view.set_flight_viewport(f64::from(W), f64::from(H));
    view.world
        .resource_mut::<csp_view::flight::FlightInput>()
        .distance_m = 300.0;
    for _ in 0..3 {
        frame(&mut view, 1);
    }
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
    let Some((device, queue)) = headless_device() else {
        eprintln!("render-gate: no headless GPU adapter (install lavapipe) — skipping");
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
    assert_eq!(
        packet.vertices.len(),
        6,
        "an empty flight scene emits only the fallback-sky quad"
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
    let flight = csp_render::flight::FlightRenderer::new(&device, FORMAT, (W, H));
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
        input.distance_m = dist;
        input.pitch = pitch;
        input.yaw = yaw;
    }
    for _ in 0..3 {
        frame(&mut view, 1);
    }
    let packet = view.take_flight_packet().unwrap();
    let img = render_flight_scene(&device, &queue, &packet);
    let name = std::env::var("DUMP_OUT").unwrap_or_else(|_| "/tmp/frame_low.ppm".into());
    let mut out = b"P6 320 240 255\n".to_vec();
    for px in img.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(&name, &out).unwrap();
    // Count near-white pixels for the sweep.
    let white = img
        .chunks_exact(4)
        .filter(|p| p[0] > 235 && p[1] > 235 && p[2] > 235)
        .count();
    eprintln!("{name}: white={white}");
}

/// Scratch dump of an actually-ascending craft: seed + throttle 1.0,
/// step until ~700 m, then rasterize the real flight packet — the
/// same VFX state the reported ~710 m screenshot came from.
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
        input.distance_m = env_f("DUMP_DIST", 60.0);
    }
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
    eprintln!("{name}: alt={a:.0} white={white}");
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

/// Scratch terrain dump at an arbitrary surface point/heading —
/// hand-builds the `TerrainPacket` (same builders `extract_flight`
/// uses) so any location/altitude/pitch can be inspected without
/// flying the sim. Env knobs: DUMP_LAT, DUMP_LON, DUMP_HEADING (deg),
/// DUMP_ALT (m), DUMP_PITCH (rad, look-down), DUMP_OUT.
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
    let lat = env("DUMP_LAT", -0.092).to_radians();
    let lon = env("DUMP_LON", 2.916).to_radians();
    let alt = env("DUMP_ALT", 710.0);
    let pitch_down = env("DUMP_PITCH", 0.55); // rad below the local horizon
    let heading = env("DUMP_HEADING", 0.0).to_radians();

    let r_body = 600_000.0;
    let field = csp_render::terrain::HeightField::bake(601, 1024, 512);
    if std::env::var("DUMP_SCAN").is_ok() {
        // Coarse normalized-height survey: + land, . shallow sea, ~ deep sea.
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
    let art = csp_view::art::load()
        .0
        .terrain
        .clone()
        .expect("terrain art pack");
    let biome = {
        // The real Terrin biome map via the view's own loader path.
        let view = View::new().unwrap();
        let cfg = view.world.resource::<csp_view::terrain::TerrainConfig>();
        cfg.bodies.get("terrin").and_then(|b| b.biome.clone())
    };

    // Surface direction under the eye.
    let (slat, clat) = lat.sin_cos();
    let (slon, clon) = lon.sin_cos();
    let dir = DVec3::new(clat * clon, clat * slon, slat);
    let h_here = field.sample_dir(dir) * 2_400.0;
    let eye_rel = dir * (r_body + h_here.max(0.0) + alt);

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
    let fwd_h = north * heading.cos() + east * heading.sin();
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
        2_400.0,
        &csp_render::terrain::LodConfig::default(),
    );
    let mut terrain_inst = Vec::new();
    let mut ocean_inst = Vec::new();
    for &id in &leaves {
        terrain_inst.push(csp_render::terrain::patch_instance(
            id, 0.0, eye_rel, r_body,
        ));
        // replicate patch_might_hold_sea's 5x5 sample rule
        let (corner, du, dv) = csp_render::terrain::patch_cube(id);
        let mut lo = f64::INFINITY;
        for j in 0..5 {
            for i in 0..5 {
                let c = corner + du * (i as f64 / 4.0) + dv * (j as f64 / 4.0);
                lo = lo.min(field.sample_dir(c / c.norm()));
            }
        }
        if lo * 2_400.0 <= 600.0 {
            ocean_inst.push(csp_render::terrain::patch_instance(
                id, 1.0, eye_rel, r_body,
            ));
        }
    }
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
    let packet = csp_render::terrain::TerrainPacket {
        uniforms: csp_render::terrain::TerrainUniforms {
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
            planet: [r_body as f32, 70_000.0, r_body as f32, 2_400.0],
            atmo: [
                5_600.0,
                if biome.is_some() { 1.0 } else { 0.0 },
                alt as f32,
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
        },
        heightfield: Some(std::sync::Arc::new(field)),
        biome,
        atmo: Some(csp_render::terrain::AtmoDesc {
            radius_m: r_body,
            top_m: 70_000.0,
            scale_height_m: 5_600.0,
        }),
        terrain: terrain_inst,
        ocean: ocean_inst,
        sky: true,
        art: Some(art),
    };
    let fp = csp_render::flight::FlightPacket {
        terrain: Some(packet),
        ..Default::default()
    };
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
    eprintln!("{name}: h_here={h_here:.0} white={white}");
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
        .distance_m = 25.0;
    for _ in 0..3 {
        frame(&mut view, 1);
    }
    let packet = view.take_flight_packet().unwrap();
    let img = render_flight_scene(&device, &queue, &packet);
    let mut out = b"P6 320 240 255\n".to_vec();
    for px in img.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write("/tmp/frame.ppm", &out).unwrap();
}
