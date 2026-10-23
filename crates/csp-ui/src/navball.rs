//! The flight attitude indicator — a KSP-style navball rendered with
//! egui primitives (§3.7's "flight instruments" presentation floor).
//!
//! Convention: the widget receives world directions already rotated
//! into the *focused vessel's body frame* (`csp_view::flight` stages
//! them — the UI stays f64-array math, §2.1). A body-frame direction
//! `c = (cx, cy, cz)` maps to the ball as `screen = (−cx, cy)` —
//! center is the nose (body +Z), screen-up is the dorsal +Y, and the
//! orthographic front hemisphere is `cz ≥ 0`. Directions on the far
//! hemisphere clamp to the rim, the usual navball rule.
//!
//! The horizon is the great circle `c·up = 0`; its front-hemisphere
//! half projects to an ellipse arc and the rest of the ground cap
//! wraps behind onto the rim — the polygon is assembled as a triangle
//! fan around the nadir projection so neither ellipse nor cap needs a
//! convexity assumption.

use egui::{Align2, Color32, FontId, Pos2, Shape, Stroke, pos2, vec2};

/// Plain-data navball input — body-frame unit vectors staged by
/// `csp-view` (`rot⁻¹·d` already applied).
#[derive(Clone, Copy, Debug, Default)]
pub struct NavballInput {
    /// Local up `û` in body frame. `[0,0,0]` = no reference (no
    /// planet) — the ball draws bare.
    pub up: [f64; 3],
    /// Local level north `n̂` in body frame (horizon-plane compass).
    pub north: [f64; 3],
    /// Local level east `ê` in body frame (`n̂ × û`).
    pub east: [f64; 3],
    /// Surface-relative prograde in body frame — `None` while
    /// surface-stationary (a parked craft has no prograde to show).
    pub prograde: Option<[f64; 3]>,
    /// Compass heading of the nose, degrees `[0, 360)` — NaN draws no
    /// tape (undefined pointing straight up).
    pub heading_deg: f64,
}

// ---------------------------------------------------------------------------
// tiny f64 vec3 helpers on [f64; 3] — csp-ui keeps no csp-core edge (§2.1)
// ---------------------------------------------------------------------------

type V3 = [f64; 3];

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(v: V3) -> f64 {
    dot(v, v).sqrt()
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// An orthonormal pair spanning `u⊥` — deterministic basis for the
/// circle parametrizations (axis nearest-aligned component removed
/// from +X, else +Y).
fn ortho_basis(u: V3) -> (V3, V3) {
    let seed = if u[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let mut a = [
        seed[0] - u[0] * dot(u, seed),
        seed[1] - u[1] * dot(u, seed),
        seed[2] - u[2] * dot(u, seed),
    ];
    let n = norm(a);
    a = [a[0] / n, a[1] / n, a[2] / n];
    (a, cross(u, a))
}

/// Circle `v·u = k` on the unit sphere (`|k| ≤ 1`) — parametric
/// `t ↦ k·u + √(1−k²)·(a cos t + b sin t)` over `samples` segments,
/// returning only the front-hemisphere (`v_z > 0`) runs as screen
/// offsets in units of ball radius (`(−vx, vy)`).
fn circle_front_points(u: V3, k: f64, samples: usize) -> Vec<(f32, f32)> {
    let (a, b) = ortho_basis(u);
    let c = (1.0 - k * k).max(0.0).sqrt();
    let mut out = Vec::new();
    for i in 0..=samples {
        let t = 2.0 * std::f64::consts::PI * (i as f64) / (samples as f64);
        let (ct, st) = (t.cos(), t.sin());
        let v = [
            k * u[0] + c * (a[0] * ct + b[0] * st),
            k * u[1] + c * (a[1] * ct + b[1] * st),
            k * u[2] + c * (a[2] * ct + b[2] * st),
        ];
        if v[2] > 0.0 {
            out.push((-v[0] as f32, v[1] as f32));
        }
    }
    out
}

/// The navball itself, drawn at `center` with `radius` px. Paint order:
/// sky disc → ground cap → pitch ladder → compass letters → prograde/
/// retrograde markers → rim + fixed nose reticle.
#[allow(clippy::too_many_lines)] // paint order reads as one instrument pass
pub fn navball(painter: &egui::Painter, center: Pos2, radius: f32, input: &NavballInput) {
    let sky = Color32::from_rgb(56, 96, 160);
    let ground = Color32::from_rgb(112, 84, 52);
    let mark = Color32::from_rgb(255, 170, 40);
    let white = Color32::from_gray(230);
    let to_px = |v: V3| -> Pos2 { center + vec2(-v[0] as f32 * radius, -v[1] as f32 * radius) };

    painter.circle_filled(center, radius, sky);

    let up = input.up;
    let (ux, uy, uz) = (up[0], up[1], up[2]);
    let rho = (ux * ux + uy * uy).sqrt();
    if norm(up) < 1.0e-6 {
        // No reference body — bare instrument.
    } else if rho < 1.0e-6 {
        // Nose on the vertical — one cap fills the disc.
        if uz < 0.0 {
            painter.circle_filled(center, radius, ground);
        }
    } else {
        // Screen-space (math convention, y-up) unit vectors: `m` points
        // at the zenith projection, `mp` completes the ellipse frame.
        // The front-hemisphere horizon is the half-ellipse
        // `p = m·(uz·cos θ) + mp·sin θ` for θ ∈ (π/2, 3π/2); the ground
        // cap's rim arc is the half of the limb centered on the nadir
        // rim angle atan2(−uy, ux).
        let m = (-ux / rho, uy / rho);
        let mp = (-m.1, m.0);
        let mut boundary: Vec<Pos2> = Vec::with_capacity(72);
        for i in 0..=40 {
            let t = std::f64::consts::PI * (0.5 + i as f64 / 40.0);
            let (ct, st) = (t.cos(), t.sin());
            let (px, py) = (m.0 * uz * ct + mp.0 * st, m.1 * uz * ct + mp.1 * st);
            boundary.push(center + vec2(px as f32 * radius, -py as f32 * radius));
        }
        // Rim arc on the ground side: α ∈ (α_n − π/2, α_n + π/2).
        let a_nadir = (-uy).atan2(ux);
        for i in 1..40 {
            let a =
                a_nadir - std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * (i as f64 / 40.0);
            boundary.push(center + vec2(a.cos() as f32 * radius, -a.sin() as f32 * radius));
        }
        // Star fan about the nadir projection — the cap is always
        // star-shaped from there, so no convexity assumption.
        let nadir = to_px([-ux, -uy, -uz]);
        let mut mesh = egui::Mesh::default();
        let n_idx = mesh.vertices.len() as u32;
        mesh.colored_vertex(nadir, ground);
        let mut indices = Vec::with_capacity(boundary.len());
        for p in &boundary {
            indices.push(mesh.vertices.len() as u32);
            mesh.colored_vertex(*p, ground);
        }
        for w in indices.windows(2) {
            mesh.add_triangle(n_idx, w[0], w[1]);
        }
        painter.add(egui::Shape::mesh(mesh));
        // The horizon line itself — the ellipse arc.
        painter.add(Shape::line(
            boundary[..41].to_vec(),
            Stroke::new(1.5, white),
        ));
    }

    // Pitch ladder — small circles v·û = sin(±10°, ±20°, ±30°).
    let ladder = Color32::from_white_alpha(120);
    for deg in [-30.0_f64, -20.0, -10.0, 10.0, 20.0, 30.0] {
        let k = deg.to_radians().sin();
        let pts: Vec<Pos2> = circle_front_points(up, k, 48)
            .iter()
            .map(|&(x, y)| center + vec2(x * radius, -y * radius))
            .collect();
        if pts.len() > 1 {
            painter.add(Shape::line(pts, Stroke::new(0.8, ladder)));
        }
    }

    // Compass letters on the horizon: N E S W from the level frame.
    let letters = [
        ("N", input.north),
        ("E", input.east),
        ("S", [-input.north[0], -input.north[1], -input.north[2]]),
        ("W", [-input.east[0], -input.east[1], -input.east[2]]),
    ];
    for (s, d) in letters {
        if norm(d) < 1.0e-6 {
            continue;
        }
        let mut v = d;
        if v[2] < 0.0 {
            let l = (v[0] * v[0] + v[1] * v[1]).sqrt().max(1.0e-9);
            v = [v[0] / l * 0.97, v[1] / l * 0.97, v[2]];
        } else {
            let l = norm(v);
            v = [v[0] / l * 0.93, v[1] / l * 0.93, v[2]];
        }
        painter.text(
            to_px(v),
            Align2::CENTER_CENTER,
            s,
            FontId::proportional(radius * 0.16),
            white,
        );
    }

    // Prograde (green ring) / retrograde (green cross) — surface frame.
    if let Some(pg) = input.prograde {
        let retro = [-pg[0], -pg[1], -pg[2]];
        for (d, is_pro) in [(pg, true), (retro, false)] {
            let mut v = d;
            if v[2] < 0.0 {
                // Far side — clamp to the rim.
                let l = (v[0] * v[0] + v[1] * v[1]).sqrt();
                if l < 1.0e-9 {
                    continue; // dead astern — under the reticle's opposite
                }
                v = [v[0] / l * 0.95, v[1] / l * 0.95, 0.0];
            }
            let p = to_px(v);
            let g = Color32::from_rgb(80, 220, 80);
            if is_pro {
                painter.circle_stroke(p, radius * 0.09, Stroke::new(1.4, g));
            } else {
                let e = radius * 0.07;
                painter.line_segment([p - vec2(e, e), p + vec2(e, e)], Stroke::new(1.4, g));
                painter.line_segment([p + vec2(-e, e), p + vec2(e, -e)], Stroke::new(1.4, g));
            }
        }
    }

    // Rim + fixed nose reticle (the ball rotates under it).
    painter.circle_stroke(center, radius, Stroke::new(2.0, mark));
    painter.circle_stroke(center, radius * 0.06, Stroke::new(1.6, mark));
    let w = radius * 0.35;
    painter.line_segment(
        [center + vec2(-w, 0.0), center + vec2(-radius * 0.09, 0.0)],
        Stroke::new(1.6, mark),
    );
    painter.line_segment(
        [center + vec2(radius * 0.09, 0.0), center + vec2(w, 0.0)],
        Stroke::new(1.6, mark),
    );
}

/// The heading tape — a scrolling compass strip under the ball:
/// cardinal letters plus 30° numerals, center caret marks the readout.
/// A NaN heading draws nothing.
pub fn heading_tape(painter: &egui::Painter, top_center: Pos2, width: f32, heading_deg: f64) {
    if !heading_deg.is_finite() {
        return;
    }
    let h = 16.0;
    let rect = egui::Rect::from_center_size(top_center + vec2(0.0, h * 0.5), vec2(width, h));
    painter.rect_filled(rect, 2.0, Color32::from_black_alpha(160));
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, Color32::from_gray(120)),
        egui::StrokeKind::Inside,
    );
    let clip = painter.with_clip_rect(rect);
    let px_per_deg = width / 90.0; // ±45° visible
    for deg in (0..360).step_by(15) {
        let d = deg as f64;
        let mut off = heading_deg - d;
        off -= (off / 360.0).round() * 360.0;
        let x = rect.center().x - (off as f32) * px_per_deg;
        let major = deg % 90 == 0;
        let labeled = deg % 30 == 0;
        let y0 = rect.top() + if major { 0.0 } else { 4.0 };
        clip.line_segment(
            [pos2(x, y0), pos2(x, rect.bottom())],
            Stroke::new(
                if major { 1.4 } else { 0.7 },
                Color32::from_gray(if major { 220 } else { 140 }),
            ),
        );
        if labeled {
            let text = match deg {
                0 => "N".to_string(),
                90 => "E".to_string(),
                180 => "S".to_string(),
                270 => "W".to_string(),
                _ => format!("{}", deg / 10),
            };
            clip.text(
                pos2(x, rect.top() + 2.0),
                Align2::CENTER_TOP,
                text,
                FontId::proportional(9.0),
                Color32::from_gray(220),
            );
        }
    }
    // Center caret.
    let cx = rect.center().x;
    painter.line_segment(
        [pos2(cx, rect.top()), pos2(cx, rect.bottom())],
        Stroke::new(1.8, Color32::from_rgb(255, 170, 40)),
    );
}
