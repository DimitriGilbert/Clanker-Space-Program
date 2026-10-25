//! The M4c kitbash tier — PLAN §3.7's "silhouette-correct kitbashed
//! parts", the presentation floor that replaced the through-M3
//! placeholder boxes. Every placed part renders as the procedural
//! primitive its *resolved* geometry names instead of a bounding box:
//!
//! - a resolved `Box` collider stays a box (batteries, the fallback
//!   for point sets that are not clean solids of revolution);
//! - a resolved `Cylinder`/`Convex` collider (`Points` to the render
//!   tier) **lathes its own silhouette**: the point set is reduced to
//!   `(z, ring radius)` stations and revolved about the stack axis, so
//!   an octagonal-prism collider renders as a true 24-segment cylinder
//!   and an authored ring-plus-tip set (the nose cone) as an ogive;
//! - two **kind overrides** sit on top, driven by what the resolved
//!   part *does* rather than its collider flavor: a `Thrust` force
//!   site (`ModuleDef::Engine` on the def side) reshapes the cylinder
//!   collider into a mount cylinder + bell nozzle, and crew seats
//!   (`CrewSeat`/`crew_seats`) into the capsule taper.
//!
//! The classifier inputs are exactly what both resolved part views
//! carry: `EffectivePart` (the VAB, modules + resolved attach nodes
//! intact) and `WeldPart` (the flight view's `CraftDef`). Everything
//! is a pure function of that data — deterministic by construction,
//! no RNG anywhere on the mesh path.

use csp_core::DVec3;
use csp_data::ModuleDef;
use csp_phys::{ColliderShape, EffectivePart, ForceSiteKind, WeldPart};

/// Revolution facets for [`PartShape::Lathe`] — the kitbash tier's
/// radial fidelity: round at chase-camera distances, ~30 k triangles
/// for the §5.4 300-part VAB worst case.
pub const LATHE_SEGMENTS: usize = 24;

/// A station radius at or under this bound is an apex (a cone tip)
/// rather than a ring — the lathe emits a fan to the axis point.
const APEX_RADIUS_M: f64 = 1e-9;

/// One triangle of a part's kitbash mesh — part-local positions plus
/// the unit outward face normal. The flight/VAB vertex streams carry
/// per-vertex normals + material terms lit per pixel in the fragment
/// shader ([`emit_smooth`] supplies the per-vertex normals); this
/// face-normal stream remains for silhouette/VFX paths that only need
/// the flat facet normal.
#[derive(Clone, Copy, Debug)]
pub struct MeshTri {
    /// Corner a, part-local.
    pub a: DVec3,
    /// Corner b, part-local.
    pub b: DVec3,
    /// Corner c, part-local.
    pub c: DVec3,
    /// Unit outward face normal, part-local.
    pub n: DVec3,
}

/// A part's silhouette primitive — the kitbash shape a resolved part
/// presents as. Everything part-local; +z is the stack axis per §3.1's
/// geometry convention.
#[derive(Clone, Debug, PartialEq)]
pub enum PartShape {
    /// Axis-aligned box over the collider bounds.
    Box {
        /// Part-local AABB min corner.
        lo: DVec3,
        /// Part-local AABB max corner.
        hi: DVec3,
    },
    /// A solid of revolution about the collider's z axis — `(z,
    /// radius)` stations in ascending z lathed at
    /// [`LATHE_SEGMENTS`] facets with flat end caps. A station whose
    /// radius is under [`APEX_RADIUS_M`] is an apex.
    Lathe {
        /// The axis's xy — the z line the profile revolves about (the
        /// collider's xy centre; `(0,0)` for every authored primitive).
        axis: [f64; 2],
        /// `(z, radius)` stations, ascending z, ≥ 2 entries, `r ≥ 0`.
        profile: Vec<(f64, f64)>,
    },
}

impl PartShape {
    /// Part-local AABB — camera framing reads it where the box tier
    /// read `size_m` half-extents.
    pub fn local_aabb(&self) -> (DVec3, DVec3) {
        match self {
            Self::Box { lo, hi } => (*lo, *hi),
            Self::Lathe { axis, profile } => {
                let r = profile.iter().map(|s| s.1).fold(0.0_f64, f64::max);
                let z_lo = profile.first().map_or(0.0, |s| s.0);
                let z_hi = profile.last().map_or(0.0, |s| s.0);
                (
                    DVec3::new(axis[0] - r, axis[1] - r, z_lo),
                    DVec3::new(axis[0] + r, axis[1] + r, z_hi),
                )
            }
        }
    }
}

/// The silhouette classifier — the def-driven mapping shared by the
/// VAB and flight packet producers.
///
/// - `collider` — the resolved collider (`EffectivePart::weld_input
///   .collider` in the VAB, `WeldPart::collider` in flight). `Box`
///   stays a box; `Points` becomes a lathe profile of its z-station
///   ring radii (cylinder colliders → cylinders, the ring-plus-tip
///   `Convex` authoring of a nose cone → an ogive), or falls back to
///   the box when a station is not a clean ring.
/// - `engine` — a `Thrust` site exists (`ModuleDef::Engine` on the def
///   side). Overrides the collider profile to the bell nozzle — a
///   cylinder-collider engine still reads as an engine.
/// - `crew` — crew seats exist (`ModuleDef::CrewSeat` /
///   `WeldPart::crew_seats`). Overrides to the capsule taper.
/// - `top_diameter_m` — the up-stack attach node's size class where
///   authored (`EffectivePart::attach_nodes`; the weld view carries
///   no nodes) — the pod's taper target.
pub fn classify(
    collider: &ColliderShape,
    engine: bool,
    crew: bool,
    top_diameter_m: Option<f64>,
) -> PartShape {
    let verts = collider.vertices();
    let Some((lo, hi)) = aabb(&verts) else {
        // An empty collider has no silhouette — a degenerate box at
        // the origin produces only zero-area (dropped) triangles.
        return PartShape::Box {
            lo: DVec3::ZERO,
            hi: DVec3::ZERO,
        };
    };
    let axis = [(lo.x + hi.x) * 0.5, (lo.y + hi.y) * 0.5];
    // The stack radius the kind overrides lathe from: the collider's
    // real radial extent, or the box's wider face half-extent.
    let r = match collider {
        ColliderShape::Box { half_extents } => half_extents.x.max(half_extents.y),
        ColliderShape::Points(pts) => pts
            .iter()
            .map(|v| (v.x - axis[0]).hypot(v.y - axis[1]))
            .fold(0.0_f64, f64::max),
    };
    if engine {
        return PartShape::Lathe {
            axis,
            profile: engine_profile(lo.z, hi.z, r),
        };
    }
    if crew {
        return PartShape::Lathe {
            axis,
            profile: pod_profile(lo.z, hi.z, r, top_diameter_m.map_or(0.5 * r, |d| 0.5 * d)),
        };
    }
    match collider {
        ColliderShape::Box { .. } => PartShape::Box { lo, hi },
        ColliderShape::Points(pts) => match ring_profile(pts, axis) {
            Some(profile) if profile.len() >= 2 => PartShape::Lathe {
                axis,
                profile: if tip_at_top(&profile) {
                    ogive_profile(&profile)
                } else {
                    profile
                },
            },
            _ => PartShape::Box { lo, hi },
        },
    }
}

/// The VAB-side call — [`classify`] from an `EffectivePart`: the
/// module list names engines and crew seats, and the resolved attach
/// nodes name the up-stack size class the pod tapers to (the +z-pointing
/// node's `diameter_m`; a pod with no top node keeps the half-radius
/// default).
pub fn classify_effective(eff: &EffectivePart) -> PartShape {
    let engine = eff
        .modules
        .iter()
        .any(|m| matches!(m, ModuleDef::Engine { .. }));
    let crew = eff
        .modules
        .iter()
        .any(|m| matches!(m, ModuleDef::CrewSeat { .. }));
    let top = eff
        .attach_nodes
        .iter()
        .filter(|n| n.direction.z > 0.9)
        .map(|n| n.diameter_m)
        .reduce(f64::max);
    classify(&eff.weld_input.collider, engine, crew, top)
}

/// The flight-side call — [`classify`] from a `WeldPart`: `Thrust`
/// force sites mark engines, `crew_seats` mark pods. The weld view
/// carries no attach nodes, so the pod taper defaults to half the
/// base radius — the stock pod's own 0.625-on-1.25 ratio.
pub fn classify_weld_part(part: &WeldPart) -> PartShape {
    let engine = part
        .force_sites
        .iter()
        .any(|s| s.kind == ForceSiteKind::Thrust);
    classify(&part.collider, engine, part.crew_seats > 0, None)
}

/// The AABB of a vertex set — `None` when empty.
fn aabb(verts: &[DVec3]) -> Option<(DVec3, DVec3)> {
    let mut lo = *verts.first()?;
    let mut hi = lo;
    for v in &verts[1..] {
        lo = DVec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
        hi = DVec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
    }
    Some((lo, hi))
}

/// Reduce a collider point set to its revolution profile: each
/// distinct z station contributes `(z, max ring radius)` in ascending
/// z. `None` when a station's points are not a clean ring — a lone
/// off-axis vertex at a real radius says the set is not a solid of
/// revolution, and the caller falls back to the box.
fn ring_profile(pts: &[DVec3], axis: [f64; 2]) -> Option<Vec<(f64, f64)>> {
    if pts.len() < 4 {
        return None;
    }
    let (lo, hi) = aabb(pts)?;
    let eps = 1e-9 * (hi - lo).norm().max(1.0);
    let r_max = pts
        .iter()
        .map(|v| (v.x - axis[0]).hypot(v.y - axis[1]))
        .fold(0.0_f64, f64::max);
    // Distinct z stations, merged at a fraction of the extent so
    // float-identical authored vertices land in one group. `pts` order
    // is irrelevant — groups are found by scan, not position.
    let mut zs: Vec<f64> = pts.iter().map(|v| v.z).collect();
    zs.sort_by(f64::total_cmp);
    zs.dedup_by(|a, b| (*b - *a).abs() <= eps);
    let mut profile = Vec::with_capacity(zs.len());
    for z in zs {
        let mut r_min = f64::INFINITY;
        let mut r_max_at_z = 0.0_f64;
        for v in pts.iter().filter(|v| (v.z - z).abs() <= eps) {
            let r = (v.x - axis[0]).hypot(v.y - axis[1]);
            r_min = r_min.min(r);
            r_max_at_z = r_max_at_z.max(r);
        }
        // Apex stations (a cone tip ON the axis) are legal rings of
        // zero radius; a station whose radii spread under half its max
        // is not a ring at all.
        if r_max_at_z > 0.02 * r_max && r_min < 0.5 * r_max_at_z {
            return None;
        }
        profile.push((z, r_max_at_z));
    }
    Some(profile)
}

/// A profile that lands on a top-side point — last station under 2%
/// of the max radius — is a cone/frustum-to-apex (the nose cone's
/// authored collider). Those get the curved ogive regeneration rather
/// than rendering as a faceted pyramid.
fn tip_at_top(profile: &[(f64, f64)]) -> bool {
    let r_max = profile.iter().map(|s| s.1).fold(0.0_f64, f64::max);
    profile.len() >= 2 && profile.last().is_some_and(|&(_, r)| r <= 0.02 * r_max)
}

/// Re-lathe a tip-at-top profile as a pointed ogive: `r(t) =
/// r_base·(1−t)^0.75` — convex flanks with a pointed tip, the authored
/// cone's silhouette class (§3.7 cone/ogive).
fn ogive_profile(profile: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let z0 = profile.first().map_or(0.0, |s| s.0);
    let z1 = profile.last().map_or(0.0, |s| s.0);
    let r_base = profile.iter().map(|s| s.1).fold(0.0_f64, f64::max);
    const T: [f64; 8] = [0.0, 0.08, 0.2, 0.38, 0.58, 0.76, 0.9, 1.0];
    T.iter()
        .map(|&t| (z0 + t * (z1 - z0), r_base * (1.0 - t).powf(0.75)))
        .collect()
}

/// The engine silhouette: a mount cylinder under the stack, a throat,
/// and the bell nozzle flaring nearly to the stack radius at the −z
/// lip (the nozzle points −z per §3.1's stack convention). The upper
/// mount's step *up* from the throat is what separates the bell from
/// a pod taper in silhouette.
fn engine_profile(z0: f64, z1: f64, r: f64) -> Vec<(f64, f64)> {
    let h = z1 - z0;
    [
        (0.0, 0.97),
        (0.08, 0.83),
        (0.26, 0.58),
        (0.45, 0.46),
        (1.0, 0.58),
    ]
    .into_iter()
    .map(|(t, k)| (z0 + t * h, k * r))
    .collect()
}

/// The crew-pod silhouette: full radius over the base shoulder, then
/// a smoothstep taper to the top-stack attach radius — the capsule
/// curve, not a straight cone.
fn pod_profile(z0: f64, z1: f64, r: f64, r_top: f64) -> Vec<(f64, f64)> {
    let h = z1 - z0;
    let rt = r_top.clamp(0.1 * r, r);
    const T: [f64; 7] = [0.0, 0.15, 0.35, 0.55, 0.75, 0.9, 1.0];
    T.iter()
        .map(|&t| {
            let u = ((t - 0.15) / 0.85).clamp(0.0, 1.0);
            (z0 + t * h, r + (rt - r) * u * u * (3.0 - 2.0 * u))
        })
        .collect()
}

/// Append `shape`'s triangles to `out` — the part-local [`MeshTri`]
/// stream both packet producers transform and project from.
pub fn emit(shape: &PartShape, out: &mut Vec<MeshTri>) {
    match shape {
        PartShape::Box { lo, hi } => emit_box(*lo, *hi, out),
        PartShape::Lathe { axis, profile } => emit_lathe(*axis, profile, out),
    }
}

/// A triangle carrying per-vertex unit outward normals — the
/// smooth-shaded stream [`emit_smooth`] produces for the flight view.
/// Lathe sides shade continuously around the ring (a tank reads as a
/// cylinder rather than 24 flat panels), caps stay flat, and a `Box`
/// keeps its face normals.
#[derive(Clone, Copy, Debug)]
pub struct SmoothTri {
    /// Corner a, part-local.
    pub a: DVec3,
    /// Corner b, part-local.
    pub b: DVec3,
    /// Corner c, part-local.
    pub c: DVec3,
    /// Unit outward normal at `a`.
    pub na: DVec3,
    /// Unit outward normal at `b`.
    pub nb: DVec3,
    /// Unit outward normal at `c`.
    pub nc: DVec3,
}

/// Append `shape`'s triangles to `out` with per-vertex analytic
/// normals — the same silhouette as [`emit`], but each corner carries
/// the true surface normal so consumers can shade per vertex.
pub fn emit_smooth(shape: &PartShape, out: &mut Vec<SmoothTri>) {
    match shape {
        PartShape::Box { lo, hi } => {
            let mut flat = Vec::new();
            emit_box(*lo, *hi, &mut flat);
            out.extend(flat.iter().map(|t| SmoothTri {
                a: t.a,
                b: t.b,
                c: t.c,
                na: t.n,
                nb: t.n,
                nc: t.n,
            }));
        }
        PartShape::Lathe { axis, profile } => emit_lathe_smooth(*axis, profile, out),
    }
}

/// The smooth lathe — same ring/strip layout as [`emit_lathe`], but
/// each vertex carries the analytic normal: the profile-space normal
/// `(-dr, dz)` revolved about the axis for ring vertices, the axis
/// direction for apexes, ±z for the flat caps.
fn emit_lathe_smooth(axis: [f64; 2], profile: &[(f64, f64)], out: &mut Vec<SmoothTri>) {
    let n_st = profile.len();
    if n_st < 2 {
        return;
    }
    // Profile-space station normals `(n_z, n_r)` — the perpendicular
    // of the (z, r) profile tangent, signed so `n_r` is positive
    // (outward) on an ascending-z profile.
    let pnorm: Vec<(f64, f64)> = (0..n_st)
        .map(|i| {
            let (dz, dr) = if i == 0 {
                (profile[1].0 - profile[0].0, profile[1].1 - profile[0].1)
            } else if i + 1 == n_st {
                (
                    profile[i].0 - profile[i - 1].0,
                    profile[i].1 - profile[i - 1].1,
                )
            } else {
                (
                    profile[i + 1].0 - profile[i - 1].0,
                    profile[i + 1].1 - profile[i - 1].1,
                )
            };
            let l = (dz * dz + dr * dr).sqrt();
            if l > 1e-12 {
                (-dr / l, dz / l)
            } else {
                (0.0, 1.0)
            }
        })
        .collect();
    // Per-station rings + normals — empty vecs mark an apex.
    let stations: Vec<(f64, Vec<DVec3>, Vec<DVec3>)> = profile
        .iter()
        .zip(&pnorm)
        .map(|(&(z, r), &(nz, nr))| {
            if r <= APEX_RADIUS_M {
                (z, Vec::new(), Vec::new())
            } else {
                let (ring, nrms): (Vec<DVec3>, Vec<DVec3>) = (0..LATHE_SEGMENTS)
                    .map(|i| {
                        let a = (i as f64) * (2.0 * core::f64::consts::PI) / LATHE_SEGMENTS as f64;
                        let (ca, sa) = (a.cos(), a.sin());
                        (
                            DVec3::new(axis[0] + r * ca, axis[1] + r * sa, z),
                            DVec3::new(nr * ca, nr * sa, nz),
                        )
                    })
                    .unzip();
                (z, ring, nrms)
            }
        })
        .collect();
    let tri =
        |out: &mut Vec<SmoothTri>, va: (DVec3, DVec3), vb: (DVec3, DVec3), vc: (DVec3, DVec3)| {
            let e = (vb.0 - va.0).cross(vc.0 - va.0);
            if e.norm_squared() > 1e-30 {
                out.push(SmoothTri {
                    a: va.0,
                    b: vb.0,
                    c: vc.0,
                    na: va.1,
                    nb: vb.1,
                    nc: vc.1,
                });
            }
        };
    // Side strips between consecutive stations.
    for w in stations.windows(2) {
        let ((z0, r0, n0), (z1, r1, n1)) = (&w[0], &w[1]);
        let apex =
            |z: f64, away: f64| (DVec3::new(axis[0], axis[1], z), DVec3::new(0.0, 0.0, away));
        for i in 0..LATHE_SEGMENTS {
            let j = (i + 1) % LATHE_SEGMENTS;
            match (r0.is_empty(), r1.is_empty()) {
                (false, false) => {
                    tri(out, (r0[i], n0[i]), (r0[j], n0[j]), (r1[j], n1[j]));
                    tri(out, (r0[i], n0[i]), (r1[j], n1[j]), (r1[i], n1[i]));
                }
                (true, false) => tri(out, apex(*z0, -1.0), (r1[i], n1[i]), (r1[j], n1[j])),
                (false, true) => tri(out, (r0[i], n0[i]), apex(*z1, 1.0), (r0[j], n0[j])),
                (true, true) => {}
            }
        }
    }
    // Flat caps on the two open ends (apexes need none) — the cap
    // normal is the axis direction, so the disc shades flat.
    for (k, sgn) in [(0usize, -1.0f64), (n_st - 1, 1.0)] {
        let (z, ring, _) = &stations[k];
        if ring.is_empty() {
            continue;
        }
        let n = DVec3::new(0.0, 0.0, sgn);
        let cap = (DVec3::new(axis[0], axis[1], *z), n);
        for i in 0..LATHE_SEGMENTS {
            let j = (i + 1) % LATHE_SEGMENTS;
            tri(out, cap, (ring[i], n), (ring[j], n));
        }
    }
}

/// `true` for a lathe profile ending in a top apex — the cone/ogive
/// class the nose-cone model claims (a flat-topped cylinder reads as
/// an adapter instead).
pub fn is_pointed(shape: &PartShape) -> bool {
    matches!(shape, PartShape::Lathe { profile, .. } if tip_at_top(profile))
}

/// One triangle of a baked real model ([`crate::art::PartModel`]) —
/// per-corner positions, per-corner unit normals, and per-corner
/// authored albedo (linear RGBA, baked from the source materials).
#[derive(Clone, Copy, Debug)]
pub struct ModelTri {
    /// Corner a, part-local.
    pub a: DVec3,
    /// Corner b, part-local.
    pub b: DVec3,
    /// Corner c, part-local.
    pub c: DVec3,
    /// Unit outward normal at `a`, part-local.
    pub na: DVec3,
    /// Normal at `b`.
    pub nb: DVec3,
    /// Normal at `c`.
    pub nc: DVec3,
    /// Albedo at `a` (linear RGBA).
    pub ca: [f32; 4],
    /// Albedo at `b`.
    pub cb: [f32; 4],
    /// Albedo at `c`.
    pub cc: [f32; 4],
    /// Baked material terms at `a` — `[spec strength, spec exponent,
    /// occlusion]` (see [`crate::art::ModelVert::mat`]).
    pub ma: [f32; 3],
    /// Material terms at `b`.
    pub mb: [f32; 3],
    /// Material terms at `c`.
    pub mc: [f32; 3],
}

/// Emit `model`'s triangles fitted into the part-local box `[lo, hi]`
/// (the resolved collider's AABB — `PartShape::local_aabb`). Per-axis
/// affine fit keeps the model inside its collider exactly; normals go
/// through the inverse-transpose so a non-uniform stretch still
/// shades correctly.
pub fn emit_model(model: &crate::art::PartModel, lo: DVec3, hi: DVec3, out: &mut Vec<ModelTri>) {
    let span_t = hi - lo;
    let span_m = model.hi - model.lo;
    let s = DVec3::new(
        if span_m.x > 1e-9 {
            span_t.x / span_m.x
        } else {
            1.0
        },
        if span_m.y > 1e-9 {
            span_t.y / span_m.y
        } else {
            1.0
        },
        if span_m.z > 1e-9 {
            span_t.z / span_m.z
        } else {
            1.0
        },
    );
    let fit = |p: [f32; 3]| -> DVec3 {
        DVec3::new(
            lo.x + (f64::from(p[0]) - model.lo.x) * s.x,
            lo.y + (f64::from(p[1]) - model.lo.y) * s.y,
            lo.z + (f64::from(p[2]) - model.lo.z) * s.z,
        )
    };
    let nrm = |n: [f32; 3]| -> DVec3 {
        let v = DVec3::new(
            f64::from(n[0]) / s.x,
            f64::from(n[1]) / s.y,
            f64::from(n[2]) / s.z,
        );
        let l = v.norm();
        if l > 1e-12 { v / l } else { DVec3::Z }
    };
    for tri in model.indices.chunks_exact(3) {
        let (Some(a), Some(b), Some(c)) = (
            model.verts.get(tri[0] as usize).copied(),
            model.verts.get(tri[1] as usize).copied(),
            model.verts.get(tri[2] as usize).copied(),
        ) else {
            continue;
        };
        let (pa, pb, pc) = (fit(a.pos), fit(b.pos), fit(c.pos));
        let e = (pb - pa).cross(pc - pa);
        if e.norm_squared() <= 1e-30 {
            continue; // degenerate after the fit — drop, never corrupt
        }
        out.push(ModelTri {
            a: pa,
            b: pb,
            c: pc,
            na: nrm(a.nrm),
            nb: nrm(b.nrm),
            nc: nrm(c.nrm),
            ca: a.col,
            cb: b.col,
            cc: c.col,
            ma: a.mat,
            mb: b.mat,
            mc: c.mat,
        });
    }
}

/// `shape` offset outward by `margin` — the hull shell the reentry
/// sheath emits. Box faces push out along their axes; lathe rings
/// fatten by the margin and the two end stations extend along the
/// axis (apexes round off into small rings). The result hugs the part
/// silhouette — it is the hull grown a skin, never a bounding box.
pub fn inflated(shape: &PartShape, margin: f64) -> PartShape {
    match shape {
        PartShape::Box { lo, hi } => {
            let d = DVec3::new(margin, margin, margin);
            PartShape::Box {
                lo: *lo - d,
                hi: *hi + d,
            }
        }
        PartShape::Lathe { axis, profile } => {
            let last = profile.len().saturating_sub(1);
            // The end stations move ∓margin along the axis — a margin
            // wider than half the z-span (a flat part, or a big plasma
            // intensity) would swap their order and emit a
            // self-intersecting shell. Clamp the margin so station
            // order is preserved.
            let z_span = profile
                .first()
                .zip(profile.last())
                .map_or(0.0, |(f, l)| l.0 - f.0);
            let margin = margin.clamp(0.0, (z_span * 0.5).max(0.0));
            PartShape::Lathe {
                axis: *axis,
                profile: profile
                    .iter()
                    .enumerate()
                    .map(|(i, &(z, r))| {
                        let dz = if i == 0 {
                            -margin
                        } else if i == last {
                            margin
                        } else {
                            0.0
                        };
                        (z + dz, r + margin)
                    })
                    .collect(),
            }
        }
    }
}

/// One triangle, wound so its normal faces away from `center` — every
/// emitted primitive is star-convex about its bounds/axis centre, so
/// the centroid−centre dot is the outward test. Zero-area slivers are
/// dropped rather than emitted with a NaN normal.
fn tri(out: &mut Vec<MeshTri>, a: DVec3, b: DVec3, c: DVec3, center: DVec3) {
    let n = (b - a).cross(c - a);
    let len = n.norm();
    if !len.is_finite() || len <= 1e-18 {
        return;
    }
    let mut n = n / len;
    let mid = (a + b + c) * (1.0 / 3.0);
    if n.dot(mid - center) < 0.0 {
        n = -n;
    }
    out.push(MeshTri { a, b, c, n });
}

/// The box's 12 triangles — corners from `lo`/`hi`, same face table
/// the placeholder pass used.
fn emit_box(lo: DVec3, hi: DVec3, out: &mut Vec<MeshTri>) {
    let c = [
        DVec3::new(lo.x, lo.y, lo.z),
        DVec3::new(hi.x, lo.y, lo.z),
        DVec3::new(hi.x, hi.y, lo.z),
        DVec3::new(lo.x, hi.y, lo.z),
        DVec3::new(lo.x, lo.y, hi.z),
        DVec3::new(hi.x, lo.y, hi.z),
        DVec3::new(hi.x, hi.y, hi.z),
        DVec3::new(lo.x, hi.y, hi.z),
    ];
    const FACES: [[usize; 4]; 6] = [
        [0, 1, 2, 3],
        [5, 4, 7, 6],
        [4, 0, 3, 7],
        [1, 5, 6, 2],
        [4, 5, 1, 0],
        [3, 2, 6, 7],
    ];
    let center = (lo + hi) * 0.5;
    for f in FACES {
        tri(out, c[f[0]], c[f[1]], c[f[2]], center);
        tri(out, c[f[0]], c[f[2]], c[f[3]], center);
    }
}

/// The lathe — rings at each nonzero-radius station, side quads (or
/// apex fans) between consecutive stations, flat cap fans at the two
/// open ends.
fn emit_lathe(axis: [f64; 2], profile: &[(f64, f64)], out: &mut Vec<MeshTri>) {
    if profile.len() < 2 {
        return;
    }
    let z_lo = profile.first().map_or(0.0, |s| s.0);
    let z_hi = profile.last().map_or(0.0, |s| s.0);
    let center = DVec3::new(axis[0], axis[1], (z_lo + z_hi) * 0.5);
    // Per-station rings — an empty vec marks an apex.
    let stations: Vec<(f64, Vec<DVec3>)> = profile
        .iter()
        .map(|&(z, r)| {
            if r <= APEX_RADIUS_M {
                (z, Vec::new())
            } else {
                let ring = (0..LATHE_SEGMENTS)
                    .map(|i| {
                        let a = (i as f64) * (2.0 * core::f64::consts::PI) / LATHE_SEGMENTS as f64;
                        DVec3::new(axis[0] + r * a.cos(), axis[1] + r * a.sin(), z)
                    })
                    .collect();
                (z, ring)
            }
        })
        .collect();
    // Side strips between consecutive stations.
    for w in stations.windows(2) {
        let ((z0, r0), (z1, r1)) = (&w[0], &w[1]);
        let apex = |z: f64| DVec3::new(axis[0], axis[1], z);
        for i in 0..LATHE_SEGMENTS {
            let j = (i + 1) % LATHE_SEGMENTS;
            match (r0.is_empty(), r1.is_empty()) {
                (false, false) => {
                    tri(out, r0[i], r0[j], r1[j], center);
                    tri(out, r0[i], r1[j], r1[i], center);
                }
                (true, false) => tri(out, apex(*z0), r1[i], r1[j], center),
                (false, true) => tri(out, r0[i], apex(*z1), r0[j], center),
                (true, true) => {}
            }
        }
    }
    // Flat caps on the two open ends (apexes need none).
    for (z, ring) in [stations.first(), stations.last()].into_iter().flatten() {
        if ring.is_empty() {
            continue;
        }
        let cap = DVec3::new(axis[0], axis[1], *z);
        for i in 0..LATHE_SEGMENTS {
            let j = (i + 1) % LATHE_SEGMENTS;
            tri(out, cap, ring[i], ring[j], center);
        }
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn cylinder_collider(r: f64, h: f64) -> ColliderShape {
        // The resolver's own octagonal prism (resolve::scale_collider):
        // two 8-point rings at ±h — what every cylinder part resolves to.
        const K: f64 = core::f64::consts::FRAC_1_SQRT_2;
        let ring = [
            (1.0, 0.0),
            (K, K),
            (0.0, 1.0),
            (-K, K),
            (-1.0, 0.0),
            (-K, -K),
            (0.0, -1.0),
            (K, -K),
        ];
        ColliderShape::Points(
            ring.iter()
                .flat_map(|&(x, y)| [DVec3::new(r * x, r * y, h), DVec3::new(r * x, r * y, -h)])
                .collect(),
        )
    }

    fn shape_tris(s: &PartShape) -> Vec<MeshTri> {
        let mut t = Vec::new();
        emit(s, &mut t);
        t
    }

    /// A cylinder collider produces a cylinder: two equal-radius
    /// stations and 24·4 = 96 flat-shaded triangles — not the box's 12.
    #[test]
    fn cylinder_collider_lathes_to_cylinder() {
        let s = classify(&cylinder_collider(0.625, 1.0), false, false, None);
        let PartShape::Lathe { profile, .. } = &s else {
            panic!("cylinder collider must classify to a lathe, got {s:?}");
        };
        assert_eq!(profile.len(), 2);
        assert!((profile[0].1 - profile[1].1).abs() < 1e-12);
        let tris = shape_tris(&s);
        assert_eq!(tris.len(), 96, "24 side quads + 2·24 cap fans");
        // The mesh IS the silhouette: its radius matches the collider's.
        let r = tris
            .iter()
            .map(|t| t.a.x.hypot(t.a.y))
            .fold(0.0_f64, f64::max);
        assert!((r - 0.625).abs() < 1e-12);
    }

    /// Engine + crew overrides win over the shared cylinder collider:
    /// the bell's max radius sits at the bottom station (nozzle −z),
    /// the pod tapers toward +z, and neither is the plain cylinder.
    #[test]
    fn engine_and_pod_overrides_differ_from_cylinder() {
        let cyl = cylinder_collider(0.625, 0.9);
        let PartShape::Lathe { profile: bell, .. } = classify(&cyl, true, false, None) else {
            panic!("engine must classify to a lathe");
        };
        // Bell flare opens downward — max radius on the lowest station.
        assert!(bell[0].1 > 0.9 * 0.625);
        assert!(bell.last().unwrap().1 < bell[0].1);
        // And the mount steps back up past the throat — non-monotonic,
        // unlike the pod taper.
        assert!(bell.windows(2).any(|w| w[1].1 > w[0].1));
        let PartShape::Lathe { profile: pod, .. } = classify(&cyl, false, true, None) else {
            panic!("crew part must classify to a lathe");
        };
        // Monotone taper, never under the 10%-of-base floor.
        assert!(pod.windows(2).all(|w| w[1].1 <= w[0].1 + 1e-12));
        assert!(pod.last().unwrap().1 < 0.55 * 0.625);
    }

    /// The authored ring-plus-tip `Convex` (the nose cone's collider
    /// authoring style) reads as an ogive: the last station is a true
    /// apex (zero radius), and intermediate stations curve inward —
    /// not a straight cone and not a box.
    #[test]
    fn ring_plus_tip_lathes_to_ogive() {
        let pts = vec![
            DVec3::new(0.625, 0.0, -0.45),
            DVec3::new(-0.625, 0.0, -0.45),
            DVec3::new(0.0, 0.625, -0.45),
            DVec3::new(0.0, -0.625, -0.45),
            DVec3::new(0.0, 0.0, 0.45),
        ];
        let s = classify(&ColliderShape::Points(pts), false, false, None);
        let PartShape::Lathe { profile, .. } = &s else {
            panic!("cone collider must classify to a lathe, got {s:?}");
        };
        assert_eq!(profile.last().unwrap().1, 0.0, "ogive tip is an apex");
        // Ogive curvature: mid-body radius beats the straight cone's.
        let mid = profile[profile.len() / 2];
        let t = (mid.0 - profile[0].0) / (profile.last().unwrap().0 - profile[0].0);
        assert!(mid.1 > 0.625 * (1.0 - t) + 0.01);
        let tris = shape_tris(&s);
        // 7 ring strips + apex fan + base cap over 8 stations.
        assert_eq!(tris.len(), 6 * 48 + 24 + 24);
    }

    /// Box colliders stay boxes (the battery's shape) and a point set
    /// that is not a clean solid of revolution falls back to one —
    /// the §3.7 floor still bounds every part.
    #[test]
    fn box_and_dirty_sets_fall_back_to_box() {
        let boxed = ColliderShape::Box {
            half_extents: DVec3::new(0.3, 0.3, 0.2),
        };
        let s = classify(&boxed, false, false, None);
        assert!(matches!(s, PartShape::Box { .. }));
        assert_eq!(shape_tris(&s).len(), 12);
        // A z station whose vertices sit at wildly different radii is
        // not a ring — the set is not a solid of revolution.
        let pts = vec![
            DVec3::new(0.6, 0.0, 0.0),
            DVec3::new(-0.6, 0.0, 0.0),
            DVec3::new(0.0, 0.1, 0.0),
            DVec3::new(0.0, -0.1, 0.0),
            DVec3::new(0.0, 0.0, 0.5),
        ];
        let s = classify(&ColliderShape::Points(pts), false, false, None);
        assert!(
            matches!(s, PartShape::Box { .. }),
            "non-revolution set must fall back to box, got {s:?}"
        );
    }

    /// Every emitted triangle's normal points away from the shape
    /// centre — the lambert shading depends on outward normals.
    #[test]
    fn normals_face_outward() {
        for s in [
            classify(&cylinder_collider(0.625, 1.0), false, false, None),
            classify(&cylinder_collider(0.625, 0.9), true, false, None),
            classify(&cylinder_collider(0.625, 0.5), false, true, Some(0.625)),
        ] {
            let (lo, hi) = s.local_aabb();
            let center = (lo + hi) * 0.5;
            for t in shape_tris(&s) {
                let mid = (t.a + t.b + t.c) * (1.0 / 3.0);
                assert!(
                    t.n.dot(mid - center) > 0.0,
                    "inward normal on {t:?} of {s:?}"
                );
            }
        }
    }

    /// `emit_smooth` emits the same triangle stream as `emit` (one
    /// triangle per emitted facet) with unit outward per-vertex
    /// normals: a cylinder's side normals point radially out, its cap
    /// normals along the axis.
    #[test]
    fn smooth_lathe_normals_are_unit_and_outward() {
        let s = classify(&cylinder_collider(0.625, 1.0), false, false, None);
        let mut flat = Vec::new();
        emit(&s, &mut flat);
        let mut smooth = Vec::new();
        emit_smooth(&s, &mut smooth);
        assert_eq!(flat.len(), smooth.len());
        for t in &smooth {
            for (p, n) in [(t.a, t.na), (t.b, t.nb), (t.c, t.nc)] {
                assert!((n.norm() - 1.0).abs() < 1e-9, "non-unit normal {n:?}");
                // The collider is centred on the origin: side normals
                // dot positive with the radial component, cap normals
                // with the axial one — either way `n·p > 0`.
                assert!(n.dot(p) > 0.0, "inward normal {n:?} at {p:?}");
            }
        }
    }

    /// The sheath's hull offset: a lathe inflates radially AND along
    /// the axis ends (apexes round into small rings), a box grows on
    /// every face — the margin is the skin standoff, never an AABB.
    #[test]
    fn inflated_offsets_the_hull() {
        let s = classify(&cylinder_collider(0.625, 1.0), false, false, None);
        let PartShape::Lathe { profile, .. } = inflated(&s, 0.1) else {
            panic!("inflated lathe must stay a lathe");
        };
        assert!((profile[0].1 - 0.725).abs() < 1e-12, "rings fatten");
        assert!((profile[0].0 - -1.1).abs() < 1e-12, "ends extend");
        let boxed = PartShape::Box {
            lo: DVec3::new(-0.3, -0.3, -0.2),
            hi: DVec3::new(0.3, 0.3, 0.2),
        };
        let PartShape::Box { lo, hi } = inflated(&boxed, 0.05) else {
            panic!("inflated box must stay a box");
        };
        assert_eq!(lo, DVec3::new(-0.35, -0.35, -0.25));
        assert_eq!(hi, DVec3::new(0.35, 0.35, 0.25));
    }

    /// A margin wider than half the z-span (flat parts, big plasma
    /// margins) must clamp — extending the ends further would swap
    /// the station order into a self-intersecting shell.
    #[test]
    fn inflated_never_inverts_station_order() {
        let flat = PartShape::Lathe {
            axis: [0.0, 0.0],
            profile: vec![(-0.01, 0.4), (0.0, 0.4), (0.01, 0.4)],
        };
        let PartShape::Lathe { profile, .. } = inflated(&flat, 0.1) else {
            panic!("inflated lathe must stay a lathe");
        };
        for w in profile.windows(2) {
            assert!(
                w[0].0 <= w[1].0,
                "stations inverted: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
    }
}
