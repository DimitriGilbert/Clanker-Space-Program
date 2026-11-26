//! Water — the ocean's buoyancy and drag, applied per part.
//!
//! # What a part displaces
//!
//! A part displaces the part of its collider below the free surface.
//! The surface is the plane tangent to the ocean's datum sphere at the
//! body's centre of mass ([`WaterPlane`], body frame); over a 50 m
//! craft it leaves the sphere by under a millimetre at the ends. A hull
//! is classified from its welded vertex set whenever its body is within
//! reach of the water:
//!
//! - the resolver's `Cylinder` tessellation (two regular octagons on a
//!   common circle) is cut as the **true cylinder** — the exact
//!   primitive its inertia is computed from — by a closed form;
//! - an oriented **box** is cut across its six known faces;
//! - anything else is cut as its own convex polytope, faces re-derived
//!   from the vertex set ([`crate::hull::facet_planes`]).
//!
//! Each yields the submerged volume and its centroid, so buoyancy acts
//! where the displaced water was: a top-heavy craft capsizes, a
//! bottom-heavy one rights itself.
//!
//! # Forces on one wet part
//!
//! - **Buoyancy** `ρ·g·V_sub` along local up, at the submerged
//!   centroid — the weight of the displaced water.
//! - **Quadratic drag** `½·ρ·Cd·A·f·|v|·v`, against the submerged
//!   centroid's own velocity relative to the co-rotating water, `A`
//!   the collider's projected area along the flow and `f` its submerged
//!   fraction.
//! - **Linear damping** `k·ρ·V_sub·v` and `k·ρ·V_sub·κ²·ω` on the same
//!   point — the wave-radiation stand-in that brings a floating craft
//!   to rest (quadratic drag vanishes with speed, so alone it never
//!   ends a bob). `κ²` is the hull's mean-squared vertex radius.
//!
//! That velocity is `v + ω×r`, and it is only ever a dissipation while
//! `ω` and the arm `r` are read in one frame: crossed in different ones
//! it names a flow no part has, the drag stops opposing the part's
//! motion, and a craft that rotates in the water is pumped instead of
//! stilled.
//!
//! # A stiff tick
//!
//! Water is about 840 times denser than sea-level air, so at entry
//! speeds a tick's drag is more than the motion it acts on: a 1 m² face
//! at 200 m/s reads 2 000 g, and one explicit tick would throw the part
//! back out of the water. The load is never scaled to fit the step.
//! Instead a tick whose drag is stiff ([`WaterLoads::stiffness`]) is
//! integrated in sub-steps of the body's velocity — fourth-order
//! Runge–Kutta, at the tick's frozen pose, every other force held at its
//! tick-start value. A tick that is not stiff takes the backend's own
//! step, bit for bit.
//!
//! # Craft that cannot touch water
//!
//! [`provably_dry`] culls by distance — every collider vertex above the
//! datum by more than the body's reach — and a culled body takes the
//! pre-water code path unchanged, bit for bit.

use csp_core::{DQuat, DVec3};
use serde::{Deserialize, Serialize};

use crate::part::MAX_COLLIDER_VERTICES;
use crate::simple::{Environment, PHYSICS_DT, friction_basis};
use crate::tensor::SymMat3;
use crate::weld::WeldedBody;

/// The no-data water density [`Environment::for_body`] stamps every
/// ocean with — standard seawater, kg/m³. `balance.ron`'s
/// `water_density_kg_m3` is the authored value.
pub const DEFAULT_WATER_DENSITY_KG_M3: f64 = 1_025.0;

/// The no-data quadratic water-drag coefficient — bluff-body order,
/// the same default the pre-voxel air drag carries. `balance.ron`'s
/// `water_drag_cd` is the authored value.
pub const DEFAULT_WATER_DRAG_CD: f64 = 1.0;

/// The linear water-damping rate (1/s). A floating craft displaces its
/// own mass of water, so this is also the fraction of its own momentum
/// the water sheds per second: `1.5` halves a craft's heave envelope
/// every 0.92 s whatever its size, which brings a bobbing craft inside
/// the §2.6.4 settle thresholds in seconds and leaves it within a
/// millimetre of its waterline when the latch freezes it.
pub const DEFAULT_WATER_LINEAR_DAMPING_PER_S: f64 = 1.5;

/// A body's ocean: water fills everything below `datum_radius_m`, from
/// the seabed (the terrain surface, where it lies below the datum) up
/// to the datum sphere. Carried by [`Environment::ocean`]; `None` is a
/// dry body and no water force exists there.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ocean {
    /// Radius of the free surface above the body's centre (m).
    pub datum_radius_m: f64,
    /// Water density (kg/m³).
    pub density_kg_m3: f64,
    /// Quadratic drag coefficient.
    pub drag_cd: f64,
    /// Linear damping rate (1/s).
    pub linear_damping_per_s: f64,
}

impl Ocean {
    /// An ocean over an authored datum at the no-data tunables — the
    /// `Environment::for_body` stamp, which the balance file then
    /// overrides.
    pub fn at_datum(datum_radius_m: f64) -> Self {
        Self {
            datum_radius_m,
            density_kg_m3: DEFAULT_WATER_DENSITY_KG_M3,
            drag_cd: DEFAULT_WATER_DRAG_CD,
            linear_damping_per_s: DEFAULT_WATER_LINEAR_DAMPING_PER_S,
        }
    }
}

/// The free surface as the force model cuts it, in body frame: `up` is
/// local up at the body's centre of mass and `height` that centre's
/// height above the water (negative when the centre is submerged). A
/// body-frame vertex `v` sits `height + (v − com_local)·up` above the
/// water.
struct WaterPlane {
    up: DVec3,
    height: f64,
}

impl WaterPlane {
    fn of(env: &Environment, b: &WeldedBody, ocean: &Ocean) -> Option<Self> {
        let rel = b.com_world() - env.body_center;
        let r = rel.norm();
        // NaN-rejecting gate: `is_nan() || <= 0.0` is the lint-clean
        // spelling of "not `> 0.0`".
        if r.is_nan() || r <= 0.0 {
            return None;
        }
        Some(Self {
            up: b.rot.conjugate().rotate(rel / r),
            height: r - ocean.datum_radius_m,
        })
    }

    fn height(&self, v: DVec3, com: DVec3) -> f64 {
        (v - com).dot(self.up) + self.height
    }
}

/// Can no part of `b` touch water? True without an ocean, and when the
/// centre of mass clears the datum sphere by more than the body's
/// reach (`max |v − com_local|` over every collider vertex) — then
/// every vertex has strictly positive height above [`WaterPlane`]. The
/// triangle-inequality proof is `provably_surface_free`'s; a non-finite
/// input falls through to the full evaluation.
pub(crate) fn provably_dry(env: &Environment, b: &WeldedBody) -> bool {
    let Some(ocean) = &env.ocean else {
        return true;
    };
    let mut reach2 = 0.0f64;
    for hull in &b.collider.hulls {
        for v in &hull.vertices {
            reach2 = reach2.max((*v - b.com_local).norm_squared());
        }
    }
    let gap = (b.com_world() - env.body_center).norm() - libm::sqrt(reach2) - ocean.datum_radius_m;
    !(gap.is_nan() || gap < 0.0)
}

/// Does any part of `b` dip into the water — the "in water" behind
/// the `Splashed` situation? The same dry test the force model applies
/// hull by hull (no volume is integrated): a convex hull crosses a
/// plane iff one of its vertices does — save a cylinder, whose round
/// rim reaches beyond its tessellation points.
pub(crate) fn touches_water(env: &Environment, b: &WeldedBody) -> bool {
    let Some(ocean) = &env.ocean else {
        return false;
    };
    if provably_dry(env, b) {
        return false;
    }
    let Some(plane) = WaterPlane::of(env, b, ocean) else {
        return false;
    };
    let mut heights = [0.0f64; MAX_COLLIDER_VERTICES];
    b.collider.hulls.iter().any(|hull| {
        match hull_in_water(&hull.vertices, &plane, b.com_local, &mut heights) {
            Some(in_water) => !in_water.shape.dry(&in_water, plane.up),
            None => false,
        }
    })
}

/// One hull posed against the water: its shape, the centre that shape
/// is measured from, the vertices' heights above [`WaterPlane`] and the
/// centre's depth below it (negative when the centre is above). `None`
/// for an empty hull, or one past the weld's collider cap — outside
/// the model, and dry.
fn hull_in_water<'h>(
    verts: &[DVec3],
    plane: &WaterPlane,
    com: DVec3,
    heights: &'h mut [f64],
) -> Option<HullInWater<'h>> {
    if verts.is_empty() || verts.len() > MAX_COLLIDER_VERTICES {
        return None;
    }
    for (i, v) in verts.iter().enumerate() {
        heights[i] = plane.height(*v, com);
    }
    let (shape, centre) = Shape::of(verts);
    Some(HullInWater {
        shape,
        centre,
        depth: -plane.height(centre, com),
        s: &heights[..verts.len()],
    })
}

/// One hull posed against the water — see [`hull_in_water`].
struct HullInWater<'h> {
    shape: Shape,
    centre: DVec3,
    depth: f64,
    s: &'h [f64],
}

/// One wet part's water loads for a tick — the record
/// [`crate::simple::SimpleBackend::water_loads`] hands the §2.4 breakup
/// budget. Its geometry is the pose the tick's forces were evaluated
/// at; its drag follows the body's velocity, sampled wherever the
/// tick's integration goes.
#[derive(Clone, Debug)]
pub struct WaterPart {
    /// Index into `WeldedBody::collider::hulls` — the part.
    pub hull: u32,
    /// The submerged centroid the loads act at, bubble frame.
    pub at: DVec3,
    /// The largest force the tick put on the part — its buoyancy plus
    /// the peak of its drag — bubble frame.
    pub force: DVec3,
    /// The damping couple at that peak, body frame.
    pub torque: DVec3,
    /// `|drag|²` of the recorded peak.
    peak: f64,
    buoyancy: DVec3,
    /// `at − com_world`: the moment arm, the same at every sample.
    arm: DVec3,
    /// The same arm, body frame — the frame the inertia is in.
    arm_body: DVec3,
    /// The water's own velocity at `at` — it turns with the body.
    flow: DVec3,
    /// The body's spin, body frame — what the couple damps against.
    spin_body: DVec3,
    /// `½·ρ·Cd`: with the collider's silhouette along the flow, the
    /// submerged fraction and the flow speed, the quadratic coefficient.
    unit_drag: f64,
    fraction: f64,
    /// `k·ρ·V_sub`, the linear coefficient.
    linear: f64,
    /// The couple's: `linear` times the hull's mean-squared vertex radius.
    couple: f64,
    /// The collider, for its silhouette along a flow the sub-steps turn.
    shape: Shape,
    /// The hull's vertices, for a polytope's silhouette; empty otherwise.
    verts: Vec<DVec3>,
    centre: DVec3,
}

/// The stiffness ([`WaterLoads::stiffness`]) one sub-step is sized to
/// resolve. A fourth-order step leaves an error under 1e-5 of a mode at
/// this stiffness, where the backend's single explicit step would
/// already have over-stopped it by 4%.
const SUBSTEP_STIFFNESS: f64 = 0.25;

/// The most sub-steps one tick is integrated in, and so the greatest
/// stiffness resolved unscaled: `MAX_SUBSTEPS·SUBSTEP_STIFFNESS` (128).
/// Past it the drag's remainder is scaled to keep the step bounded —
/// the one place the water's load is not its own.
const MAX_SUBSTEPS: usize = 512;

/// A body's water loads for one tick: `parts` is empty unless some part
/// is wet, and `buoyant` says whether the whole body is lighter than
/// the water its full volume displaces.
#[derive(Clone, Debug)]
pub(crate) struct WaterLoads {
    pub buoyant: bool,
    pub parts: Vec<WaterPart>,
    /// `PHYSICS_DT·Σ mobility` over the wet parts at the tick's
    /// velocity: the greatest share of the motion it acts on that one
    /// explicit tick's drag could claim.
    stiffness: f64,
    /// What the drag and damping are scaled by: one, or
    /// [`MAX_SUBSTEPS`]' remainder.
    scale: f64,
}

impl WaterLoads {
    /// In water and held up by it — the body floats. What the settle
    /// latch and the parked-support recheck accept in place of ground
    /// contact; a sinker never qualifies, so it is never latched
    /// hanging in mid-water at its slow terminal speed.
    pub fn supported(&self) -> bool {
        self.buoyant && !self.parts.is_empty()
    }

    /// The water loads on `b`: `None` without an ocean or when `b`
    /// provably cannot touch it (the distance cull), otherwise `Some`
    /// whose `parts` list stays empty while every hull is dry.
    pub fn of(env: &Environment, b: &WeldedBody) -> Option<Self> {
        let ocean = env.ocean.as_ref()?;
        if provably_dry(env, b) {
            return None;
        }
        let plane = WaterPlane::of(env, b, ocean)?;
        let mut loads = Self {
            buoyant: false,
            parts: Vec::new(),
            stiffness: 0.0,
            scale: 1.0,
        };
        let mut full_volume_m3 = 0.0;
        let mut clipper = Clipper::default();
        let mut heights = [0.0f64; MAX_COLLIDER_VERTICES];
        let inv_i = b.inertia_local.inverse();
        for (index, hull) in b.collider.hulls.iter().enumerate() {
            let verts = &hull.vertices[..];
            let Some(in_water) = hull_in_water(verts, &plane, b.com_local, &mut heights) else {
                continue;
            };
            full_volume_m3 += in_water.shape.whole(verts, in_water.centre, &mut clipper).0;
            let wet = in_water
                .shape
                .submerged(verts, &in_water, plane.up, &mut clipper);
            if let Some(wet) = wet {
                let part = part_load(env, b, ocean, in_water.shape, &wet, index as u32);
                let (_, _, coefficient) = part.drag(b.rot, b.vel, b.ang_vel);
                loads.stiffness += part.mobility(b.mass_kg, coefficient, inv_i);
                loads.parts.push(part);
            }
        }
        loads.buoyant = b.mass_kg < ocean.density_kg_m3 * full_volume_m3;
        loads.stiffness *= PHYSICS_DT;
        let cap = MAX_SUBSTEPS as f64 * SUBSTEP_STIFFNESS;
        if loads.stiffness > cap {
            loads.scale = cap / loads.stiffness;
        }
        Some(loads)
    }

    /// The sub-steps the tick's velocity is integrated in: one while
    /// the drag is not stiff — the backend's own step — else one per
    /// [`SUBSTEP_STIFFNESS`] of it, up to [`MAX_SUBSTEPS`]. A
    /// non-finite stiffness is not stiff.
    pub(crate) fn substeps(&self) -> usize {
        let steps = (self.stiffness / SUBSTEP_STIFFNESS).ceil();
        if steps.is_nan() {
            1
        } else {
            (steps as usize).clamp(1, MAX_SUBSTEPS)
        }
    }

    /// The water's force and torque on a body posed at `rot` and moving
    /// at `(vel, ang_vel)` — each part's buoyancy and drag at its
    /// submerged centroid, its couple turned into the bubble frame —
    /// added to `force` and `torque`. Each part keeps the largest drag
    /// it shows over one tick's calls as its record. With no wet part
    /// the inputs come back untouched, bit for bit.
    pub(crate) fn at_velocity(
        &mut self,
        rot: DQuat,
        vel: DVec3,
        ang_vel: DVec3,
        force: DVec3,
        torque: DVec3,
    ) -> (DVec3, DVec3) {
        let (mut force, mut torque) = (force, torque);
        for p in &mut self.parts {
            let (drag, damping, _) = p.drag(rot, vel, ang_vel);
            let (drag, damping) = (drag * self.scale, damping * self.scale);
            let load = p.buoyancy + drag;
            let peak = drag.norm_squared();
            if peak > p.peak {
                p.peak = peak;
                p.force = load;
                p.torque = damping;
            }
            force = force + load;
            torque = torque + p.arm.cross(load) + rot.rotate(damping);
        }
        (force, torque)
    }
}

impl WaterPart {
    /// The part's drag force (bubble frame), damping couple (body
    /// frame) and drag coefficient under `rot` at body velocity
    /// `(vel, ang_vel)`.
    fn drag(&self, rot: DQuat, vel: DVec3, ang_vel: DVec3) -> (DVec3, DVec3, f64) {
        let v_rel = (vel + rot.rotate(ang_vel).cross(self.arm)) - self.flow;
        let speed = v_rel.norm();
        let quadratic = if speed > 0.0 {
            let area = self.shape.projected_area(
                &self.verts,
                self.centre,
                rot.conjugate().rotate(v_rel / speed),
            );
            self.unit_drag * area * self.fraction * speed
        } else {
            0.0
        };
        (
            v_rel * -(quadratic + self.linear),
            (ang_vel - self.spin_body) * -self.couple,
            quadratic + self.linear,
        )
    }

    /// How strongly a drag of `coefficient` and this part's couple
    /// couple into the motion of a body of `mass_kg` and inverse inertia
    /// `inv_i`: the most any mode of the body can be damped by them.
    /// A point's inverse effective mass is `1/M + [r]×I⁻¹[r]×ᵀ`, whose
    /// largest eigenvalue is at most `1/M` plus `|arm|²` times the
    /// greatest inverse inertia about an axis across the arm — `r × n`
    /// lies across `r` and is at most `|r|` long, whichever way the
    /// drag points — and a couple's is at most the inertia's greatest
    /// eigenvalue. Both are read where they act rather than whole: a
    /// part at the end of a long stack cannot roll the stack, only turn
    /// it about its hard axes, and reading `tr I⁻¹` there — mostly the
    /// roll axis's share — overstated its stiffness a thousandfold.
    fn mobility(&self, mass_kg: f64, coefficient: f64, inv_i: Option<SymMat3>) -> f64 {
        let Some(inv_i) = inv_i else {
            return coefficient / mass_kg;
        };
        let turn = self.arm_body.norm_squared() * across(&inv_i, self.arm_body);
        coefficient * (1.0 / mass_kg + turn) + self.couple * greatest(inv_i)
    }
}

/// An upper bound on `inv_i`'s greatest eigenvalue, exact for an
/// inertia that is diagonal in the body frame: the lesser of its trace
/// and its largest absolute row sum — every eigenvalue of a symmetric
/// matrix lies in one of its Gershgorin discs.
fn greatest(inv_i: SymMat3) -> f64 {
    let rows = [
        inv_i.xx.abs() + inv_i.xy.abs() + inv_i.xz.abs(),
        inv_i.xy.abs() + inv_i.yy.abs() + inv_i.yz.abs(),
        inv_i.xz.abs() + inv_i.yz.abs() + inv_i.zz.abs(),
    ];
    (inv_i.xx + inv_i.yy + inv_i.zz)
        .min(rows[0])
        .min(rows[1])
        .min(rows[2])
}

/// The greatest inverse inertia `inv_i` carries about an axis
/// perpendicular to `arm` (body frame): the larger eigenvalue of its
/// restriction to that plane, read in the basis [`friction_basis`]
/// pins. Zero without an arm.
fn across(inv_i: &SymMat3, arm: DVec3) -> f64 {
    let norm = arm.norm();
    if norm == 0.0 {
        return 0.0;
    }
    let (t1, t2) = friction_basis(arm / norm);
    let (a, m, c) = (
        t1.dot(inv_i.mul_vec(t1)),
        t1.dot(inv_i.mul_vec(t2)),
        t2.dot(inv_i.mul_vec(t2)),
    );
    let half = 0.5 * (a - c);
    0.5 * (a + c) + libm::sqrt(half * half + m * m)
}

/// One hull's submersion — the geometry the force model needs.
struct Wet<'a> {
    verts: &'a [DVec3],
    centre: DVec3,
    volume_m3: f64,
    /// Submerged share of the collider's volume.
    fraction: f64,
    /// Submerged centroid, body frame.
    centroid: DVec3,
}

/// The force model on one wet hull: buoyancy at the submerged centroid,
/// drag and damping against the water's own (co-rotating) flow past
/// that point. All of it but the drag is the tick's — a function of the
/// pose alone.
fn part_load(
    env: &Environment,
    b: &WeldedBody,
    ocean: &Ocean,
    shape: Shape,
    wet: &Wet<'_>,
    hull: u32,
) -> WaterPart {
    let at = b.pos + b.rot.rotate(wet.centroid);
    let rel = at - env.body_center;
    let water_kg = ocean.density_kg_m3 * wet.volume_m3;
    let linear = ocean.linear_damping_per_s * water_kg;
    let buoyancy = crate::simple::gravity_force_world(env, rel, -water_kg).unwrap_or(DVec3::ZERO);
    let arm = at - b.com_world();
    let polytope = matches!(shape, Shape::Polytope { .. });
    WaterPart {
        hull,
        at,
        force: buoyancy,
        torque: DVec3::ZERO,
        peak: -1.0,
        buoyancy,
        arm,
        arm_body: b.rot.conjugate().rotate(arm),
        flow: env.body_spin.cross(rel),
        spin_body: b.rot.conjugate().rotate(env.body_spin),
        unit_drag: 0.5 * ocean.density_kg_m3 * ocean.drag_cd,
        fraction: wet.fraction,
        linear,
        couple: linear * shape.mean_vertex_radius2(wet.verts, wet.centre),
        shape,
        verts: if polytope {
            wet.verts.to_vec()
        } else {
            Vec::new()
        },
        centre: wet.centre,
    }
}

// ---------------------------------------------------------------------------
// Collider geometry
// ---------------------------------------------------------------------------

/// Relative tolerance of the shape classification. A welded hull's
/// vertices are authored constants under one quaternion, so members of
/// a class agree to rounding, and the tests pin that nothing the
/// resolver emits from another class comes near passing.
const SHAPE_TOL: f64 = 1e-9;

/// Which solid a hull's vertex set describes — decided per tick, from
/// the vertices alone, because that is all the weld keeps.
#[derive(Clone, Debug)]
enum Shape {
    /// The resolver's `Cylinder`: two regular octagons on a circle of
    /// `radius` at `±half_height` about `axis` through the hull's
    /// centre. Displaces the cylinder, not the octagons.
    Cylinder {
        axis: DVec3,
        radius: f64,
        half_height: f64,
    },
    /// An oriented box: right-handed unit `axes`, `half` extents, and
    /// `corners[bits]` — the vertex at `centre + Σ bit_k · 2·half_k·axes_k`.
    Box {
        axes: [DVec3; 3],
        half: [f64; 3],
        corners: [usize; 8],
    },
    /// Any other convex polytope: outward-ordered vertex loops, one
    /// per facet.
    Polytope { faces: Vec<Vec<usize>> },
}

impl Shape {
    /// Classify `verts`; the second element is the vertex mean — the
    /// centre of a cylinder or box, an interior point of any hull.
    fn of(verts: &[DVec3]) -> (Self, DVec3) {
        let centre = verts.iter().fold(DVec3::ZERO, |c, v| c + *v) / verts.len() as f64;
        if let Some((axis, radius, half_height)) = cylinder_of(verts, centre) {
            return (
                Self::Cylinder {
                    axis,
                    radius,
                    half_height,
                },
                centre,
            );
        }
        if let Some((axes, half, corners)) = box_of(verts) {
            return (
                Self::Box {
                    axes,
                    half,
                    corners,
                },
                centre,
            );
        }
        (
            Self::Polytope {
                faces: polytope_faces(verts),
            },
            centre,
        )
    }

    /// The collider whole: its volume and the centroid of that volume.
    fn whole(&self, verts: &[DVec3], centre: DVec3, clipper: &mut Clipper) -> (f64, DVec3) {
        match self {
            Self::Cylinder {
                radius,
                half_height,
                ..
            } => (
                core::f64::consts::PI * radius * radius * (2.0 * half_height),
                centre,
            ),
            Self::Box { half, .. } => (8.0 * half[0] * half[1] * half[2], centre),
            Self::Polytope { faces } => {
                let loops: Vec<&[usize]> = faces.iter().map(Vec::as_slice).collect();
                clipper.moments(verts, &loops, centre)
            }
        }
    }

    /// Is the hull clear of the water? A cylinder's extent along `up`
    /// is set by its rim, not by the eight tessellation points on it;
    /// every other hull crosses a plane iff one of its vertices does.
    fn dry(&self, in_water: &HullInWater<'_>, up: DVec3) -> bool {
        match self {
            Self::Cylinder {
                axis,
                radius,
                half_height,
            } => in_water.depth <= -cylinder_reach(*axis, *radius, *half_height, up),
            _ => in_water.s.iter().all(|h| *h >= 0.0),
        }
    }

    /// The part of the collider below the water — `None` while dry.
    fn submerged<'v>(
        &self,
        verts: &'v [DVec3],
        in_water: &HullInWater<'_>,
        up: DVec3,
        clipper: &mut Clipper,
    ) -> Option<Wet<'v>> {
        let centre = in_water.centre;
        let (full_volume, whole_centroid) = self.whole(verts, centre, clipper);
        let (volume_m3, centroid) = match self {
            Self::Cylinder {
                axis,
                radius,
                half_height,
            } => {
                let reach = cylinder_reach(*axis, *radius, *half_height, up);
                if in_water.depth >= reach {
                    (full_volume, centre)
                } else if in_water.depth <= -reach {
                    (0.0, centre)
                } else {
                    cylinder_submerged(*axis, *radius, *half_height, up, in_water.depth, centre)
                }
            }
            Self::Box { corners, .. } => {
                let faces = box_faces(corners);
                let loops: [&[usize]; 6] = faces.each_ref().map(|face| &face[..]);
                clip_polytope(
                    verts,
                    &loops,
                    in_water.s,
                    whole_centroid,
                    up,
                    full_volume,
                    clipper,
                )
            }
            Self::Polytope { faces } => {
                let loops: Vec<&[usize]> = faces.iter().map(Vec::as_slice).collect();
                clip_polytope(
                    verts,
                    &loops,
                    in_water.s,
                    whole_centroid,
                    up,
                    full_volume,
                    clipper,
                )
            }
        };
        (volume_m3 > 0.0).then(|| Wet {
            verts,
            centre,
            volume_m3,
            fraction: (volume_m3 / full_volume).min(1.0),
            centroid,
        })
    }

    /// The collider's silhouette area along the unit direction `dir`.
    fn projected_area(&self, verts: &[DVec3], centre: DVec3, dir: DVec3) -> f64 {
        match self {
            Self::Cylinder {
                axis,
                radius,
                half_height,
            } => {
                let along = dir.dot(*axis).abs();
                let across = libm::sqrt((1.0 - along * along).max(0.0));
                core::f64::consts::PI * radius * radius * along
                    + 4.0 * radius * half_height * across
            }
            Self::Box { axes, half, .. } => {
                let p = axes.each_ref().map(|a| dir.dot(*a).abs());
                4.0 * (half[0] * half[1] * p[2]
                    + half[0] * half[2] * p[1]
                    + half[1] * half[2] * p[0])
            }
            Self::Polytope { faces } => {
                // Each face's area vector folded onto `dir`: a closed
                // surface's two silhouette halves add to twice the
                // projected area.
                0.5 * faces
                    .iter()
                    .map(|face| loop_area_vector(face, verts, centre).dot(dir).abs())
                    .sum::<f64>()
            }
        }
    }

    /// Mean squared distance of the vertices from `centre`.
    fn mean_vertex_radius2(&self, verts: &[DVec3], centre: DVec3) -> f64 {
        verts
            .iter()
            .map(|v| (*v - centre).norm_squared())
            .sum::<f64>()
            / verts.len() as f64
    }
}

/// A vertex loop's area vector — area times outward unit normal for an
/// outward-ordered planar loop (half the cross-product sum).
fn loop_area_vector(face: &[usize], verts: &[DVec3], centre: DVec3) -> DVec3 {
    face.iter()
        .zip(face.iter().cycle().skip(1))
        .map(|(&a, &b)| (verts[a] - centre).cross(verts[b] - centre))
        .fold(DVec3::ZERO, |sum, term| sum + term)
        * 0.5
}

/// A polytope's submersion: dry or whole by its vertices (a convex
/// hull crosses a plane iff some vertex is on each side), else the
/// face-by-face clip. `whole_centroid` is the whole solid's centre of
/// volume — any interior point — where the clip's tetrahedra fan from.
fn clip_polytope(
    verts: &[DVec3],
    loops: &[&[usize]],
    s: &[f64],
    whole_centroid: DVec3,
    up: DVec3,
    full_volume: f64,
    clipper: &mut Clipper,
) -> (f64, DVec3) {
    let wet = s.iter().filter(|h| **h < 0.0).count();
    if wet == 0 {
        (0.0, DVec3::ZERO)
    } else if wet == s.len() {
        (full_volume, whole_centroid)
    } else {
        clipper.submerged(verts, loops, s, up, whole_centroid)
    }
}

/// The box's six outward loops, face `2·k + σ` spanning axes `k+1` and
/// `k+2` at sign `σ`. With `(j, l)` cyclic after `k`, the ring
/// `(σ,0,0) (σ,0,1) (σ,1,1) (σ,1,0)` faces `−axis_k`; the `σ = 1` face
/// walks it backwards to face `+axis_k`.
fn box_faces(corners: &[usize; 8]) -> [[usize; 4]; 6] {
    let mut faces = [[0usize; 4]; 6];
    for k in 0..3 {
        let (j, l) = ((k + 1) % 3, (k + 2) % 3);
        for sigma in 0..2 {
            let base = sigma << k;
            let ring = [
                corners[base],
                corners[base | (1 << l)],
                corners[base | (1 << j) | (1 << l)],
                corners[base | (1 << j)],
            ];
            faces[2 * k + sigma] = if sigma == 0 {
                ring
            } else {
                [ring[0], ring[3], ring[2], ring[1]]
            };
        }
    }
    faces
}

/// The resolver's cylinder, if `verts` is one. Candidate axes are the
/// vertex differences — the twin edges joining the two rings are
/// parallel to the axis — and a wrong one fails `rings_about` on its
/// first vertex or two.
fn cylinder_of(verts: &[DVec3], centre: DVec3) -> Option<(DVec3, f64, f64)> {
    if verts.len() != 16 {
        return None;
    }
    let tol = SHAPE_TOL
        * verts
            .iter()
            .map(|v| (*v - centre).norm())
            .fold(0.0, f64::max);
    for i in 0..verts.len() {
        for j in (i + 1)..verts.len() {
            let edge = verts[j] - verts[i];
            let len = edge.norm();
            if len <= tol {
                continue;
            }
            let axis = edge / len;
            if let Some((radius, half_height)) = rings_about(verts, centre, axis, tol) {
                return Some((axis, radius, half_height));
            }
        }
    }
    None
}

/// Are `verts` two eight-point rings of one radius at `±h` about
/// `axis`, with the half-eighth-turn symmetry of a regular octagon?
fn rings_about(verts: &[DVec3], centre: DVec3, axis: DVec3, tol: f64) -> Option<(f64, f64)> {
    let first = verts[0] - centre;
    let (radius, half_height) = (first.cross(axis).norm(), first.dot(axis).abs());
    let mut upper = 0usize;
    for v in verts {
        let w = *v - centre;
        let z = w.dot(axis);
        if (w.cross(axis).norm() - radius).abs() > tol || (z.abs() - half_height).abs() > tol {
            return None;
        }
        upper += usize::from(z > 0.0);
    }
    (upper == 8 && half_eighth_symmetric(verts, centre, axis, tol)).then_some((radius, half_height))
}

/// Does a half-eighth turn about `axis` through `centre` permute `verts`?
fn half_eighth_symmetric(verts: &[DVec3], centre: DVec3, axis: DVec3, tol: f64) -> bool {
    let t = core::f64::consts::FRAC_1_SQRT_2;
    verts.iter().all(|v| {
        let p = *v - centre;
        let turned = centre + (p + axis.cross(p)) * t + axis * (axis.dot(p) * (1.0 - t));
        verts
            .iter()
            .any(|u| (*u - turned).norm_squared() <= tol * tol)
    })
}

/// An oriented box, if `verts` is one: three mutually orthogonal edges
/// out of the first vertex whose 0/1 combinations are the other seven.
fn box_of(verts: &[DVec3]) -> Option<([DVec3; 3], [f64; 3], [usize; 8])> {
    if verts.len() != 8 {
        return None;
    }
    let origin = verts[0];
    let d: [DVec3; 7] = core::array::from_fn(|m| verts[m + 1] - origin);
    let len: [f64; 7] = core::array::from_fn(|m| d[m].norm());
    let tol = SHAPE_TOL * len.iter().fold(0.0f64, |m, l| m.max(*l));
    for i in 0..7 {
        for j in (i + 1)..7 {
            for k in (j + 1)..7 {
                let [a, b, c] = [i, j, k];
                if len[a] <= tol
                    || len[b] <= tol
                    || len[c] <= tol
                    || !orthogonal(&d, &len, tol, [a, b, c])
                {
                    continue;
                }
                let mut axes = [d[a] / len[a], d[b] / len[b], d[c] / len[c]];
                let mut half = [len[a] * 0.5, len[b] * 0.5, len[c] * 0.5];
                // Right-handed, whichever way the vertex order picked
                // the edges: `box_faces` orients its loops on that.
                if axes[0].cross(axes[1]).dot(axes[2]) < 0.0 {
                    axes.swap(1, 2);
                    half.swap(1, 2);
                }
                if let Some(corners) = box_corners(&d, &axes, &half, tol) {
                    return Some((axes, half, corners));
                }
            }
        }
    }
    None
}

fn orthogonal(d: &[DVec3; 7], len: &[f64; 7], tol: f64, pick: [usize; 3]) -> bool {
    [[0, 1], [0, 2], [1, 2]]
        .iter()
        .all(|&[p, q]| d[pick[p]].dot(d[pick[q]]).abs() <= tol * len[pick[p]] * len[pick[q]])
}

/// The corner table: each of the seven differences, resolved in the
/// edge frame, must land on a distinct 0/1 corner.
fn box_corners(d: &[DVec3; 7], axes: &[DVec3; 3], half: &[f64; 3], tol: f64) -> Option<[usize; 8]> {
    let mut corners = [usize::MAX; 8];
    corners[0] = 0;
    for (m, diff) in d.iter().enumerate() {
        let mut bits = 0usize;
        for (k, axis) in axes.iter().enumerate() {
            let along = diff.dot(*axis);
            let bit = if along.abs() <= tol {
                0
            } else if (along - 2.0 * half[k]).abs() <= tol {
                1 << k
            } else {
                return None;
            };
            bits |= bit;
        }
        if corners[bits] != usize::MAX {
            return None;
        }
        corners[bits] = m + 1;
    }
    Some(corners)
}

/// A general hull's face loops: every facet plane the brute-force sweep
/// finds, with its on-plane vertices wrapped into an outward order.
fn polytope_faces(verts: &[DVec3]) -> Vec<Vec<usize>> {
    let eps = crate::hull::plane_epsilon(verts);
    let Some(planes) = crate::hull::facet_planes(verts, eps) else {
        return Vec::new();
    };
    let (mut flat, mut order) = (Vec::new(), Vec::new());
    planes
        .iter()
        .filter_map(|plane| {
            let on: Vec<usize> = (0..verts.len())
                .filter(|&i| (plane.n.dot(verts[i]) - plane.d).abs() <= eps)
                .collect();
            if on.len() < 3 {
                return None;
            }
            let pts: Vec<DVec3> = on.iter().map(|&i| verts[i]).collect();
            wrap_order(&pts, plane.n, &mut flat, &mut order);
            Some(order.iter().map(|&k| on[k]).collect())
        })
        .collect()
}

/// Outside walk (gift wrapping) of a planar point set's convex hull:
/// from the lowest point each step turns as far clockwise as no point
/// lies right of it, so the loop's normal is `normal` and collinear
/// and interior points drop out. Indices into `pts`, in `out`.
fn wrap_order(pts: &[DVec3], normal: DVec3, flat: &mut Vec<(f64, f64)>, out: &mut Vec<usize>) {
    out.clear();
    if pts.len() < 3 {
        return;
    }
    let (e1, e2) = crate::simple::friction_basis(normal);
    flat.clear();
    flat.extend(pts.iter().map(|p| (p.dot(e1), p.dot(e2))));
    let mut start = 0;
    for i in 1..flat.len() {
        let (lower_x, lowest_y) = (flat[i].0 < flat[start].0, flat[i].1 < flat[start].1);
        if lower_x || (flat[i].0 == flat[start].0 && lowest_y) {
            start = i;
        }
    }
    let mut current = start;
    out.push(start);
    for _ in 0..flat.len() {
        let c = flat[current];
        let mut next: Option<usize> = None;
        for (j, f) in flat.iter().enumerate() {
            if *f == c {
                continue;
            }
            next = Some(match next {
                None => j,
                Some(n) => {
                    let (a, b) = ((flat[n].0 - c.0, flat[n].1 - c.1), (f.0 - c.0, f.1 - c.1));
                    let turn = a.0 * b.1 - a.1 * b.0;
                    let beyond = b.0 * b.0 + b.1 * b.1 > a.0 * a.0 + a.1 * a.1;
                    if turn < 0.0 || (turn == 0.0 && beyond) {
                        j
                    } else {
                        n
                    }
                }
            });
        }
        match next {
            Some(n) if n != start => {
                out.push(n);
                current = n;
            }
            _ => break,
        }
    }
}

/// A closed surface's tetrahedron fan about an origin point: `six_v`
/// is `Σ 6·V_tet` and `weighted` `Σ 6·V_tet·(tet centroid)`, positive
/// for outward-ordered loops.
#[derive(Clone, Copy, Default)]
struct Moments {
    six_v: f64,
    weighted: DVec3,
}

impl Moments {
    fn tet(&mut self, origin: DVec3, a: DVec3, b: DVec3, c: DVec3) {
        let (a, b, c) = (a - origin, b - origin, c - origin);
        let six = a.dot(b.cross(c));
        self.six_v += six;
        self.weighted = self.weighted + (origin * 4.0 + a + b + c) * (six * 0.25);
    }

    fn fan(&mut self, origin: DVec3, poly: &[DVec3]) {
        for j in 1..poly.len().saturating_sub(1) {
            self.tet(origin, poly[0], poly[j], poly[j + 1]);
        }
    }

    fn finish(self, fallback: DVec3) -> (f64, DVec3) {
        if self.six_v > 0.0 {
            (self.six_v / 6.0, self.weighted / self.six_v)
        } else {
            (0.0, fallback)
        }
    }
}

/// Face-by-face clipper for a hull with known outward face loops,
/// reusing its buffers across the hulls of one body.
#[derive(Default)]
struct Clipper {
    cross_keys: Vec<(usize, usize)>,
    cross_pts: Vec<DVec3>,
    poly: Vec<DVec3>,
    flat: Vec<(f64, f64)>,
    order: Vec<usize>,
}

impl Clipper {
    /// Volume and centroid of the whole hull.
    fn moments(&mut self, verts: &[DVec3], loops: &[&[usize]], origin: DVec3) -> (f64, DVec3) {
        let mut m = Moments::default();
        for face in loops {
            self.poly.clear();
            self.poly.extend(face.iter().map(|&i| verts[i]));
            m.fan(origin, &self.poly);
        }
        m.finish(origin)
    }

    /// Volume and centroid of the hull's part with `s ≤ 0`: each face
    /// keeps its wet vertices and gains the crossings of its edges, and
    /// the crossings — which all lie on the cut — close the solid as a
    /// cap facing up out of the water.
    fn submerged(
        &mut self,
        verts: &[DVec3],
        loops: &[&[usize]],
        s: &[f64],
        up: DVec3,
        origin: DVec3,
    ) -> (f64, DVec3) {
        self.cross_keys.clear();
        self.cross_pts.clear();
        let mut m = Moments::default();
        for face in loops {
            self.poly.clear();
            for k in 0..face.len() {
                let (a, b) = (face[k], face[(k + 1) % face.len()]);
                if s[a] < 0.0 {
                    self.poly.push(verts[a]);
                }
                if (s[a] < 0.0) != (s[b] < 0.0) {
                    let p = self.crossing(verts, s, a, b);
                    self.poly.push(p);
                }
            }
            m.fan(origin, &self.poly);
        }
        wrap_order(&self.cross_pts, up, &mut self.flat, &mut self.order);
        for pair in self.order.windows(2).skip(1) {
            m.tet(
                origin,
                self.cross_pts[self.order[0]],
                self.cross_pts[pair[0]],
                self.cross_pts[pair[1]],
            );
        }
        m.finish(origin)
    }

    /// The point where hull edge `a–b` meets the water — computed once
    /// per edge from its lower-indexed end, so both faces sharing the
    /// edge and the cap use the identical point and the solid closes.
    fn crossing(&mut self, verts: &[DVec3], s: &[f64], a: usize, b: usize) -> DVec3 {
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        if let Some(i) = self.cross_keys.iter().position(|&key| key == (lo, hi)) {
            return self.cross_pts[i];
        }
        let t = s[lo] / (s[lo] - s[hi]);
        let p = verts[lo] + (verts[hi] - verts[lo]) * t;
        self.cross_keys.push((lo, hi));
        self.cross_pts.push(p);
        p
    }
}

// ---------------------------------------------------------------------------
// The cylinder's closed form
// ---------------------------------------------------------------------------

/// The unit disk's area on the `ξ ≤ μ` side of the chord `ξ = μ` —
/// a circular segment — and that region's `ξ`-moment.
fn disk_area(mu: f64) -> f64 {
    mu * disk_height(mu) + libm::asin(mu) + core::f64::consts::FRAC_PI_2
}

fn disk_moment(mu: f64) -> f64 {
    let g = disk_height(mu);
    -g * g * g * 2.0 / 3.0
}

fn disk_height(mu: f64) -> f64 {
    libm::sqrt((1.0 - mu * mu).max(0.0))
}

/// The antiderivatives the transition piece integrates with: of the
/// area, of the moment, and of `μ·area`.
fn disk_area_int(mu: f64) -> f64 {
    let g = disk_height(mu);
    mu * libm::asin(mu) + g - g * g * g / 3.0 + core::f64::consts::FRAC_PI_2 * mu
}

fn disk_moment_int(mu: f64) -> f64 {
    -(mu * (5.0 - 2.0 * mu * mu) * disk_height(mu) + 3.0 * libm::asin(mu)) / 12.0
}

fn disk_area_second_int(mu: f64) -> f64 {
    let g = disk_height(mu);
    (mu * g * (2.0 * mu * mu + 1.0) + (4.0 * mu * mu - 1.0) * libm::asin(mu)) / 8.0
        + core::f64::consts::FRAC_PI_4 * mu * mu
}

/// The relative rounding error a cylinder piece's closed form is
/// held to. A piece whose antiderivative differences cannot resolve
/// their terms to this is integrated by the midpoint expansion instead.
const CYLINDER_CLOSED_FORM_TOLERANCE: f64 = 1e-10;

/// Bounds on the section antiderivatives over the disk — the size of
/// the terms whose difference a piece is measured by.
const DISK_AREA_INT_BOUND: f64 = 3.2;
const DISK_MOMENT_INT_BOUND: f64 = 0.4;
const DISK_AREA_SECOND_INT_BOUND: f64 = 1.4;

/// How far a cylinder reaches along `up` from its centre: the rim's
/// contribution sideways plus a flat end's along the axis.
fn cylinder_reach(axis: DVec3, radius: f64, half_height: f64, up: DVec3) -> f64 {
    let c = up.dot(axis).abs();
    radius * libm::sqrt((1.0 - c * c).max(0.0)) + half_height * c
}

/// The submerged part of a cylinder that is neither dry nor whole: its
/// volume and centroid in body frame. `depth` is the centre's depth
/// below the water (negative above).
fn cylinder_submerged(
    axis: DVec3,
    radius: f64,
    half_height: f64,
    up: DVec3,
    depth: f64,
    centre: DVec3,
) -> (f64, DVec3) {
    let along = up.dot(axis);
    let c = along.abs();
    let s = libm::sqrt((1.0 - c * c).max(0.0));
    let (v, mx, mz) = cylinder_moments(c, s, half_height / radius, depth / radius);
    if v.is_nan() || v <= 0.0 {
        return (0.0, centre);
    }
    // `e1` is `up`'s part perpendicular to the axis — the x axis of the
    // frame the moments are expressed in, whose z axis is the axis
    // signed so `up` leans on it positively.
    let e1 = if s > 0.0 {
        (up - axis * along) / s
    } else {
        DVec3::X
    };
    let x = mx / v * radius;
    let z = mz / v * radius * if along < 0.0 { -1.0 } else { 1.0 };
    let r3 = radius * radius * radius;
    (v * r3, centre + e1 * x + axis * z)
}

/// A unit-radius cylinder of half-height `h`, axis `z`, cut by
/// `s·x + c·z ≤ depth` with `c, s ≥ 0`: its volume and first moments,
/// in units of `r³`, `r⁴`, `r⁴`. Integrating along the axis, the
/// section at `ζ` is the disk's part `ξ ≤ μ(ζ)`.
fn cylinder_moments(c: f64, s: f64, h: f64, depth: f64) -> (f64, f64, f64) {
    let mu = |zeta: f64| {
        if s > 0.0 {
            ((depth - c * zeta) / s).clamp(-1.0, 1.0)
        } else if c * zeta <= depth {
            1.0
        } else {
            -1.0
        }
    };
    // Where the section stops being the whole disk and where it
    // empties — unbounded when the plane runs along the axis.
    let (whole, empty) = if c > 0.0 {
        ((depth - s) / c, (depth + s) / c)
    } else {
        (f64::NEG_INFINITY, f64::INFINITY)
    };
    let z1 = whole.clamp(-h, h);
    let z2 = empty.clamp(z1, h);
    let pi = core::f64::consts::PI;
    let mut v = pi * (z1 + h);
    let mut mz = pi * (z1 * z1 - h * h) * 0.5;
    let mut mx = 0.0;
    if z2 > z1 {
        let (mu_a, mu_b) = (mu(z1), mu(z2));
        let (dv, dmx, dmz) =
            cylinder_transition(c, s, depth, mu_a, mu_b, (z1 + z2) * 0.5, (z2 - z1) * 0.5);
        v += dv;
        mx += dmx;
        mz += dmz;
    }
    (v, mx, mz)
}

/// The section antiderivatives' differences across `mu_b .. mu_a`.
fn section_differences(mu_a: f64, mu_b: f64) -> (f64, f64, f64) {
    (
        disk_area_int(mu_a) - disk_area_int(mu_b),
        disk_moment_int(mu_a) - disk_moment_int(mu_b),
        disk_area_second_int(mu_a) - disk_area_second_int(mu_b),
    )
}

/// The relative rounding error of `difference`, whose terms are bounded
/// by `bound`.
fn relative_rounding(difference: f64, bound: f64) -> f64 {
    2.0 * f64::EPSILON * bound / difference.abs().max(f64::MIN_POSITIVE)
}

/// A transition piece by whichever integration resolves it. The
/// antiderivatives are used while each of their three differences
/// meets [`CYLINDER_CLOSED_FORM_TOLERANCE`]; a difference that has
/// collapsed — a hair-thin piece, a section vanishing at the rim, a
/// plane nearing the axis — is integrated by the midpoint expansion
/// instead, which has no differences to lose.
fn cylinder_transition(
    c: f64,
    s: f64,
    depth: f64,
    mu_a: f64,
    mu_b: f64,
    z_mid: f64,
    e: f64,
) -> (f64, f64, f64) {
    let (d_area, d_moment, d_second) = section_differences(mu_a, mu_b);
    let resolved = [
        relative_rounding(d_area, DISK_AREA_INT_BOUND),
        relative_rounding(d_moment, DISK_MOMENT_INT_BOUND),
        relative_rounding(
            depth * d_area - s * d_second,
            DISK_AREA_INT_BOUND * depth.abs() + DISK_AREA_SECOND_INT_BOUND * s,
        ),
    ]
    .into_iter()
    .all(|error| error < CYLINDER_CLOSED_FORM_TOLERANCE);
    if resolved {
        (
            (s / c) * d_area,
            (s / c) * d_moment,
            (s / (c * c)) * (depth * d_area - s * d_second),
        )
    } else {
        cylinder_transition_expanded(c, s, depth, z_mid, e)
    }
}

/// A transition piece by the midpoint expansion in `ζ`, for a section
/// that barely changes across it — where the antiderivative
/// differences above would cancel. Over `ζ̄ ± e` each integrand is
/// `f + f''·e²/6 + f⁖·e⁴/120`, with `dμ/dζ = −c/s`.
fn cylinder_transition_expanded(c: f64, s: f64, depth: f64, z_mid: f64, e: f64) -> (f64, f64, f64) {
    let mu = ((depth - c * z_mid) / s).clamp(-1.0, 1.0);
    let (area, moment) = (disk_area(mu), disk_moment(mu));
    let g = disk_height(mu);
    if g <= 0.0 {
        return (2.0 * e * area, 2.0 * e * moment, 2.0 * e * z_mid * area);
    }
    let k = c / s;
    let (k2, k3, k4) = (k * k, k * k * k, k * k * k * k);
    let (g3, g5) = (g * g * g, g * g * g * g * g);
    let (e2, e4) = (e * e, e * e * e * e);
    let a2 = -2.0 * k2 * mu / g;
    let a4 = -6.0 * k4 * mu / g5;
    let b2 = 2.0 * k2 * (1.0 - 2.0 * mu * mu) / g;
    let b4 = -6.0 * k4 / g5;
    let p2 = -4.0 * k * g + z_mid * a2;
    let p4 = 8.0 * k3 / g3 + z_mid * a4;
    (
        2.0 * e * (area + a2 * e2 / 6.0 + a4 * e4 / 120.0),
        2.0 * e * (moment + b2 * e2 / 6.0 + b4 * e4 / 120.0),
        2.0 * e * (z_mid * area + p2 * e2 / 6.0 + p4 * e4 / 120.0),
    )
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers
// shipping fallible paths, not test code.
mod tests {
    use super::*;
    use crate::backend::{BodyHandle, PhysicsBackend};
    use crate::graph::{PartGraph, PartNode, PartNodeId, PartTransform};
    use crate::part::{ColliderShape, WeldPart};
    use crate::simple::{SimpleBackend, gravity_force_world};
    use crate::tensor::SymMat3;
    use crate::weld::weld;
    use csp_core::DQuat;

    const TERRIN_R: f64 = 600_000.0;
    const TERRIN_MU: f64 = 3.5316e12;
    const PI: f64 = core::f64::consts::PI;
    const SQRT_3: f64 = 1.732_050_807_568_877_2;

    fn lifted<T>(r: Result<T, impl core::fmt::Debug>) -> T {
        match r {
            Ok(v) => v,
            Err(e) => panic!("{e:?}"),
        }
    }

    fn assert_rel(actual: f64, expected: f64, tol: f64, what: &str) {
        assert!(
            (actual - expected).abs() <= tol * expected.abs() + tol * 1e-9,
            "{what}: {actual} vs {expected}"
        );
    }

    fn assert_pt(actual: DVec3, expected: DVec3, tol: f64, what: &str) {
        assert!(
            (actual - expected).norm() <= tol,
            "{what}: {actual:?} vs {expected:?}"
        );
    }

    /// Terrin with an ocean over a bare-sphere seabed `seabed_gap`
    /// metres below the datum.
    fn ocean_env(seabed_gap: f64) -> Environment {
        Environment {
            body_center: DVec3::ZERO,
            radius_m: TERRIN_R - seabed_gap,
            mu_m3_per_s2: TERRIN_MU,
            body_spin: DVec3::ZERO,
            atmosphere: None,
            settle_v: crate::simple::DEFAULT_SETTLE_V,
            settle_w: crate::simple::DEFAULT_SETTLE_W,
            settle_ticks: crate::simple::DEFAULT_SETTLE_TICKS,
            friction_mu: crate::simple::DEFAULT_FRICTION_MU,
            drag_cd: crate::simple::DEFAULT_DRAG_CD,
            convective_heat_fraction: crate::DEFAULT_CONVECTIVE_HEAT_FRACTION,
            touchdown_crush_m: crate::DEFAULT_TOUCHDOWN_CRUSH_M,
            terrain: None,
            ocean: Some(Ocean::at_datum(TERRIN_R)),
        }
    }

    fn weld_part(mass_kg: f64) -> WeldPart {
        WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            parachute: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(mass_kg, DVec3::new(0.5, 0.5, 0.5)),
            collider: ColliderShape::Box {
                half_extents: DVec3::new(0.5, 0.5, 0.5),
            },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: csp_data::ThermalDef {
                heat_capacity_j_per_k: 100_000.0,
                max_temp_k: 1_200.0,
                emissivity: 0.8,
                conduction_w_per_k: 50.0,
            },
        }
    }

    /// Two unit cubes stacked along body +z — the lower one `lower_kg`,
    /// centred at the origin, the upper one `upper_kg` a metre above.
    fn cube_stack(lower_kg: f64, upper_kg: f64) -> WeldedBody {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.0, 0.0, 1.0), DQuat::IDENTITY),
                1,
            ),
        );
        lifted(weld(&g, &[weld_part(lower_kg), weld_part(upper_kg)]))
    }

    /// One part of `mass_kg` the resolver's way with a `Cylinder`
    /// of `radius` and `half_height`: the 16-point prism and the true
    /// cylinder's inertia.
    fn one_cylinder(mass_kg: f64, radius: f64, half_height: f64) -> WeldedBody {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        let mut part = weld_part(mass_kg);
        part.collider = ColliderShape::Points(cylinder_verts(radius, half_height));
        part.inertia =
            SymMat3::solid_cylinder(mass_kg, radius, half_height, crate::tensor::Axis::Z);
        lifted(weld(&g, &[part]))
    }

    fn one_cube(mass_kg: f64) -> WeldedBody {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        lifted(weld(&g, &[weld_part(mass_kg)]))
    }

    /// The resolver's `Cylinder` tessellation — `scale_collider`'s
    /// eight ring directions at `±h`.
    fn cylinder_verts(radius: f64, half_height: f64) -> Vec<DVec3> {
        let t = core::f64::consts::FRAC_1_SQRT_2;
        let ring = [
            (1.0, 0.0),
            (t, t),
            (0.0, 1.0),
            (-t, t),
            (-1.0, 0.0),
            (-t, -t),
            (0.0, -1.0),
            (t, -t),
        ];
        ring.iter()
            .chain(ring.iter())
            .enumerate()
            .map(|(i, &(x, y))| {
                let z = if i < 8 { half_height } else { -half_height };
                DVec3::new(radius * x, radius * y, z)
            })
            .collect()
    }

    /// The shipped nose cone's authored `Convex` set — a square
    /// pyramid, the general-polytope path.
    fn pyramid_verts() -> Vec<DVec3> {
        vec![
            DVec3::new(0.625, 0.0, -0.45),
            DVec3::new(-0.625, 0.0, -0.45),
            DVec3::new(0.0, 0.625, -0.45),
            DVec3::new(0.0, -0.625, -0.45),
            DVec3::new(0.0, 0.0, 0.45),
        ]
    }

    fn placed(verts: &[DVec3], q: DQuat, offset: DVec3) -> Vec<DVec3> {
        verts.iter().map(|v| q.rotate(*v) + offset).collect()
    }

    fn tilts() -> Vec<DQuat> {
        [
            (DVec3::new(1.0, 0.0, 0.0), 0.3),
            (DVec3::new(0.0, 1.0, 0.0), 1.1),
            (DVec3::new(1.0, 2.0, 3.0), 0.7),
            (DVec3::new(-2.0, 1.0, 0.5), 2.4),
            (DVec3::new(0.3, -0.4, 1.0), 1.9),
        ]
        .into_iter()
        .map(|(axis, angle)| DQuat::from_axis_angle(axis / axis.norm(), angle))
        .collect()
    }

    /// Submersion of `verts` under the plane `up·(p − centre) ≤ depth`
    /// — `(0, origin)` while dry.
    fn submersion(verts: &[DVec3], up: DVec3, depth: f64) -> (f64, DVec3) {
        let (shape, centre) = Shape::of(verts);
        let s: Vec<f64> = verts
            .iter()
            .map(|v| (*v - centre).dot(up) - depth)
            .collect();
        let in_water = HullInWater {
            shape,
            centre,
            depth,
            s: &s,
        };
        in_water
            .shape
            .submerged(verts, &in_water, up, &mut Clipper::default())
            .map_or((0.0, DVec3::ZERO), |w| (w.volume_m3, w.centroid))
    }

    // ---- references -------------------------------------------------------

    /// A tetrahedron cut by `s ≤ 0`, through its wet-vertex cases: a
    /// lone wet vertex leaves that corner's similar tetrahedron, three
    /// leave the whole less the dry tip, and two leave a triangular
    /// prism of three tetrahedra.
    fn tet_clip(p: [DVec3; 4], s: [f64; 4]) -> (f64, DVec3) {
        let tet = |q: [DVec3; 4]| {
            let v = (q[1] - q[0]).dot((q[2] - q[0]).cross(q[3] - q[0])).abs() / 6.0;
            (v, (q[0] + q[1] + q[2] + q[3]) * 0.25)
        };
        // The point where the edge `x → y` meets the plane, a fraction
        // `s[x] / (s[x] − s[y])` of the way from `x`.
        let cut = |x: usize, y: usize| p[x] + (p[y] - p[x]) * (s[x] / (s[x] - s[y]));
        let tip = |x: usize| {
            tet([
                p[x],
                cut(x, (x + 1) % 4),
                cut(x, (x + 2) % 4),
                cut(x, (x + 3) % 4),
            ])
        };
        let wet: Vec<usize> = (0..4).filter(|&i| s[i] < 0.0).collect();
        let (whole_v, whole_c) = tet(p);
        match wet[..] {
            [] => (0.0, whole_c),
            [w] => tip(w),
            [a, b] => {
                let dry: Vec<usize> = (0..4).filter(|i| s[*i] >= 0.0).collect();
                let (c, d) = (dry[0], dry[1]);
                let (v, m) = [
                    tet([p[a], p[b], cut(c, a), cut(d, a)]),
                    tet([p[b], cut(c, a), cut(c, b), cut(d, a)]),
                    tet([p[b], cut(c, b), cut(d, b), cut(d, a)]),
                ]
                .into_iter()
                .fold((0.0, DVec3::ZERO), |(v, m), (tv, tc)| (v + tv, m + tc * tv));
                (v, m / v)
            }
            _ => {
                let (mut v, mut m) = (whole_v, whole_c * whole_v);
                for d in (0..4).filter(|d| s[*d] >= 0.0) {
                    let (tv, tc) = tip(d);
                    v -= tv;
                    m = m - tc * tv;
                }
                (v, m / v)
            }
        }
    }

    /// Volume and centroid of a list of tetrahedra given as vertex
    /// index quadruples.
    fn tet_sum(pieces: &[[usize; 4]], verts: &[DVec3], s: &[f64]) -> (f64, DVec3) {
        let (mut volume, mut moment) = (0.0, DVec3::ZERO);
        for t in pieces {
            let (v, c) = tet_clip(t.map(|i| verts[i]), t.map(|i| s[i]));
            volume += v;
            moment = moment + c * v;
        }
        (volume, moment / volume)
    }

    /// The box as six tetrahedra about its main diagonal
    /// (`ColliderShape::Box` vertex order: bit 0 = z, bit 1 = y, bit 2 = x).
    fn box_reference(verts: &[DVec3], s: &[f64]) -> (f64, DVec3) {
        tet_sum(
            &[
                [0, 1, 3, 7],
                [0, 2, 3, 7],
                [0, 1, 5, 7],
                [0, 4, 5, 7],
                [0, 2, 6, 7],
                [0, 4, 6, 7],
            ],
            verts,
            s,
        )
    }

    /// The square pyramid as two tetrahedra about a base diagonal.
    fn pyramid_reference(verts: &[DVec3], s: &[f64]) -> (f64, DVec3) {
        tet_sum(&[[0, 2, 1, 4], [0, 1, 3, 4]], verts, s)
    }

    /// Composite Simpson of `f` over `[a, b]`.
    fn simpson(f: impl Fn(f64) -> f64, a: f64, b: f64, panels: usize) -> f64 {
        let h = (b - a) / panels as f64;
        let mut sum = f(a) + f(b);
        for i in 1..panels {
            sum += f(a + i as f64 * h) * if i % 2 == 1 { 4.0 } else { 2.0 };
        }
        sum * h / 3.0
    }

    /// The cylinder's submerged moments by integrating its cross
    /// sections directly — the definition `cylinder_moments` closes —
    /// panel by panel between the breakpoints.
    fn cylinder_reference(c: f64, s: f64, h: f64, depth: f64) -> (f64, f64, f64) {
        let section = |zeta: f64| {
            if s > 0.0 {
                ((depth - c * zeta) / s).clamp(-1.0, 1.0)
            } else if c * zeta <= depth {
                1.0
            } else {
                -1.0
            }
        };
        let mut cuts = vec![-h];
        if c > 0.0 {
            cuts.extend([
                ((depth - s) / c).clamp(-h, h),
                ((depth + s) / c).clamp(-h, h),
            ]);
        }
        cuts.push(h);
        cuts.sort_by(f64::total_cmp);
        cuts.iter()
            .zip(cuts.iter().skip(1))
            .filter(|(a, b)| **b > **a)
            .map(|(&a, &b)| piece_integral(section, a, b))
            .fold((0.0, 0.0, 0.0), |(v, x, z), (dv, dx, dz)| {
                (v + dv, x + dx, z + dz)
            })
    }

    fn halton(mut index: u64, base: u64) -> f64 {
        let (mut f, mut r) = (1.0, 0.0);
        while index > 0 {
            f /= base as f64;
            r += f * (index % base) as f64;
            index /= base;
        }
        r
    }

    /// Volume and centroid of `inside ∩ {up·p ≤ depth}` over a cube of
    /// half-side `bound`, by a Halton sequence — an independent check
    /// to sampling accuracy.
    fn sampled(
        inside: impl Fn(DVec3) -> bool,
        up: DVec3,
        depth: f64,
        bound: f64,
        n: u64,
    ) -> (f64, DVec3) {
        let (mut count, mut sum) = (0u64, DVec3::ZERO);
        for i in 1..=n {
            let p = DVec3::new(
                2.0 * bound * halton(i, 2) - bound,
                2.0 * bound * halton(i, 3) - bound,
                2.0 * bound * halton(i, 5) - bound,
            );
            if inside(p) && p.dot(up) <= depth {
                count += 1;
                sum = sum + p;
            }
        }
        let side = 2.0 * bound;
        (
            side * side * side * count as f64 / n as f64,
            sum / count.max(1) as f64,
        )
    }

    // ---- the disk sections ------------------------------------------------

    /// Differentiating a disk segment's area along its chord gives the
    /// chord's length, and its moment's derivative the chord's first
    /// moment — the fundamental theorem the cylinder's antiderivatives
    /// rest on.
    #[test]
    fn disk_sections_differentiate_to_their_chords() {
        assert_eq!(disk_area(-1.0), 0.0);
        assert_rel(disk_area(0.0), PI / 2.0, 1e-15, "half disk");
        assert_rel(disk_area(1.0), PI, 1e-15, "whole disk");
        assert_rel(disk_moment(1.0), 0.0, 1e-15, "whole disk moment");
        assert_rel(disk_moment(0.0), -2.0 / 3.0, 1e-14, "half disk moment");
        for i in 1..40 {
            let mu = -1.0 + i as f64 / 20.0;
            let e = 1e-6;
            let g = disk_height(mu);
            assert_rel(
                (disk_area(mu + e) - disk_area(mu - e)) / (2.0 * e),
                2.0 * g,
                1e-6,
                "area slope is the chord",
            );
            assert_rel(
                (disk_moment(mu + e) - disk_moment(mu - e)) / (2.0 * e),
                2.0 * mu * g,
                1e-5,
                "moment slope is the chord's moment",
            );
            // The antiderivatives integrate back to the segment.
            assert_rel(
                disk_area_int(mu) - disk_area_int(-1.0),
                simpson(disk_area, -1.0, mu, 4000),
                1e-8,
                "area integral",
            );
            assert_rel(
                disk_moment_int(mu) - disk_moment_int(-1.0),
                simpson(disk_moment, -1.0, mu, 4000),
                1e-8,
                "moment integral",
            );
            assert_rel(
                disk_area_second_int(mu) - disk_area_second_int(-1.0),
                simpson(|x| x * disk_area(x), -1.0, mu, 4000),
                1e-8,
                "second integral",
            );
        }
    }

    // ---- submersion -------------------------------------------------------

    /// A box displaces what its geometry says: half, three quarters and
    /// a quarter under, with the centroid of the submerged slab — and a
    /// face exactly on the waterline is dry, not wet.
    #[test]
    fn box_submersion_matches_the_closed_forms() {
        let verts = ColliderShape::Box {
            half_extents: DVec3::new(0.5, 0.5, 0.5),
        }
        .vertices();
        for (depth, volume, centroid_z) in [
            (0.0, 0.5, -0.25),
            (0.25, 0.75, -0.125),
            (-0.25, 0.25, -0.375),
            (0.5, 1.0, 0.0),
            (0.9, 1.0, 0.0),
        ] {
            let (v, c) = submersion(&verts, DVec3::Z, depth);
            assert_rel(v, volume, 1e-12, "box volume");
            assert_pt(c, DVec3::new(0.0, 0.0, centroid_z), 1e-12, "box centroid");
        }
        assert_eq!(
            submersion(&verts, DVec3::Z, -0.5).0,
            0.0,
            "a face on the waterline is dry"
        );
        assert_eq!(submersion(&verts, DVec3::Z, -0.6).0, 0.0);
        // Tipped onto an edge: the plane through opposite corners of
        // the square section leaves exactly half the box under.
        let diagonal = DVec3::new(1.0, 0.0, 1.0) / core::f64::consts::SQRT_2;
        assert_rel(
            submersion(&verts, diagonal, 0.0).0,
            0.5,
            1e-12,
            "diagonal half",
        );
        // A translated solid displaces the same.
        let half = submersion(&verts, DVec3::Z, 0.0).0;
        let shifted: Vec<DVec3> = verts
            .iter()
            .map(|p| *p + DVec3::new(0.0, 0.0, 3.0))
            .collect();
        assert_rel(
            submersion(&shifted, DVec3::Z, 0.0).0,
            half,
            1e-12,
            "translated",
        );
    }

    /// Tilted cuts of a slab and of the nose cone's pyramid agree with
    /// an independent tetrahedron-by-tetrahedron reference, volume and
    /// centroid alike.
    #[test]
    fn box_and_pyramid_submersion_match_a_tetrahedral_reference() {
        let half = DVec3::new(0.5, 0.75, 0.2);
        let slab = ColliderShape::Box { half_extents: half }.vertices();
        let pyramid = pyramid_verts();
        let up = DVec3::new(0.3, -0.5, 0.81) / DVec3::new(0.3, -0.5, 0.81).norm();
        for q in tilts() {
            for (base, reference, full) in [
                (
                    &slab[..],
                    box_reference as fn(&[DVec3], &[f64]) -> (f64, DVec3),
                    8.0 * half.x * half.y * half.z,
                ),
                (
                    &pyramid[..],
                    pyramid_reference as fn(&[DVec3], &[f64]) -> (f64, DVec3),
                    0.625 * 0.625 * 2.0 * 0.9 / 3.0,
                ),
            ] {
                let verts = placed(base, q, DVec3::ZERO);
                // The nose cone's vertex mean is not its origin: both
                // readings take the waterline relative to it.
                let centre = Shape::of(&verts).1;
                let bound = verts
                    .iter()
                    .map(|v| (*v - centre).dot(up))
                    .fold(f64::NEG_INFINITY, f64::max);
                for depth in [
                    -bound * 1.1,
                    -bound * 0.9,
                    -bound * 0.3,
                    0.0,
                    bound * 0.2,
                    bound * 0.8,
                    bound * 1.1,
                ] {
                    let s: Vec<f64> = verts
                        .iter()
                        .map(|v| (*v - centre).dot(up) - depth)
                        .collect();
                    let (rv, rc) = reference(&verts, &s);
                    let (v, c) = submersion(&verts, up, depth);
                    assert!(
                        (0.0..=full * (1.0 + 1e-12)).contains(&v),
                        "submerged volume out of range: {v}"
                    );
                    assert_rel(v, rv, 1e-11, "volume vs tetrahedra");
                    if rv > 1e-9 {
                        assert_pt(c, rc, 1e-9, "centroid vs tetrahedra");
                    }
                }
            }
        }
    }

    /// A cylinder displaces the cylinder, not its octagons: upright and
    /// on its side the closed forms are exact, a rim dips below a
    /// waterline all sixteen tessellation points clear, and whole and
    /// dry are whole and dry.
    #[test]
    fn cylinder_submersion_matches_the_closed_forms() {
        let (r, h) = (0.625, 2.0);
        let verts = cylinder_verts(r, h);
        let (shape, centre) = Shape::of(&verts);
        let Shape::Cylinder {
            axis,
            radius,
            half_height,
        } = shape
        else {
            panic!("the resolver's prism must classify as a cylinder");
        };
        assert!(
            axis.x.abs() < 1e-12 && axis.y.abs() < 1e-12 && (axis.z.abs() - 1.0).abs() < 1e-12,
            "axis {axis:?}"
        );
        assert_rel(radius, r, 1e-12, "radius");
        assert_rel(half_height, h, 1e-12, "half height");
        assert_pt(centre, DVec3::ZERO, 1e-12, "centre");
        let full = PI * r * r * 2.0 * h;
        assert_rel(
            shape.whole(&verts, DVec3::ZERO, &mut Clipper::default()).0,
            full,
            1e-12,
            "full volume",
        );
        // Upright, drafted `draft` metres: a shorter cylinder.
        assert_eq!(
            submersion(&verts, DVec3::Z, -h).0,
            0.0,
            "a dry cylinder displaces nothing"
        );
        for draft in [0.3, 1.9, 3.9999, 4.0] {
            let (v, c) = submersion(&verts, DVec3::Z, draft - h);
            assert_rel(v, PI * r * r * draft, 1e-9, "upright volume");
            assert_pt(
                c,
                DVec3::new(0.0, 0.0, -h + draft * 0.5),
                1e-7,
                "upright centroid",
            );
        }
        // On its side, sunk to the chord `mu`: a circular segment
        // extruded along the axis.
        for mu in [-0.9, -0.4, 0.0, 0.35, 0.8] {
            let (v, c) = submersion(&verts, DVec3::X, r * mu);
            assert_rel(v, r * r * disk_area(mu) * 2.0 * h, 1e-9, "sideways volume");
            assert_rel(
                c.x,
                r * disk_moment(mu) / disk_area(mu),
                1e-9,
                "sideways centroid",
            );
            assert!(
                c.z.abs() < 1e-9 && c.y.abs() < 1e-9,
                "sideways centroid on the axis: {c:?}"
            );
        }
        // Whole and dry.
        assert_eq!(submersion(&verts, DVec3::Z, h + 1.0).0, full);
        assert_eq!(submersion(&verts, DVec3::Z, -h - 1.0).0, 0.0);
    }

    /// A cylinder leans between its tessellation points: the round rim
    /// dips below a waterline every one of the sixteen points clears,
    /// and touching the rim exactly is still dry.
    #[test]
    fn cylinder_rim_dips_below_its_tessellation_points() {
        let (r, h) = (0.625, 2.0);
        let verts = cylinder_verts(r, h);
        // Tilted half a radian, leaning halfway between two ring points.
        let (lean, off) = (0.5, core::f64::consts::PI / 8.0);
        let up = DVec3::new(
            libm::sin(lean) * libm::cos(off),
            libm::sin(lean) * libm::sin(off),
            libm::cos(lean),
        );
        let reach = r * libm::sin(lean) + h * libm::cos(lean);
        let lowest = verts
            .iter()
            .map(|v| v.dot(up))
            .fold(f64::INFINITY, f64::min);
        assert!(
            -lowest < reach,
            "the tessellation points under-report the rim"
        );
        assert_eq!(
            submersion(&verts, up, -reach).0,
            0.0,
            "touching the rim is dry"
        );
        let sliver = submersion(&verts, up, lowest + 1e-6).0;
        assert!(
            sliver > 0.0,
            "the rim dips below a waterline the points clear"
        );
        assert_rel(
            submersion(&verts, up, reach).0,
            PI * r * r * 2.0 * h,
            1e-9,
            "water at the top of the rim is whole",
        );
        assert!(
            sliver < PI * r * r * 2.0 * h * 1e-4,
            "the sliver is thin: {sliver}"
        );
    }

    /// `in_water` is the force model's own dry test, not a vertex scan:
    /// a cylinder leaning between its tessellation points is in the
    /// water — and displacing — while all sixteen of them are dry.
    #[test]
    fn in_water_sees_the_rim_between_the_tessellation_points() {
        let env = ocean_env(500.0);
        let (r, h) = (0.625, 2.0);
        let lean = 0.5;
        let mut body = one_cylinder(500.0, r, h);
        // Half a radian off world up (+x), leaning halfway between two
        // ring points.
        let turn = DQuat::from_axis_angle(DVec3::Y, core::f64::consts::FRAC_PI_2 - lean);
        body.rot = turn * DQuat::from_axis_angle(DVec3::Z, core::f64::consts::PI / 8.0);
        let lowest_point =
            r * libm::sin(lean) * libm::cos(core::f64::consts::PI / 8.0) + h * libm::cos(lean);
        let rim = r * libm::sin(lean) + h * libm::cos(lean);
        for (height, expect_wet, what) in [
            (
                lowest_point + 0.01,
                true,
                "the rim is under, the points clear",
            ),
            (rim + 0.01, false, "clear of the rim"),
        ] {
            body.pos = DVec3::new(TERRIN_R + height, 0.0, 0.0);
            let wet = SimpleBackend::new(env.clone()).in_water(&body);
            let parts = lifted_loads(&env, &body).parts.len();
            assert_eq!(
                (wet, parts),
                (expect_wet, usize::from(expect_wet)),
                "{what}"
            );
        }
    }

    /// Every lean and draft of the closed form agrees with integrating
    /// the cylinder's cross sections directly — volume, lateral moment
    /// and axial moment.
    #[test]
    fn cylinder_moments_match_their_section_integrals() {
        let h = 3.2;
        let scale = PI * 2.0 * h;
        let mut worst = 0.0f64;
        for i in 0..=16 {
            let lean = i as f64 / 16.0 * core::f64::consts::FRAC_PI_2;
            let (c, s) = (libm::cos(lean), libm::sin(lean));
            let reach = s + h * c;
            for k in 0..=32 {
                let depth = -reach + 2.0 * reach * k as f64 / 32.0;
                let got = cylinder_moments(c, s, h, depth);
                let want = cylinder_reference(c, s, h, depth);
                for (g, w, what) in [
                    (got.0, want.0, "volume"),
                    (got.1, want.1, "lateral moment"),
                    (got.2, want.2, "axial moment"),
                ] {
                    let err = (g - w).abs() / scale;
                    worst = worst.max(err);
                    assert!(
                        err < 1e-8,
                        "{what} at lean {lean}, depth {depth}: {g} vs {w}"
                    );
                }
            }
        }
        assert!(worst > 0.0, "the sweep compared real values");
    }

    /// Every way a transition piece can collapse — a hair-thin piece,
    /// a section fading out at the rim, a plane leaning onto the axis —
    /// still integrates to its cross sections, by whichever branch the
    /// conditioning picks.
    #[test]
    fn cylinder_transition_pieces_match_their_section_integrals() {
        let mut worst = 0.0f64;
        for (c, h) in [(0.3, 1.6), (0.9, 0.2), (0.05, 3.2), (0.003, 2.0)] {
            let s = libm::sqrt(1.0 - c * c);
            let reach = s + h * c;
            for k in 0..=8 {
                let depth = -reach + 2.0 * reach * k as f64 / 8.0;
                let z1 = ((depth - s) / c).clamp(-h, h);
                let z2 = ((depth + s) / c).clamp(z1, h);
                // The whole piece, and slivers of it at either end.
                for (a, b) in [
                    (z1, z2),
                    (z1, z1 + (z2 - z1) * 1e-3),
                    (z2 - (z2 - z1) * 1e-3, z2),
                ] {
                    if b <= a {
                        continue;
                    }
                    let mu = |zeta: f64| ((depth - c * zeta) / s).clamp(-1.0, 1.0);
                    let got = cylinder_transition(
                        c,
                        s,
                        depth,
                        mu(a),
                        mu(b),
                        (a + b) * 0.5,
                        (b - a) * 0.5,
                    );
                    let want = piece_integral(mu, a, b);
                    let scale = PI * 2.0 * h;
                    for (g, w, what) in [
                        (got.0, want.0, "volume"),
                        (got.1, want.1, "lateral"),
                        (got.2, want.2, "axial"),
                    ] {
                        let err = (g - w).abs() / scale;
                        worst = worst.max(err);
                        assert!(
                            err < 1e-8,
                            "{what} at c={c}, h={h}, depth={depth}, piece {a}..{b}: {g} vs {w}"
                        );
                    }
                }
            }
        }
        assert!(worst > 0.0);
    }

    /// A transition piece's moments, integrating its cross sections
    /// with the mesh graded into any rim at an end.
    fn piece_integral(mu: impl Fn(f64) -> f64, a: f64, b: f64) -> (f64, f64, f64) {
        let (rim_a, rim_b) = (mu(a).abs() >= 1.0 - 1e-12, mu(b).abs() >= 1.0 - 1e-12);
        // `ζ(t)` and `dζ/dt` over `t ∈ [0, 1]`.
        let at = |t: f64| {
            let graded = (a + (b - a) * t * t, 2.0 * (b - a) * t);
            if rim_a {
                graded
            } else if rim_b {
                (b - (graded.0 - a), graded.1)
            } else {
                (a + (b - a) * t, b - a)
            }
        };
        let integrate = |f: &dyn Fn(f64) -> f64| {
            simpson(
                |t| {
                    let (zeta, slope) = at(t);
                    f(zeta) * slope
                },
                0.0,
                1.0,
                4000,
            )
        };
        (
            integrate(&|z| disk_area(mu(z))),
            integrate(&|z| disk_moment(mu(z))),
            integrate(&|z| z * disk_area(mu(z))),
        )
    }

    /// Depth is the integral of the waterline's section, so nudging it
    /// may move the result only as far as that section allows — no
    /// step at any breakpoint or branch switch.
    #[test]
    fn cylinder_moments_are_continuous_in_depth() {
        let h = 3.2;
        for i in 0..=8 {
            let lean = i as f64 / 8.0 * core::f64::consts::FRAC_PI_2;
            let (c, s) = (libm::cos(lean), libm::sin(lean));
            let reach = s + h * c;
            let mut previous = cylinder_moments(c, s, h, -reach);
            let (mut jumps, bound) = (0.0f64, PI * c + 4.0 * h * s);
            for k in 1..=8000 {
                let depth = -reach + 2.0 * reach * k as f64 / 8000.0;
                let now = cylinder_moments(c, s, h, depth);
                let delta = (now.0 - previous.0).abs();
                assert!(
                    delta <= 1.1 * bound * (2.0 * reach / 8000.0) + 1e-12,
                    "volume step {delta} at lean {lean}, depth {depth}"
                );
                jumps = jumps.max(delta);
                previous = now;
            }
            assert!(jumps > 0.0);
        }
    }

    /// Against point sampling, a leaning cylinder's displaced volume
    /// and centroid are the cylinder's — a check with no formula in
    /// common with the closed form.
    #[test]
    fn leaning_cylinder_submersion_matches_sampling() {
        let (r, h) = (1.0, 1.5);
        let verts = cylinder_verts(r, h);
        let up = DVec3::new(0.45, -0.3, 0.84) / DVec3::new(0.45, -0.3, 0.84).norm();
        let inside = |p: DVec3| p.x * p.x + p.y * p.y <= 1.0 && p.z.abs() <= h;
        for depth in [-0.6, 0.2, 0.9] {
            let (v, c) = submersion(&verts, up, depth);
            let (sv, sc) = sampled(inside, up, depth, h, 1 << 18);
            assert_rel(v, sv, 0.02, "sampled volume");
            assert_pt(c, sc, 0.03, "sampled centroid");
        }
    }

    /// Each solid's silhouette: a box by its faces and their
    /// projections, a cylinder by its end and its side, the pyramid by
    /// its base alone when seen from its apex.
    #[test]
    fn projected_areas_match_the_known_faces() {
        let half = DVec3::new(0.5, 0.75, 0.2);
        let (slab, slab_centre) = Shape::of(&ColliderShape::Box { half_extents: half }.vertices());
        let diagonal = DVec3::new(1.0, 1.0, 1.0) / SQRT_3;
        for (dir, area) in [
            (DVec3::X, 1.5 * 0.4),
            (DVec3::Y, 1.0 * 0.4),
            (DVec3::Z, 1.0 * 1.5),
            (diagonal, 4.0 * (0.375 + 0.1 + 0.15) / SQRT_3),
        ] {
            assert_rel(
                slab.projected_area(&[], slab_centre, dir),
                area,
                1e-12,
                "slab silhouette",
            );
        }
        let verts = cylinder_verts(0.625, 2.0);
        let (cyl, cyl_centre) = Shape::of(&verts);
        assert_rel(
            cyl.projected_area(&verts, cyl_centre, DVec3::Z),
            PI * 0.625 * 0.625,
            1e-12,
            "cylinder end",
        );
        assert_rel(
            cyl.projected_area(&verts, cyl_centre, DVec3::X),
            4.0 * 0.625 * 2.0,
            1e-12,
            "cylinder side",
        );
        let lean = DVec3::new(0.6, 0.0, 0.8);
        assert_rel(
            cyl.projected_area(&verts, cyl_centre, lean),
            PI * 0.625 * 0.625 * 0.8 + 4.0 * 0.625 * 2.0 * 0.6,
            1e-12,
            "cylinder leaning",
        );
        let pyramid = pyramid_verts();
        let (pyr, pyr_centre) = Shape::of(&pyramid);
        assert_rel(
            pyr.projected_area(&pyramid, pyr_centre, DVec3::Z),
            0.78125,
            1e-12,
            "pyramid from the apex",
        );
        let Shape::Polytope { faces } = pyr else {
            panic!("the nose cone is a general polytope");
        };
        assert_eq!(faces.len(), 5, "a square pyramid has five facets");
        let mut clipper = Clipper::default();
        let loops: Vec<&[usize]> = faces.iter().map(Vec::as_slice).collect();
        let (whole, centroid) = clipper.moments(&pyramid, &loops, pyr_centre);
        assert_rel(whole, 0.234_375, 1e-12, "pyramid volume");
        assert_pt(
            centroid,
            DVec3::new(0.0, 0.0, -0.225),
            1e-12,
            "pyramid centroid, a quarter above its base",
        );
    }

    /// The classes are read off the vertices wherever the craft puts
    /// them — and nothing else is let in: a prism with one vertex off
    /// its circle is a polytope, and so is a flat octagon.
    #[test]
    fn shapes_are_classified_from_their_vertices() {
        let (r, h) = (0.625, 2.0);
        let q = DQuat::from_axis_angle(
            DVec3::new(1.0, -2.0, 3.0) / DVec3::new(1.0, -2.0, 3.0).norm(),
            0.9,
        );
        let offset = DVec3::new(0.3, -1.1, 4.0);
        let far = placed(&cylinder_verts(r, h), q, offset);
        match Shape::of(&far) {
            (
                Shape::Cylinder {
                    axis,
                    radius,
                    half_height,
                },
                centre,
            ) => {
                let craft_axis = q.rotate(DVec3::Z);
                assert_pt(
                    axis * axis.dot(craft_axis),
                    craft_axis,
                    1e-9,
                    "axis follows the craft",
                );
                assert_rel(radius, r, 1e-9, "radius");
                assert_rel(half_height, h, 1e-9, "half height");
                assert_pt(centre, offset, 1e-9, "centre");
            }
            _ => panic!("a placed resolver prism is a cylinder"),
        }
        let mut irregular = cylinder_verts(r, h);
        irregular[3].y += 0.01;
        assert!(
            matches!(Shape::of(&irregular).0, Shape::Polytope { .. }),
            "a vertex off the circle is not a cylinder"
        );
        let flat: Vec<DVec3> = cylinder_verts(r, h)[..8].to_vec();
        assert!(
            matches!(Shape::of(&flat).0, Shape::Polytope { .. }),
            "a flat octagon is not a box"
        );
        let slab = ColliderShape::Box {
            half_extents: DVec3::new(0.5, 0.75, 0.2),
        }
        .vertices();
        let turned = placed(&slab, q, offset);
        let Shape::Box {
            axes,
            half,
            corners,
        } = Shape::of(&turned).0
        else {
            panic!("a turned box is a box");
        };
        for (want, extent) in [(DVec3::X, 0.5), (DVec3::Y, 0.75), (DVec3::Z, 0.2)] {
            let craft_axis = q.rotate(want);
            let k = (0..3)
                .find(|k| (axes[*k].dot(craft_axis)).abs() > 1.0 - 1e-9)
                .unwrap_or_else(|| panic!("no box axis along {want:?}"));
            assert_rel(half[k], extent, 1e-9, "box extent along {want:?}");
        }
        let mut seen = [false; 8];
        for &index in &corners {
            seen[index] = true;
        }
        assert!(seen.into_iter().all(|s| s), "eight distinct corners");
    }

    // ---- forces -----------------------------------------------------------

    fn lifted_loads(env: &Environment, body: &WeldedBody) -> WaterLoads {
        WaterLoads::of(env, body).unwrap_or_else(|| panic!("the body is in the ocean's reach"))
    }

    /// Where a floating box comes to rest, the water under it carries
    /// exactly its weight — and a box heavier than its own volume of
    /// water displaces the whole volume and is no floater.
    #[test]
    fn buoyancy_balances_a_floating_boxs_weight_at_its_waterline() {
        let env = ocean_env(500.0);
        let (mass, rho) = (500.0, DEFAULT_WATER_DENSITY_KG_M3);
        // Archimedes: `mass` displaces `mass / ρ` of the cube's 1 m³.
        let draft = mass / rho;
        let mut body = one_cube(mass);
        body.pos = DVec3::new(TERRIN_R + 0.5 - draft, 0.0, 0.0);
        let loads = lifted_loads(&env, &body);
        assert!(loads.supported(), "a {mass} kg cube is lighter than water");
        let weight = gravity_force_world(&env, body.com_world() - env.body_center, mass)
            .unwrap_or_else(|| panic!("the body is off the body centre"));
        assert_eq!(loads.parts.len(), 1, "one wet part");
        let p = &loads.parts[0];
        // Gravity is local: it is 8.5e-7 weaker a quarter metre up at
        // the submerged slab's centroid than at the cube's own centre.
        assert!(
            (p.force + weight).norm() < 1e-5 * mass * 9.81,
            "net force {:?}",
            p.force + weight
        );
        assert_eq!(
            p.torque,
            DVec3::ZERO,
            "at rest relative to the water there is no drag"
        );
        assert_pt(
            p.at,
            DVec3::new(TERRIN_R - draft * 0.5, 0.0, 0.0),
            1e-12,
            "the force acts at the submerged slab's centre",
        );

        let mut sinker = one_cube(2000.0);
        sinker.pos = DVec3::new(TERRIN_R - 100.0, 0.0, 0.0);
        let loads = lifted_loads(&env, &sinker);
        assert!(!loads.supported(), "a 2000 kg cube sinks");
        let up = gravity_force_world(&env, sinker.com_world() - env.body_center, -rho)
            .unwrap_or_else(|| panic!("the body is off the body centre"));
        assert_pt(
            loads.parts[0].force,
            up,
            1e-9,
            "a submerged cube displaces its 1 m³",
        );
    }

    /// Stability is where the weight sits relative to the displaced
    /// volume: nudged off vertical, a top-heavy stack is pushed further
    /// over and a bottom-heavy one pushed back — upright or inverted.
    #[test]
    fn buoyancy_capsizes_a_top_heavy_stack_and_rights_a_bottom_heavy_one() {
        let env = ocean_env(500.0);
        let rho = DEFAULT_WATER_DENSITY_KG_M3;
        let nudge = 0.09;
        // The 1 600 kg stack displaces 1.561 m³: the lower cube and
        // 0.561 of the upper one, whichever is "upper" at the time.
        let submerged = 1600.0 / rho;
        let waterline = 0.5 + (submerged - 1.0);
        for (lower, upper, inverted, stable) in [
            (100.0, 1500.0, false, false),
            (1500.0, 100.0, true, false),
            (1500.0, 100.0, false, true),
            (100.0, 1500.0, true, true),
        ] {
            let mut body = cube_stack(lower, upper);
            let com_z = upper / (lower + upper);
            // Upright the waterline is `waterline` up the stack; the
            // centre of mass sits at `com_z`. Inverted, both flip.
            let com_depth = if inverted {
                com_z + (waterline - 1.0)
            } else {
                waterline - com_z
            };
            let yaw = if inverted {
                -core::f64::consts::FRAC_PI_2 + nudge
            } else {
                core::f64::consts::FRAC_PI_2 + nudge
            };
            body.rot = DQuat::from_axis_angle(DVec3::Y, yaw);
            body.pos = DVec3::new(TERRIN_R - com_depth, 0.0, 0.0) - body.rot.rotate(body.com_local);
            let loads = lifted_loads(&env, &body);
            let torque = loads.parts.iter().fold(DVec3::ZERO, |t, p| {
                t + (p.at - body.com_world()).cross(p.force)
            });
            let (what, sign) = if stable {
                ("rights", -1.0)
            } else {
                ("capsizes", 1.0)
            };
            assert!(
                torque.y * sign > 100.0,
                "lower {lower} kg, inverted {inverted}: must {what}, torque {torque:?}"
            );
        }
    }

    /// Drag opposes the flow at `½ρCdA·f·v²` — the face the flow sees,
    /// scaled by the submerged share — plus the linear term on the
    /// displaced water; the couple damps spin on the same water.
    #[test]
    fn drag_opposes_the_flow_in_proportion_to_the_submerged_share() {
        let env = ocean_env(50_000.0);
        let (rho, k) = (
            DEFAULT_WATER_DENSITY_KG_M3,
            DEFAULT_WATER_LINEAR_DAMPING_PER_S,
        );
        // The water's force and torque on the cube, other loads none.
        let loads_at = |alt: f64, vel: DVec3, ang_vel: DVec3| {
            let mut body = one_cube(1000.0);
            body.pos = DVec3::new(TERRIN_R + alt, 0.0, 0.0);
            body.vel = vel;
            body.ang_vel = ang_vel;
            let mut loads = lifted_loads(&env, &body);
            assert_eq!(loads.parts.len(), 1, "one wet part");
            loads.at_velocity(body.rot, vel, ang_vel, DVec3::ZERO, DVec3::ZERO)
        };
        let r = TERRIN_R - 100.0;
        let buoyancy = rho * TERRIN_MU / (r * r);
        // Sinking at 10 m/s, 100 m down: the whole 1 m² face and the
        // whole displaced cubic metre resist.
        let full = loads_at(-100.0, DVec3::new(-10.0, 0.0, 0.0), DVec3::ZERO);
        assert_rel(
            full.0.x,
            0.5 * rho * 1.0 * 1.0 * 100.0 + k * rho * 10.0 + buoyancy,
            1e-12,
            "fully submerged drag is quadratic and up",
        );
        assert_eq!((full.0.y, full.0.z, full.1), (0.0, 0.0, DVec3::ZERO));
        // Twice the speed: four times the quadratic term.
        let fast = loads_at(-100.0, DVec3::new(-20.0, 0.0, 0.0), DVec3::ZERO);
        assert_rel(
            fast.0.x,
            0.5 * rho * 400.0 + k * rho * 20.0 + buoyancy,
            1e-12,
            "quadratic in speed",
        );
        // Half submerged, drifting sideways: half the face, half the
        // water, and only the buoyancy of the half that is under.
        let half = loads_at(0.0, DVec3::new(0.0, 10.0, 0.0), DVec3::ZERO);
        // Local gravity where the force acts: a quarter metre down,
        // at the submerged half's centroid.
        let r_half = TERRIN_R - 0.25;
        assert_rel(
            half.0.y,
            -(0.5 * rho * 0.5 * 100.0 + k * rho * 0.5 * 10.0),
            1e-12,
            "half the drag of the whole cube",
        );
        assert_rel(
            half.0.x,
            0.5 * rho * TERRIN_MU / (r_half * r_half),
            1e-12,
            "half the displaced water",
        );
        // Spinning at 2 rad/s: the displaced water's inertia is the
        // cube's mean-squared vertex radius, 0.75 m².
        let spin = loads_at(-100.0, DVec3::ZERO, DVec3::new(0.0, 0.0, 2.0));
        assert_pt(
            spin.1,
            DVec3::new(0.0, 0.0, -k * rho * 0.75 * 2.0),
            1e-9,
            "spin damping",
        );
    }

    /// The splashdown stack of `csp-sim`'s test: three 1.25 m cubes of
    /// 900, 800 and 800 kg stacked along body +z, falling engine-first
    /// along world −x, the water `depth` metres up its bottom cube.
    fn splashdown_stack(depth: f64, speed: f64) -> WeldedBody {
        let (half, side) = (0.625, 1.25);
        let mut g = PartGraph::new(PartNodeId(1));
        let mut parts = Vec::with_capacity(3);
        for (i, mass_kg) in [900.0, 800.0, 800.0].into_iter().enumerate() {
            // Each cube one side above its parent's frame: a closed stack.
            let parent = (i > 0).then_some(PartNodeId(i as u64));
            let lift = DVec3::new(0.0, 0.0, side);
            g.insert(
                PartNodeId(i as u64 + 1),
                PartNode::new(parent, PartTransform::new(lift, DQuat::IDENTITY), i as u32),
            );
            let mut part = weld_part(mass_kg);
            part.collider = ColliderShape::Box {
                half_extents: DVec3::new(half, half, half),
            };
            part.inertia = SymMat3::solid_box(mass_kg, DVec3::new(half, half, half));
            parts.push(part);
        }
        let mut body = lifted(weld(&g, &parts));
        body.rot = DQuat::from_axis_angle(DVec3::Y, core::f64::consts::FRAC_PI_2);
        // The stack's centre of mass sits 1.2 m above the bottom cube's
        // centre: `1.825 − depth` over the water.
        body.pos = DVec3::new(TERRIN_R - depth + 0.5 * side + 1.2, 0.0, 0.0)
            - body.rot.rotate(body.com_local);
        body.vel = DVec3::new(-speed, 0.0, 0.0);
        body
    }

    /// An entry load is the drag law's own. The bottom cube, 96% under,
    /// reads `½ρCdA·f·v² + kρV·v` — at a parachute's 17 m/s as at
    /// 1 000 — whatever tick carries it: the backend's own at 17 m/s,
    /// sub-steps from 200, and never a scaled load.
    #[test]
    fn an_entry_load_is_the_drag_law_s_own_never_scaled() {
        let env = ocean_env(50_000.0);
        let (rho, k) = (
            DEFAULT_WATER_DENSITY_KG_M3,
            DEFAULT_WATER_LINEAR_DAMPING_PER_S,
        );
        let (side, face, wet) = (1.25, 1.25 * 1.25, 1.2);
        // At a parachute's 17 m/s the stack's drag is not stiff — one
        // explicit tick carries it (measured, 0.23); from 200 m/s it is.
        for (speed, stiff) in [(17.0, false), (200.0, true), (1_000.0, true)] {
            // The water `wet` metres up the bottom cube: that share of
            // it under, the other two dry.
            let body = splashdown_stack(wet, speed);
            let mut loads = lifted_loads(&env, &body);
            assert_eq!(
                loads.parts.len(),
                1,
                "at {speed} m/s only the bottom cube is wet"
            );
            assert_eq!(
                loads.stiffness > SUBSTEP_STIFFNESS,
                stiff,
                "stiff at {speed} m/s: {}",
                loads.stiffness
            );
            assert_eq!(loads.substeps() > 1, stiff, "sub-stepped at {speed} m/s");
            assert_eq!(loads.scale, 1.0, "the load is not scaled at {speed} m/s");
            let (force, torque) =
                loads.at_velocity(body.rot, body.vel, DVec3::ZERO, DVec3::ZERO, DVec3::ZERO);
            let lift = gravity_force_world(
                &env,
                loads.parts[0].at - env.body_center,
                -(rho * face * wet),
            )
            .unwrap_or_else(|| panic!("the cube is off the body centre"));
            assert_rel(
                force.x - lift.x,
                (0.5 * rho * face * (wet / side) * speed + k * rho * face * wet) * speed,
                1e-12,
                "the bottom cube's load at {speed} m/s",
            );
            // 1 000 m/s reads 770 MN on the cube: the 1e-6 is the
            // round-off of that load's cross product.
            assert_pt(torque, DVec3::ZERO, 1e-6, "axial at {speed} m/s");
        }
    }

    /// A stiff tick is integrated, so it lands where the drag law says.
    /// A tonne cube entering at 200 m/s leaves its first wet tick within
    /// 1e-4 of the law's own solution — measured, 8e-6 — and the record
    /// the joint loads read holds the entry load, not what was left of
    /// it.
    #[test]
    fn a_stiff_tick_lands_on_the_drag_law_s_own_solution() {
        let env = ocean_env(50_000.0);
        let (mass, rho, k) = (
            1000.0,
            DEFAULT_WATER_DENSITY_KG_M3,
            DEFAULT_WATER_LINEAR_DAMPING_PER_S,
        );
        let (r, entry) = (TERRIN_R - 100.0, 200.0);
        let mut body = one_cube(mass);
        body.pos = DVec3::new(r, 0.0, 0.0);
        body.vel = DVec3::new(-entry, 0.0, 0.0);
        let mut backend = SimpleBackend::new(env.clone());
        let mut bodies = vec![body];
        backend.step(&mut bodies, &[]);
        let body = &bodies[0];
        // The law itself, downward positive: the cube's weight less its
        // buoyancy, against `½ρCdA·v² + kρV·v` on its square metre —
        // 4096 fourth-order steps of it, whose own error is under 1e-8.
        let g = TERRIN_MU / (r * r);
        let accel = |s: f64| g * (mass - rho) / mass - (0.5 * rho * s + k * rho) * s / mass;
        let (mut reference, h) = (entry, PHYSICS_DT / 4096.0);
        for _ in 0..4096 {
            let a = accel(reference);
            let b = accel(reference + a * (0.5 * h));
            let c = accel(reference + b * (0.5 * h));
            let d = accel(reference + c * h);
            reference += (a + (b + c) * 2.0 + d) * (h / 6.0);
        }
        assert!(
            backend.in_water(body),
            "the cube is still under after the tick"
        );
        assert_rel(
            -body.vel.x,
            reference,
            1e-4,
            "the tick's exit speed against the drag law",
        );
        assert_eq!(body.ang_vel, DVec3::ZERO, "nothing to turn it");
        // The joint loads read the entry load: the full quadratic term
        // at 200 m/s plus the linear one, beside the cube's buoyancy.
        let record = backend.water_loads(BodyHandle(0));
        assert_eq!(record.len(), 1, "one wet part on the record");
        let lift = gravity_force_world(&env, DVec3::new(r, 0.0, 0.0), -rho)
            .unwrap_or_else(|| panic!("the cube is off the body centre"));
        assert_rel(
            record[0].force.x - lift.x,
            (0.5 * rho * entry + k * rho) * entry,
            1e-12,
            "the recorded load is the entry load",
        );
        assert_pt(
            record[0].at,
            DVec3::new(r, 0.0, 0.0),
            1e-12,
            "at the cube's centre",
        );
    }

    /// A 3 km/s entry is 24 ticks' worth of stiffness for one tick to
    /// carry. Sub-stepped, it stays finite, never comes back out faster
    /// than it went in, and the water stops the cube.
    #[test]
    fn a_hypersonic_entry_stays_finite_and_slows() {
        let env = ocean_env(50_000.0);
        let mut body = one_cube(1000.0);
        body.pos = DVec3::new(TERRIN_R + 5.0, 0.0, 0.0);
        body.vel = DVec3::new(-3000.0, 0.0, 0.0);
        let mut backend = SimpleBackend::new(env);
        let mut bodies = vec![body];
        let (mut entered, mut fastest) = (false, 0.0f64);
        for _ in 0..600 {
            backend.step(&mut bodies, &[]);
            let b = &bodies[0];
            assert!(
                b.pos.is_finite() && b.vel.is_finite() && b.rot.is_finite(),
                "finite through the entry"
            );
            if !entered && backend.in_water(b) {
                entered = true;
            }
            if entered {
                fastest = fastest.max(b.vel.norm());
            }
        }
        assert!(entered, "the cube reached the water");
        assert!(
            fastest <= 3000.2,
            "no overshoot past the entry speed: {fastest}"
        );
        assert!(
            bodies[0].vel.norm() < 1.0,
            "water stopped the cube: {:?}",
            bodies[0].vel
        );
    }

    /// A tower of `n` slabs — 1 × 1 × ¼ m, 100 kg each, stacked along
    /// body +z at a quarter-metre pitch, the perf suite's 300-part craft
    /// — standing along world +x, its bottom slab's centre `depth` under
    /// the water and sinking at `speed`.
    fn slab_tower(n: usize, depth: f64, speed: f64) -> WeldedBody {
        let half = DVec3::new(0.5, 0.5, 0.125);
        let mut g = PartGraph::new(PartNodeId(1));
        let mut parts = Vec::with_capacity(n);
        for i in 0..n {
            let parent = (i > 0).then_some(PartNodeId(i as u64));
            let lift = DVec3::new(0.0, 0.0, if i > 0 { 0.25 } else { 0.0 });
            g.insert(
                PartNodeId(i as u64 + 1),
                PartNode::new(parent, PartTransform::new(lift, DQuat::IDENTITY), i as u32),
            );
            let mut part = weld_part(100.0);
            part.collider = ColliderShape::Box { half_extents: half };
            part.inertia = SymMat3::solid_box(100.0, half);
            parts.push(part);
        }
        let mut body = lifted(weld(&g, &parts));
        body.rot = DQuat::from_axis_angle(DVec3::Y, core::f64::consts::FRAC_PI_2);
        body.pos = DVec3::new(TERRIN_R - depth, 0.0, 0.0);
        body.vel = DVec3::new(-speed, 0.0, 0.0);
        body
    }

    /// The drag's true stiffest mode at the body's velocity: the largest
    /// eigenvalue of `M⁻¹K`, `K` being every wet part's damper and
    /// couple, by power iteration in the body frame.
    fn true_stiffness(body: &WeldedBody, loads: &WaterLoads) -> f64 {
        let inv_i = body
            .inertia_local
            .inverse()
            .unwrap_or_else(|| panic!("the body's inertia turns over"));
        let dampers: Vec<(f64, DVec3, f64)> = loads
            .parts
            .iter()
            .map(|p| {
                let (_, _, coefficient) = p.drag(body.rot, body.vel, body.ang_vel);
                (coefficient, p.arm_body, p.couple)
            })
            .collect();
        // Velocity (linear, angular), body frame, of unit length.
        let (mut v, mut w) = (DVec3::new(0.3, -0.5, 0.8), DVec3::new(-0.2, 0.7, 0.4));
        let mut norm = 1.0;
        for _ in 0..20_000 {
            let (mut force, mut torque) = (DVec3::ZERO, DVec3::ZERO);
            for &(coefficient, arm, couple) in &dampers {
                let flow = v + w.cross(arm);
                force = force + flow * coefficient;
                torque = torque + (arm.cross(flow) * coefficient + w * couple);
            }
            let (dv, dw) = (force / body.mass_kg, inv_i.mul_vec(torque));
            norm = libm::sqrt(dv.dot(dv) + dw.dot(dw));
            v = dv / norm;
            w = dw / norm;
        }
        norm
    }

    /// The stiffness bound is the sub-step count's warrant. Under the
    /// truth, a stiff tick gets too few steps; far over it, and the cap
    /// scales a load that never needed it — with the inertia read whole
    /// instead of across the arm, the 300-slab tower's drag came out a
    /// thousand times stiffer than it is, and every tick it spent in the
    /// water ran the full 512 sub-steps, at the cap. Against the exact
    /// stiffest mode the bound holds — never under, for a unit velocity's
    /// image under `M⁻¹K` is never longer than that eigenvalue — and
    /// stays within 1.8 times it: 1.44 for the cube, 1.77 for the stack,
    /// 1.01 for the tower.
    #[test]
    fn the_stiffness_bound_brackets_the_drag_s_true_stiffness() {
        let env = ocean_env(5_000.0);
        let lone = {
            let mut b = one_cube(1000.0);
            b.pos = DVec3::new(TERRIN_R - 2.0, 0.0, 0.0);
            b.vel = DVec3::new(-3.0, 0.0, 0.0);
            b.ang_vel = DVec3::new(0.4, 0.0, 0.0);
            b
        };
        let stack = {
            let mut b = cube_stack(800.0, 800.0);
            b.rot = DQuat::from_axis_angle(DVec3::Y, 0.4);
            b.pos = DVec3::new(TERRIN_R - 3.0, 0.0, 0.0) - b.rot.rotate(b.com_local);
            b.vel = DVec3::new(-5.0, 2.0, 0.0);
            b.ang_vel = DVec3::new(0.0, 0.0, 1.0);
            b
        };
        let tower = slab_tower(300, 20.0, 5.0);
        for (what, body) in [
            ("a lone cube", &lone),
            ("the cube stack", &stack),
            ("the slab tower", &tower),
        ] {
            let loads = lifted_loads(&env, body);
            let truth = PHYSICS_DT * true_stiffness(body, &loads);
            assert!(
                loads.stiffness >= truth * (1.0 - 1e-9),
                "{what}: the bound {} is under the truth {truth}",
                loads.stiffness
            );
            assert!(
                loads.stiffness <= 2.0 * truth,
                "{what}: the bound {} is far over the truth {truth}",
                loads.stiffness
            );
        }
        // So the tower, 20 m under and sinking at 5 m/s, takes two
        // sub-steps, its load its own.
        let loads = lifted_loads(&env, &tower);
        assert_eq!((loads.substeps(), loads.scale), (2, 1.0));
        assert_eq!(
            loads.parts.len(),
            81,
            "eighty slabs under, one at the waterline"
        );
    }

    // ---- reach, artifacts, the step ---------------------------------------

    /// The distance cull fires only on a provable margin: a body clear
    /// of the datum by its reach never evaluates, one lower does.
    #[test]
    fn provably_dry_fires_only_on_a_provable_margin() {
        let env = ocean_env(500.0);
        let reach = DVec3::new(0.5, 0.5, 0.5).norm();
        let mut b = one_cube(500.0);
        for (alt, dry, wet) in [
            (reach + 1.0, true, false),
            (reach + 1e-4, true, false),
            (0.5, false, false),
            (0.4, false, true),
            (-300.0, false, true),
        ] {
            b.pos = DVec3::new(TERRIN_R + alt, 0.0, 0.0);
            assert_eq!(provably_dry(&env, &b), dry, "altitude {alt}");
            assert_eq!(touches_water(&env, &b), wet, "altitude {alt}");
            assert_eq!(WaterLoads::of(&env, &b).is_none(), dry, "altitude {alt}");
        }
        let land = Environment {
            ocean: None,
            ..ocean_env(500.0)
        };
        b.pos = DVec3::new(TERRIN_R - 300.0, 0.0, 0.0);
        assert!(provably_dry(&land, &b), "no ocean, no water");
        assert!(!touches_water(&land, &b));
    }

    /// `for_body` stamps an authored ocean at the no-data tunables;
    /// a pre-field CBOR payload (the save codec) decodes as a dry
    /// world, and both codecs round-trip an ocean.
    #[test]
    fn ocean_is_stamped_by_for_body_and_is_save_compatible() {
        let mut body = csp_data::BodyDef {
            schema_version: csp_data::BODY_SCHEMA_VERSION,
            id: "planet".into(),
            display_name: "body.planet.name".into(),
            parent: None,
            radius_m: TERRIN_R,
            mu_m3_per_s2: TERRIN_MU,
            soi_m: None,
            rotation_period_s: None,
            tilt_rad: 0.0,
            orbit: None,
            atmosphere: None,
            ocean: None,
            terrain: csp_data::TerrainDef {
                noise_seed: 601,
                heightmap: None,
                amplitude_frac: 0.0,
                relief: Default::default(),
            },
            biome_map: None,
            surface_resources: Vec::new(),
            insolation_w_m2: Some(1_361.0),
        };
        assert!(
            Environment::for_body(&body, DVec3::ZERO, None)
                .ocean
                .is_none(),
            "no authored ocean"
        );
        body.ocean = Some(csp_data::OceanDef {
            datum_radius_m: TERRIN_R,
        });
        assert_eq!(
            Environment::for_body(&body, DVec3::ZERO, None).ocean,
            Some(Ocean {
                datum_radius_m: TERRIN_R,
                density_kg_m3: DEFAULT_WATER_DENSITY_KG_M3,
                drag_cd: DEFAULT_WATER_DRAG_CD,
                linear_damping_per_s: DEFAULT_WATER_LINEAR_DAMPING_PER_S,
            })
        );

        #[derive(serde::Serialize, serde::Deserialize)]
        struct PreOceanEnvironment {
            body_center: DVec3,
            radius_m: f64,
            mu_m3_per_s2: f64,
            body_spin: DVec3,
            atmosphere: Option<csp_data::AtmosphereDef>,
            settle_v: f64,
            settle_w: f64,
            settle_ticks: u32,
            friction_mu: f64,
            drag_cd: f64,
            convective_heat_fraction: f64,
            terrain: Option<crate::simple::TerrainSurface>,
        }
        let old = PreOceanEnvironment {
            body_center: DVec3::ZERO,
            radius_m: TERRIN_R,
            mu_m3_per_s2: TERRIN_MU,
            body_spin: DVec3::ZERO,
            atmosphere: None,
            settle_v: crate::simple::DEFAULT_SETTLE_V,
            settle_w: crate::simple::DEFAULT_SETTLE_W,
            settle_ticks: crate::simple::DEFAULT_SETTLE_TICKS,
            friction_mu: crate::simple::DEFAULT_FRICTION_MU,
            drag_cd: crate::simple::DEFAULT_DRAG_CD,
            convective_heat_fraction: crate::DEFAULT_CONVECTIVE_HEAT_FRACTION,
            terrain: None,
        };
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&old, &mut bytes).unwrap_or_else(|e| panic!("{e:?}"));
        let compat: Environment =
            ciborium::de::from_reader(&bytes[..]).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            compat.ocean, None,
            "a pre-ocean save decodes as a dry world"
        );
        assert_eq!(
            compat.touchdown_crush_m,
            crate::DEFAULT_TOUCHDOWN_CRUSH_M,
            "a pre-field save decodes at the shipped crush distance"
        );
        assert_eq!(compat.radius_m, TERRIN_R);

        let wet = ocean_env(500.0);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&wet, &mut bytes).unwrap_or_else(|e| panic!("{e:?}"));
        let back: Environment =
            ciborium::de::from_reader(&bytes[..]).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(back, wet, "CBOR round-trips the ocean");
        let bytes = bincode::serde::encode_to_vec(&wet, bincode::config::standard())
            .unwrap_or_else(|e| panic!("{e:?}"));
        let back: Environment =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .map(|(v, _)| v)
                .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(back, wet, "bincode round-trips the ocean");
    }

    /// A craft the water cannot reach steps exactly as it did before
    /// water existed — parked on a pad above the datum, and falling
    /// the last dry metres toward the surface.
    #[test]
    fn a_craft_out_of_the_water_steps_bit_identically_with_and_without_an_ocean() {
        let dry_world = |ground: f64| Environment {
            radius_m: ground,
            ocean: None,
            ..ocean_env(5000.0)
        };
        let wet_world = |ground: f64| Environment {
            radius_m: ground,
            ..ocean_env(5000.0)
        };
        for (ground, alt, ticks, what) in [
            (
                TERRIN_R + 52.0,
                TERRIN_R + 52.5,
                600,
                "parked on a pad above the datum",
            ),
            (
                TERRIN_R - 5000.0,
                TERRIN_R + 1.5,
                20,
                "falling the last dry metre and a half",
            ),
        ] {
            // The same craft in both worlds, down to the bit.
            let spawn = || {
                let mut b = one_cube(500.0);
                b.pos = DVec3::new(alt, 0.0, 0.0);
                b.vel = DVec3::new(0.0, 3.0, 0.0);
                b.ang_vel = DVec3::new(0.1, 0.2, 0.3);
                b
            };
            let (mut dry, mut wet) = (
                SimpleBackend::new(dry_world(ground)),
                SimpleBackend::new(wet_world(ground)),
            );
            let (mut a, mut b) = (vec![spawn()], vec![spawn()]);
            for _ in 0..ticks {
                dry.step(&mut a, &[]);
                wet.step(&mut b, &[]);
                let (x, y) = (&a[0], &b[0]);
                for (p, q) in [
                    (x.pos, y.pos),
                    (x.vel, y.vel),
                    (x.ang_vel, y.ang_vel),
                    (
                        DVec3::new(x.rot.x, x.rot.y, x.rot.z),
                        DVec3::new(y.rot.x, y.rot.y, y.rot.z),
                    ),
                ] {
                    assert_eq!(
                        (p.x.to_bits(), p.y.to_bits(), p.z.to_bits()),
                        (q.x.to_bits(), q.y.to_bits(), q.z.to_bits()),
                        "{what}: the ocean moved a dry craft"
                    );
                }
                assert_eq!(x.settle, y.settle, "{what}: settle bookkeeping");
                assert!(!wet.in_water(&b[0]), "{what}: still dry");
            }
        }
    }

    /// The settle latch counts a floating craft's quiet ticks like a
    /// parked one's; a sinker hanging in mid-water never qualifies.
    #[test]
    fn a_floating_craft_latches_and_a_sinker_does_not() {
        let mut backend = SimpleBackend::new(ocean_env(5000.0));
        let mut floater = one_cube(500.0);
        floater.pos = DVec3::new(TERRIN_R + 1.0, 0.0, 0.0);
        let mut sinker = one_cube(2000.0);
        sinker.pos = DVec3::new(TERRIN_R - 200.0, 50.0, 0.0);
        let mut bodies = vec![floater, sinker];
        let mut latched_at = None;
        for tick in 0..6000 {
            backend.step(&mut bodies, &[]);
            if bodies[0].settle.latched {
                latched_at = Some(tick);
                break;
            }
        }
        let tick = latched_at.unwrap_or_else(|| panic!("the floater never latched"));
        // Archimedes again: 500 kg of a 1 m³ cube floats with its
        // centre a centimetre and a quarter up, to well under a
        // hundredth of its draft.
        let draft = 500.0 / DEFAULT_WATER_DENSITY_KG_M3;
        let rest = TERRIN_R + 0.5 - draft;
        assert!(
            (bodies[0].pos.x - rest).abs() < draft * 0.002,
            "rest depth {} vs {rest}",
            bodies[0].pos.x
        );
        assert_eq!(bodies[0].vel, DVec3::ZERO, "latched at rest");
        assert!(backend.floating(&bodies[0]), "latched afloat");
        // The sinker fell the whole time and was never latched.
        assert!(
            !bodies[1].settle.latched,
            "a sinker does not latch in mid-water"
        );
        assert!(
            !backend.floating(&bodies[1]),
            "a sinker is not supported by the water"
        );
        assert!(backend.in_water(&bodies[1]), "though it is in water");
        let seconds = tick as f64 / 64.0;
        assert!(
            bodies[1].pos.x < TERRIN_R - 200.0 - seconds,
            "and it kept sinking, a metre a second at least"
        );
    }

    /// Drag at a part opposes that part's own motion through the water,
    /// `v + ω×r` with the angular velocity and the arm in one frame. A
    /// stack spinning about both its axes with no other motion reads, on
    /// every cube, a force antiparallel to that cube's velocity, and the
    /// water takes rotational energy: the cross product of a body-frame
    /// `ω` with a world-frame arm instead read a flow no part had, and
    /// pumped a tipped floater into a tumble that never ended.
    #[test]
    fn drag_on_a_rotating_stack_opposes_each_part_s_velocity() {
        let env = ocean_env(500.0);
        let mut body = cube_stack(100.0, 1500.0);
        body.rot = DQuat::from_axis_angle(DVec3::Y, core::f64::consts::FRAC_PI_2);
        body.pos = DVec3::new(TERRIN_R - 20.0, 0.0, 0.0) - body.rot.rotate(body.com_local);
        // Body frame: about the stack's own axis and across it.
        body.ang_vel = DVec3::new(0.5, 0.0, 2.0);
        let mut loads = lifted_loads(&env, &body);
        assert_eq!(loads.parts.len(), 2, "both cubes under");
        let (omega, com) = (body.rot.rotate(body.ang_vel), body.com_world());
        for p in &loads.parts {
            let u = omega.cross(p.at - com);
            let (drag, _, _) = p.drag(body.rot, body.vel, body.ang_vel);
            assert!(drag.dot(u) < 0.0, "the drag opposes the part's motion");
            assert_pt(
                drag.cross(u),
                DVec3::ZERO,
                1e-9 * drag.norm() * u.norm(),
                "antiparallel to it",
            );
        }
        let (_, torque) =
            loads.at_velocity(body.rot, body.vel, body.ang_vel, DVec3::ZERO, DVec3::ZERO);
        assert!(
            omega.dot(torque) < 0.0,
            "the water takes rotational energy, not gives it"
        );
    }

    /// A stack that splashes engine-first floats in an attitude it cannot
    /// hold — its centre of mass (1.2 m up) is above its centre of
    /// buoyancy — so the least tilt grows until it tips over. Tipped, it
    /// comes to rest on its side and latches afloat: from 0.01 rad/s it
    /// swings through 1.65 rad/s and latches horizontal at tick 619.
    #[test]
    fn a_tipped_stack_settles_on_its_side() {
        let env = ocean_env(5_000.0);
        let mut body = splashdown_stack(-(1.5 + 1.825), 17.0);
        body.ang_vel = DVec3::new(0.0, 0.01, 0.0);
        let mut backend = SimpleBackend::new(env);
        let mut bodies = vec![body];
        let (mut latched_at, mut fastest) = (None, 0.0f64);
        for tick in 0..900 {
            backend.step(&mut bodies, &[]);
            let b = &bodies[0];
            if b.settle.latched {
                latched_at = Some(tick);
                break;
            }
            fastest = fastest.max(b.ang_vel.norm());
        }
        let tick = latched_at.unwrap_or_else(|| panic!("the tipped stack never latched"));
        let b = &bodies[0];
        assert!(
            fastest > 1.0,
            "it tipped over: the swing reached {fastest} rad/s"
        );
        // The stack axis against the water plane.
        let tilt = b.rot.rotate(DVec3::Z).x.abs();
        assert!(
            tilt < 0.05,
            "at rest on its side, axis {tilt} off horizontal"
        );
        assert!(backend.floating(b), "latched afloat");
        assert!(
            (500..=700).contains(&tick),
            "still within ten seconds, not {tick} ticks"
        );
    }
}
