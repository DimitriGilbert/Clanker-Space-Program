//! Convex-hull geometry helpers shared by the §2.4.1 voxelizer and the
//! §2.9 vessel-vessel contact generator: facet-plane extraction from a
//! bare vertex set, and point-vs-hull penetration queries.
//!
//! `CompoundCollider` hulls are canonicalized vertex *sets* — no face
//! topology is stored (weld output must stay serde/plain-data). The
//! hull's facet planes are therefore re-detected from the vertex set on
//! demand by the brute-force [`facet_planes`] sweep: `O(n⁴)` in the
//! vertex count, `n` being a convex hull's extreme-point count (8 for
//! the ubiquitous box), so this stays cheap at craft scale — and is a
//! pure function of the vertex set, satisfying §2.4/§7.3's
//! derived-cache rule.

use csp_core::DVec3;

/// Half-open facet plane, outward normal — the containment test
/// primitive for solid hulls: `p` is inside (or on) the hull iff
/// `n·p − d ≤ 0` for every facet plane.
pub(crate) struct Plane {
    /// Unit outward normal.
    pub(crate) n: DVec3,
    /// Plane offset — `n·v` for any vertex `v` on the facet.
    pub(crate) d: f64,
    /// Hull thickness along `n` — `d − min_v(n·v)`, the distance
    /// between this facet and the farthest opposite support. The
    /// penetration rule uses it to reject "far wall" candidates: a
    /// vertex `≥ thick − tol` deep sits on the *opposite* boundary —
    /// grazing there, not penetrating here (the aligned-boxes
    /// distinction).
    pub(crate) thick: f64,
}

/// Detect a hull's facet planes from its vertex set: a vertex triple
/// forms a facet when every other vertex lies on one side. Returns
/// `None` for degenerate inputs the solid path cannot handle
/// (<3 vertices, or a fully coplanar set — those have no interior).
pub(crate) fn facet_planes(verts: &[DVec3], plane_eps: f64) -> Option<Vec<Plane>> {
    if verts.len() < 3 {
        return None;
    }
    let n = verts.len();
    let mut planes = Vec::new();
    for a in 0..n {
        for b in (a + 1)..n {
            for c in (b + 1)..n {
                let normal = (verts[b] - verts[a]).cross(verts[c] - verts[a]);
                let len = normal.norm();
                if len < plane_eps * plane_eps {
                    continue;
                }
                let nu = normal / len;
                let d = nu.dot(verts[a]);
                let mut min_d = f64::INFINITY;
                let mut max_d = f64::NEG_INFINITY;
                for (i, v) in verts.iter().enumerate() {
                    if i == a || i == b || i == c {
                        continue;
                    }
                    let dist = nu.dot(*v) - d;
                    min_d = min_d.min(dist);
                    max_d = max_d.max(dist);
                }
                if max_d <= plane_eps {
                    // Outward `nu`: thickness = −(min_v (nu·v − d)).
                    planes.push(Plane {
                        n: nu,
                        d,
                        thick: -min_d,
                    });
                } else if min_d >= -plane_eps {
                    // Outward `−nu`: thickness = −(min_v (−nu·v + d))
                    // = max_v (nu·v − d).
                    planes.push(Plane {
                        n: -nu,
                        d: -d,
                        thick: max_d,
                    });
                }
            }
        }
    }
    // Coplanar degenerate: every vertex within eps of the first facet
    // plane → the "solid" is a flat polygon.
    let first = planes.first()?;
    let coplanar = verts
        .iter()
        .all(|v| (first.n.dot(*v) - first.d).abs() <= plane_eps);
    if coplanar || planes.len() < 3 {
        return None;
    }
    // Distinct vertex triples of one facet produce duplicate planes —
    // a box's quad facet yields C(4,3) = 4 copies. Merge coplanar
    // duplicates (keep the first — ascending triple order already
    // walked them) so each real facet emits one manifold point, not
    // four: duplicate pair constraints act as parallel springs and
    // over-correct. Compare `(n, d)` at `plane_eps` — the same fuzz
    // as detection — in ascending plane order.
    let mut dedup: Vec<Plane> = Vec::with_capacity(planes.len());
    'outer: for pl in planes {
        for kept in &dedup {
            let same_normal = (kept.n - pl.n).norm() <= plane_eps;
            if same_normal && (kept.d - pl.d).abs() <= plane_eps {
                continue 'outer;
            }
        }
        dedup.push(pl);
    }
    Some(dedup)
}

/// The vertex-contact margin — how far `p` may sit *outside* an
/// untouched facet and still count as inside for the candidate rule.
/// `plane_epsilon` is a numeric fuzz (~1e-9·diag); resting contact
/// drifts vertices across untouched boundary facets by solver-scale
/// amounts (~1e-5 m/tick of tangent motion), so the geometric slop is
/// `1e-3·diag` — millimetre scale for metre-scale hulls, far above
/// solver noise and far below the facets' real extent.
pub(crate) fn vertex_margin(verts: &[DVec3]) -> f64 {
    let Some(first) = verts.first() else {
        return 1e-3;
    };
    let (mut lo, mut hi) = (*first, *first);
    for v in &verts[1..] {
        lo = DVec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
        hi = DVec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
    }
    1e-3 * (hi - lo).norm().max(1.0)
}

/// The facet-detection tolerance shared by the voxelizer and the
/// contact generator: `1e-9·diag` (with a 1 m floor) where `diag` is
/// the hull AABB's diagonal — the scale under which a vertex triple is
/// judged degenerate/coplanar.
pub(crate) fn plane_epsilon(verts: &[DVec3]) -> f64 {
    let Some(first) = verts.first() else {
        return 1e-9;
    };
    let (mut lo, mut hi) = (*first, *first);
    for v in &verts[1..] {
        lo = DVec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
        hi = DVec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
    }
    1e-9 * (hi - lo).norm().max(1.0)
}

/// Point `p` (in the hull's own frame) against a facet-plane set:
/// the list of `(face, depth)` pairs for every facet `p` entered
/// through — empty when `p` is outside or only grazing.
///
/// Two cases, evaluated in order:
///
/// - **Strictly interior** (`depth_i = d_i − n_i·p > 0` for every
///   facet): the single minimum-depth facet — the standard
///   vertex-face MTD reading (a deeply interior point has exactly one
///   meaningful exit).
/// - **On the boundary** (some `depth_j ≤ 0`): **every** facet `f`
///   the vertex is on the near half of — `depth_f > 0`, `p` inside
///   every *other* facet within `tol` (`depth_j ≥ −tol`, `j ≠ f`),
///   and `depth_f < thick_f/2 − tol` (the side of `f`'s slab `p`
///   could only have reached by crossing `f`). Each is a genuine
///   crossing and earns its own manifold point — a vertex inside two
///   slabs must not collapse to the *nearest* face: the shallow graze
///   through a side facet would mask the deep support contact through
///   another (a stacked box's vertex is inside both the support top
///   facet and, by millimetres of lateral drift, a side facet).
///
/// The boundary branch — not "strictly inside every facet" — is what
/// lets **aligned** hulls contact: two same-size boxes with coplanar
/// side faces put the penetrating vertex exactly ON the untouched
/// boundary (`depth_j = 0`), so a strict all-facets-inside test sees
/// a measure-zero boundary point and never generates a manifold even
/// though the volumes genuinely overlap. The half-thickness bound
/// keeps it honest: a vertex merely ON a boundary edge/corner has all
/// its positive depths at the full slab width — none is a real entry
/// facet — while a vertex 1 mm through a face has exactly one shallow
/// candidate. The rule still requires strict `depth_f > 0` on every
/// reported facet: a grazing-only vertex produces nothing, the same
/// zero-authority rule the ground generator applies.
pub(crate) fn point_penetration(planes: &[Plane], p: DVec3, tol: f64) -> Vec<(usize, f64)> {
    let depths: Vec<f64> = planes.iter().map(|pl| pl.d - pl.n.dot(p)).collect();
    if depths.iter().all(|&d| d > 0.0) {
        // Strictly interior: the minimum-depth facet is the MTD.
        let Some((i, d)) = depths
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, &d)| (i, d))
        else {
            return Vec::new();
        };
        return vec![(i, d)];
    }
    let mut out = Vec::new();
    for (i, &df) in depths.iter().enumerate() {
        if df <= 0.0 || df >= planes[i].thick * 0.5 - tol {
            continue;
        }
        let inside_others = depths
            .iter()
            .enumerate()
            .all(|(j, &dj)| j == i || dj >= -tol);
        if inside_others {
            out.push((i, df));
        }
    }
    out
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn box_verts(h: DVec3) -> Vec<DVec3> {
        let mut v = Vec::with_capacity(8);
        for sx in [-1.0, 1.0] {
            for sy in [-1.0, 1.0] {
                for sz in [-1.0, 1.0] {
                    v.push(DVec3::new(h.x * sx, h.y * sy, h.z * sz));
                }
            }
        }
        v
    }

    /// A unit-half box yields its six axis-aligned facet planes, and
    /// `point_penetration` reads inside/on/outside correctly.
    #[test]
    fn box_planes_and_penetration() {
        let verts = box_verts(DVec3::new(1.0, 1.0, 1.0));
        let eps = plane_epsilon(&verts);
        let planes = facet_planes(&verts, eps).unwrap();
        // Each face's 4 coplanar corners contribute C(4,3) = 4 triples
        // re-deriving that face's plane; the dedupe pass merges them —
        // one entry per real facet.
        assert_eq!(planes.len(), 6);
        for pl in &planes {
            let axis_aligned = pl.n.x.abs() > 0.99 || pl.n.y.abs() > 0.99 || pl.n.z.abs() > 0.99;
            assert!(axis_aligned, "non-axis normal: {:?}", pl.n);
            assert!((pl.d - 1.0).abs() < 1e-12);
        }
        // Interior point 0.25 from the +z face.
        let hits = point_penetration(&planes, DVec3::new(0.0, 0.0, 0.75), eps);
        assert_eq!(hits.len(), 1);
        let (face, depth) = hits[0];
        assert!((depth - 0.25).abs() < 1e-12);
        assert!((planes[face].n - DVec3::Z).norm() < 1e-12);
        // Aligned-boundary case: a vertex exactly ON the ±x/±y faces
        // but 0.25 through +z still reports via the +z face — the
        // degenerate same-size-box overlap the candidate rule exists
        // for.
        let hits = point_penetration(&planes, DVec3::new(1.0, -1.0, 0.75), eps);
        assert_eq!(hits.len(), 1);
        let (face, depth) = hits[0];
        assert!((depth - 0.25).abs() < 1e-12);
        assert!((planes[face].n - DVec3::Z).norm() < 1e-12);
        // A vertex ON one boundary facet inside TWO other slabs
        // reports both — the shallow graze must not mask the deeper
        // support entry: x = 1.0 is exactly on +x, y = −0.75 is 0.25
        // inside −y, z = 0.98 is 0.02 inside +z.
        let mut hits = point_penetration(&planes, DVec3::new(1.0, -0.75, 0.98), eps);
        hits.sort_by(|a, b| a.1.total_cmp(&b.1));
        assert_eq!(hits.len(), 2);
        assert!((hits[0].1 - 0.02).abs() < 1e-12);
        assert!((planes[hits[0].0].n - DVec3::Z).norm() < 1e-12);
        assert!((hits[1].1 - 0.25).abs() < 1e-12);
        assert!((planes[hits[1].0].n + DVec3::Y).norm() < 1e-12);
        // Exactly on a face with nothing else penetrated → no
        // positive-depth candidate → grazing, not a contact.
        assert!(point_penetration(&planes, DVec3::new(0.0, 0.0, 1.0), eps).is_empty());
        // Outside → empty.
        assert!(point_penetration(&planes, DVec3::new(0.0, 0.0, 1.5), eps).is_empty());
        assert!(point_penetration(&planes, DVec3::new(3.0, 0.0, 0.0), eps).is_empty());
    }

    /// Degenerate inputs: <3 vertices and coplanar sets have no
    /// interior — `facet_planes` is `None` and such hulls can never be
    /// a penetrated target.
    #[test]
    fn degenerate_hulls_have_no_planes() {
        assert!(facet_planes(&[DVec3::ZERO], 1e-9).is_none());
        assert!(facet_planes(&[DVec3::ZERO, DVec3::X], 1e-9).is_none());
        // Coplanar square — a flat polygon, not a solid.
        let flat = vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(0.0, 1.0, 0.0),
        ];
        assert!(facet_planes(&flat, plane_epsilon(&flat)).is_none());
    }
}
