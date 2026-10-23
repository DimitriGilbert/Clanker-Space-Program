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
    Stats {
        non_clear,
        distinct: buckets.iter().filter(|n| **n > 0).count(),
        max_spread: (0..3).map(|c| hi[c] - lo[c]).max().unwrap_or(0),
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
    let color_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render-gate-target"),
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
    let target = color_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut flight = csp_render::flight::FlightRenderer::new(device, FORMAT, (W, H));
    let mut terrain = csp_render::terrain::TerrainRenderer::new(device, FORMAT);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render-gate"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target,
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
        // The app's draw order: surface pass (sky backdrop + terrain)
        // before the craft vertex stream.
        if let Some(tp) = &packet.terrain {
            terrain.draw(&mut pass, device, queue, tp);
        }
        flight.draw(&mut pass, device, queue, packet);
    }
    let row_bytes = W as usize * 4;
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
        "seeded scene: non_clear={}/{} distinct={} spread={}",
        stats.non_clear, total, stats.distinct, stats.max_spread
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
