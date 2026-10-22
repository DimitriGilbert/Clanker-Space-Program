//! §2.4.1 aero model — weld **Stage 2**: isotropic voxelization of the
//! `WeldedBody` collider, a 66-direction geodesic exposure/shielding
//! sweep, and the table-driven force model that replaces M2's
//! pre-voxel analytic drag.
//!
//! # Pieces
//!
//! - [`direction_set`] — the deterministic 66-direction unit sphere.
//!   The plan pins the *count* ("geodesic sphere of 66 directions") and
//!   the *interpolation* ("barycentric over the triangulated direction
//!   hull"). Its parenthetical "(icosahedron, 1 subdivision)" is
//!   arithmetically off — that construction yields 42 vertices
//!   (`10·v²+2`, v=2). The pinned count 66 is produced here by a
//!   **frequency-4 octahedron geodesic** (`4·v²+2`, v=4): 8 faces × 16
//!   subtriangles = 128 triangles over 66 vertices, uniform enough that
//!   stability derivatives differenced from the table stay inside the
//!   exit criterion's 5%.
//! - [`VoxelGrid`]/[`voxelize_craft`] — isotropic voxels of fixed edge length
//!   [`VOXEL_TARGET_EDGE_M`] (0.15 m), capped at [`VOXEL_BUDGET`] total
//!   cells; on overflow the edge doubles until the grid fits and the
//!   table records [`CraftAeroTables::voxel_budget_exceeded`]. The grid
//!   is built **once in body frame**; all 66 directions sweep the same
//!   grid (§2.4.1's "never re-voxelize per direction").
//! - The per-direction sweep — §2.4.1's "sweep rays in rotated
//!   frames", [`sweep_dir`] — runs a 2D ray lattice ⊥ `d` at pitch
//!   `edge` (realized as bins over the projected plane): each lattice
//!   ray credits its foremost occupied cell with `edge²` of projected
//!   area (plus the entry-face centre as a CoP sample and the entry
//!   face's outward normal as a lift sample); later *distinct* parts
//!   on the same ray count `edge²` of blocked area once each. Per
//!   part it produces: total exposed projected area `area[66]` (m²),
//!   the occluded fraction `shielded[66]` (∈ [0,1]), the unblocked
//!   entry-face centroid `cop[66]` (the aero application point), and
//!   the area-weighted mean unblocked-face outward normal
//!   `lift_axis[66]`.
//! - [`build_aero_tables`] assembles a [`CraftAeroTables`] for one
//!   [`ConfigMask`]: per-part tables, the craft reference
//!   area/length, and the diagnostic knot grid (`cd`/`cl`/`cm`
//!   sampled at [`AERO_MACH_KNOTS`] × [`AERO_AOA_KNOTS_RAD`]) the
//!   `Cm_α` exit criterion is checked against.
//! - [`CraftAeroTables::force_torque_body`] is the per-tick runtime:
//!   barycentric-interpolate the directional tables at the body-frame
//!   flow direction, apply the part's authored `cd_curve`/`cl_curve`
//!   at the current Mach plus a Reynolds skin-friction term, and sum
//!   per-part forces at their centres of pressure — drag **and**
//!   torque about the CoM emerge from the same computation.
//!
//! # ConfigMask / shielding semantics (§2.4.1 "per configuration")
//!
//! [`ConfigMask`] is a bitset over the craft's nodes in ascending
//! [`PartNodeId`] order (bit i = i-th node). Bit set = the part is
//! **aero-active**: it rasterizes into the voxel grid, occludes rays,
//! and produces force. Bit clear = the part is treated as absent for
//! aero purposes (jettisoned fairing, opened bay doors, stowed gear):
//! no contribution and no occlusion. `ConfigMask::ALL` is the default
//! every-part-present configuration. Masks beyond 64 nodes are
//! unsupported — bits index `u64`; nodes ≥64 are always active
//! (documented bound).
//!
//! # Force model
//!
//! Per part `p`, flow direction `ŵ` (unit vector, the direction the
//! wind *travels*, `= −v̂_rel` in body frame):
//!
//! ```text
//! A_eff   = area_p(ŵ)·(1 − shielded_p(ŵ))
//! Cd_used = cd_curve_p(Mach) + Cf(Re_p)         // flat-plate skin friction
//! F_p     = q·A_eff·(Cd_used·ŵ + Cl_curve_p(Mach)·l̂_p(ŵ))
//! τ_p     = (CoP_p(ŵ) − CoM) × F_p
//! ```
//!
//! `l̂_p(ŵ)` is the interpolated `lift_axis`'s component ⊥ `ŵ`,
//! **negated** and re-normalized — the table stores *outward* surface
//! normals (into the wind), while pressure pushes the body the other
//! way, so `l̂ = −normalize(n̂ − ŵ(n̂·ŵ))`. Zero when symmetric faces
//! cancel (needle axial, symmetric craft) and maximal for a flat
//! plate face-on, which is what makes the model produce AoA-dependent
//! lift with no extra data. `Cf(Re)` is the
//! flat-plate skin-friction coefficient — laminar `1.328/√Re` below
//! `Re = 5·10⁵`, Prandtl–Schlichting `0.455/log₁₀(Re)^2.58` above —
//! applied to the exposed area (the v1 approximation; per-part wetted
//! area is not voxelized). `Re_p = ρ·|v|·ref_len_p/μ_air` with
//! `μ_air = `[`AIR_VISCOSITY_PA_S`] — `AtmosphereDef` carries no
//! viscosity field in v1, so the v1 constant is documented here;
//! per-body authored viscosity is a data addition for a later
//! milestone.
//!
//! Everything is `f64`, `libm`-only, iterated in fixed orders
//! (ascending `PartNodeId`, fixed cell order, fixed direction and
//! triangle order) — table builds are bit-identical for identical
//! inputs, which is what the Stage-2 debug byte-compare
//! ([`CraftAeroTables::bit_eq`]) verifies.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use csp_core::{DVec3, SimError};
use csp_data::AeroDef;

use crate::graph::{PartGraph, PartNodeId};
use crate::hull::facet_planes;
use crate::part::WeldPart;
use crate::weld::WeldedBody;

/// §2.4.1's pinned direction count — a frequency-4 octahedron
/// geodesic (see the module doc for the icosahedron discrepancy).
pub const AERO_DIRECTION_COUNT: usize = 66;

/// §2.4.1 `target_voxel_m` default — isotropic voxel edge (m).
pub const VOXEL_TARGET_EDGE_M: f64 = 0.15;

/// §2.4.1 `voxel_budget` default — total grid cell cap; the edge
/// doubles until `nx·ny·nz` fits.
pub const VOXEL_BUDGET: usize = 4_000_000;

/// §2.4.1's mask cap — at most this many `ConfigMask` tables per
/// craft; beyond it the runtime resolves to the nearest computed mask
/// by Hamming distance.
pub const MAX_CONFIG_MASKS: usize = 8;

/// The craft-level knot grid's Mach knots (the prompt's "4–6 Mach
/// knots"). Chosen to span the curve range in the authored data
/// (subsonic through hypersonic, with a knee across Mach 1).
pub const AERO_MACH_KNOTS: [f64; 5] = [0.0, 0.8, 1.2, 3.0, 8.0];

/// The craft-level knot grid's angle-of-attack knots, radians —
/// 0°..90° in 15° steps. AoA is measured in the craft's pitch plane:
/// flow `ŵ(α) = −(ẑ·cos α + ŷ·sin α)`, so `α = 0` is the longitudinal
/// (+z) axis pointing into the wind.
pub const AERO_AOA_KNOTS_RAD: [f64; 6] = [
    0.0,
    15.0 * core::f64::consts::PI / 180.0,
    30.0 * core::f64::consts::PI / 180.0,
    45.0 * core::f64::consts::PI / 180.0,
    60.0 * core::f64::consts::PI / 180.0,
    90.0 * core::f64::consts::PI / 180.0,
];

/// v1 air viscosity (Pa·s) for the Reynolds skin-friction term —
/// sea-level-air order of magnitude. `AtmosphereDef` has no viscosity
/// field yet; this constant is the documented v1 stand-in.
pub const AIR_VISCOSITY_PA_S: f64 = 1.8e-5;

/// Laminar/turbulent flat-plate transition for [`skin_friction_cd`].
const RE_TRANSITION: f64 = 5.0e5;

/// Canonical reference conditions for the knot grid: the coefficient
/// table needs a concrete `(ρ, a)` so the skin-friction term has a
/// Reynolds number to read — sea-level standard values, documented.
const KNOT_REF_DENSITY: f64 = 1.225;
const KNOT_REF_SOUND_SPEED: f64 = 340.0;

/// Barycentric-inside tolerance for the direction-hull lookup.
const TRI_EPS: f64 = 1e-9;

/// Voxelizer schema version — bumps when the rasterization/sweep
/// semantics change (§2.4.1's `<voxelizer_version>` cache key).
/// v2: lattice-bin occlusion sweep, `area_m2` = total projected area,
/// tol-0 rasterization + thin-hull dilation, inward lift axis.
pub const VOXELIZER_VERSION: u32 = 2;

/// §2.4.1's `ConfigMask` — a bitset over the craft's nodes in
/// ascending [`PartNodeId`] order (see the module doc). Bits past 63
/// don't exist; nodes at positions ≥ 64 are therefore always
/// aero-active. **No `Default` derive**: `ConfigMask(0)` would mean
/// "no part aero-active" — a silent zero-drag footgun — so the
/// explicit [`Default`] impl is [`ConfigMask::ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfigMask(pub u64);

impl Default for ConfigMask {
    /// The default mask is every part aero-active.
    fn default() -> Self {
        Self::ALL
    }
}

impl ConfigMask {
    /// Every bit set — the default "all parts aero-active" mask.
    pub const ALL: Self = Self(u64::MAX);

    /// `true` when node position `i` is aero-active.
    pub fn active(&self, i: usize) -> bool {
        if i >= 64 {
            return true;
        }
        self.0 & (1u64 << i) != 0
    }

    /// Hamming distance between two masks — §2.4.1's nearest-mask key.
    pub fn hamming(&self, other: Self) -> u32 {
        (self.0 ^ other.0).count_ones()
    }
}

/// A direction-hull triangle: the three direction-set vertices plus
/// the barycentric solve precomputation. For a unit query direction
/// `w`, `wᵢ = (w·eᵢ)·inv_det` are the un-normalized barycentric
/// coordinates (all ≥ 0 inside the triangle); `eᵢ` are the dual basis
/// vectors `dⱼ×dₖ` and `det = d₀·(d₁×d₂)`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DirTri {
    /// Direction indices into [`DirectionSet::dirs`].
    pub v: [u32; 3],
    /// Dual basis: `e[i] = d_j × d_k` for cyclic `(i,j,k)`.
    pub e: [DVec3; 3],
    /// `1/det` — `det = d₀·(d₁×d₂)`, positive for outward windings.
    pub inv_det: f64,
}

/// The 66-direction geodesic set plus its direction hull
/// triangulation — the interpolation structure §2.4.1 names. Built by
/// [`direction_set`]; identical on every build (integer-lattice
/// construction — the only float step is the final normalization).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DirectionSet {
    /// The 66 unit directions, in construction order.
    pub dirs: Vec<DVec3>,
    /// The 128 direction-hull triangles, in construction order.
    pub tris: Vec<DirTri>,
}

impl DirectionSet {
    /// Barycentric-interpolation weights for unit direction `w`:
    /// `(vertex_ids, weights)` with `Σ weights = 1`. The first
    /// triangle (fixed order) whose un-normalized coordinates are all
    /// ≥ `−TRI_EPS` wins — shared edges/vertices therefore resolve
    /// deterministically. Degenerate inputs (non-finite, or no
    /// containing triangle — impossible for a unit `w` on a closed
    /// hull) fall back to the single nearest direction.
    pub fn weights(&self, w: DVec3) -> ([u32; 3], [f64; 3]) {
        if w.is_finite() {
            for t in &self.tris {
                let w0 = w.dot(t.e[0]) * t.inv_det;
                let w1 = w.dot(t.e[1]) * t.inv_det;
                let w2 = w.dot(t.e[2]) * t.inv_det;
                if w0 >= -TRI_EPS && w1 >= -TRI_EPS && w2 >= -TRI_EPS {
                    let s = w0 + w1 + w2;
                    if s > 0.0 {
                        return (t.v, [w0 / s, w1 / s, w2 / s]);
                    }
                }
            }
        }
        // Fallback: nearest single direction by max dot product.
        let mut best = 0u32;
        let mut best_dot = f64::NEG_INFINITY;
        for (i, d) in self.dirs.iter().enumerate() {
            let c = w.dot(*d);
            if c > best_dot {
                best_dot = c;
                best = i as u32;
            }
        }
        ([best, best, best], [1.0, 0.0, 0.0])
    }
}

/// Build the 66-direction geodesic set — see the module doc for why
/// this is a frequency-4 octahedron. Construction is integer-lattice
/// exact until the final normalization, and shared-edge vertices are
/// deduplicated by their integer coordinates, so the output is
/// deterministic and bit-identical on every call.
pub fn direction_set() -> DirectionSet {
    const F: i64 = 4;
    // The octahedron's axis corners as integer vectors.
    let axis = |s: i64, a: usize| -> [i64; 3] {
        let mut v = [0i64; 3];
        v[a] = s;
        v
    };
    let sub = |a: [i64; 3], b: [i64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [i64; 3], b: [i64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [i64; 3], b: [i64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];

    let mut ids: BTreeMap<[i64; 3], u32> = BTreeMap::new();
    let mut dirs: Vec<DVec3> = Vec::with_capacity(AERO_DIRECTION_COUNT);
    let mut tris: Vec<DirTri> = Vec::with_capacity(128);

    // Vertex (i, j) of a face with integer corner vectors (A, B, C):
    // position i·A + j·B + (F−i−j)·C — exact integer coordinates.
    fn vid(
        i: i64,
        j: i64,
        abc: [[i64; 3]; 3],
        f: i64,
        ids: &mut BTreeMap<[i64; 3], u32>,
        dirs: &mut Vec<DVec3>,
    ) -> u32 {
        let [a, b, c] = abc;
        let k = f - i - j;
        let key = [
            i * a[0] + j * b[0] + k * c[0],
            i * a[1] + j * b[1] + k * c[1],
            i * a[2] + j * b[2] + k * c[2],
        ];
        if let Some(&id) = ids.get(&key) {
            return id;
        }
        let v = DVec3::new(key[0] as f64, key[1] as f64, key[2] as f64);
        let n = v.norm();
        let d = if n > 0.0 { v / n } else { DVec3::Z };
        let id = u32::try_from(dirs.len()).unwrap_or(u32::MAX);
        dirs.push(d);
        ids.insert(key, id);
        id
    }

    for sx in [-1i64, 1] {
        for sy in [-1i64, 1] {
            for sz in [-1i64, 1] {
                let a = axis(sx, 0);
                let mut b = axis(sy, 1);
                let mut c = axis(sz, 2);
                // Orient the face outward: (B−A)×(C−A) must point the
                // same way as the face centroid (A+B+C); swap B/C to
                // flip the winding.
                let n = cross(sub(b, a), sub(c, a));
                if dot(
                    n,
                    [a[0] + b[0] + c[0], a[1] + b[1] + c[1], a[2] + b[2] + c[2]],
                ) < 0
                {
                    core::mem::swap(&mut b, &mut c);
                }
                // Lattice vertices, then triangles T1/T2 in fixed
                // (i, j) scan order — 16 subtriangles per face.
                fn emit(tris: &mut Vec<DirTri>, dirs: &[DVec3], x: u32, y: u32, z: u32) {
                    let d0 = dirs[x as usize];
                    let d1 = dirs[y as usize];
                    let d2 = dirs[z as usize];
                    let e0 = d1.cross(d2);
                    let det = d0.dot(e0);
                    if det.abs() < 1e-15 {
                        return;
                    }
                    tris.push(DirTri {
                        v: [x, y, z],
                        e: [e0, d2.cross(d0), d0.cross(d1)],
                        inv_det: 1.0 / det,
                    });
                }
                for i in 0..F {
                    for j in 0..(F - i) {
                        let v0 = vid(i, j, [a, b, c], F, &mut ids, &mut dirs);
                        let v1 = vid(i + 1, j, [a, b, c], F, &mut ids, &mut dirs);
                        let v2 = vid(i, j + 1, [a, b, c], F, &mut ids, &mut dirs);
                        emit(&mut tris, &dirs, v0, v1, v2);
                        if i + j + 1 < F {
                            let v3 = vid(i + 1, j + 1, [a, b, c], F, &mut ids, &mut dirs);
                            emit(&mut tris, &dirs, v1, v3, v2);
                        }
                    }
                }
            }
        }
    }
    DirectionSet { dirs, tris }
}

/// Why a Stage-2 build can fail — the typed boundary per §2.13.
/// Practically unreachable on weld-validated input (finite hulls,
/// in-bounds effective indices); each variant maps to
/// `SimError::SaveBlocking`/`Recoverable` like the weld's own errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AeroError {
    /// A graph node's `effective_index` is out of bounds for `parts`.
    EffectiveIndexOutOfRange {
        /// The offending node.
        node: PartNodeId,
        /// The index it carried.
        index: u32,
        /// `parts.len()`.
        len: usize,
    },
    /// `body.part_index` has no entry for a graph node — the body was
    /// not welded from this graph.
    MissingPartWeld {
        /// The unmapped node.
        node: PartNodeId,
    },
    /// A computed quantity came out non-finite despite finite inputs.
    NonFinite(&'static str),
}

impl AeroError {
    /// Static reason text for the `SimError` mapping (§2.13).
    pub fn reason(self) -> &'static str {
        match self {
            Self::EffectiveIndexOutOfRange { .. } => "aero: effective index out of range",
            Self::MissingPartWeld { .. } => "aero: part_index misses a graph node",
            Self::NonFinite(f) => f,
        }
    }
}

impl fmt::Display for AeroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason())
    }
}

impl std::error::Error for AeroError {}

impl From<AeroError> for SimError {
    fn from(e: AeroError) -> Self {
        SimError::SaveBlocking(e.reason())
    }
}

/// `part_of` empty sentinel.
const EMPTY: u32 = u32::MAX;

/// The voxelized collider — one flat cell array with per-cell owning
/// part (slot into the build's active-node list) and the occupied-cell
/// enumeration in fixed order. Body frame, isotropic `edge`.
struct VoxelGrid {
    edge: f64,
    /// Grid minimum corner, body frame.
    origin: DVec3,
    dims: [usize; 3],
    /// Per cell: owning part slot or [`EMPTY`].
    part_of: Vec<u32>,
    /// Flat indices of occupied cells, in insertion order — per part,
    /// `rasterize_hull`'s ascending `i→j→k` scan (first-marked wins, so
    /// the order is deterministic but not globally sorted by index).
    occupied: Vec<u32>,
}

impl VoxelGrid {
    fn index(&self, i: i64, j: i64, k: i64) -> usize {
        (k as usize * self.dims[1] + j as usize) * self.dims[0] + i as usize
    }

    fn in_bounds(&self, i: i64, j: i64, k: i64) -> bool {
        i >= 0
            && j >= 0
            && k >= 0
            && (i as usize) < self.dims[0]
            && (j as usize) < self.dims[1]
            && (k as usize) < self.dims[2]
    }

    fn center(&self, i: i64, j: i64, k: i64) -> DVec3 {
        self.origin
            + DVec3::new(
                (i as f64 + 0.5) * self.edge,
                (j as f64 + 0.5) * self.edge,
                (k as f64 + 0.5) * self.edge,
            )
    }

    fn coords(&self, idx: u32) -> (i64, i64, i64) {
        let idx = idx as usize;
        let i = idx % self.dims[0];
        let j = (idx / self.dims[0]) % self.dims[1];
        let k = idx / (self.dims[0] * self.dims[1]);
        (i as i64, j as i64, k as i64)
    }

    fn occupied_at(&self, i: i64, j: i64, k: i64) -> bool {
        self.in_bounds(i, j, k) && self.part_of[self.index(i, j, k)] != EMPTY
    }
}

/// Point-in-triangle test (3D): `p` is within `tol` metres of the
/// triangle's plane and inside all three edge half-planes expanded by
/// `tol`. The edge terms are normalized by edge length so the
/// tolerance is in metres, not m².
fn in_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3, tol: f64) -> bool {
    let n = (b - a).cross(c - a);
    let nl = n.norm();
    if nl < 1e-15 {
        return false;
    }
    let nu = n / nl;
    if (nu.dot(p - a)).abs() > tol {
        return false;
    }
    let edge_test = |u: DVec3, v: DVec3| -> bool {
        let e = v - u;
        let el = e.norm();
        if el < 1e-15 {
            return true;
        }
        e.cross(p - u).dot(nu) >= -tol * el
    };
    edge_test(a, b) && edge_test(b, c) && edge_test(c, a)
}

/// Rasterize one hull's body-frame vertex set into `grid`, stamping
/// `slot` into every cell whose centre lies within `tol` of the convex
/// volume — solid containment via [`facet_planes`], a flat-polygon fan
/// for coplanar/degenerate hulls, and vertex/segment coverage for
/// point and line colliders. First-marked wins: a cell already owned
/// by an earlier part keeps that part (fixed ascending order).
/// Returns the number of cells this call marked; the caller runs the
/// strict `tol = 0` pass first and re-runs with a half-cell-diagonal
/// `tol` only when that pass marked nothing — the one-layer dilation
/// confined to sub-cell-thin hulls so they still occlude and produce
/// area. `tol` therefore enters both the candidate-range expansion and
/// the containment tolerance.
fn rasterize_hull(grid: &mut VoxelGrid, verts: &[DVec3], slot: u32, tol: f64) -> usize {
    if verts.is_empty() {
        return 0;
    }
    let mut marked = 0usize;
    // Hull AABB expanded by tol → the candidate cell range.
    let (mut lo, mut hi) = (verts[0], verts[0]);
    for v in &verts[1..] {
        lo = DVec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
        hi = DVec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
    }
    let e = grid.edge;
    let plane_eps = crate::hull::plane_epsilon(verts);
    let idx = |p: f64, o: f64| -> i64 { libm::floor((p - o) / e) as i64 };
    let i0 = idx(lo.x - tol, grid.origin.x);
    let i1 = idx(hi.x + tol, grid.origin.x);
    let j0 = idx(lo.y - tol, grid.origin.y);
    let j1 = idx(hi.y + tol, grid.origin.y);
    let k0 = idx(lo.z - tol, grid.origin.z);
    let k1 = idx(hi.z + tol, grid.origin.z);

    let planes = facet_planes(verts, plane_eps);
    let mut mark = |grid: &mut VoxelGrid, i: i64, j: i64, k: i64| {
        if !grid.in_bounds(i, j, k) {
            return;
        }
        let ix = grid.index(i, j, k);
        if grid.part_of[ix] != EMPTY {
            return;
        }
        grid.part_of[ix] = slot;
        grid.occupied.push(ix as u32);
        marked += 1;
    };

    match &planes {
        Some(pl) => {
            for i in i0..=i1 {
                for j in j0..=j1 {
                    for k in k0..=k1 {
                        let c = grid.center(i, j, k);
                        if pl.iter().all(|p| p.n.dot(c) - p.d <= tol) {
                            mark(grid, i, j, k);
                        }
                    }
                }
            }
        }
        None => {
            // Flat/degenerate path: the hull is a point, segment, or
            // coplanar polygon. Cover it by triangles fanned over all
            // vertex pairs — the union of (v0, vi, vj) equals the
            // convex polygon for coplanar sets, and for n < 3 the
            // segments/points alone mark their cells.
            let seg_dist = |c: DVec3, a: DVec3, b: DVec3| -> f64 {
                let ab = b - a;
                let l2 = ab.norm_squared();
                if l2 < 1e-15 {
                    return (c - a).norm();
                }
                let t = ((c - a).dot(ab) / l2).clamp(0.0, 1.0);
                (c - (a + ab * t)).norm()
            };
            for i in i0..=i1 {
                for j in j0..=j1 {
                    for k in k0..=k1 {
                        let c = grid.center(i, j, k);
                        let near_vertex = verts.iter().any(|v| (*v - c).norm() <= tol);
                        let near_edge = verts.len() >= 2
                            && (0..verts.len() - 1).any(|a| {
                                (a + 1..verts.len()).any(|b| seg_dist(c, verts[a], verts[b]) <= tol)
                            });
                        let near_tri = verts.len() >= 3
                            && (1..verts.len() - 1).any(|a| {
                                (a + 1..verts.len())
                                    .any(|b| in_triangle(c, verts[0], verts[a], verts[b], tol))
                            });
                        if near_vertex || near_edge || near_tri {
                            mark(grid, i, j, k);
                        }
                    }
                }
            }
        }
    }
    marked
}

/// Per-part directional accumulation from one direction sweep — the
/// intermediate that becomes a [`PartAeroTable`].
struct SweepAccum {
    /// Unblocked projected area per direction (m²) — lattice rays
    /// where this part is the foremost hit.
    area: [f64; AERO_DIRECTION_COUNT],
    /// Projected area occluded by another part per direction (m²) —
    /// lattice rays where this part sits behind a different part's
    /// silhouette. The part's *total* exposed projected area is
    /// `area + blocked`; the shielded fraction is
    /// `blocked/(area+blocked)`.
    blocked: [f64; AERO_DIRECTION_COUNT],
    /// Area-weighted exposed-face centroid numerator, per direction.
    cop_num: [DVec3; AERO_DIRECTION_COUNT],
    /// Area-weighted exposed-face normal numerator, per direction.
    nrm_num: [DVec3; AERO_DIRECTION_COUNT],
}

impl Default for SweepAccum {
    fn default() -> Self {
        Self {
            area: [0.0; AERO_DIRECTION_COUNT],
            blocked: [0.0; AERO_DIRECTION_COUNT],
            cop_num: [DVec3::ZERO; AERO_DIRECTION_COUNT],
            nrm_num: [DVec3::ZERO; AERO_DIRECTION_COUNT],
        }
    }
}

/// One part's §2.4.1 output: `AeroTable { area[66], shield[66],
/// ref_len }` — extended with the application point (`cop`) and mean
/// exposed-normal (`lift_axis`) the per-part force site needs, plus
/// the authored [`AeroDef`] the Mach/Re curves come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartAeroTable {
    /// The graph node this table belongs to.
    pub node: PartNodeId,
    /// The resolved authored curves + reference length (carried so
    /// the runtime needs no second lookup — tables are self-contained).
    pub aero: AeroDef,
    /// Total exposed projected area per direction (m²), len
    /// [`AERO_DIRECTION_COUNT`] — the part's full upwind silhouette
    /// *including* the occluded share. The force-producing area is
    /// `area_m2·(1 − shielded)`; keeping the total here is what makes
    /// that product the unblocked area rather than double-counted
    /// shielding.
    pub area_m2: Vec<f64>,
    /// Fraction of the part's exposed projected area occluded by
    /// upwind voxels of *other* parts per direction ∈ [0,1] (the
    /// §2.4.1 shielding rule: occluded area produces no force).
    pub shielded: Vec<f64>,
    /// Area-weighted centre of the part's *unblocked* entry faces
    /// per direction, body frame — the aero force application point.
    /// The part's body-frame CoM when the unblocked area is zero
    /// (unused then).
    pub cop: Vec<DVec3>,
    /// Area-weighted mean of the unblocked entry faces' *outward*
    /// normals per direction — `‖·‖ ≤ 1` (a mean of unit vectors,
    /// e.g. ‖0.707‖ over a 45°-split silhouette, not itself a unit
    /// vector). The lift axis is the *negated* component ⊥ flow,
    /// re-normalized at query time — pressure pushes inward.
    pub lift_axis: Vec<DVec3>,
}

/// §2.4.1 Stage-2 output for one `ConfigMask` — installed on the
/// vessel by command (or computed sim-side on the same deterministic
/// path) and attached to the `WeldedBody`. **Never serialized or
/// hashed**: a pure function of `(craft, mask)`, re-derived on load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CraftAeroTables {
    /// [`VOXELIZER_VERSION`] — the cache-key component.
    pub voxelizer_version: u32,
    /// The craft identity this table was built for — the install
    /// command's binding check (`u64` of the canonical craft hash).
    pub graph_hash: u64,
    /// The configuration the table was built under.
    pub mask: ConfigMask,
    /// The welded body's `com_local` at build time — the torque
    /// reference; the weld-derived field it mirrors is a pure function
    /// of the same craft, so it cannot drift.
    pub com_local: DVec3,
    /// The geodesic direction hull the per-part arrays index.
    pub directions: DirectionSet,
    /// One table per aero-active node, ascending `PartNodeId`.
    pub parts: Vec<PartAeroTable>,
    /// The voxel edge actually used (m) — above
    /// [`VOXEL_TARGET_EDGE_M`] when the budget forced doubling.
    pub voxel_edge_m: f64,
    /// `true` when the grid exceeded [`VOXEL_BUDGET`] at 0.15 m and
    /// the edge had to double (the editor-facing warning flag).
    pub voxel_budget_exceeded: bool,
    /// Craft reference area (m²): the max over directions of the
    /// per-part total exposed projected area summed across parts —
    /// the `A_ref` the knot grid's coefficients normalize against.
    pub ref_area_m2: f64,
    /// Craft reference length (m): the collider AABB's largest
    /// dimension — the `L_ref` for `Cm`.
    pub ref_len_m: f64,
    /// Knot grid: `cd[aoa_i][mach_j]`, `aoa_i` major over
    /// [`AERO_AOA_KNOTS_RAD`], `mach_j` minor over [`AERO_MACH_KNOTS`].
    /// Diagnostic/table artifact — evaluated at canonical reference
    /// conditions ([`KNOT_REF_DENSITY`], [`KNOT_REF_SOUND_SPEED`]).
    pub cd_knots: Vec<f64>,
    /// See [`Self::cd_knots`].
    pub cl_knots: Vec<f64>,
    /// Pitch-moment coefficient about body +x at each knot —
    /// `Cm = τ·x̂/(q·A_ref·L_ref)`. See [`Self::cd_knots`].
    pub cm_knots: Vec<f64>,
}

impl CraftAeroTables {
    /// The per-tick force model (module doc): `(force, torque)` in
    /// **body frame** — the backend rotates both into the bubble frame.
    /// `w` is the unit flow direction (`−v̂_rel` body frame), `q` the
    /// dynamic pressure `½ρv²`, `mach` the Mach number, `re_per_len`
    /// the Reynolds number per metre `ρ·|v|/μ_air` (each part's `Re`
    /// is `re_per_len·ref_len_p`). Torque is about the body CoM.
    pub fn force_torque_body(
        &self,
        w: DVec3,
        q: f64,
        mach: f64,
        re_per_len: f64,
    ) -> (DVec3, DVec3) {
        let (ids, wt) = self.directions.weights(w);
        let mut force = DVec3::ZERO;
        let mut torque = DVec3::ZERO;
        for p in &self.parts {
            let Some((fp, cop)) = Self::part_force(p, w, ids, wt, q, mach, re_per_len) else {
                continue;
            };
            force = force + fp;
            torque = torque + (cop - self.com_local).cross(fp);
        }
        (force, torque)
    }

    /// One part's aero force + application point (body frame) —
    /// [`None`] when the part's effective exposed area is zero.
    /// Shared by [`Self::force_torque_body`] (identical arithmetic,
    /// so the total is bitwise unchanged) and
    /// [`Self::part_forces_body`].
    fn part_force(
        p: &PartAeroTable,
        w: DVec3,
        ids: [u32; 3],
        wt: [f64; 3],
        q: f64,
        mach: f64,
        re_per_len: f64,
    ) -> Option<(DVec3, DVec3)> {
        let (i0, i1, i2) = (ids[0] as usize, ids[1] as usize, ids[2] as usize);
        let area = wt[0] * p.area_m2[i0] + wt[1] * p.area_m2[i1] + wt[2] * p.area_m2[i2];
        let shield = wt[0] * p.shielded[i0] + wt[1] * p.shielded[i1] + wt[2] * p.shielded[i2];
        let a_eff = area * (1.0 - shield);
        if a_eff <= 0.0 {
            return None;
        }
        let cop = p.cop[i0] * wt[0] + p.cop[i1] * wt[1] + p.cop[i2] * wt[2];
        let nrm = p.lift_axis[i0] * wt[0] + p.lift_axis[i1] * wt[1] + p.lift_axis[i2] * wt[2];
        let lift = {
            let l = nrm - w * nrm.dot(w);
            let n = l.norm();
            if n > 1e-12 { -l / n } else { DVec3::ZERO }
        };
        let cd = eval_aero_curve(&p.aero.cd_curve, mach)
            + skin_friction_cd(re_per_len * p.aero.ref_len_m);
        let cl = eval_aero_curve(&p.aero.cl_curve, mach);
        Some(((w * cd + lift * cl) * (q * a_eff), cop))
    }

    /// The per-part breakdown of [`Self::force_torque_body`] —
    /// `(node, force, application point)` per aero-active part in
    /// ascending `PartNodeId` order, same arithmetic so the vector
    /// sum is bitwise the total force. The §2.4 breakup machinery's
    /// structural evaluation needs per-part aero loads, not just the
    /// body total.
    pub fn part_forces_body(
        &self,
        w: DVec3,
        q: f64,
        mach: f64,
        re_per_len: f64,
    ) -> Vec<(PartNodeId, DVec3, DVec3)> {
        let (ids, wt) = self.directions.weights(w);
        self.parts
            .iter()
            .filter_map(|p| {
                Self::part_force(p, w, ids, wt, q, mach, re_per_len)
                    .map(|(f, cop)| (p.node, f, cop))
            })
            .collect()
    }

    /// Pitch-moment derivative `∂Cm/∂α` (per radian) at a Mach-knot
    /// index — the table-derived stability derivative the exit
    /// criterion compares to a direct computation.
    pub fn cm_alpha(&self, mach_idx: usize) -> f64 {
        let i0 = mach_idx;
        let i1 = AERO_MACH_KNOTS.len() + mach_idx;
        (self.cm_knots[i1] - self.cm_knots[i0]) / (AERO_AOA_KNOTS_RAD[1] - AERO_AOA_KNOTS_RAD[0])
    }

    /// The Stage-2 byte-compare (§2.4: "debug builds byte-compare a
    /// worker-computed table against a sim-thread recompute"). Full
    /// structural equality at `f64::to_bits` resolution — serde is
    /// excluded (the table never enters any hashed stream anyway).
    pub fn bit_eq(&self, other: &Self) -> bool {
        fn fv(a: &[f64], b: &[f64]) -> bool {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
        }
        fn vv(a: &[DVec3], b: &[DVec3]) -> bool {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| {
                    x.x.to_bits() == y.x.to_bits()
                        && x.y.to_bits() == y.y.to_bits()
                        && x.z.to_bits() == y.z.to_bits()
                })
        }
        fn knots(a: &[csp_data::AeroKnot], b: &[csp_data::AeroKnot]) -> bool {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| {
                    x.mach.to_bits() == y.mach.to_bits() && x.coeff.to_bits() == y.coeff.to_bits()
                })
        }
        fn pt(a: &PartAeroTable, b: &PartAeroTable) -> bool {
            a.node == b.node
                && knots(&a.aero.cd_curve, &b.aero.cd_curve)
                && knots(&a.aero.cl_curve, &b.aero.cl_curve)
                && a.aero.ref_len_m.to_bits() == b.aero.ref_len_m.to_bits()
                && fv(&a.area_m2, &b.area_m2)
                && fv(&a.shielded, &b.shielded)
                && vv(&a.cop, &b.cop)
                && vv(&a.lift_axis, &b.lift_axis)
        }
        self.voxelizer_version == other.voxelizer_version
            && self.graph_hash == other.graph_hash
            && self.mask == other.mask
            && self.voxel_edge_m.to_bits() == other.voxel_edge_m.to_bits()
            && self.voxel_budget_exceeded == other.voxel_budget_exceeded
            && self.ref_area_m2.to_bits() == other.ref_area_m2.to_bits()
            && self.ref_len_m.to_bits() == other.ref_len_m.to_bits()
            && self.com_local.x.to_bits() == other.com_local.x.to_bits()
            && self.com_local.y.to_bits() == other.com_local.y.to_bits()
            && self.com_local.z.to_bits() == other.com_local.z.to_bits()
            && self.dirs_bit_eq(other)
            && self.parts.len() == other.parts.len()
            && self.parts.iter().zip(&other.parts).all(|(a, b)| pt(a, b))
            && fv(&self.cd_knots, &other.cd_knots)
            && fv(&self.cl_knots, &other.cl_knots)
            && fv(&self.cm_knots, &other.cm_knots)
    }

    fn dirs_bit_eq(&self, other: &Self) -> bool {
        let (a, b) = (&self.directions, &other.directions);
        a.dirs.len() == b.dirs.len()
            && a.dirs.iter().zip(&b.dirs).all(|(x, y)| {
                x.x.to_bits() == y.x.to_bits()
                    && x.y.to_bits() == y.y.to_bits()
                    && x.z.to_bits() == y.z.to_bits()
            })
            && a.tris.len() == b.tris.len()
            && a.tris.iter().zip(&b.tris).all(|(x, y)| {
                x.v == y.v
                    && x.e.iter().zip(&y.e).all(|(u, v)| {
                        u.x.to_bits() == v.x.to_bits()
                            && u.y.to_bits() == v.y.to_bits()
                            && u.z.to_bits() == v.z.to_bits()
                    })
                    && x.inv_det.to_bits() == y.inv_det.to_bits()
            })
    }
}

/// Piecewise-linear Mach → coefficient curve (§3.1 `AeroKnot`
/// semantics — the temperature-table convention): clamped to the ends
/// outside the knots; an empty curve yields `0.0` (hand-authored
/// `WeldPart`s may carry no aero data — geometry still blocks rays,
/// it just produces no force).
pub fn eval_aero_curve(curve: &[csp_data::AeroKnot], mach: f64) -> f64 {
    let Some(first) = curve.first() else {
        return 0.0;
    };
    if mach <= first.mach {
        return first.coeff;
    }
    for w in curve.windows(2) {
        let (a, b) = (w[0], w[1]);
        if mach <= b.mach {
            let span = b.mach - a.mach;
            if span <= 0.0 {
                return a.coeff;
            }
            let t = (mach - a.mach) / span;
            return a.coeff + t * (b.coeff - a.coeff);
        }
    }
    curve.last().map_or(first.coeff, |k| k.coeff)
}

/// Flat-plate skin-friction coefficient vs Reynolds number — laminar
/// `1.328/√Re` below [`RE_TRANSITION`], Prandtl–Schlichting
/// `0.455/log₁₀(Re)^2.58` above. `Re < 10³` contributes nothing
/// (creeping flow is not a flight regime). Added onto the authored
/// `Cd` — the "Mach/Re-resolved" half of the model; magnitude ~0.003
/// at reentry Reynolds numbers.
pub fn skin_friction_cd(re: f64) -> f64 {
    if !re.is_finite() || re < 1.0e3 {
        return 0.0;
    }
    if re < RE_TRANSITION {
        1.328 / libm::sqrt(re)
    } else {
        0.455 / libm::pow(libm::log10(re), 2.58)
    }
}

/// The flow direction for a pitch-plane angle of attack `alpha`
/// (radians): `ŵ(α) = −(ẑ·cosα + ŷ·sinα)` — `α = 0` puts air arriving
/// from ahead along −z (the craft moving nose-first along +z).
pub fn flow_dir_at_aoa(alpha: f64) -> DVec3 {
    let (s, c) = (libm::sin(alpha), libm::cos(alpha));
    let w = DVec3::new(0.0, -s, -c);
    let n = w.norm();
    if n > 0.0 {
        w / n
    } else {
        DVec3::new(0.0, 0.0, -1.0)
    }
}

/// One aero-active node's weld binding — voxelizer input.
struct Active<'a> {
    node: PartNodeId,
    part: &'a WeldPart,
    hull: u32,
}

/// The mask-filtered active-node list in ascending `PartNodeId` —
/// bit i of `mask` covers graph position i.
fn collect_active<'a>(
    body: &WeldedBody,
    graph: &PartGraph,
    parts: &'a [WeldPart],
    mask: ConfigMask,
) -> Result<Vec<Active<'a>>, AeroError> {
    let mut active = Vec::new();
    for (i, (id, node)) in graph.nodes.iter().enumerate() {
        if !mask.active(i) {
            continue;
        }
        let eff = node.effective_index as usize;
        let Some(part) = parts.get(eff) else {
            return Err(AeroError::EffectiveIndexOutOfRange {
                node: *id,
                index: node.effective_index,
                len: parts.len(),
            });
        };
        let Some(w) = body.part_index.get(id) else {
            return Err(AeroError::MissingPartWeld { node: *id });
        };
        active.push(Active {
            node: *id,
            part,
            hull: w.hull_index,
        });
    }
    Ok(active)
}

/// The voxelization step: AABB over the active hulls, isotropic grid
/// at [`VOXEL_TARGET_EDGE_M`] with the §2.4.1 budget-doubling rule,
/// and rasterization in ascending-node order. Returns the grid, the
/// world AABB, and the budget flag.
fn voxelize_craft(body: &WeldedBody, active: &[Active<'_>]) -> (VoxelGrid, DVec3, DVec3, bool) {
    let mut lo = DVec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut hi = DVec3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for a in active {
        let hull = &body.collider.hulls[a.hull as usize];
        for v in &hull.vertices {
            lo = DVec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
            hi = DVec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
        }
    }
    let mut edge = VOXEL_TARGET_EDGE_M;
    let mut dims = [0usize; 3];
    let mut exceeded = false;
    loop {
        let span = hi - lo;
        dims[0] = (libm::ceil(span.x.max(0.0) / edge) as usize).max(1);
        dims[1] = (libm::ceil(span.y.max(0.0) / edge) as usize).max(1);
        dims[2] = (libm::ceil(span.z.max(0.0) / edge) as usize).max(1);
        if dims[0] * dims[1] * dims[2] <= VOXEL_BUDGET {
            break;
        }
        exceeded = true;
        edge *= 2.0;
    }
    let mut grid = VoxelGrid {
        edge,
        origin: lo,
        dims,
        part_of: vec![EMPTY; dims[0] * dims[1] * dims[2]],
        occupied: Vec::new(),
    };
    // Strict centre-in-hull pass (`tol = 0`): a cell is occupied only
    // when its centre lies inside the hull — no blanket dilation, so
    // thick parts rasterize at their true silhouette. A hull that
    // marks nothing at `tol = 0` is sub-cell-thin (no cell centre
    // falls inside it); it alone gets one half-cell-diagonal dilation
    // pass so thin plates/fins still occlude and produce area — the
    // bias is explicit and confined to hulls that would otherwise
    // voxelize to nothing.
    let dilate = edge * libm::sqrt(3.0) / 2.0;
    for (slot, a) in active.iter().enumerate() {
        let hull = &body.collider.hulls[a.hull as usize];
        if rasterize_hull(&mut grid, &hull.vertices, slot as u32, 0.0) == 0 {
            rasterize_hull(&mut grid, &hull.vertices, slot as u32, dilate);
        }
    }
    (grid, lo, hi, exceeded)
}

/// One part's accumulations for a single sweep direction — what
/// [`sweep_dir`] returns per slot and [`sweep_directions`] scatters
/// into the per-direction [`SweepAccum`] rows. `area` is the
/// *unblocked* projected area (m²), `blocked` the projected area
/// sitting behind another part's silhouette, `cop_num`/`nrm_num` the
/// area-weighted entry-face centre / outward-normal sums over the
/// credited cells.
#[derive(Clone, Copy, Default)]
struct DirSample {
    area: f64,
    blocked: f64,
    cop_num: DVec3,
    nrm_num: DVec3,
}

/// A part's foremost cell inside one lattice bin — the ray-ordering
/// unit [`sweep_dir`] sorts on.
#[derive(Clone, Copy)]
struct BinHit {
    /// Owning part slot.
    slot: u32,
    /// Along-ray position `c·d` — least is foremost (upwind-most).
    pw: f64,
    /// The flat cell index achieving `pw`.
    cell: u32,
}

/// A deterministic orthonormal basis `(u, v)` of the plane ⊥ `d`
/// (unit): the lattice axes the per-direction sweep bins projected
/// cell centres onto. `u` is crossed out of the coordinate axis
/// *least* aligned with `d` (lowest index on ties) so the cross
/// product stays well-conditioned (`≥ √(2/3)`); `v = d × u` completes
/// the frame.
fn ray_basis(d: DVec3) -> (DVec3, DVec3) {
    let ax = [d.x.abs(), d.y.abs(), d.z.abs()];
    let r = if ax[0] <= ax[1] && ax[0] <= ax[2] {
        DVec3::X
    } else if ax[1] <= ax[2] {
        DVec3::Y
    } else {
        DVec3::Z
    };
    let uc = r.cross(d);
    let u = uc / uc.norm();
    (u, d.cross(u))
}

/// The upwind entry face of a lattice-credited cell — the CoP/lift
/// sample the sweep keeps as `(face centre, outward normal)`. Among
/// the upwind-facing sides (outward normal `−sign(dₐ)·axisₐ` for each
/// axis `d` has a nonzero component on), prefer a genuinely *exposed*
/// face — an empty neighbour across it — checked in decreasing `|dₐ|`
/// order (lowest axis index on ties). When every upwind side has an
/// occupied neighbour — a binning artifact where a ray's foremost
/// cell is interior — the dominant-axis face stands in: the sample is
/// still within a cell of the surface the ray actually pierced.
fn entry_face(grid: &VoxelGrid, cell: u32, d: DVec3) -> (DVec3, DVec3) {
    let (i, j, k) = grid.coords(cell);
    let c = grid.center(i, j, k);
    let da = [d.x, d.y, d.z];
    let e = grid.edge;
    let face = |a: usize| -> (DVec3, DVec3) {
        let s = da[a].signum();
        let (n, f) = match a {
            0 => (DVec3::new(-s, 0.0, 0.0), DVec3::new(-s * e * 0.5, 0.0, 0.0)),
            1 => (DVec3::new(0.0, -s, 0.0), DVec3::new(0.0, -s * e * 0.5, 0.0)),
            _ => (DVec3::new(0.0, 0.0, -s), DVec3::new(0.0, 0.0, -s * e * 0.5)),
        };
        (c + f, n)
    };
    let mut order = [0usize, 1, 2];
    order.sort_by(|&a, &b| {
        da[b]
            .abs()
            .partial_cmp(&da[a].abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for &a in &order {
        if da[a].abs() <= 1e-12 {
            continue;
        }
        let s = da[a].signum() as i64;
        let (ni, nj, nk) = match a {
            0 => (i - s, j, k),
            1 => (i, j - s, k),
            _ => (i, j, k - s),
        };
        if !grid.occupied_at(ni, nj, nk) {
            return face(a);
        }
    }
    face(order[0])
}

/// One direction of §2.4.1's "sweep rays in rotated frames": a 2D ray
/// lattice ⊥ `d` at pitch `edge`, realized as *bins* — every occupied
/// cell belongs to the lattice ray containing its projected centre,
/// and each ray credits its **foremost** cell (least `c·d`) with
/// `edge²` of projected area, that cell's entry-face centre as a CoP
/// sample, and the entry face's outward normal as a lift sample.
/// Later *distinct* parts on the same ray count `edge²` of `blocked`
/// area once each; further cells of parts already accounted for
/// contribute nothing — a part's projection onto one ray is one
/// `edge²` patch.
///
/// Binning rather than a literal per-ray DDA is what makes the model
/// exhaustive: every occupied cell lands on exactly one ray, so no
/// thin feature slips between lattice lines, no first-step
/// self-shielding exists (the foremost cell is *credited*, never
/// occluded — grazing rays cannot eat a part's own silhouette), and
/// ordering is an explicit `(c·d, slot)` sort, not float-tie luck on a
/// march. For axis-aligned `d` each grid column is exactly one bin,
/// so the counted projected area is exact; off-axis it is a pitch-`e`
/// sample with no first-cell bias.
fn sweep_dir(grid: &VoxelGrid, d: DVec3, n_parts: usize) -> Vec<DirSample> {
    let (u, v) = ray_basis(d);
    let e = grid.edge;
    let e2 = e * e;
    let mut out = vec![DirSample::default(); n_parts];
    // bin → per-part foremost cell (kept at the least `c·d` seen).
    let mut bins: BTreeMap<(i64, i64), Vec<BinHit>> = BTreeMap::new();
    for &ci in &grid.occupied {
        let (i, j, k) = grid.coords(ci);
        let c = grid.center(i, j, k);
        let bin = (
            libm::floor(c.dot(u) / e) as i64,
            libm::floor(c.dot(v) / e) as i64,
        );
        let pw = c.dot(d);
        let slot = grid.part_of[ci as usize];
        let hits = bins.entry(bin).or_default();
        match hits.iter_mut().find(|h| h.slot == slot) {
            Some(h) if pw < h.pw => {
                h.pw = pw;
                h.cell = ci;
            }
            Some(_) => {}
            None => hits.push(BinHit { slot, pw, cell: ci }),
        }
    }
    for (_, mut hits) in bins {
        hits.sort_by(|a, b| {
            a.pw.partial_cmp(&b.pw)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.slot.cmp(&b.slot))
        });
        let (fc, nrm) = entry_face(grid, hits[0].cell, d);
        let first = &mut out[hits[0].slot as usize];
        first.area += e2;
        first.cop_num = first.cop_num + fc * e2;
        first.nrm_num = first.nrm_num + nrm * e2;
        for h in &hits[1..] {
            out[h.slot as usize].blocked += e2;
        }
    }
    out
}

/// The 66-direction sweep: [`sweep_dir`] once per geodesic direction,
/// scattered into per-part [`SweepAccum`] rows — the §2.4.1
/// "deterministic freestream ray traversal" over the single
/// body-frame grid.
fn sweep_directions(
    grid: &VoxelGrid,
    directions: &DirectionSet,
    n_parts: usize,
) -> Vec<SweepAccum> {
    let mut accums: Vec<SweepAccum> = (0..n_parts).map(|_| SweepAccum::default()).collect();
    for (di, d) in directions.dirs.iter().enumerate() {
        let samples = sweep_dir(grid, *d, n_parts);
        for (slot, s) in samples.iter().enumerate() {
            let acc = &mut accums[slot];
            acc.area[di] = s.area;
            acc.blocked[di] = s.blocked;
            acc.cop_num[di] = s.cop_num;
            acc.nrm_num[di] = s.nrm_num;
        }
    }
    accums
}

/// Per-part table assembly: normalize the sweep accumulators into
/// [`PartAeroTable`] rows.
fn assemble_part_tables(
    body: &WeldedBody,
    active: &[Active<'_>],
    accums: &[SweepAccum],
) -> Vec<PartAeroTable> {
    let mut tables = Vec::with_capacity(active.len());
    for (slot, a) in active.iter().enumerate() {
        let acc = &accums[slot];
        let verts = &body.collider.hulls[a.hull as usize].vertices;
        let part_com = verts.iter().fold(DVec3::ZERO, |s, v| s + *v) / verts.len().max(1) as f64;
        let mut area_m2 = Vec::with_capacity(AERO_DIRECTION_COUNT);
        let mut shielded = Vec::with_capacity(AERO_DIRECTION_COUNT);
        let mut cop = Vec::with_capacity(AERO_DIRECTION_COUNT);
        let mut lift_axis = Vec::with_capacity(AERO_DIRECTION_COUNT);
        for di in 0..AERO_DIRECTION_COUNT {
            let a_dir = acc.area[di];
            // `area_m2` is the part's *total* exposed projected area —
            // unblocked + occluded — so `area·(1 − shielded)` applied
            // downstream recovers exactly the force-producing share
            // (§2.4.1's `{area[66], shield[66]}` pair).
            let tot = a_dir + acc.blocked[di];
            area_m2.push(tot);
            shielded.push(if tot > 0.0 {
                acc.blocked[di] / tot
            } else {
                0.0
            });
            cop.push(if a_dir > 0.0 {
                acc.cop_num[di] / a_dir
            } else {
                part_com
            });
            lift_axis.push(if a_dir > 0.0 {
                acc.nrm_num[di] / a_dir
            } else {
                DVec3::ZERO
            });
        }
        tables.push(PartAeroTable {
            node: a.node,
            aero: a.part.aero.clone(),
            area_m2,
            shielded,
            cop,
            lift_axis,
        });
    }
    tables
}

/// Build the Stage-2 [`CraftAeroTables`] for `mask` — the §2.4.1
/// pure function the install command's byte-compare and the sim-side
/// fallback share. Iterates `graph.nodes` in ascending `PartNodeId`
/// (bit i of `mask` = i-th node), rasterizes each active part's
/// welded hull into one isotropic grid, sweeps the 66-direction
/// geodesic set, and evaluates the knot grid at canonical reference
/// conditions. Deterministic: no I/O, no threads, fixed orders
/// throughout.
pub fn build_aero_tables(
    body: &WeldedBody,
    graph: &PartGraph,
    parts: &[WeldPart],
    mask: ConfigMask,
    graph_hash: u64,
) -> Result<CraftAeroTables, AeroError> {
    let directions = direction_set();
    let active = collect_active(body, graph, parts, mask)?;
    if active.is_empty() {
        return Ok(CraftAeroTables {
            voxelizer_version: VOXELIZER_VERSION,
            graph_hash,
            mask,
            com_local: body.com_local,
            directions,
            parts: Vec::new(),
            voxel_edge_m: VOXEL_TARGET_EDGE_M,
            voxel_budget_exceeded: false,
            ref_area_m2: 0.0,
            ref_len_m: 1.0,
            cd_knots: vec![0.0; 30],
            cl_knots: vec![0.0; 30],
            cm_knots: vec![0.0; 30],
        });
    }
    let (grid, lo, hi, exceeded) = voxelize_craft(body, &active);
    let accums = sweep_directions(&grid, &directions, active.len());
    let tables = assemble_part_tables(body, &active, &accums);
    let edge = grid.edge;
    // Craft references.
    let mut ref_area = 0.0f64;
    for di in 0..AERO_DIRECTION_COUNT {
        let total: f64 = tables.iter().map(|p| p.area_m2[di]).sum();
        ref_area = ref_area.max(total);
    }
    let ref_len = (hi - lo).x.max((hi - lo).y).max((hi - lo).z).max(1.0);

    let mut out = CraftAeroTables {
        voxelizer_version: VOXELIZER_VERSION,
        graph_hash,
        mask,
        com_local: body.com_local,
        directions,
        parts: tables,
        voxel_edge_m: edge,
        voxel_budget_exceeded: exceeded,
        ref_area_m2: ref_area,
        ref_len_m: ref_len,
        cd_knots: Vec::with_capacity(30),
        cl_knots: Vec::with_capacity(30),
        cm_knots: Vec::with_capacity(30),
    };

    // Knot grid at canonical reference conditions — each knot runs the
    // same per-part model the tick uses.
    for &alpha in &AERO_AOA_KNOTS_RAD {
        let w = flow_dir_at_aoa(alpha);
        let l_pitch = DVec3::new(0.0, libm::cos(alpha), -libm::sin(alpha));
        for &m in &AERO_MACH_KNOTS {
            let v = m * KNOT_REF_SOUND_SPEED;
            // q = 1: forces scale linearly with q, so the coefficient
            // extraction `F/(q·A_ref)` is exact at q = 1 for every
            // Mach — including the zero-speed knot where ½ρv² = 0.
            let re_per_len = KNOT_REF_DENSITY * v / AIR_VISCOSITY_PA_S;
            let (f, tau) = out.force_torque_body(w, 1.0, m, re_per_len);
            out.cd_knots
                .push(f.dot(w) / out.ref_area_m2.max(f64::EPSILON));
            out.cl_knots
                .push(f.dot(l_pitch) / out.ref_area_m2.max(f64::EPSILON));
            out.cm_knots
                .push(tau.x / (out.ref_area_m2.max(f64::EPSILON) * out.ref_len_m));
        }
    }
    // Non-finite guard — a table that computed NaN anywhere is a build
    // bug, surfaced typed rather than shipped into the tick.
    let finite = out
        .cd_knots
        .iter()
        .chain(&out.cl_knots)
        .chain(&out.cm_knots)
        .all(|v| v.is_finite())
        && out.parts.iter().all(|p| {
            p.area_m2.iter().all(|v| v.is_finite())
                && p.shielded.iter().all(|v| v.is_finite())
                && p.cop.iter().all(|v| v.is_finite())
                && p.lift_axis.iter().all(|v| v.is_finite())
        });
    if !finite {
        return Err(AeroError::NonFinite(
            "aero: table contains non-finite values",
        ));
    }
    Ok(out)
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::graph::{PartNode, PartTransform};
    use crate::part::{ColliderShape, WeldPart};
    use crate::tensor::SymMat3;
    use crate::weld::weld;
    use csp_core::DQuat;

    fn test_thermal() -> csp_data::ThermalDef {
        csp_data::ThermalDef {
            heat_capacity_j_per_k: 100_000.0,
            max_temp_k: 1_200.0,
            emissivity: 0.8,
            conduction_w_per_k: 50.0,
        }
    }

    fn aero_def(cd: f64, cl: f64, ref_len: f64) -> AeroDef {
        AeroDef {
            cd_curve: vec![
                csp_data::AeroKnot {
                    mach: 0.0,
                    coeff: cd,
                },
                csp_data::AeroKnot {
                    mach: 1.2,
                    coeff: cd * 1.5,
                },
                csp_data::AeroKnot {
                    mach: 8.0,
                    coeff: cd * 1.2,
                },
            ],
            cl_curve: vec![csp_data::AeroKnot {
                mach: 0.0,
                coeff: cl,
            }],
            ref_len_m: ref_len,
        }
    }

    fn box_part(mass_kg: f64, half: DVec3, com: DVec3) -> WeldPart {
        WeldPart {
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg,
            com,
            inertia: SymMat3::solid_box(mass_kg, half),
            collider: ColliderShape::Box { half_extents: half },
            force_sites: vec![],
            aero: aero_def(0.5, 0.1, 1.0),
            thermal: test_thermal(),
        }
    }

    fn single_node_body(part: WeldPart, offset: DVec3) -> WeldedBody {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::new(offset, DQuat::IDENTITY), 0),
        );
        weld(&g, &[part]).unwrap()
    }

    fn two_part_body(
        p0: WeldPart,
        t0: PartTransform,
        p1: WeldPart,
        t1: PartTransform,
    ) -> WeldedBody {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(PartNodeId(1), PartNode::new(None, t0, 0));
        g.insert(PartNodeId(2), PartNode::new(Some(PartNodeId(1)), t1, 1));
        weld(&g, &[p0, p1]).unwrap()
    }

    #[test]
    fn direction_set_has_66_dirs_and_covers_sphere() {
        let ds = direction_set();
        assert_eq!(ds.dirs.len(), AERO_DIRECTION_COUNT);
        assert_eq!(ds.tris.len(), 128);
        for d in &ds.dirs {
            assert!((d.norm() - 1.0).abs() < 1e-12, "non-unit dir {d:?}");
        }
        // Every direction interpolates to weights summing to 1, and a
        // sample of non-set directions lands inside some triangle.
        for d in &ds.dirs {
            let (_, w) = ds.weights(*d);
            assert!((w[0] + w[1] + w[2] - 1.0).abs() < 1e-9);
        }
        let probe = DVec3::new(0.3, 0.4, 0.85);
        let probe = probe / probe.norm();
        let (ids, w) = ds.weights(probe);
        assert!((w[0] + w[1] + w[2] - 1.0).abs() < 1e-12);
        let re = ds.dirs[ids[0] as usize] * w[0]
            + ds.dirs[ids[1] as usize] * w[1]
            + ds.dirs[ids[2] as usize] * w[2];
        // Barycentric interpolation reproduces the direction up to
        // chord-vs-arc error — compare the normalized result.
        let rn = re / re.norm();
        assert!(rn.dot(probe) > 0.999, "interp dir err {}", rn.dot(probe));
    }

    #[test]
    fn direction_set_is_deterministic() {
        let (a, b) = (direction_set(), direction_set());
        assert_eq!(a, b);
    }

    #[test]
    fn voxelize_box_fills_volume() {
        let body = single_node_body(
            box_part(100.0, DVec3::new(0.5, 0.5, 0.5), DVec3::ZERO),
            DVec3::ZERO,
        );
        let g = two_part_graph_single();
        let parts = [box_part(100.0, DVec3::new(0.5, 0.5, 0.5), DVec3::ZERO)];
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        // A 1 m cube at 0.15 m voxels: 7×7×0.0225 ≈ 1.1 m² frontal
        // area face-on — exact at the axis-aligned vertices.
        let di = t
            .directions
            .dirs
            .iter()
            .position(|d| *d == DVec3::new(0.0, 0.0, -1.0))
            .unwrap();
        let face_on = t.parts[0].area_m2[di];
        assert!(
            (face_on - 1.0).abs() < 0.15,
            "unit cube frontal area {face_on}"
        );
        // Off-axis the projected area of a cube rises toward √2; the
        // lattice sample inflates it by at most a staircase band.
        let max_area = t.parts[0].area_m2.iter().cloned().fold(0.0, f64::max);
        assert!(max_area < 2.0, "unit cube max area {max_area}");
    }

    fn two_part_graph_single() -> PartGraph {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g
    }

    /// A needle (long thin box on z): axial frontal area ≪ lateral.
    #[test]
    fn needle_low_axial_high_radial_area() {
        let body = single_node_body(
            box_part(100.0, DVec3::new(0.05, 0.05, 2.0), DVec3::ZERO),
            DVec3::ZERO,
        );
        let g = two_part_graph_single();
        let parts = [box_part(100.0, DVec3::new(0.05, 0.05, 2.0), DVec3::ZERO)];
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        // Axial direction: the dir nearest −z.
        let nearest = |probe: DVec3| {
            t.directions
                .dirs
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.dot(probe).partial_cmp(&b.dot(probe)).unwrap())
                .map(|(i, _)| i)
                .unwrap()
        };
        let axial = t.parts[0].area_m2[nearest(DVec3::new(0.0, 0.0, -1.0))];
        let radial = t.parts[0].area_m2[nearest(DVec3::new(-1.0, 0.0, 0.0))];
        assert!(axial < 0.05, "axial area {axial}");
        assert!(radial > 0.25, "radial area {radial}");
        assert!(radial / axial.max(1e-9) > 5.0);
    }

    /// A flat plate: face-on ≫ edge-on — the AoA dependence.
    #[test]
    fn flat_plate_area_is_aoa_dependent() {
        let body = single_node_body(
            box_part(100.0, DVec3::new(1.0, 1.0, 0.03), DVec3::ZERO),
            DVec3::ZERO,
        );
        let g = two_part_graph_single();
        let parts = [box_part(100.0, DVec3::new(1.0, 1.0, 0.03), DVec3::ZERO)];
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        let nearest = |probe: DVec3| {
            t.directions
                .dirs
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.dot(probe).partial_cmp(&b.dot(probe)).unwrap())
                .map(|(i, _)| i)
                .unwrap()
        };
        let face_on = t.parts[0].area_m2[nearest(DVec3::new(0.0, 0.0, -1.0))];
        let edge_on = t.parts[0].area_m2[nearest(DVec3::new(-1.0, 0.0, 0.0))];
        assert!(face_on > 2.0, "face-on {face_on}");
        assert!(edge_on < 0.4, "edge-on {edge_on}");
        // The knot grid reflects it: the craft-level `Cd` normalizes
        // by `A_ref` (the face-on max), so `Cd(90°)` is a small
        // fraction of `Cd(0°)` and `Cl` peaks mid-sweep — the plate's
        // AoA response in coefficient space.
        let cd0 = t.cd_knots[0];
        let cd90 = t.cd_knots[5 * AERO_MACH_KNOTS.len()]; // aoa idx 5, mach 0
        let cl45 = t.cl_knots[3 * AERO_MACH_KNOTS.len()]; // aoa idx 3
        let cl0 = t.cl_knots[0];
        assert!(cd90 < 0.2 * cd0, "cd90 {cd90} vs cd0 {cd0}");
        // Signed, not .abs(): with the corrected lift axis a tilted
        // plate produces *positive* Cl along the pitch-lift
        // convention; a sign flip here means the axis is 180° out
        // again.
        assert!(cl45 > 0.005 && cl45 > cl0, "cl45 {cl45} vs cl0 {cl0}");
    }

    /// Signed lift-axis regression: a flat plate at +15° AoA produces
    /// force *along* the pitch-lift direction for `Cl > 0` — the
    /// outward-normal (unnegated) axis read `−l_pitch` here.
    #[test]
    fn flat_plate_lift_sign_is_physical() {
        let body = single_node_body(
            box_part(100.0, DVec3::new(1.0, 1.0, 0.03), DVec3::ZERO),
            DVec3::ZERO,
        );
        let g = two_part_graph_single();
        let parts = [box_part(100.0, DVec3::new(1.0, 1.0, 0.03), DVec3::ZERO)];
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        let alpha = 15.0 * core::f64::consts::PI / 180.0;
        let w = flow_dir_at_aoa(alpha);
        let l_pitch = DVec3::new(0.0, libm::cos(alpha), -libm::sin(alpha));
        let re = KNOT_REF_DENSITY * 0.8 * KNOT_REF_SOUND_SPEED / AIR_VISCOSITY_PA_S;
        let (f, _tau) = t.force_torque_body(w, 1.0, 0.8, re);
        assert!(
            f.dot(l_pitch) > 0.0,
            "lift component {} must be > 0 for Cl>0 at +15°",
            f.dot(l_pitch)
        );
    }

    /// Near-isotropic shape: an icosahedron's projected area varies
    /// by only ~10–20% — the "sphere → isotropic Cd" exit shape.
    #[test]
    fn icosahedron_area_is_isotropic() {
        // Icosahedron vertices (12): (0,±φ,±1), (±1,0,±φ), (±φ,±1,0).
        let phi = 1.618_033_988_749_895_f64;
        let mut verts = Vec::new();
        for s in [-1.0, 1.0] {
            for t in [-1.0, 1.0] {
                verts.push(DVec3::new(0.0, s * phi, t));
                verts.push(DVec3::new(s, 0.0, t * phi));
                verts.push(DVec3::new(s * phi, t, 0.0));
            }
        }
        let p = WeldPart {
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: 100.0,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(100.0, DVec3::new(0.5, 0.5, 0.5)),
            collider: ColliderShape::Points(verts),
            force_sites: vec![],
            aero: aero_def(0.5, 0.1, 2.0),
            thermal: test_thermal(),
        };
        let body = single_node_body(p.clone(), DVec3::ZERO);
        let g = two_part_graph_single();
        let t = build_aero_tables(&body, &g, &[p], ConfigMask::ALL, 0).unwrap();
        let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
        for &a in &t.parts[0].area_m2 {
            lo = lo.min(a);
            hi = hi.max(a);
        }
        let ratio = hi / lo.max(1e-9);
        assert!(ratio > 1.0 && ratio < 1.6, "icosa dir ratio {ratio}");
    }

    /// The fairing fixture: payload (node 1, root at origin) inside a
    /// closed shell of 6 thin plates (nodes 2–7, children of root).
    /// Returns `(graph, parts, payload_node, shell_nodes)`.
    fn fairing_fixture() -> (PartGraph, Vec<WeldPart>, PartNodeId, Vec<PartNodeId>) {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        let mut parts = vec![box_part(500.0, DVec3::new(0.5, 0.5, 0.5), DVec3::ZERO)];
        // Six plates spanning ±1.1 at ±1.0 offsets — they overlap by
        // 0.2 at every edge, so the voxelized shell is sealed: no
        // lattice-ray pinholes at the seams.
        let plates = [
            (DVec3::new(1.1, 1.1, 0.02), DVec3::new(0.0, 0.0, 1.0)),
            (DVec3::new(1.1, 1.1, 0.02), DVec3::new(0.0, 0.0, -1.0)),
            (DVec3::new(1.1, 0.02, 1.1), DVec3::new(0.0, 1.0, 0.0)),
            (DVec3::new(1.1, 0.02, 1.1), DVec3::new(0.0, -1.0, 0.0)),
            (DVec3::new(0.02, 1.1, 1.1), DVec3::new(1.0, 0.0, 0.0)),
            (DVec3::new(0.02, 1.1, 1.1), DVec3::new(-1.0, 0.0, 0.0)),
        ];
        let mut shells = Vec::new();
        for (i, (half, off)) in plates.iter().enumerate() {
            let id = PartNodeId(2 + i as u64);
            g.insert(
                id,
                PartNode::new(
                    Some(PartNodeId(1)),
                    PartTransform::new(*off, DQuat::IDENTITY),
                    1 + i as u32,
                ),
            );
            parts.push(box_part(50.0, *half, DVec3::ZERO));
            shells.push(id);
        }
        (g, parts, PartNodeId(1), shells)
    }

    /// Fairing shielding: a payload fully inside a closed shell shows
    /// ~100% blocked in every direction and contributes ~no force.
    #[test]
    fn fairing_shields_payload() {
        let (g, parts, payload, _) = fairing_fixture();
        let body = weld(&g, &parts).unwrap();
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        let payload_t = t.parts.iter().find(|p| p.node == payload).unwrap();
        // No unblocked area in any direction — and wherever the
        // payload's silhouette projects, its shielded fraction reads
        // ~1 (the §2.4.1 shielded-part detection, inlined: shielded ≥
        // 0.99 on every direction that exposes area).
        let max_eff = payload_t
            .area_m2
            .iter()
            .zip(&payload_t.shielded)
            .map(|(&a, &s)| a * (1.0 - s))
            .fold(0.0, f64::max);
        assert!(max_eff < 1e-9, "shielded payload effective area {max_eff}");
        assert!(
            payload_t
                .shielded
                .iter()
                .zip(&payload_t.area_m2)
                .all(|(&s, &a)| a <= f64::EPSILON || s >= 0.99),
            "payload not fully shielded"
        );
    }

    /// Mask the shell parts out → the payload becomes aero-active —
    /// the staged-vs-deployed table divergence the exit row names.
    #[test]
    fn config_mask_unshields() {
        let (g, parts, payload, _shells) = fairing_fixture();
        let body = weld(&g, &parts).unwrap();
        // Bit 0 = payload (first node); bits 1..7 = the shell plates.
        let t = build_aero_tables(&body, &g, &parts, ConfigMask(1), 0).unwrap();
        let p = t.parts.iter().find(|p| p.node == payload).unwrap();
        let max_a = p.area_m2.iter().cloned().fold(0.0, f64::max);
        assert!(max_a > 0.5, "unshielded payload area {max_a}");
        // And the total exposed area diverges measurably between
        // staged (ALL) and deployed (payload-only) tables.
        let staged = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        let dep_total: f64 = t
            .parts
            .iter()
            .map(|p| p.area_m2.iter().cloned().fold(0.0, f64::max))
            .sum();
        let st_total: f64 = staged
            .parts
            .iter()
            .map(|p| p.area_m2.iter().cloned().fold(0.0, f64::max))
            .sum();
        assert!(
            (dep_total - st_total).abs() > 1.0,
            "{dep_total} vs {st_total}"
        );
    }

    /// Partial shielding end-to-end (the `area·(1−shielded)` identity):
    /// a slab occluding the right half of a grid-aligned thin plate.
    /// `area_m2` is the plate's *total* silhouette and `shielded` ≈
    /// 0.5 along −ẑ, so `A_eff` recovers exactly the exposed half —
    /// the fixture is grid-aligned so the cell-quantized and analytic
    /// answers coincide; one cell of tolerance covers the check.
    #[test]
    fn partial_shielding_a_eff_matches_analytic() {
        let mut g = PartGraph::new(PartNodeId(1));
        // Root (node 1): plate 2.1×2.1×0.2 centred at z = −1.0 —
        // 14×14×1 cells at the 0.15 m pitch, rasterized at tol = 0
        // (thick enough that no dilation fuzzes the count).
        g.insert(
            PartNodeId(1),
            PartNode::new(
                None,
                PartTransform::new(DVec3::new(0.0, 0.0, -1.0), DQuat::IDENTITY),
                0,
            ),
        );
        // Child (node 2): solid slab covering x ∈ [0, 1.05] — exactly
        // the plate's right half — at z ∈ [0.5, 1.5]. Thick enough to
        // rasterize at tol = 0, so its coverage is cell-exact.
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.525, 0.0, 2.0), DQuat::IDENTITY),
                1,
            ),
        );
        let parts = vec![
            box_part(50.0, DVec3::new(1.05, 1.05, 0.1), DVec3::ZERO),
            box_part(200.0, DVec3::new(0.525, 1.05, 0.5), DVec3::ZERO),
        ];
        let body = weld(&g, &parts).unwrap();
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        let plate = t.parts.iter().find(|p| p.node == PartNodeId(1)).unwrap();
        // −ẑ is a direction-set vertex, so this table row is the exact
        // per-direction sweep, not an interpolation.
        let di = t
            .directions
            .dirs
            .iter()
            .position(|d| *d == DVec3::new(0.0, 0.0, -1.0))
            .unwrap();
        let s = plate.shielded[di];
        let a_eff = plate.area_m2[di] * (1.0 - s);
        let e2 = t.voxel_edge_m * t.voxel_edge_m;
        assert!(s > 0.3 && s < 0.7, "expected partial shielding, got {s}");
        assert!(
            (a_eff - 2.205).abs() <= e2,
            "A_eff {a_eff} vs analytic 2.205 (shielded {s})"
        );
    }

    /// Table determinism — same inputs, bit-identical output.
    #[test]
    fn tables_are_bit_deterministic() {
        let parts = [
            box_part(120.0, DVec3::new(0.4, 0.3, 1.2), DVec3::new(0.1, 0.0, 0.2)),
            box_part(80.0, DVec3::new(0.2, 0.2, 0.4), DVec3::ZERO),
        ];
        let body = two_part_body(
            parts[0].clone(),
            PartTransform::IDENTITY,
            parts[1].clone(),
            PartTransform::new(DVec3::new(0.3, 0.1, -0.6), DQuat::IDENTITY),
        );
        // Same graph the body was welded from: node 2 at the same
        // transform (hulls are baked — only the node set and
        // effective-index binding matter here).
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.3, 0.1, -0.6), DQuat::IDENTITY),
                1,
            ),
        );
        let a = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 7).unwrap();
        let b = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 7).unwrap();
        assert!(a.bit_eq(&b));
        assert_eq!(a, b);
    }

    /// A dart: heavy nose (small dense mass at +z) plus a wide, light
    /// drag tail at −z — CoM forward of the dominant-CoP tail →
    /// aerodynamically stable (restoring Cm), the shuttlecock form.
    /// `reverse` swaps the mass ratio for the unstable mirror.
    fn dart_fixture(reverse: bool) -> (PartGraph, Vec<WeldPart>, WeldedBody) {
        let (nose_mass, tail_mass) = if reverse {
            (50.0, 800.0)
        } else {
            (800.0, 50.0)
        };
        let nose = WeldPart {
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: nose_mass,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(nose_mass, DVec3::new(0.15, 0.15, 0.4)),
            collider: ColliderShape::Box {
                half_extents: DVec3::new(0.15, 0.15, 0.4),
            },
            force_sites: vec![],
            aero: aero_def(0.4, 0.1, 0.8),
            thermal: test_thermal(),
        };
        let tail = WeldPart {
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: tail_mass,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(tail_mass, DVec3::new(0.5, 0.5, 0.15)),
            collider: ColliderShape::Box {
                half_extents: DVec3::new(0.5, 0.5, 0.15),
            },
            force_sites: vec![],
            // Drag-dominated tail: a flat disc's physical normal force
            // does carry a small perpendicular component, but a *drag*
            // tail stabilizes through `Cd·ŵ` applied behind the CoM —
            // authored `Cl` stays low so the fixture exercises the
            // intended physics rather than an arbitrary lift ratio.
            aero: aero_def(0.8, 0.05, 1.0),
            thermal: test_thermal(),
        };
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(
                None,
                PartTransform::new(DVec3::new(0.0, 0.0, 0.6), DQuat::IDENTITY),
                0,
            ),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.0, 0.0, -1.6), DQuat::IDENTITY),
                1,
            ),
        );
        let parts = vec![nose, tail];
        let body = weld(&g, &parts).unwrap();
        (g, parts, body)
    }

    /// The exit criterion, non-tautologically: `Cm`/`Cm_α` from the
    /// knot grid within 5% of a **direct re-sweep** at each knot
    /// direction. The direct path bypasses the table — it voxelizes,
    /// runs the single-direction lattice sweep at the exact `ŵ`, and
    /// applies the per-part force model without interpolation — so
    /// the residual is genuine interpolation error, not a comparison
    /// of the table against itself.
    #[test]
    fn cm_alpha_table_vs_direct() {
        let (g, parts, body) = dart_fixture(false);
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        let active = collect_active(&body, &g, &parts, ConfigMask::ALL).unwrap();
        let (grid, _lo, _hi, _exceeded) = voxelize_craft(&body, &active);
        // Direct per-direction Cm at each (aoa, mach) knot — same
        // canonical conditions, but a fresh sweep at the exact
        // direction rather than barycentric interpolation over the 66
        // tabulated vertices.
        let direct_cm = |ai: usize, mi: usize| -> f64 {
            let w = flow_dir_at_aoa(AERO_AOA_KNOTS_RAD[ai]);
            let m = AERO_MACH_KNOTS[mi];
            let v = m * KNOT_REF_SOUND_SPEED;
            let re = KNOT_REF_DENSITY * v / AIR_VISCOSITY_PA_S;
            let samples = sweep_dir(&grid, w, active.len());
            let mut tau = DVec3::ZERO;
            for (slot, s) in samples.iter().enumerate() {
                if s.area <= 0.0 {
                    continue;
                }
                let cop = s.cop_num / s.area;
                let nrm = s.nrm_num / s.area;
                let lift = {
                    let l = nrm - w * nrm.dot(w);
                    let n = l.norm();
                    if n > 1e-12 { -l / n } else { DVec3::ZERO }
                };
                let aero = &active[slot].part.aero;
                let cd = eval_aero_curve(&aero.cd_curve, m) + skin_friction_cd(re * aero.ref_len_m);
                let cl = eval_aero_curve(&aero.cl_curve, m);
                let fp = (w * cd + lift * cl) * s.area;
                tau = tau + (cop - body.com_local).cross(fp);
            }
            tau.x / (t.ref_area_m2 * t.ref_len_m)
        };
        // Peak |Cm| over the knot grid is the 5% scale — a per-knot
        // relative denominator would explode on near-zero knots
        // (α = 0 is ~0 by symmetry).
        let mut direct = [[0.0; AERO_MACH_KNOTS.len()]; 6];
        let mut peak = 0.0f64;
        for (ai, row) in direct.iter_mut().enumerate() {
            for (mi, d) in row.iter_mut().enumerate() {
                *d = direct_cm(ai, mi);
                peak = peak.max(d.abs());
            }
        }
        let tol = 0.05 * peak.max(1e-9);
        for (ai, row) in direct.iter().enumerate() {
            for (mi, &d) in row.iter().enumerate() {
                let ix = ai * AERO_MACH_KNOTS.len() + mi;
                let err = (t.cm_knots[ix] - d).abs();
                assert!(
                    err <= tol,
                    "cm[{ai}][{mi}] table {} vs direct {d} (5% of peak {peak})",
                    t.cm_knots[ix]
                );
            }
        }
        // The α-slope off the grid vs the slope off the direct
        // evaluations — compared in Cm units (same 5%-of-peak scale).
        let d_alpha = AERO_AOA_KNOTS_RAD[1] - AERO_AOA_KNOTS_RAD[0];
        for (mi, (&d1, &d0)) in direct[1].iter().zip(&direct[0]).enumerate() {
            let direct_slope = (d1 - d0) / d_alpha;
            let err = (t.cm_alpha(mi) - direct_slope).abs() * d_alpha;
            assert!(
                err <= tol,
                "cm_alpha[{mi}] {} vs {direct_slope}",
                t.cm_alpha(mi)
            );
        }
        // Sign sanity on this shape is covered by
        // `cm_alpha_sign_tracks_stability`.
    }

    /// `cm_alpha` sign convention: the dart (CoM forward of CoP) is
    /// stable → negative slope; the reversed dart (CoP forward) →
    /// positive.
    #[test]
    fn cm_alpha_sign_tracks_stability() {
        let (g, parts, body) = dart_fixture(false);
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        assert!(t.cm_alpha(0) < 0.0, "stable slope {}", t.cm_alpha(0));
        let (g, parts, body) = dart_fixture(true);
        let t = build_aero_tables(&body, &g, &parts, ConfigMask::ALL, 0).unwrap();
        assert!(t.cm_alpha(0) > 0.0, "unstable slope {}", t.cm_alpha(0));
    }
}
