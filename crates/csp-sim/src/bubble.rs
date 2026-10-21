//! The §2.4 physics bubble's *type* surface — the vessel-residence
//! states (`VesselState`), the weld inputs a physical spawn carries
//! (`CraftDef`), the §2.6.4 body-fixed latch record (`SurfacePose`),
//! and the world's physics construction config (`PhysicsConfig`).
//!
//! Frame conventions (§2.3): the bubble frame is an inertial frame —
//! `Environment::body_center` is where the dominant body's centre sits
//! in bubble coordinates — so a bubble position minus `body_center` is
//! a parent-centered inertial vector in exactly the frame
//! `orbit::cartesian_to_elements` consumes. The body's *rotation*
//! (`body_spin`) only enters through the environment's co-rotation
//! terms (drag `v_rel`, moving contact surface); it never rotates the
//! frame itself.
//!
//! The §2.6.4 surface-rails record stores `(lat, lon, alt,
//! orientation)` in the body's *rotating, body-fixed* frame: a fixed
//! frame vector `p_fix` has inertial coordinates
//! `body_center + R(axis, ω·t)·p_fix` at sim-time `t` seconds, with
//! `axis = body_spin/‖body_spin‖` and `ω = ‖body_spin‖`. For a
//! non-rotating environment the phase rotation is identity and the
//! record is the inertial pose verbatim.

use csp_core::{DQuat, DVec3, SimTime, StableDiscriminant, TICK_HZ};
use csp_data::BodyId;
use csp_phys::{EffectivePart, Environment, PartGraph, WeldedBody};
use serde::{Deserialize, Serialize};

use crate::orbit::OrbitState;

/// The weld inputs a `SpawnPhysicalVessel` command carries — §2.4's
/// `weld(part_graph, &[EffectivePart])` arguments, verbatim.
///
/// The craft def IS the vessel's authoritative physical description:
/// `WeldedBody`'s mass properties are its pure derived cache (§7.3 —
/// re-created by re-welding, bit-identical per §7.5), so the sim keeps
/// the def and re-welds on re-materialization (unlatch, bubble
/// re-entry) rather than serializing mass properties. A vessel that
/// never had one — an elements-only `SpawnVessel` — carries `None` and
/// can never become physics-resident.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CraftDef {
    /// The §2.4 attachment graph.
    pub graph: PartGraph,
    /// Resolved per-part data, indexed by `PartNode::effective_index`.
    pub parts: Vec<EffectivePart>,
}

/// Where a vessel lives (§2.4/§2.6.2): on rails, physics-resident in
/// the bubble, or surface-latched (§2.6.4's "surface rails").
///
/// Discriminants are the §7.1 rule-7 hash encoding — explicit, stable,
/// append-only.
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VesselState {
    /// On patched-conic rails — epoch-anchored closed-form state
    /// (§2.6.1: unchanged by interval advancement).
    OnRails(OrbitState) = 0,
    /// Physics-resident: the sim's tick loop integrates the welded body
    /// every integer tick (§2.6.2). The `WeldedBody` carries both the
    /// authoritative kinematic/control state (`pos`, `rot`, `vel`,
    /// `ang_vel`, `throttle`, `settle`) and the craft-derived mass
    /// properties (§7.3 — re-derived on restore, never themselves
    /// hashed: the craft covers them).
    InBubble(WeldedBody) = 1,
    /// §2.6.4 surface rails: the vessel has left the bubble; its
    /// body-fixed pose is the whole state, exactly reproduced on
    /// unlatch. No integration occurs — no drift is possible.
    SurfaceLatched(SurfacePose) = 2,
}

impl StableDiscriminant for VesselState {
    fn discriminant(&self) -> u16 {
        match self {
            Self::OnRails(_) => 0,
            Self::InBubble(_) => 1,
            Self::SurfaceLatched(_) => 2,
        }
    }
}

/// §2.6.4's stored pose: `(lat, lon, alt, orientation)` in the dominant
/// body's *rotating* frame — see the module doc for the phase
/// convention. Geocentric spherical coordinates of the vessel's body
/// origin (its `pos` point); `orientation` is the vessel's inertial
/// `rot` expressed in the fixed frame, so unlatch reproduces the pose
/// at any later tick's phase exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfacePose {
    /// The body the vessel is latched to — the environment's dominant.
    pub body: BodyId,
    /// Geocentric latitude of the body origin, rad.
    pub latitude_rad: f64,
    /// Longitude, rad — eastward from the fixed frame's +x.
    pub longitude_rad: f64,
    /// Altitude of the body origin above the datum radius, m.
    pub altitude_m: f64,
    /// Body-fixed orientation: `R(axis, −ω·t_latch) ∘ rot` at capture.
    pub orientation: DQuat,
}

/// The spin-phase rotation `R(axis, angle)` as a quaternion — callers
/// pass `IDENTITY`-equivalent inputs (a zero angle) on non-rotating
/// environments.
fn phase_quat(axis: DVec3, angle_rad: f64) -> DQuat {
    if angle_rad == 0.0 {
        return DQuat::IDENTITY;
    }
    DQuat::from_axis_angle(axis, angle_rad)
}

/// Capture `body`'s pose as a §2.6.4 surface record at `tick`.
/// `dominant` names the body the record is relative to (the
/// environment's central body).
///
/// `p_fix = R(axis, −ω·t)·(pos − body_center)`, then spherical: the
/// record lives in the rotating frame so a latched vessel parked on a
/// spinning world keeps a *constant* record (its inertial pose is a
/// pure function of the record and the tick — that is what "no
/// integration, no drift" means concretely).
pub fn surface_pose_capture(
    env: &Environment,
    dominant: &BodyId,
    body: &WeldedBody,
    tick: SimTime,
) -> SurfacePose {
    let omega = env.body_spin.norm();
    let (axis, seconds) = if omega > 0.0 {
        (env.body_spin / omega, tick.ticks() as f64 / TICK_HZ as f64)
    } else {
        (DVec3::Z, 0.0)
    };
    // Inertial → fixed frame: unrotate by the phase. `R` is identity
    // on a non-rotating environment, so `rel` is the inertial offset.
    let phase = phase_quat(axis, omega * seconds);
    let rel = phase.conjugate().rotate(body.pos - env.body_center);
    let r = rel.norm();
    let altitude_m = r - env.radius_m;
    let latitude_rad = if r > 0.0 {
        libm::asin((rel.z / r).clamp(-1.0, 1.0))
    } else {
        0.0
    };
    let longitude_rad = if rel.x != 0.0 || rel.y != 0.0 {
        libm::atan2(rel.y, rel.x)
    } else {
        0.0
    };
    // Fixed-frame orientation: unrotate the inertial rot by the phase.
    let orientation = (phase.conjugate() * body.rot).normalize();
    SurfacePose {
        body: dominant.clone(),
        latitude_rad,
        longitude_rad,
        altitude_m,
        orientation,
    }
}

/// The unlatch half of §2.6.4: reproduce the inertial pose exactly at
/// the unlatch `tick` — `pos = body_center + R(ω·t)·p_fix`,
/// `rot = R(ω·t)·orientation`, with `p_fix` rebuilt from
/// `(lat, lon, alt)`. Velocity is the surface co-rotation
/// `body_spin × (pos − body_center)`; angular velocity is the
/// co-rotation expressed in body frame — the state a body-fixed object
/// actually has, so an unlatched body re-enters physics mid-rotation
/// with no impulse.
///
/// Returns `(pos, rot, vel, ang_vel)` in the bubble frame (ang_vel in
/// body frame, matching `WeldedBody`'s convention).
pub fn surface_pose_restore(
    env: &Environment,
    pose: &SurfacePose,
    tick: SimTime,
) -> (DVec3, DQuat, DVec3, DVec3) {
    let omega = env.body_spin.norm();
    let (axis, seconds) = if omega > 0.0 {
        (env.body_spin / omega, tick.ticks() as f64 / TICK_HZ as f64)
    } else {
        (DVec3::Z, 0.0)
    };
    let (sin_lat, cos_lat) = (libm::sin(pose.latitude_rad), libm::cos(pose.latitude_rad));
    let (sin_lon, cos_lon) = (libm::sin(pose.longitude_rad), libm::cos(pose.longitude_rad));
    let r = env.radius_m + pose.altitude_m;
    let p_fix = DVec3::new(r * cos_lat * cos_lon, r * cos_lat * sin_lon, r * sin_lat);
    let phase = phase_quat(axis, omega * seconds);
    let pos = env.body_center + phase.rotate(p_fix);
    let rot = (phase * pose.orientation).normalize();
    let vel = env.body_spin.cross(pos - env.body_center);
    let ang_vel = rot.conjugate().rotate(env.body_spin);
    (pos, rot, vel, ang_vel)
}

/// The world's physics construction config (§2.4/§9's "the World owns
/// the backend + environment"): the §2.9 backend's environment plus the
/// dominant body the bubble frame is centered on. Not sim state —
/// data-derived, handed in at construction and at `World::restore`
/// (the §7.6 replay artifact carries it so a replay rebuilds the same
/// construction without re-running the recipe).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhysicsConfig {
    /// The backend environment — gravity, atmosphere, contact sphere,
    /// settle/solver tunables (§2.9/§3.0).
    pub environment: Environment,
    /// The dominant body — the bubble frame's central body. All bubble
    /// quantities are expressed in its parent-centered inertial frame;
    /// `ToRails`/`ToBubble` conversions and the §2.6.4 latch record use
    /// it as the vessel's rails parent / latched-to body.
    pub dominant: BodyId,
}
