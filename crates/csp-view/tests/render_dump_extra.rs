//! Scratch visual dumps for the two scenes the render gate does not
//! cover: the orbit MAP view and the VAB 3D editor scene. Same
//! machinery as `render_gate.rs` (surfaceless device, offscreen
//! Rgba8UnormSrgb target, copy_texture_to_buffer readback, P6 PPM) —
//! run explicitly and convert to PNG:
//!
//! `cargo test -p csp-view --test render_dump_extra -- --ignored --nocapture`

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_view::{View, ViewMode};

/// Same offscreen size as the gate (row alignment: 320·4 = 5·256).
const W: u32 = 320;
const H: u32 = 240;
/// The app's scene-pass clear color (csp-app `paint`, linear RGBA).
const CLEAR: [f64; 4] = [0.02, 0.02, 0.05, 1.0];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Mirrors render_gate.rs `headless_device`.
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
        label: Some("render-dump-extra"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .ok()?;
    Some((device, queue))
}

/// Mirrors render_gate.rs `frame` — one §2.5 loop iteration.
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
}

/// Offscreen color target + view, RENDER_ATTACHMENT | COPY_SRC.
fn offscreen_target(device: &wgpu::Device, label: &'static str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
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
    })
}

/// A 4×-multisampled attachment texture — the pipelines all run
/// MSAA now; dumps render into this and resolve into the readback.
fn msaa_target(device: &wgpu::Device, label: &'static str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
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
    })
}

/// Render-pass color attachment rendering to `msaa` and resolving
/// into `view`, cleared to CLEAR.
fn color_attach<'a>(
    msaa: &'a wgpu::TextureView,
    view: &'a wgpu::TextureView,
) -> wgpu::RenderPassColorAttachment<'a> {
    wgpu::RenderPassColorAttachment {
        view: msaa,
        resolve_target: Some(view),
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
    }
}

/// Submit + read the whole color texture back as RGBA bytes.
fn readback(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Vec<u8> {
    let row_bytes = W as usize * 4;
    debug_assert_eq!(row_bytes % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize, 0);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("dump-extra-readback"),
        size: (row_bytes * H as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dump-extra-copy"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
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

/// RGBA bytes → P6 PPM at `path` (same style as render_gate's dumps).
fn write_ppm(img: &[u8], path: &str) {
    let mut out = format!("P6 {W} {H} 255\n").into_bytes();
    for px in img.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(path, &out).unwrap();
    eprintln!("wrote {path}");
}

/// Orbit MAP view dump — the app's map path: `set_mode(Map)` (refocus),
/// map viewport, one frame for the solved camera + extract, then
/// `MapRenderer` into the offscreen target. The map pipelines carry no
/// depth state, so the pass attaches none (csp-app `paint` does the
/// same — wgpu validates pass/pipeline depth compatibility).
#[test]
#[ignore]
fn dump_map() {
    let Some((device, queue)) = headless_device() else {
        eprintln!("render-dump-extra: no headless GPU adapter — skipping");
        return;
    };
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    view.set_map_viewport(f64::from(W), f64::from(H));
    view.set_mode(ViewMode::Map);
    for _ in 0..3 {
        frame(&mut view, 1);
    }
    assert_eq!(view.mode(), ViewMode::Map);
    let packet = view
        .take_map_packet()
        .expect("RenderExtract produced a map packet");
    eprintln!(
        "map packet: bg={} lines={} markers={}",
        packet.bg.vertices.len(),
        packet.lines.vertices.len(),
        packet.markers.vertices.len()
    );

    let tex = offscreen_target(&device, "map-dump-target");
    let msaa_tex = msaa_target(&device, "map-dump-msaa");
    let msaa = msaa_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let target = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut map = csp_render::map::MapRenderer::new(&device, FORMAT);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("map-dump"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("map"),
            color_attachments: &[Some(color_attach(&msaa, &target))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        map.draw(&mut pass, &device, &queue, &packet);
    }
    queue.submit([encoder.finish()]);
    let img = readback(&device, &queue, &tex);
    write_ppm(&img, "/tmp/map.ppm");
}

/// Part-model lineup — every baked model in `parts.csar` laid out in
/// a grid and drawn through the real `FlightRenderer` (the same
/// per-pixel lighting path the craft uses). Env knobs: DUMP_YAW
/// (orbit rad), DUMP_PITCH, DUMP_DIST, DUMP_OUT.
/// `cargo test -p csp-view --test render_dump_extra dump_parts -- --ignored --nocapture`.
#[test]
#[ignore]
#[allow(clippy::too_many_lines)]
fn dump_parts() {
    use csp_core::DVec3;
    let Some((device, queue)) = headless_device() else {
        eprintln!("render-dump-extra: no headless GPU adapter — skipping");
        return;
    };
    let env = |k: &str, d: f64| {
        std::env::var(k)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(d)
    };
    let yaw = env("DUMP_YAW", 0.6);
    let pitch = env("DUMP_PITCH", 0.35);
    let dist = env("DUMP_DIST", 9.0);

    // Look-at camera: eye orbits the origin, forward = −eye_dir.
    let eye_dir = DVec3::new(
        yaw.cos() * pitch.cos(),
        yaw.sin() * pitch.cos(),
        pitch.sin(),
    );
    let eye = eye_dir * dist;
    let fwd = -eye_dir;
    let right0 = fwd.cross(DVec3::Z);
    let right = right0 / right0.norm();
    let up = right.cross(fwd);
    let f = 1.0 / (0.55f64).tan(); // ~56° vertical fov
    let aspect = f64::from(W) / f64::from(H);
    let project = |p: DVec3| -> Option<[f32; 3]> {
        let rel = p - eye;
        let zc = rel.dot(fwd);
        if zc <= 1e-3 {
            return None;
        }
        Some([
            (rel.dot(right) * f / aspect / zc) as f32,
            (rel.dot(up) * f / zc) as f32,
            (0.05 / zc) as f32, // reverse-Z: near=0.05 → ndc 1, ∞ → 0
        ])
    };

    let art = csp_view::art::load();
    const KEYS: &[&str] = &[
        "pod",
        "nosecone",
        "tank",
        "booster",
        "engine_a",
        "engine_b",
        "decoupler",
        "adapter",
        "battery",
        "science",
        "fins",
    ];
    // DUMP_ONLY=key renders a single part filling the frame instead
    // of the grid (off = part offset for the grid layout).
    let only = std::env::var("DUMP_ONLY").ok();
    let mut verts = Vec::new();
    let mut placed = 0usize;
    for (k, key) in KEYS.iter().enumerate() {
        let Some(model) = art.model(key) else {
            continue;
        };
        let off = if only.as_deref() == Some(*key) {
            DVec3::ZERO
        } else {
            let (col, row) = (k % 4, k / 4);
            DVec3::new((col as f64 - 1.5) * 2.2, (1.0 - row as f64) * 2.4, 0.0)
        };
        if let Some(o) = &only
            && o != key
        {
            continue;
        }
        let h = DVec3::new(0.62, 0.62, 0.62);
        let (lo, hi) = (off - h, off + h);
        let mut tris = Vec::new();
        csp_view::part_mesh::emit_model(&model, lo, hi, &mut tris);
        for t in &tris {
            let w = [t.a, t.b, t.c];
            let [Some(c0), Some(c1), Some(c2)] = w.map(project) else {
                continue;
            };
            for (i, &(n, alb, mt)) in [(t.na, t.ca, t.ma), (t.nb, t.cb, t.mb), (t.nc, t.cc, t.mc)]
                .iter()
                .enumerate()
            {
                let rel = w[i] - eye;
                verts.push(csp_render::flight::FlightVertex {
                    pos: [c0, c1, c2][i],
                    world: [rel.x as f32, rel.y as f32, rel.z as f32],
                    nrm: [n.x as f32, n.y as f32, n.z as f32],
                    color: alb,
                    mat: mt,
                });
            }
        }
        placed += 1;
    }
    if only.is_none() {
        assert_eq!(placed, KEYS.len(), "parts.csar missing models");
    }
    eprintln!("dump_parts: {placed} models, {} verts", verts.len());

    let packet = csp_render::flight::FlightPacket {
        vertices: verts,
        light: csp_render::PartLight {
            sun_dir: [0.42, 0.35, 0.84, 0.0],
            sun_col: [0.95, 0.92, 0.85, 0.0],
            amb_up: [0.16, 0.20, 0.27, 0.0],
            amb_dn: [0.07, 0.06, 0.05, 0.0],
            hemi: [0.0, 0.0, 1.0, 0.12],
        },
        ..Default::default()
    };
    let tex = offscreen_target(&device, "parts-dump-target");
    let msaa_tex = msaa_target(&device, "parts-dump-msaa");
    let msaa = msaa_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let target = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut flight = csp_render::flight::FlightRenderer::new(&device, FORMAT, (W, H));
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("parts-dump"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("parts"),
            color_attachments: &[Some(color_attach(&msaa, &target))],
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
        flight.draw(&mut pass, &device, &queue, &packet);
    }
    queue.submit([encoder.finish()]);
    let img = readback(&device, &queue, &tex);
    let name = std::env::var("DUMP_OUT").unwrap_or_else(|_| "/tmp/parts.ppm".into());
    write_ppm(&img, &name);

    // ASCII preview in stderr — a fast silhouette sanity check.
    let chars = " .:-=+*#%@";
    for y in (0..H as usize).step_by(4) {
        let mut row = String::new();
        for x in (0..W as usize).step_by(2) {
            let o = (y * W as usize + x) * 4;
            let lum = (img[o] as u32 + img[o + 1] as u32 + img[o + 2] as u32) / 3;
            row.push(chars.as_bytes()[(lum as usize * 10 / 256).min(9)] as char);
        }
        eprintln!("{row}");
    }
}

/// VAB editor scene dump — `set_mode(Vab)` after the starter seed (the
/// seed leaves the same craft loaded in the VAB session), vab viewport,
/// frames for the camera's first-focus auto-framing, then
/// `VabRenderer` with its reverse-Z depth attached like the app's
/// scene pass.
#[test]
#[ignore]
fn dump_vab() {
    let Some((device, queue)) = headless_device() else {
        eprintln!("render-dump-extra: no headless GPU adapter — skipping");
        return;
    };
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    view.set_vab_viewport(f64::from(W), f64::from(H));
    view.set_mode(ViewMode::Vab);
    for _ in 0..3 {
        frame(&mut view, 1);
    }
    assert_eq!(view.mode(), ViewMode::Vab);
    let packet = view
        .take_vab_packet()
        .expect("RenderExtract produced a vab packet");
    eprintln!(
        "vab packet: vertices={} pick_ids={} pick_generation={}",
        packet.vertices.len(),
        packet.pick_ids.len(),
        packet.pick_generation
    );

    let tex = offscreen_target(&device, "vab-dump-target");
    let msaa_tex = msaa_target(&device, "vab-dump-msaa");
    let msaa = msaa_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let target = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut vab = csp_render::vab::VabRenderer::new(&device, FORMAT, (W, H));
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("vab-dump"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vab"),
            color_attachments: &[Some(color_attach(&msaa, &target))],
            // Reverse-Z target owned by the renderer, like the app pass.
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: vab.depth_view(),
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
        vab.draw(&mut pass, &device, &queue, &packet);
    }
    queue.submit([encoder.finish()]);
    let img = readback(&device, &queue, &tex);
    write_ppm(&img, "/tmp/vab.ppm");
}
