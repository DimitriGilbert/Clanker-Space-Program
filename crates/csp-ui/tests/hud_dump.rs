//! Scratch HUD rasterizer — runs `csp_ui`'s overlay draws through
//! `egui::Context::run_pass`, tessellates the shapes, and rasterizes
//! the triangles (sampling the real font atlas) onto a CPU framebuffer
//! over a bright worst-case backdrop. Writes `/tmp/hud_*.ppm` plus a
//! contrast report. Same scratch-dump convention as
//! `csp-view/tests/render_gate.rs`:
//!
//! `cargo test -p csp-ui --test hud_dump -- --ignored --nocapture`

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use egui::epaint::{Primitive, Vertex};

const W: usize = 960;
const H: usize = 600;

/// One managed texture (the font atlas) stitched from `textures_delta`.
struct Atlas {
    pixels: Vec<egui::Color32>,
    size: [usize; 2],
}

impl Atlas {
    fn sample(&self, u: f32, v: f32) -> egui::Color32 {
        let x = (u * self.size[0] as f32).clamp(0.0, self.size[0] as f32 - 1.0) as usize;
        let y = (v * self.size[1] as f32).clamp(0.0, self.size[1] as f32 - 1.0) as usize;
        self.pixels[y * self.size[0] + x]
    }
}

/// The fake scene the HUD must stay readable over — a bright
/// sky→terrain gradient (the wash-out case bare yellow text died on).
fn backdrop(x: usize, y: usize) -> [f32; 3] {
    let t = y as f32 / H as f32;
    let wob = ((x as f32 * 0.05).sin() * 0.5 + 0.5) * 30.0;
    [
        150.0 + 90.0 * t + wob * t,
        175.0 + 55.0 * t,
        205.0 - 80.0 * t,
    ]
}

fn blend(dst: &mut [f32; 4], v: Vertex, tex: egui::Color32) {
    let a = (v.color.a() as f32 / 255.0) * (tex.a() as f32 / 255.0);
    let srgb = [
        v.color.r() as f32 * tex.r() as f32 / 255.0,
        v.color.g() as f32 * tex.g() as f32 / 255.0,
        v.color.b() as f32 * tex.b() as f32 / 255.0,
    ];
    for c in 0..3 {
        dst[c] = srgb[c] * a + dst[c] * (1.0 - a);
    }
}

/// Barycentric triangle rasterizer for one egui mesh.
fn draw_mesh(mesh: &egui::Mesh, clip: egui::Rect, atlas: &Atlas, fb: &mut [[f32; 4]]) {
    for tri in mesh.indices.chunks_exact(3) {
        let [i0, i1, i2] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let (a, b, c) = (mesh.vertices[i0], mesh.vertices[i1], mesh.vertices[i2]);
        let min_x = a.pos.x.min(b.pos.x).min(c.pos.x).max(clip.left()).max(0.0);
        let min_y = a.pos.y.min(b.pos.y).min(c.pos.y).max(clip.top()).max(0.0);
        let max_x = a
            .pos
            .x
            .max(b.pos.x)
            .max(c.pos.x)
            .min(clip.right())
            .min(W as f32);
        let max_y = a
            .pos
            .y
            .max(b.pos.y)
            .max(c.pos.y)
            .min(clip.bottom())
            .min(H as f32);
        if min_x >= max_x || min_y >= max_y {
            continue;
        }
        let d =
            (b.pos.y - c.pos.y) * (a.pos.x - c.pos.x) + (c.pos.x - b.pos.x) * (a.pos.y - c.pos.y);
        if d.abs() < 1e-6 {
            continue;
        }
        for py in min_y as usize..max_y.ceil() as usize {
            for px in min_x as usize..max_x.ceil() as usize {
                let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
                let w0 =
                    ((b.pos.y - c.pos.y) * (x - c.pos.x) + (c.pos.x - b.pos.x) * (y - c.pos.y)) / d;
                let w1 =
                    ((c.pos.y - a.pos.y) * (x - c.pos.x) + (a.pos.x - c.pos.x) * (y - c.pos.y)) / d;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let uv = |p: Vertex, q: Vertex, r: Vertex| {
                    (
                        w0 * p.uv.x + w1 * q.uv.x + w2 * r.uv.x,
                        w0 * p.uv.y + w1 * q.uv.y + w2 * r.uv.y,
                    )
                };
                let (u, v) = uv(a, b, c);
                // Vertex color is uniform per glyph quad — nearest is fine.
                let vert = if w0 > 0.5 {
                    a
                } else if w1 > 0.5 {
                    b
                } else {
                    c
                };
                blend(&mut fb[py * W + px], vert, atlas.sample(u, v));
            }
        }
    }
}

/// Runs one egui pass over the backdrop and returns the composited
/// framebuffer (RGBA8) plus the drawn text/panel pixel stats.
/// The font atlas arrives as a full-texture set on a frame's delta —
/// grab the biggest Color image seen.
fn grab_atlas(atlas: &mut Atlas, out: &egui::FullOutput) {
    for deltas in out.textures_delta.set.values() {
        for delta in deltas {
            let egui::ImageData::Color(img) = &delta.image;
            if delta.pos.is_none() || img.size[0] * img.size[1] > atlas.pixels.len() {
                *atlas = Atlas {
                    pixels: img.pixels.clone(),
                    size: img.size,
                };
            }
        }
    }
}

fn render_pass(draw: impl Fn(&egui::Context)) -> Vec<u8> {
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(W as f32, H as f32),
        )),
        ..Default::default()
    };
    // egui fades Areas/frames IN over ~0.4 s of stable_dt: frame 1
    // paints `Shape::Noop` at opacity 0, later frames ease to full —
    // run past the animation before tessellating.
    let mut atlas = Atlas {
        pixels: vec![egui::Color32::TRANSPARENT],
        size: [1, 1],
    };
    let mut out = ctx.run_pass(input.clone(), &draw);
    grab_atlas(&mut atlas, &out);
    out.textures_delta.clear();
    for _ in 0..45 {
        out = ctx.run_pass(input.clone(), &draw);
        out.textures_delta.clear();
    }
    grab_atlas(&mut atlas, &out);
    let prims = ctx.tessellate(out.shapes.clone(), out.pixels_per_point);
    eprintln!(
        "shapes={} prims={} tris={} delta_sets={} ppp={:?}",
        out.shapes.len(),
        prims.len(),
        prims
            .iter()
            .map(|p| match &p.primitive {
                Primitive::Mesh(m) => m.indices.len() / 3,
                _ => 0,
            })
            .sum::<usize>(),
        out.textures_delta.set.len(),
        out.pixels_per_point,
    );

    // `TexturesDelta` is `#[must_use]`-dropped unless consumed — the
    // atlas is already cloned out, so mark it handled.
    out.textures_delta.clear();

    let mut fb = vec![[0.0f32; 4]; W * H];
    for y in 0..H {
        for x in 0..W {
            let [r, g, b] = backdrop(x, y);
            fb[y * W + x] = [r, g, b, 255.0];
        }
    }
    for cp in &prims {
        if let Primitive::Mesh(mesh) = &cp.primitive {
            draw_mesh(mesh, cp.clip_rect, &atlas, &mut fb);
        }
    }
    let mut img = Vec::with_capacity(W * H * 3);
    for p in &fb {
        img.extend_from_slice(&[p[0] as u8, p[1] as u8, p[2] as u8]);
    }
    img
}

fn write_ppm(path: &str, img: &[u8]) {
    let mut out = format!("P6 {W} {H} 255\n").into_bytes();
    out.extend_from_slice(img);
    std::fs::write(path, &out).unwrap();
    eprintln!("wrote {path}");
}

/// ASCII luminance preview — lets the log itself show whether the HUD
/// reads (dark chips with bright text) without opening the PPM.
fn ascii_preview(img: &[u8], name: &str) {
    const COLS: usize = 118;
    const ROWS: usize = 42;
    let ramp = b" .:-=+*#%@";
    eprintln!("--- {name} (luminance) ---");
    for ry in 0..ROWS {
        let mut line = String::with_capacity(COLS);
        for rx in 0..COLS {
            let mut sum = 0.0f32;
            let mut n = 0.0f32;
            for sy in 0..(H / ROWS) {
                for sx in 0..(W / COLS) {
                    let (x, y) = (rx * (W / COLS) + sx, ry * (H / ROWS) + sy);
                    let i = (y * W + x) * 3;
                    sum += 0.2126 * img[i] as f32
                        + 0.7152 * img[i + 1] as f32
                        + 0.0722 * img[i + 2] as f32;
                    n += 1.0;
                }
            }
            let lum = (sum / n / 25.6).clamp(0.0, 9.9) as usize;
            line.push(ramp[lum] as char);
        }
        eprintln!("{line}");
    }
}

/// Contrast check on the notice-bar band: bright text pixels vs the
/// chip's dark fill must split the luminance histogram cleanly.
fn notice_contrast(img: &[u8]) {
    let lum = |x: usize, y: usize| -> f32 {
        let i = (y * W + x) * 3;
        0.2126 * img[i] as f32 + 0.7152 * img[i + 1] as f32 + 0.0722 * img[i + 2] as f32
    };
    // The notice bar sits top-center (~y 44..90).
    let mut dark = 0usize;
    let mut bright = 0usize;
    let mut mid = 0usize;
    for y in 40..95 {
        for x in (W / 4)..(3 * W / 4) {
            let l = lum(x, y);
            if l < 60.0 {
                dark += 1;
            } else if l > 150.0 {
                bright += 1;
            } else {
                mid += 1;
            }
        }
    }
    eprintln!("notice band: dark(chip)={dark} bright(text)={bright} mid(AA)={mid}");
    assert!(
        bright > 400,
        "notice text should paint bright pixels over the chip"
    );
    assert!(
        dark > bright,
        "the chip should dominate the notice band's background"
    );
}

fn flight_input<'a>(
    keys: &'a csp_ui::Keymap,
    key_labels: &'a BTreeMap<String, String>,
    prop: &'a [csp_ui::PropellantGauge],
) -> csp_ui::FlightHudInput<'a> {
    csp_ui::FlightHudInput {
        has_vessel: true,
        latched: false,
        alt_m: 12_482.0,
        surface_speed_mps: 812.4,
        orbital_speed_mps: 1_204.6,
        vspeed_mps: 138.2,
        heading_deg: 87.5,
        pitch_deg: 42.0,
        roll_deg: -8.0,
        throttle: 0.68,
        parts: 7,
        navball: csp_ui::navball::NavballInput::default(),
        warp: 4,
        vessel_count: 1,
        debris_count: 0,
        apo_alt_m: Some(84_210.0),
        pe_alt_m: Some(-6_400.0),
        t_apo_s: Some(214.0),
        t_pe_s: None,
        period_s: None,
        propellant: prop,
        stages_left: 2,
        stages_total: 3,
        mass_kg: 12_840.0,
        thrust_n: 148_300.0,
        thrust_full_n: 218_000.0,
        twr: Some(1.18),
        twr_surface: Some(1.72),
        accel_mps2: Some(11.55),
        accel_gee: Some(1.18),
        isp_s: Some(310.0),
        mass_flow_kg_s: Some(48.6),
        fuel_depletion_s: Some(94.0),
        burn_full_s: Some(64.0),
        delta_v_mps: Some(1_940.0),
        delta_v_stages_mps: 420.0,
        keys,
        key_labels,
    }
}

/// Scratch visual dump — the flight HUD + notice stack + status strip
/// over a bright backdrop. `/tmp/hud_flight.ppm`.
#[test]
#[ignore]
fn dump_hud_flight() {
    let keys = csp_ui::Keymap::default();
    let key_labels = BTreeMap::new();
    let prop = [
        csp_ui::PropellantGauge {
            resource: "liquidFuel".into(),
            units: 312.0,
            capacity: 540.0,
        },
        csp_ui::PropellantGauge {
            resource: "oxidizer".into(),
            units: 381.0,
            capacity: 660.0,
        },
    ];
    let img = render_pass(|ctx| {
        csp_ui::program::program_status(ctx, 12_480.0, 37.5, 3);
        csp_ui::notice_overlay(
            ctx,
            &[
                "ENGINE IGNITION".to_string(),
                "SESSION IS READ-ONLY — WARP DEMOTION NOT APPLIED".to_string(),
            ],
        );
        csp_ui::flight_hud(ctx, &flight_input(&keys, &key_labels, &prop));
    });
    write_ppm("/tmp/hud_flight.ppm", &img);
    ascii_preview(&img, "flight HUD + notices");
    notice_contrast(&img);
}

/// Scratch visual dump — the map overlay's chips (orbit readout +
/// hint bar) over the same backdrop. `/tmp/hud_map.ppm`.
#[test]
#[ignore]
fn dump_hud_map() {
    let keys = csp_ui::Keymap::default();
    let key_labels = BTreeMap::new();
    let labels = vec![
        csp_ui::MapLabel {
            ndc: [-0.3, 0.4, 0.0],
            text: "terrin".into(),
            color: [0.4, 0.7, 1.0, 1.0],
            kind: csp_ui::MapLabelKind::Body,
        },
        csp_ui::MapLabel {
            ndc: [0.2, -0.1, 0.0],
            text: "Ap".into(),
            color: [1.0, 0.8, 0.3, 1.0],
            kind: csp_ui::MapLabelKind::Apsis,
        },
    ];
    let orbit = csp_ui::MapOrbitInfo {
        name: "vessel 1".into(),
        parent_name: "terrin".into(),
        apo_r_m: Some(720_000.0),
        pe_r_m: 610_000.0,
        parent_radius_m: Some(600_000.0),
        period_s: Some(5_412.0),
    };
    let img = render_pass(|ctx| {
        csp_ui::map_overlay(
            ctx,
            "terrin",
            4.0e6,
            &labels,
            Some(&orbit),
            &keys,
            &key_labels,
        );
    });
    write_ppm("/tmp/hud_map.ppm", &img);
    ascii_preview(&img, "map overlay");
}
