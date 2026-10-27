//! Procedural rocket-part authoring — the part-model source that
//! replaced the Kenney Space Kit GLBs. The kit models were stylized
//! cartoon primitives (~50–340 tris); no amount of subdivision or
//! lighting makes a cartoon capsule read as aerospace hardware. These
//! builders author the real silhouettes instead:
//!
//! - **engine_a / engine_b** — powerhead cylinder, gimbal block,
//!   real Rao-flared bell nozzle as a *single continuous sheet*
//!   (outer wall, throat fold, inner wall sealed at the lip) with
//!   corrugated cooling channels, a copper-alloy liner grading to
//!   char, plus turbopump feed plumbing and gimbal actuators.
//! - **pod** — Apollo-class capsule: ablative heat-shield dome, rim
//!   ring, conical crew cabin with recessed glass windows in framed
//!   bays, rounded shoulder, docking tunnel + probe, RCS nubs.
//! - **tank** — barrel with ellipsoidal domes and real weld rings,
//!   plus a painted marking band.
//! - **nosecone** — tangent-ogive profile with a machined tip and
//!   base flange.
//! - **booster** — SRB: barrel, nose frustum, segment rings, skirt
//!   flare, recessed nozzle.
//! - **decoupler** — machined ring with a recessed pyro seam and
//!   bolt bumps; **adapter** — corrugated interstage with end
//!   flanges.
//! - **battery** — superellipse-rounded pack with ribs and
//!   terminals.
//! - **science** — instrument canister with a panel door, domed
//!   lid, and a paraboloid antenna dish.
//! - **fins** — a three-fin can: swept trapezoid fins on a root hub.
//!
//! Everything emits the bake's working mesh (`pos` + colored `Face`s)
//! and finishes through the same `edge_shade`/`finish` pass the kit
//! loader used — position-welded, wedge-normalled, cavity-AO baked —
//! so the `.csar` format and the `csp-view` decoder are untouched.
//! Authored at real part dimensions (m, z-up stack axis);
//! `emit_model` affine-fits each model into its collider AABB.
//!
//! All transcendentals here go through the `libm` crate, never
//! `f64::sin`/`cos`/`powf`/`sqrt` etc: `bake_art --check` byte-compares
//! a fresh bake against the committed packs on CI-HOST, and std
//! transcendentals resolve to the *host's* libm, whose ulps differ
//! across glibc versions. libm is pure Rust and identical everywhere
//! (the same rule the sim itself follows).

use std::collections::BTreeMap;

use crate::{Face, MeshEntry, cross, dot, edge_shade, finish, sub, unit};

const TAU: f64 = core::f64::consts::TAU;
const PI: f64 = core::f64::consts::PI;

// ---------------------------------------------------------------------------
// Material palette — sRGB face colors; `mat_params` in bake_art maps each
// to its spec pair. Chosen so the classification lands on the intended
// class (dark metal / glass / ablative / accent / painted hull / brushed).
// ---------------------------------------------------------------------------

/// Painted white-grey hull.
const HULL: [u8; 4] = [213, 217, 223, 255];
/// Hull panel variant — a fraction darker, for panel banding.
const HULL_LO: [u8; 4] = [190, 196, 204, 255];
/// Brushed / machined aluminum.
const METAL: [u8; 4] = [150, 157, 166, 255];
/// Machined dark metal (throat hardware, rings, skirt).
const METAL_DARK: [u8; 4] = [86, 92, 102, 255];
/// Gunmetal nozzle exterior.
const NOZZLE: [u8; 4] = [70, 74, 82, 255];
/// Darker nozzle extension skirt.
const NOZZLE_LO: [u8; 4] = [52, 55, 62, 255];
/// Copper-alloy bell liner — warm and glossy.
const COPPER: [u8; 4] = [122, 70, 40, 255];
/// Charred deep bell interior / ablative surfaces (matte).
const CHAR: [u8; 4] = [54, 44, 38, 255];
/// Dark glossy glazing — blue-dominant so mat_params takes the glass path.
const GLASS: [u8; 4] = [24, 36, 58, 255];
/// Window-frame / fitting metal.
const FRAME: [u8; 4] = [178, 184, 192, 255];
/// International-orange accent paint.
const ACCENT: [u8; 4] = [206, 96, 36, 255];
/// Separator / pyro band.
const SEP: [u8; 4] = [64, 66, 72, 255];
/// Dark marking band / panel interior.
const LABEL: [u8; 4] = [46, 48, 54, 255];

// ---------------------------------------------------------------------------
// Working-mesh builder — positions weld on a 1e-6 grid like the GLB path;
// every face carries its sRGB material color.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct B {
    pos: Vec<[f64; 3]>,
    faces: Vec<Face>,
    map: BTreeMap<[i64; 3], u32>,
}

impl B {
    fn v(&mut self, p: [f64; 3]) -> u32 {
        let k = [
            (p[0] * 1e6).round() as i64,
            (p[1] * 1e6).round() as i64,
            (p[2] * 1e6).round() as i64,
        ];
        *self.map.entry(k).or_insert_with(|| {
            self.pos.push(p);
            (self.pos.len() - 1) as u32
        })
    }
    /// Emit one triangle — degenerates are dropped.
    fn tri(&mut self, a: [f64; 3], b: [f64; 3], c: [f64; 3], col: [u8; 4]) {
        let e = cross(sub(b, a), sub(c, a));
        if dot(e, e) <= 1e-24 {
            return;
        }
        let (va, vb, vc) = (self.v(a), self.v(b), self.v(c));
        if va == vb || vb == vc || vc == va {
            return;
        }
        self.faces.push(Face {
            v: [va, vb, vc],
            col,
        });
    }
    /// Emit a triangle wound so its geometric normal sides with `out`
    /// (the sheet's intended visible side).
    fn tri_out(&mut self, a: [f64; 3], b: [f64; 3], c: [f64; 3], out: [f64; 3], col: [u8; 4]) {
        if dot(cross(sub(b, a), sub(c, a)), out) < 0.0 {
            self.tri(a, c, b, col);
        } else {
            self.tri(a, b, c, col);
        }
    }
    /// Quad via `tri_out`.
    fn quad_out(
        &mut self,
        a: [f64; 3],
        b: [f64; 3],
        c: [f64; 3],
        d: [f64; 3],
        out: [f64; 3],
        col: [u8; 4],
    ) {
        self.tri_out(a, b, c, out, col);
        self.tri_out(a, c, d, out, col);
    }
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/// Solid of revolution of `profile` (`(z, radius)` stations) at `segs`
/// angular facets.
///
/// Orientation is per-quad, derived from the profile tangent: the
/// sheet's visible side is the CCW rotation of the traversal tangent —
/// `out = (Δz·cosθ, Δz·sinθ, −Δr)` — so a profile traversed with
/// ascending z faces +r (a normal outside surface), and one traversed
/// with descending z faces −r (an interior wall). Folding the profile
/// back over a lip or throat therefore produces a correctly oriented
/// double-wall sheet from a single station list.
///
/// `rmod(st, seg)` scales each ring point's radius (corrugation,
/// superellipse cross-sections); `col(st, seg)` colors each side quad
/// by its lower station index and sector. Apex stations (`r ≤ 1e-9`)
/// collapse to an axis fan; `caps` fan-closes the bottom/top ends
/// (cap colors come from the adjacent side quad).
fn lathe(
    b: &mut B,
    profile: &[(f64, f64)],
    segs: usize,
    rmod: impl Fn(usize, usize) -> f64,
    col: impl Fn(usize, usize) -> [u8; 4],
    caps: (bool, bool),
) {
    if profile.len() < 2 || segs < 3 {
        return;
    }
    let n = profile.len();
    let ring = |i: usize, j: usize| -> [f64; 3] {
        let (z, r) = profile[i];
        let a = TAU * j as f64 / segs as f64;
        let rr = r * rmod(i, j);
        [rr * libm::cos(a), rr * libm::sin(a), z]
    };
    let axis = |i: usize| -> [f64; 3] { [0.0, 0.0, profile[i].0] };
    // Intended visible-side direction for strip i at angular mid θ:
    // CCW rotation of the (z,r) tangent, revolved.
    let out_dir = |i: usize, j: usize| -> [f64; 3] {
        let (dz, dr) = (
            profile[i + 1].0 - profile[i].0,
            profile[i + 1].1 - profile[i].1,
        );
        let am = TAU * (j as f64 + 0.5) / segs as f64;
        unit([dz * libm::cos(am), dz * libm::sin(am), -dr])
    };
    for i in 0..n - 1 {
        let lo = profile[i].1 <= 1e-9;
        let hi = profile[i + 1].1 <= 1e-9;
        for j in 0..segs {
            let j1 = (j + 1) % segs;
            let c = col(i, j);
            let out = out_dir(i, j);
            match (lo, hi) {
                (false, false) => b.quad_out(
                    ring(i, j),
                    ring(i, j1),
                    ring(i + 1, j1),
                    ring(i + 1, j),
                    out,
                    c,
                ),
                (true, false) => b.tri_out(axis(i), ring(i + 1, j1), ring(i + 1, j), out, c),
                (false, true) => b.tri_out(ring(i, j), ring(i, j1), axis(i + 1), out, c),
                (true, true) => {}
            }
        }
    }
    // End caps — flat fans to the axis point, only where the end ring
    // is not itself an apex.
    if caps.0 && profile[0].1 > 1e-9 {
        for j in 0..segs {
            let j1 = (j + 1) % segs;
            b.tri_out(
                axis(0),
                ring(0, j1),
                ring(0, j),
                [0.0, 0.0, -1.0],
                col(0, j),
            );
        }
    }
    if caps.1 && profile[n - 1].1 > 1e-9 {
        for j in 0..segs {
            let j1 = (j + 1) % segs;
            b.tri_out(
                axis(n - 1),
                ring(n - 1, j),
                ring(n - 1, j1),
                [0.0, 0.0, 1.0],
                col(n - 2, j),
            );
        }
    }
}

/// Convenience wrapper — uniform radius + color.
fn shell(b: &mut B, profile: &[(f64, f64)], segs: usize, col: [u8; 4], caps: (bool, bool)) {
    lathe(b, profile, segs, |_, _| 1.0, move |_, _| col, caps);
}

/// A lathe about an off-axis line parallel to z — studs and terminals.
fn stud(b: &mut B, at: [f64; 2], z0: f64, z1: f64, r: f64, segs: usize, col: [u8; 4]) {
    let off = |p: [f64; 3]| [p[0] + at[0], p[1] + at[1], p[2]];
    let mut tmp = B::default();
    shell(
        &mut tmp,
        &[(z0, r), (z1, r), (z1, 0.0)],
        segs,
        col,
        (true, false),
    );
    for f in tmp.faces {
        let p = |i: u32| off(tmp.pos[i as usize]);
        b.tri(p(f.v[0]), p(f.v[1]), p(f.v[2]), f.col);
    }
}

/// Sweep a circular cross-section along `path` (parallel-transport
/// frames), ends capped. Plumbing, actuators, antenna stems.
fn tube(b: &mut B, path: &[[f64; 3]], r: f64, segs: usize, col: [u8; 4]) {
    if path.len() < 2 || segs < 3 {
        return;
    }
    let m = path.len();
    let tangent = |i: usize| -> [f64; 3] {
        let (a, c) = if i == 0 {
            (path[0], path[1])
        } else if i + 1 == m {
            (path[m - 2], path[m - 1])
        } else {
            (path[i - 1], path[i + 1])
        };
        unit(sub(c, a))
    };
    // First frame: any perpendicular to t0, then parallel-transport.
    let t0 = tangent(0);
    let seed = if t0[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let mut u = unit(cross(t0, seed));
    let mut frames: Vec<([f64; 3], [f64; 3])> = Vec::with_capacity(m);
    for i in 0..m {
        let t = tangent(i);
        let d = dot(u, t);
        u = unit([u[0] - d * t[0], u[1] - d * t[1], u[2] - d * t[2]]);
        frames.push((u, cross(t, u)));
    }
    let ring = |i: usize, j: usize| -> [f64; 3] {
        let a = TAU * j as f64 / segs as f64;
        let (u, v) = frames[i];
        let (ca, sa) = (libm::cos(a), libm::sin(a));
        [
            path[i][0] + r * (ca * u[0] + sa * v[0]),
            path[i][1] + r * (ca * u[1] + sa * v[1]),
            path[i][2] + r * (ca * u[2] + sa * v[2]),
        ]
    };
    for i in 0..m - 1 {
        for j in 0..segs {
            let j1 = (j + 1) % segs;
            let mid = crate::scale(crate::add(ring(i, j), ring(i + 1, j1)), 0.5);
            let axm = crate::scale(crate::add(path[i], path[i + 1]), 0.5);
            let out = sub(mid, axm);
            b.quad_out(
                ring(i, j),
                ring(i, j1),
                ring(i + 1, j1),
                ring(i + 1, j),
                out,
                col,
            );
        }
    }
    // End caps.
    for j in 0..segs {
        let j1 = (j + 1) % segs;
        b.tri_out(
            path[0],
            ring(0, j1),
            ring(0, j),
            crate::scale(tangent(0), -1.0),
            col,
        );
        b.tri_out(
            path[m - 1],
            ring(m - 1, j),
            ring(m - 1, j1),
            tangent(m - 1),
            col,
        );
    }
}

/// Axis-aligned box — six outward faces (RCS nubs, small hardware).
fn cuboid(b: &mut B, lo: [f64; 3], hi: [f64; 3], col: [u8; 4]) {
    let c = [
        [lo[0], lo[1], lo[2]],
        [hi[0], lo[1], lo[2]],
        [hi[0], hi[1], lo[2]],
        [lo[0], hi[1], lo[2]],
        [lo[0], lo[1], hi[2]],
        [hi[0], lo[1], hi[2]],
        [lo[0], hi[1], hi[2]],
        [hi[0], hi[1], hi[2]],
    ];
    const OUT: [[f64; 3]; 6] = [
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
    ];
    const FACES: [[usize; 4]; 6] = [
        [0, 1, 2, 3],
        [4, 5, 6, 7],
        [0, 1, 5, 4],
        [2, 3, 7, 6],
        [0, 3, 7, 4],
        [1, 2, 6, 5],
    ];
    for (k, f) in FACES.iter().enumerate() {
        b.quad_out(c[f[0]], c[f[1]], c[f[2]], c[f[3]], OUT[k], col);
    }
}

/// Extrude a 2D `(x, z)` polygon into a y0..y1 plate, then rotate the
/// whole solid `rot` radians about z. `side` colors the extruded edge
/// walls, `cap` the two flat faces. Fins, winglets, mounting tabs.
fn prism(b: &mut B, poly: &[(f64, f64)], y0: f64, y1: f64, rot: f64, side: [u8; 4], cap: [u8; 4]) {
    if poly.len() < 3 {
        return;
    }
    let (cr, sr) = (libm::cos(rot), libm::sin(rot));
    let rz = |x: f64, y: f64, z: f64| -> [f64; 3] { [x * cr - y * sr, x * sr + y * cr, z] };
    // Signed area → polygon winding in the (x,z) plane.
    let mut area = 0.0;
    for i in 0..poly.len() {
        let (x0, z0) = poly[i];
        let (x1, z1) = poly[(i + 1) % poly.len()];
        area += x0 * z1 - x1 * z0;
    }
    let poly: Vec<(f64, f64)> = if area < 0.0 {
        poly.iter().rev().copied().collect()
    } else {
        poly.to_vec()
    };
    let cen = poly
        .iter()
        .fold((0.0, 0.0), |(ax, az), &(x, z)| (ax + x, az + z));
    let cen = (cen.0 / poly.len() as f64, cen.1 / poly.len() as f64);
    // Edge walls — each edge's outward (x,z) normal, picked by the
    // centroid test, then rotated with the solid.
    for i in 0..poly.len() {
        let (x0, z0) = poly[i];
        let (x1, z1) = poly[(i + 1) % poly.len()];
        let (ex, ez) = (x1 - x0, z1 - z0);
        let (mx, mz) = ((x0 + x1) * 0.5 - cen.0, (z0 + z1) * 0.5 - cen.1);
        let (nx, nz) = if ex * mx + ez * mz < 0.0 {
            (ez, -ex)
        } else {
            (-ez, ex)
        };
        let out = {
            let n = unit([nx, 0.0, nz]);
            [n[0] * cr - n[1] * sr, n[0] * sr + n[1] * cr, n[2]]
        };
        b.quad_out(
            rz(x0, y0, z0),
            rz(x1, y0, z1),
            rz(x1, y1, z1),
            rz(x0, y1, z0),
            out,
            side,
        );
    }
    // Flat faces — fans to the polygon centroid at each y.
    for &(y, dy) in &[(y0, -1.0f64), (y1, 1.0)] {
        let c = rz(cen.0, y, cen.1);
        let out = [-dy * sr, dy * cr, 0.0];
        for i in 0..poly.len() {
            let (x0, z0) = poly[i];
            let (x1, z1) = poly[(i + 1) % poly.len()];
            b.tri_out(c, rz(x0, y, z0), rz(x1, y, z1), out, cap);
        }
    }
}

/// Sphere via lathe of a semicircle (gimbal blocks, dome tips).
fn sphere(b: &mut B, c: [f64; 3], r: f64, segs: usize, col: [u8; 4]) {
    let n = 10usize;
    let prof: Vec<(f64, f64)> = (0..=n)
        .map(|k| {
            let a = -PI / 2.0 + PI * k as f64 / n as f64;
            (c[2] + r * libm::sin(a), libm::cos(a).max(0.0))
        })
        .collect();
    shell(b, &prof, segs, col, (false, false));
}

// ---------------------------------------------------------------------------
// The parts
// ---------------------------------------------------------------------------

/// Run the standard finish: edge shading then wedge normals + AO.
fn done(b: B) -> MeshEntry {
    let mut tint = vec![1.0f32; b.pos.len()];
    edge_shade(&b.pos, &b.faces, &mut tint);
    finish(&b.pos, &b.faces, &tint)
}

/// Shared bell-nozzle build: a single sheet — outer wall up, throat
/// fold, inner wall down — sealed where the final station repeats the
/// first (the lip ring welds shut). `corr_amp`/`corr_lobes` fade
/// cooling-channel ribs into the outer wall; the liner colors
/// copper→char; `skirt_z` splits the exterior into a darker extension
/// band below.
#[allow(clippy::too_many_arguments)]
fn bell_nozzle(
    b: &mut B,
    segs: usize,
    z_throat: f64,
    r_throat: f64,
    z_lip: f64,
    r_exit: f64,
    corr_amp: f64,
    corr_lobes: usize,
    ext_color: [u8; 4],
    skirt_z: f64,
) {
    const N: usize = 16;
    // Rao-class expansion: fast growth just past the throat, relaxing
    // toward the lip (t: 0 = lip, 1 = throat).
    let curve = |t: f64| libm::pow(t, 2.4);
    let wall = |t: f64, inset: f64| -> (f64, f64) {
        (
            z_lip + inset + t * (z_throat - z_lip - inset),
            (r_exit - inset) - (r_exit - inset - (r_throat - 2.0 * inset)) * curve(t),
        )
    };
    let mut prof: Vec<(f64, f64)> = (0..=N).map(|k| wall(k as f64 / N as f64, 0.0)).collect();
    // Throat fold, then the inner wall back down (thinner sheet), then
    // the closure station = prof[0]'s position (the weld seals the lip).
    prof.push((z_throat + 0.015, r_throat - 0.006));
    for k in (0..=N).rev() {
        prof.push(wall(k as f64 / N as f64, 0.010));
    }
    prof.push((z_lip, r_exit));
    let last = prof.len() - 1;
    lathe(
        b,
        &prof,
        segs,
        |i, j| {
            // Corrugated cooling channels on the outer wall, fading
            // into the throat; the closure ring repeats station 0's
            // modulation so the sheet welds shut.
            let outer = i <= N || i == last;
            if outer && corr_amp > 0.0 {
                let t = if i <= N { i as f64 / N as f64 } else { 0.0 };
                let a = TAU * corr_lobes as f64 * j as f64 / segs as f64;
                1.0 + corr_amp * (0.35 + 0.65 * (1.0 - t)) * (0.5 + 0.5 * libm::cos(a))
            } else {
                1.0
            }
        },
        |i, _| {
            if i <= N {
                if prof[i].0 < skirt_z {
                    NOZZLE_LO
                } else {
                    ext_color
                }
            } else if i == N + 1 {
                COPPER // throat fold glints
            } else {
                // Inner wall: char-stained at the lip end.
                let u = (i - N - 1) as f64 / N as f64;
                if u > 0.7 { CHAR } else { COPPER }
            }
        },
        (false, false),
    );
}

/// `engine_a` — the big first-stage bell (sparc class). Powerhead with
/// machined bands, gimbal block + actuators, corrugated regen bell
/// with a copper liner, twin turbopump feed lines. z ∈ [−0.9, 0.9].
fn engine_a() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    // Powerhead / mount cylinder + thermal ring + flat top.
    lathe(
        &mut b,
        &[
            (0.50, 0.240),
            (0.55, 0.280),
            (0.60, 0.300),
            (0.64, 0.300),
            (0.66, 0.288), // machined groove
            (0.80, 0.288),
            (0.82, 0.315), // heat-shield ring
            (0.88, 0.315),
            (0.90, 0.300),
            (0.90, 0.0), // flat top
        ],
        S,
        |_, _| 1.0,
        |i, _| {
            if i >= 6 {
                METAL_DARK
            } else if i == 4 {
                FRAME
            } else {
                METAL
            }
        },
        (false, false),
    );
    // Thrust chamber — connects the powerhead to the bell's throat.
    shell(
        &mut b,
        &[(0.28, 0.180), (0.34, 0.210), (0.42, 0.250), (0.50, 0.265)],
        S,
        METAL_DARK,
        (false, false),
    );
    // Gimbal block + four actuators.
    sphere(&mut b, [0.0, 0.0, 0.48], 0.085, 24, METAL_DARK);
    for s in [-1.0f64, 1.0] {
        tube(
            &mut b,
            &[[0.21 * s, 0.0, 0.64], [0.11 * s, 0.0, 0.47]],
            0.024,
            10,
            METAL_DARK,
        );
        tube(
            &mut b,
            &[[0.0, 0.21 * s, 0.64], [0.0, 0.11 * s, 0.47]],
            0.024,
            10,
            METAL_DARK,
        );
    }
    // Turbopump feed lines hugging the thrust chamber.
    tube(
        &mut b,
        &[
            [0.26, 0.10, 0.60],
            [0.29, 0.16, 0.42],
            [0.25, 0.20, 0.28],
            [0.17, 0.15, 0.20],
        ],
        0.026,
        10,
        METAL_DARK,
    );
    tube(
        &mut b,
        &[
            [-0.24, -0.12, 0.58],
            [-0.27, -0.18, 0.40],
            [-0.21, -0.22, 0.26],
            [-0.14, -0.16, 0.18],
        ],
        0.022,
        10,
        METAL_DARK,
    );
    // The bell — corrugated regen jacket over a copper liner.
    bell_nozzle(
        &mut b, S, 0.28, 0.165, -0.90, 0.585, 0.022, 28, NOZZLE, -0.55,
    );
    done(b)
}

/// `engine_b` — the smaller engine (ember class): slimmer two-piece
/// nozzle — machined upper wall, darker extension skirt joined by a
/// flange ring — one feed line. z ∈ [−0.7, 0.7].
fn engine_b() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    lathe(
        &mut b,
        &[
            (0.40, 0.190),
            (0.46, 0.240),
            (0.52, 0.258),
            (0.62, 0.258),
            (0.64, 0.243),
            (0.68, 0.243),
            (0.70, 0.228),
            (0.70, 0.0),
        ],
        S,
        |_, _| 1.0,
        |i, _| if i >= 5 { METAL_DARK } else { METAL },
        (false, false),
    );
    shell(
        &mut b,
        &[(0.20, 0.135), (0.28, 0.165), (0.36, 0.200), (0.40, 0.210)],
        S,
        METAL_DARK,
        (false, false),
    );
    sphere(&mut b, [0.0, 0.0, 0.38], 0.068, 24, METAL_DARK);
    for s in [-1.0f64, 1.0] {
        tube(
            &mut b,
            &[[0.17 * s, 0.0, 0.50], [0.09 * s, 0.0, 0.38]],
            0.019,
            10,
            METAL_DARK,
        );
    }
    tube(
        &mut b,
        &[[0.23, 0.08, 0.48], [0.24, 0.13, 0.32], [0.17, 0.14, 0.18]],
        0.019,
        10,
        METAL_DARK,
    );
    // Slimmer vacuum-class bell — machined upper wall, darker
    // extension skirt below z = −0.20.
    bell_nozzle(
        &mut b, S, 0.20, 0.120, -0.70, 0.500, 0.0, 0, METAL_DARK, -0.20,
    );
    // Extension flange ring at the skirt joint — a real bolted bead.
    shell(
        &mut b,
        &[
            (-0.215, 0.418),
            (-0.218, 0.445),
            (-0.192, 0.445),
            (-0.190, 0.418),
        ],
        S,
        METAL_DARK,
        (false, false),
    );
    done(b)
}

/// `pod` — Apollo-class command capsule. z ∈ [−0.5, 0.5].
fn pod() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    // Ablative heat shield — a shallow dome on the −z face.
    shell(
        &mut b,
        &[
            (-0.46, 0.0),
            (-0.472, 0.22),
            (-0.492, 0.42),
            (-0.505, 0.56),
            (-0.505, 0.585),
        ],
        S,
        CHAR,
        (false, false),
    );
    // Shield-to-hull rim, then the crew cabin cone.
    shell(
        &mut b,
        &[(-0.505, 0.585), (-0.460, 0.600), (-0.435, 0.602)],
        S,
        METAL_DARK,
        (false, false),
    );
    const NC: usize = 14;
    let cone: Vec<(f64, f64)> = (0..=NC)
        .map(|k| {
            let t = k as f64 / NC as f64;
            (
                -0.435 + t * 0.735,
                0.602 + (0.330 - 0.602) * (t + 0.045 * libm::sin(PI * t)),
            )
        })
        .collect();
    // Two recessed window bays on the upper flank (segs 6 and 58 —
    // ±34°) plus a narrow center pane — glass set a hair under the
    // frame border.
    let seg_dist = |j: usize, c: i64| -> i64 {
        let d = (j as i64 - c).rem_euclid(S as i64);
        d.min(S as i64 - d)
    };
    let in_window = |i: usize, j: usize| -> bool {
        (9..=11).contains(&i)
            && (seg_dist(j, 6) <= 2 || seg_dist(j, 58) <= 2 || seg_dist(j, 0) <= 1)
    };
    let near_window = |i: usize, j: usize| -> bool {
        (8..=12).contains(&i)
            && (seg_dist(j, 6) <= 3 || seg_dist(j, 58) <= 3 || seg_dist(j, 0) <= 2)
    };
    lathe(
        &mut b,
        &cone,
        S,
        |i, j| if in_window(i, j) { 0.985 } else { 1.0 },
        |i, j| {
            if in_window(i, j) {
                GLASS
            } else if near_window(i, j) {
                FRAME
            } else if i == 4 {
                HULL_LO // panel seam band
            } else {
                HULL
            }
        },
        (false, false),
    );
    // Shoulder round-over + docking tunnel + probe.
    shell(
        &mut b,
        &[
            (0.30, 0.330),
            (0.34, 0.315),
            (0.38, 0.306),
            (0.42, 0.302),
            (0.46, 0.300),
            (0.50, 0.280),
            (0.50, 0.0),
        ],
        S,
        METAL,
        (false, false),
    );
    shell(
        &mut b,
        &[(0.435, 0.312), (0.455, 0.302)],
        S,
        FRAME,
        (false, false),
    );
    tube(
        &mut b,
        &[[0.0, 0.0, 0.44], [0.0, 0.0, 0.505]],
        0.026,
        12,
        METAL_DARK,
    );
    // Four RCS nubs on the upper cone.
    for k in 0..4 {
        let a = TAU * (k as f64 + 0.5) / 4.0;
        let (ca, sa) = (libm::cos(a), libm::sin(a));
        let (cx, cy) = (0.375 * ca, 0.375 * sa);
        cuboid(
            &mut b,
            [cx - 0.030, cy - 0.030, 0.17],
            [cx + 0.030, cy + 0.030, 0.25],
            METAL_DARK,
        );
    }
    done(b)
}

/// `nosecone` — tangent ogive, machined tip, base flange.
/// z ∈ [−0.45, 0.45].
fn nosecone() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    // Base flange — overlaps the ogive root so the joint reads as a
    // real fairing attach ring.
    shell(
        &mut b,
        &[
            (-0.45, 0.545),
            (-0.430, 0.600),
            (-0.385, 0.615),
            (-0.350, 0.600),
        ],
        S,
        METAL_DARK,
        (false, false),
    );
    // Tangent ogive: ρ = (R² + L²) / 2R; r(x) = √(ρ² − (L−x)²) + R − ρ,
    // x measured back from the tip.
    let (r_base, len) = (0.600f64, 0.80f64);
    let rho = (r_base * r_base + len * len) / (2.0 * r_base);
    let z_tip = 0.435f64;
    let z_base = z_tip - len; // −0.365
    const N: usize = 26;
    let mut prof: Vec<(f64, f64)> = (0..=N)
        .map(|k| {
            let z = z_base + (z_tip - z_base) * k as f64 / N as f64;
            let x = z_tip - z;
            let r = libm::sqrt((rho * rho - (len - x) * (len - x)).max(0.0)) + r_base - rho;
            (z, r.max(0.008))
        })
        .collect();
    prof.push((z_tip + 0.015, 0.006)); // machined tip cap
    lathe(
        &mut b,
        &prof,
        S,
        |_, _| 1.0,
        |i, _| {
            if i >= N {
                LABEL // dark tip
            } else if i == 7 || i == 19 {
                HULL_LO // fairing split lines
            } else {
                HULL
            }
        },
        (false, false),
    );
    done(b)
}

/// `tank` — barrel, ellipsoidal domes, real weld rings, marking band.
/// Model z ∈ [−1, 1]; the affine fit stretches it into the part's
/// actual length (s160 = 2 m, s320 = 4 m).
fn tank() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    let (rb, dome) = (0.620f64, 0.150f64);
    let mut prof: Vec<(f64, f64)> = Vec::new();
    // Bottom dome — quarter-ellipse from the −z apex to the barrel.
    for k in 0..=8 {
        let a = PI / 2.0 * k as f64 / 8.0;
        prof.push((-1.0 + dome * (1.0 - libm::cos(a)), rb * libm::sin(a)));
    }
    // Barrel with three real weld beads (2 raised stations each).
    prof.extend_from_slice(&[
        (-0.845, rb + 0.011),
        (-0.820, rb + 0.011),
        (-0.800, rb),
        (-0.045, rb),
        (-0.030, rb + 0.011),
        (0.000, rb + 0.011),
        (0.020, rb),
        (0.790, rb),
        (0.805, rb + 0.011),
        (0.830, rb + 0.011),
        (0.850, rb),
    ]);
    // Top dome — mirrored ellipse up to the +z apex.
    for k in 1..=8 {
        let a = PI / 2.0 * k as f64 / 8.0;
        prof.push((0.85 + dome * libm::sin(a), rb * libm::cos(a)));
    }
    lathe(
        &mut b,
        &prof,
        S,
        |_, _| 1.0,
        |i, _| {
            let (z, r) = prof[i];
            if r > rb + 0.005 {
                METAL // raised weld beads
            } else if (0.42..0.50).contains(&z) {
                ACCENT // international-orange stripe
            } else if (0.50..0.62).contains(&z) {
                LABEL // marking band
            } else {
                HULL
            }
        },
        (false, false),
    );
    // Fill/drain fitting at the top apex.
    stud(&mut b, [0.0, 0.0], 0.96, 1.005, 0.028, 16, METAL_DARK);
    done(b)
}

/// `booster` — SRB: barrel + segment rings, nose frustum, aft skirt
/// flare, recessed nozzle. z ∈ [−0.9, 0.9].
fn booster() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    let mut prof: Vec<(f64, f64)> = vec![
        (-0.55, 0.440),
        (-0.55, 0.457), // aft segment ring
        (-0.52, 0.457),
        (-0.49, 0.440),
        (0.40, 0.440),
        (0.42, 0.457), // fwd segment ring
        (0.45, 0.457),
        (0.48, 0.440),
    ];
    // Nose frustum with a flat cap.
    for k in 1..=8 {
        let t = k as f64 / 8.0;
        prof.push((
            0.48 + t * 0.36,
            0.440 - 0.330 * t + 0.030 * libm::sin(PI * t),
        ));
    }
    prof.push((0.90, 0.110));
    prof.push((0.90, 0.0));
    lathe(
        &mut b,
        &prof,
        S,
        |_, _| 1.0,
        |i, _| {
            if (prof[i].1 - 0.457).abs() < 1e-6 {
                METAL // segment rings
            } else if i >= 12 {
                METAL_DARK // nose cap
            } else {
                HULL
            }
        },
        (false, false),
    );
    // Aft skirt flare — outside, so ascending z.
    shell(
        &mut b,
        &[
            (-0.90, 0.580),
            (-0.86, 0.575),
            (-0.70, 0.500),
            (-0.56, 0.445),
        ],
        S,
        METAL_DARK,
        (false, false),
    );
    // Nozzle stub inside the skirt — outer cone up, charred inner
    // wall down (seen from below).
    shell(
        &mut b,
        &[(-0.80, 0.30), (-0.74, 0.16)],
        S,
        NOZZLE,
        (false, false),
    );
    shell(
        &mut b,
        &[(-0.73, 0.15), (-0.79, 0.285)],
        S,
        CHAR,
        (false, false),
    );
    done(b)
}

/// `decoupler` — machined ring with a recessed pyro seam and bolt
/// bumps. z ∈ [−0.125, 0.125].
fn decoupler() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    let prof = [
        (-0.125, 0.545),
        (-0.115, 0.585),
        (-0.095, 0.608),
        (-0.045, 0.615),
        (-0.020, 0.615),
        (-0.010, 0.590), // pyro separation seam — recessed + dark
        (0.010, 0.590),
        (0.020, 0.615),
        (0.045, 0.615),
        (0.095, 0.608),
        (0.115, 0.585),
        (0.125, 0.545),
    ];
    lathe(
        &mut b,
        &prof,
        S,
        |i, j| {
            if (3..=8).contains(&i) {
                // Bolt bumps around the main band — a subtle
                // machined-fastener read.
                let a = TAU * 18.0 * j as f64 / S as f64;
                1.0 + 0.007 * (0.5 + 0.5 * libm::cos(a))
            } else {
                1.0
            }
        },
        |i, _| {
            if i == 5 || i == 6 {
                SEP
            } else if i <= 1 || i >= 9 {
                METAL_DARK
            } else {
                METAL
            }
        },
        (true, true),
    );
    done(b)
}

/// `adapter` — corrugated interstage with end flanges and a vent band.
/// z ∈ [−0.25, 0.25].
fn adapter() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    let prof = [
        (-0.25, 0.540),
        (-0.235, 0.585),
        (-0.225, 0.615),
        (-0.200, 0.632), // bottom flange
        (-0.185, 0.632),
        (-0.170, 0.612),
        (0.170, 0.612),
        (0.185, 0.632),
        (0.200, 0.632), // top flange
        (0.225, 0.615),
        (0.235, 0.585),
        (0.25, 0.540),
    ];
    lathe(
        &mut b,
        &prof,
        S,
        |i, j| {
            if i == 6 {
                // Corrugated stringer wall between the flanges.
                let a = TAU * 24.0 * j as f64 / S as f64;
                1.0 + 0.008 * libm::cos(a)
            } else {
                1.0
            }
        },
        |i, _| {
            if i == 3 || i == 7 {
                METAL_DARK // flanges
            } else if i == 5 {
                LABEL // vent band
            } else {
                METAL
            }
        },
        (true, true),
    );
    done(b)
}

/// `battery` — superellipse-rounded pack, rib bands, top terminals.
/// Box collider 0.625 × 0.625 × 0.4 → model z ∈ [−0.2, 0.2].
fn battery() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 48;
    let a_sq = 0.300f64;
    let n_exp = 8.0f64;
    // Superellipse cross-section — a square with rounded corners.
    let se = move |j: usize| -> f64 {
        let t = TAU * j as f64 / S as f64;
        let (c, s) = (libm::cos(t).abs(), libm::sin(t).abs());
        a_sq * libm::pow(libm::pow(c, n_exp) + libm::pow(s, n_exp), -1.0 / n_exp)
    };
    let prof = [
        (-0.20, 0.80),
        (-0.185, 1.0),
        (-0.11, 1.0),
        (-0.105, 1.04), // rib
        (-0.075, 1.04),
        (-0.065, 1.0),
        (0.065, 1.0),
        (0.075, 1.04), // rib
        (0.105, 1.04),
        (0.11, 1.0),
        (0.185, 1.0),
        (0.20, 0.80),
    ];
    lathe(
        &mut b,
        &prof,
        S,
        |_, j| se(j),
        |i, _| {
            if (3..=4).contains(&i) || (7..=8).contains(&i) {
                METAL // ribs
            } else if i == 0 || i == 11 {
                METAL_DARK // chamfered ends
            } else {
                [170, 177, 185, 255] // bare pack metal
            }
        },
        (true, true),
    );
    // Two top terminals.
    stud(&mut b, [-0.08, 0.0], 0.185, 0.205, 0.024, 12, METAL_DARK);
    stud(&mut b, [0.08, 0.0], 0.185, 0.205, 0.024, 12, METAL_DARK);
    done(b)
}

/// `science` — instrument canister: body, panel door, domed lid,
/// paraboloid antenna dish. z ∈ [−0.3, 0.3].
fn science() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 64;
    let prof = [
        (-0.30, 0.235),
        (-0.275, 0.300),
        (-0.26, 0.305),
        (0.10, 0.305),
        (0.13, 0.295),
        (0.15, 0.268),
    ];
    // Panel door — quad rows 2..3, ±4 segs around θ = 0, recessed.
    let door = |i: usize, j: usize| -> bool { (2..=3).contains(&i) && j.min(S - j) <= 4 };
    let door_edge = |i: usize, j: usize| -> bool { (1..=4).contains(&i) && j.min(S - j) <= 5 };
    lathe(
        &mut b,
        &prof,
        S,
        |i, j| if door(i, j) { 0.99 } else { 1.0 },
        |i, j| {
            if door(i, j) {
                LABEL
            } else if door_edge(i, j) {
                FRAME // door border
            } else {
                HULL
            }
        },
        (true, false),
    );
    // Domed lid.
    let mut lid: Vec<(f64, f64)> = vec![(0.15, 0.268)];
    for k in 1..=6 {
        let a = PI / 2.0 * k as f64 / 6.0;
        lid.push((0.15 + 0.075 * libm::sin(a), 0.268 * libm::cos(a)));
    }
    lathe(
        &mut b,
        &lid,
        S,
        |_, _| 1.0,
        |i, _| if i == 0 { FRAME } else { METAL },
        (false, false),
    );
    // Antenna: stem + upward-opening paraboloid dish.
    tube(
        &mut b,
        &[[0.0, 0.0, 0.20], [0.0, 0.0, 0.27]],
        0.011,
        10,
        METAL_DARK,
    );
    shell(
        &mut b,
        &[
            (0.266, 0.012),
            (0.270, 0.045),
            (0.282, 0.075),
            (0.298, 0.098),
        ],
        S,
        FRAME,
        (false, false),
    );
    done(b)
}

/// `chute` — radial-mount parachute canister: superellipse pack,
/// orange mortar-cap dome, mount plate on −x. Box collider
/// 0.5 × 0.4 × 0.5 → model z ∈ [−0.25, 0.24].
fn chute() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 48;
    let a_sq = 0.23f64;
    let n_exp = 6.0f64;
    // Superellipse cross-section — a rounded-square canister (the
    // `battery` construction; `emit_model` fits it into the collider
    // AABB).
    let se = move |j: usize| -> f64 {
        let t = TAU * j as f64 / S as f64;
        let (c, s) = (libm::cos(t).abs(), libm::sin(t).abs());
        a_sq * libm::pow(libm::pow(c, n_exp) + libm::pow(s, n_exp), -1.0 / n_exp)
    };
    let prof = [(-0.25, 0.75), (-0.235, 1.0), (0.15, 1.0), (0.18, 0.9)];
    lathe(
        &mut b,
        &prof,
        S,
        |_, j| se(j),
        |i, _| {
            if i == 0 {
                METAL_DARK // base chamfer
            } else if i == 3 {
                FRAME // cap ring
            } else {
                HULL
            }
        },
        (true, false),
    );
    // Mortar cap — the packed-canopy dome (the orange "pop me" read).
    let mut lid: Vec<(f64, f64)> = vec![(0.18, 0.207)];
    for k in 1..=5 {
        let a = PI / 2.0 * k as f64 / 5.0;
        lid.push((0.18 + 0.06 * libm::sin(a), 0.207 * libm::cos(a)));
    }
    lathe(
        &mut b,
        &lid,
        S,
        |_, _| 1.0,
        |i, _| if i == 0 { FRAME } else { ACCENT },
        (false, false),
    );
    // Mount plate on −x.
    cuboid(
        &mut b,
        [-0.26, -0.14, -0.14],
        [-0.24, 0.14, 0.14],
        METAL_DARK,
    );
    done(b)
}

/// `fins` — a three-fin can: swept trapezoid fins on a root hub.
/// Radial extent 1.0, z ∈ [−0.4, 0.4].
fn fins() -> MeshEntry {
    let mut b = B::default();
    const S: usize = 48;
    // Root hub ring.
    shell(
        &mut b,
        &[(-0.30, 0.40), (-0.28, 0.44), (0.28, 0.44), (0.30, 0.40)],
        S,
        METAL,
        (false, false),
    );
    // Three swept fins — trapezoid plate in the (r, z) plane extruded
    // through ±y, rotated about the stack axis.
    let fin_poly: [(f64, f64); 4] = [
        (0.42, -0.36), // root trailing
        (0.98, -0.40), // tip trailing — sweeps aft
        (0.62, 0.34),  // tip leading
        (0.44, 0.38),  // root leading
    ];
    for k in 0..3 {
        let rot = TAU * k as f64 / 3.0;
        prism(&mut b, &fin_poly, -0.016, 0.016, rot, METAL_DARK, HULL);
    }
    done(b)
}

// ---------------------------------------------------------------------------
// The catalog — same keys the old GLB table used, so `csp-view`'s
// `art::model_key` mapping is untouched.
// ---------------------------------------------------------------------------

/// A part build entry: bake key + its authored-mesh constructor.
type PartBuild = (&'static str, fn() -> MeshEntry);

/// Build every part model. Returned in bake-table order.
pub fn build_all() -> Vec<(&'static str, MeshEntry)> {
    let builds: Vec<PartBuild> = vec![
        ("engine_a", engine_a),
        ("engine_b", engine_b),
        ("pod", pod),
        ("nosecone", nosecone),
        ("tank", tank),
        ("booster", booster),
        ("decoupler", decoupler),
        ("adapter", adapter),
        ("battery", battery),
        ("science", science),
        ("chute", chute),
        ("fins", fins),
    ];
    builds
        .into_iter()
        .map(|(k, f)| {
            let m = f();
            println!(
                "part {k:<10} <- procedural: {} verts {} tris",
                m.verts.len(),
                m.indices.len() / 3
            );
            (k, m)
        })
        .collect()
}
