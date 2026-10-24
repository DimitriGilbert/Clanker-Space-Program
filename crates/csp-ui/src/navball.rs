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
//! half projects to a half-ellipse arc and the rest wraps behind onto
//! the nadir half of the rim. The painted ground region is decomposed
//! into two convex fills — the nadir half-disc and the half-ellipse
//! cap — so no fill needs a concave tessellation (see
//! `ground_cap_geometry`).

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
    /// (No `heading_deg` field — the heading tape reads the HUD's own
    /// heading mirror via [`heading_tape`]'s parameter; the ball
    /// itself reads only these four staged directions.)
    pub prograde: Option<[f64; 3]>,
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
/// returning the front-hemisphere (`v_z > 0`) points as separate
/// polylines of screen offsets in units of ball radius (`(−vx, vy)`).
///
/// A small circle meets `v_z = 0` in at most two points, so the front
/// set is one contiguous arc — but that arc can straddle the t = 0/2π
/// parametrization seam and arrive as TWO runs. Flattening both into
/// one `Vec` made `Shape::line` join run one's rim-crossing endpoint
/// to run two's with a chord straight across the disc interior — a
/// fake ladder segment. Separate runs draw each piece correctly; they
/// meet at the shared seam point on the rim side.
fn circle_front_runs(u: V3, k: f64, samples: usize) -> Vec<Vec<(f32, f32)>> {
    let (a, b) = ortho_basis(u);
    let c = (1.0 - k * k).max(0.0).sqrt();
    let mut runs = Vec::new();
    let mut run: Vec<(f32, f32)> = Vec::new();
    for i in 0..=samples {
        let t = 2.0 * std::f64::consts::PI * (i as f64) / (samples as f64);
        let (ct, st) = (t.cos(), t.sin());
        let v = [
            k * u[0] + c * (a[0] * ct + b[0] * st),
            k * u[1] + c * (a[1] * ct + b[1] * st),
            k * u[2] + c * (a[2] * ct + b[2] * st),
        ];
        if v[2] > 0.0 {
            run.push((-v[0] as f32, v[1] as f32));
        } else if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs
}

/// The projected ground hemisphere as two convex fill polygons, in
/// unit-disc math coordinates (y-up; the painter maps `(x, y)` to
/// `center + (x·r, −y·r)`).
struct GroundCap {
    /// Diameter `±mp` plus the nadir half of the rim — always `ground`.
    half_disc: Vec<(f64, f64)>,
    /// The half-ellipse region between the diameter and the
    /// front-horizon arc — `ground` for `uz < 0`, a `sky` bite to
    /// paint back for `uz > 0`.
    cap: Vec<(f64, f64)>,
}

/// Compute the [`GroundCap`] decomposition for `up` — requires
/// `|up| > 0` and a nonzero screen projection `rho = √(ux² + uy²)`.
///
/// Orthographic front projection maps a direction `v` to `(−vx, vy)`.
/// The horizon `v·up = 0` projects to a half-ellipse whose major-axis
/// endpoints `±mp` sit on the rim; its back-hemisphere half clamps onto
/// the nadir half of the rim. The painted ground region is the
/// symmetric difference of the two pieces: for `uz < 0` (nose down)
/// the arc bulges onto the zenith side and the cap *adds* ground; for
/// `uz > 0` (nose up) the arc dips into the nadir half-disc and the cap
/// is the sky bite.
///
/// Both pieces are convex, so `Shape::convex_polygon` fills them
/// exactly — a single fan about the nadir projection was *not*
/// star-shaped at positive pitch (the sky side is a concave bite) and
/// overpainted sky as ground.
fn ground_cap_geometry(up: V3) -> GroundCap {
    let (ux, uy, uz) = (up[0], up[1], up[2]);
    let rho = (ux * ux + uy * uy).sqrt();
    // `m` points at the zenith projection; `mp` completes the ellipse
    // frame (the rim endpoints of the front horizon are `±mp`).
    let m = (-ux / rho, uy / rho);
    let mp = (-m.1, m.0);
    // Front horizon: `p = m·(uz·cos θ) + mp·sin θ` for θ ∈ [π/2, 3π/2]
    // — the half-ellipse from +mp to −mp through the arc dip (nadir
    // side for `uz > 0`, zenith side for `uz < 0`).
    let mut cap = Vec::with_capacity(41);
    for i in 0..=40 {
        let t = std::f64::consts::PI * (0.5 + i as f64 / 40.0);
        let (ct, st) = (t.cos(), t.sin());
        cap.push((m.0 * uz * ct + mp.0 * st, m.1 * uz * ct + mp.1 * st));
    }
    // Nadir half-disc: rim arc α ∈ [α_n − π/2, α_n + π/2] centered on
    // the nadir rim angle, running from +mp to −mp through the nadir.
    let a_nadir = (-uy).atan2(ux);
    let mut half_disc = Vec::with_capacity(41);
    for i in 0..=40 {
        let a = a_nadir - std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * (i as f64 / 40.0);
        half_disc.push((a.cos(), a.sin()));
    }
    GroundCap { half_disc, cap }
}

/// Horizon-compass letter anchor: front directions sit at 0.93 radius,
/// far-side directions clamp to the rim at 0.97 — `None` for a
/// degenerate direction or one dead along the nose axis (either way
/// it would paint under the reticle at ball center: astern has no xy
/// angle to clamp to, and dead ahead the 0.93·sin foreshortening drops
/// the letter onto the same spot).
fn letter_anchor(d: V3) -> Option<V3> {
    if norm(d) < 1.0e-6 {
        return None;
    }
    // Dead ahead or dead astern — no xy angle to anchor at, and the
    // projected point sits on the reticle. Skip it like the prograde
    // marker does.
    let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
    if l < 1.0e-9 {
        return None;
    }
    if d[2] < 0.0 {
        // Far side — clamp to the rim.
        Some([d[0] / l * 0.97, d[1] / l * 0.97, d[2]])
    } else {
        let n = norm(d);
        Some([d[0] / n * 0.93, d[1] / n * 0.93, d[2]])
    }
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
        // Ground hemisphere = nadir half-disc ⊕ front-horizon
        // half-ellipse cap (see `ground_cap_geometry`): `uz < 0` puts
        // the cap on the zenith side so it *adds* ground; `uz > 0`
        // bites into the nadir half-disc and must be painted back to
        // `sky`. Both fills are convex — the old single fan about the
        // nadir projection painted sky as ground at positive pitch.
        let cap_fill = if uz < 0.0 { ground } else { sky };
        let GroundCap { half_disc, cap } = ground_cap_geometry(up);
        let to_px_pts = |pts: &[(f64, f64)]| -> Vec<Pos2> {
            pts.iter()
                .map(|&(x, y)| center + vec2(x as f32 * radius, -y as f32 * radius))
                .collect()
        };
        painter.add(Shape::convex_polygon(
            to_px_pts(&half_disc),
            ground,
            Stroke::NONE,
        ));
        painter.add(Shape::convex_polygon(
            to_px_pts(&cap),
            cap_fill,
            Stroke::NONE,
        ));
        // The horizon line itself — the half-ellipse arc.
        painter.add(Shape::line(to_px_pts(&cap), Stroke::new(1.5, white)));
    }

    // Pitch ladder — small circles v·û = sin(±10°, ±20°, ±30°). Each
    // circle's front arc is stroked per run: one wrapped over the
    // t = 0/2π seam comes back as two pieces, and joining them would
    // paint a rim-to-rim chord across the disc (see
    // `circle_front_runs`).
    let ladder = Color32::from_white_alpha(120);
    for deg in [-30.0_f64, -20.0, -10.0, 10.0, 20.0, 30.0] {
        let k = deg.to_radians().sin();
        for run in circle_front_runs(up, k, 48) {
            let pts: Vec<Pos2> = run
                .iter()
                .map(|&(x, y)| center + vec2(x * radius, -y * radius))
                .collect();
            if pts.len() > 1 {
                painter.add(Shape::line(pts, Stroke::new(0.8, ladder)));
            }
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
        let Some(v) = letter_anchor(d) else {
            continue;
        };
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Even-odd point-in-polygon (ray cast along +x) — the test oracle
    /// for which disc pixels a fill covers.
    fn in_poly(p: (f64, f64), poly: &[(f64, f64)]) -> bool {
        let mut inside = false;
        let n = poly.len();
        for i in 0..n {
            let (x0, y0) = poly[i];
            let (x1, y1) = poly[(i + 1) % n];
            if (y0 > p.1) != (y1 > p.1) {
                let x_at = x0 + (p.1 - y0) * (x1 - x0) / (y1 - y0);
                if p.0 < x_at {
                    inside = !inside;
                }
            }
        }
        inside
    }

    /// What the paint pass puts down: `ground` = half-disc XOR cap
    /// (the cap repaints sky inside the nadir half-disc for `uz > 0`,
    /// adds disjoint ground on the zenith side for `uz < 0`).
    fn painted_ground(up: V3, s: (f64, f64)) -> bool {
        let GroundCap { half_disc, cap } = ground_cap_geometry(up);
        in_poly(s, &half_disc) != in_poly(s, &cap)
    }

    /// Analytic truth: a disc point `s` (math coords) is the front
    /// projection of `v = (−sx, sy, +√(1−|s|²))`, which is ground iff
    /// `v·up < 0`. Back-hemisphere ground clamps onto rim points only
    /// (measure zero), so this is the exact intended region.
    fn true_ground(up: V3, s: (f64, f64)) -> bool {
        let vz = (1.0 - s.0 * s.0 - s.1 * s.1).max(0.0).sqrt();
        -s.0 * up[0] + s.1 * up[1] + vz * up[2] < 0.0
    }

    /// R18-F1 regression: the ground fill must match the true horizon
    /// split at every pitch — the old star fan overpainted ~9–47% of
    /// the disc as ground for +15°…+85°. The only legitimate mismatch
    /// is the chord-vs-arc sliver from 40-segment arcs (≪1%).
    #[test]
    fn ground_cap_matches_true_horizon_split() {
        // (up vector, label) — level pitch ladder plus a rolled case.
        let ups: Vec<(V3, String)> = [-80.0_f64, -45.0, -15.0, 15.0, 30.0, 45.0, 60.0, 85.0]
            .iter()
            .map(|deg| {
                let p = deg.to_radians();
                ([0.0, p.cos(), p.sin()], format!("pitch {deg}"))
            })
            .chain([(
                [0.866 * 0.643, 0.866 * 0.766, 0.5],
                "rolled pitch+30/roll+40".to_string(),
            )])
            .collect();
        for (up, label) in ups {
            let (mut over, mut missed, mut total) = (0_u32, 0_u32, 0_u32);
            for ix in -60..=60 {
                for iy in -60..=60 {
                    let s = (ix as f64 / 62.0, iy as f64 / 62.0);
                    if s.0 * s.0 + s.1 * s.1 > 1.0 {
                        continue;
                    }
                    total += 1;
                    let painted = painted_ground(up, s);
                    let truth = true_ground(up, s);
                    if painted && !truth {
                        over += 1; // sky painted ground — the R18-F1 defect
                    }
                    if !painted && truth {
                        missed += 1; // ground left sky
                    }
                }
            }
            let over_frac = over as f64 / total as f64;
            let missed_frac = missed as f64 / total as f64;
            assert!(
                over_frac < 0.01,
                "{label} up={up:?}: {over_frac:.3} of disc sky painted ground"
            );
            assert!(
                missed_frac < 0.01,
                "{label} up={up:?}: {missed_frac:.3} of disc ground missed"
            );
        }
    }

    /// The nadir-half-disc + cap decomposition really is an XOR: for
    /// `uz > 0` the cap polygon sits inside the half-disc (a sky bite),
    /// for `uz < 0` it is disjoint (zenith-side ground).
    #[test]
    fn cap_side_follows_uz_sign() {
        let up_down = [0.0, 0.866, -0.5]; // nose down
        let GroundCap { half_disc, cap } = ground_cap_geometry(up_down);
        // Cap midpoint (θ = π) should land on the zenith side, outside
        // the nadir half-disc.
        let mid = cap[20];
        assert!(mid.1 > 0.0 && !in_poly(mid, &half_disc));
        let up_up = [0.0, 0.866, 0.5]; // nose up
        let GroundCap { half_disc, cap } = ground_cap_geometry(up_up);
        let mid = cap[20];
        assert!(mid.1 < 0.0 && in_poly(mid, &half_disc));
    }

    /// Dead-astern AND dead-ahead compass letters must be skipped, not
    /// drawn at the ball center over the reticle (the prograde
    /// marker's own rule: astern has no rim angle, ahead foreshortens
    /// onto the reticle).
    #[test]
    fn dead_ahead_and_astern_letters_are_skipped() {
        assert_eq!(letter_anchor([0.0, 0.0, -1.0]), None);
        assert_eq!(letter_anchor([1.0e-12, 0.0, -1.0]), None);
        assert_eq!(letter_anchor([0.0, 0.0, 1.0]), None);
        assert_eq!(letter_anchor([0.0, 1.0e-12, 1.0]), None);
        assert_eq!(letter_anchor([0.0, 0.0, 0.0]), None);
        // Far-side-but-not-astern still clamps to the rim.
        let a = letter_anchor([0.0, -0.5, -0.5]).unwrap();
        assert!(((a[0] * a[0] + a[1] * a[1]).sqrt() - 0.97).abs() < 1.0e-9);
        // Front side sits just inside the ball.
        let a = letter_anchor([0.0, 0.5, 0.5]).unwrap();
        let want = 0.93 * 0.5_f64.hypot(0.5);
        assert!(((a[0] * a[0] + a[1] * a[1]).sqrt() - want).abs() < 1.0e-9);
    }

    /// Pitch-ladder regression: a pitch circle whose front arc wraps
    /// the t = 0/2π seam must come back as two runs — flattened into
    /// one `Vec`, `Shape::line` drew a rim-to-rim chord across the
    /// disc interior (a fake ladder segment between the two rim
    /// crossings).
    #[test]
    fn front_arc_wrapping_the_seam_splits_into_runs() {
        // Nose up 45°, the +30° circle: front for sin t < 0.577 — the
        // interval (π − 0.615, 2π] ∪ [0, 0.615) wraps the seam.
        let p = 45.0_f64.to_radians();
        let up = [0.0, p.cos(), p.sin()];
        let runs = circle_front_runs(up, 30.0_f64.to_radians().sin(), 48);
        assert_eq!(runs.len(), 2, "wrapped front arc must yield two runs");
        // Every in-run step is a real arc increment (≤ 2π·c/48 ≈ 0.14
        // at c ≤ 1); the old flat Vec's seam join was a ~1.4-radius
        // chord between the two rim crossings (screen (±0.707, 0.707)).
        for run in &runs {
            for w in run.windows(2) {
                let step = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
                assert!(step < 0.2, "ghost-chord step {step} inside a run");
            }
        }
        // A circle wholly on the front hemisphere stays one run.
        let p = 85.0_f64.to_radians();
        let up = [0.0, p.cos(), p.sin()];
        let runs = circle_front_runs(up, 30.0_f64.to_radians().sin(), 48);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 49);
    }
}
