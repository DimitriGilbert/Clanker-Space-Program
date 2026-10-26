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

use std::collections::BTreeMap;

use csp_core::{DQuat, DVec3, SimTime, StableDiscriminant, TICK_HZ};
use csp_data::{BodyId, PartId};
use csp_phys::{Environment, PartGraph, PartNodeId, WeldPart, WeldedBody};
use serde::{Deserialize, Serialize};

use crate::orbit::OrbitState;

/// The vessel's M4b thermal record — per-part lumped temperatures
/// (K) keyed by graph node. **Authoritative sim state**: serialized
/// on the vessel, walked into `state_hash`, and persistent across
/// residence changes (the bubble tick steps it only while
/// `InBubble`; on rails and latched the record is held fixed — the
/// documented v1 approximation; a rails cooldown model is not part
/// of §4's scope).
///
/// The key set mirrors the craft's `PartGraph` nodes exactly:
/// spawned with every node at the ambient temperature, pruned
/// alongside the graph on jettison/thermal destruction. An
/// elements-only vessel (no `craft`) carries an empty map.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VesselThermal {
    /// Per-part lump temperature (K), ascending `PartNodeId`.
    pub part_temps_k: BTreeMap<PartNodeId, f64>,
}

impl VesselThermal {
    /// Every graph node at `ambient_k` — the spawn/materialization
    /// initial condition.
    pub fn at_ambient(graph: &PartGraph, ambient_k: f64) -> Self {
        Self {
            part_temps_k: graph.nodes.keys().map(|id| (*id, ambient_k)).collect(),
        }
    }
}

/// The weld inputs a `SpawnPhysicalVessel` command carries — §2.4's
/// `weld(part_graph, &[WeldPart])` arguments, verbatim.
///
/// The craft def IS the vessel's authoritative physical description:
/// `WeldedBody`'s mass properties are its pure derived cache (§7.3 —
/// re-created by re-welding, bit-identical per §7.5), so the sim keeps
/// the def and re-welds on re-materialization (unlatch, bubble
/// re-entry) rather than serializing mass properties. A vessel that
/// never had one — an elements-only `SpawnVessel` — carries `None` and
/// can never become physics-resident.
///
/// `parts` holds the weld-consumed view of each node's resolved
/// `csp_phys::EffectivePart` (its `weld_input` field), produced by the
/// §3.1 resolver `csp_phys::resolve` from `BuiltPart` instance records.
/// The records themselves ride the §7.2 part-instance arena when it
/// lands (M3-C+); the frozen resolved numbers carried here are
/// bit-stable for the vessel's life regardless — the §3.3
/// never-retroactive guarantee made concrete.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CraftDef {
    /// The §2.4 attachment graph.
    pub graph: PartGraph,
    /// Resolved per-part data, indexed by `PartNode::effective_index`.
    pub parts: Vec<WeldPart>,
    /// M7-C: each part's resolved funds cost (`EffectivePart::
    /// cost_funds`, frozen at build — the same never-retroactive
    /// convention as the weld inputs). Parallel to `parts`, indexed
    /// the same way, so a node's cost travels with its part through
    /// every graph surgery that reuses `effective_index` — jettison/
    /// breakup/undock/merge need no cost bookkeeping. The economy
    /// module reads Σ over *live* nodes for the spawn-time
    /// manufacture charge and the recovery payout. A test/scenario
    /// craft legitimately authors `0.0` (free).
    pub part_costs: Vec<f64>,
    /// M9b §3.4: each part's resolved reliability spec
    /// (`ReliabilitySpec::from_effective` over the same resolved
    /// parts), parallel to `parts` and `part_costs` — frozen at
    /// build like both. An EMPTY table is legal and means "no
    /// reliability model" (hand-built test craft); a non-empty
    /// table must cover `parts` exactly (the `part_costs` spawn
    /// gate).
    pub reliability: Vec<csp_phys::ReliabilitySpec>,
    /// M7-B §3.3: each part's catalog identity — the `PartId` whose
    /// `PartDef` authored it, parallel to `parts`/`part_costs`/
    /// `reliability` (the same `effective_index` convention, frozen
    /// at build). This is what lets the spawn/manufacture gate
    /// enforce `tech_requirement`: the apply resolves each id
    /// against the world's installed part catalog and refuses the
    /// craft when the world has a tech tree and the def's node is
    /// unresearched — a locked part is a rejected build, never a
    /// silent launch.
    ///
    /// An EMPTY table is legal and means "identity not recorded"
    /// (hand-built test craft, pre-gate saves, debug craft) — the
    /// gate is a no-op on it, the same convention the reliability
    /// table uses for "no model". A non-empty table must cover
    /// `parts` exactly.
    ///
    /// Serialized but **not hashed**: post-apply it is inert
    /// gate/build metadata — nothing reads it after the spawn
    /// check (re-welds consume `parts`, payouts consume
    /// `part_costs`), so two worlds differing only here evolve
    /// bit-identically. The gate's verdict itself is deterministic:
    /// the id table rides the logged command and resolves against
    /// construction-config catalog + hashed `TechState` at drain.
    #[serde(default)]
    pub part_ids: Vec<PartId>,
    /// The `craft_graph_hash` memo — a pure cache, never content:
    /// `Clone` drops it (clone-modify-reassign is THE craft-mutation
    /// convention, so a clone inheriting the old hash would lie
    /// stale — a cleared memo just recomputes), `PartialEq` ignores
    /// it, `serde` skips it, and `hash_craft` never walks it.
    /// **Write rule**: mutate a `CraftDef` only before it is shared,
    /// or by replacing it wholesale — an in-place write to a memoized
    /// def leaves the memo stale (wrong direction).
    #[doc(hidden)]
    #[serde(skip)]
    pub hash_memo: CraftHashMemo,
}

/// The `craft_graph_hash` memo living on [`CraftDef::hash_memo`].
/// `0` encodes "not computed"; a stored value is `hash + 1`
/// (wrapping), so a computed hash of `u64::MAX` simply never caches
/// and the `0` a hasher fault produces costs a recompute, never a
/// wrong answer.
///
/// The type's trait impls are the safety contract:
/// [`Clone`] returns EMPTY (the clone may be about to change —
/// memoizing across a clone would risk a stale hit), and
/// [`PartialEq`] is always-true (a cache is not part of the def's
/// identity — two defs equal by content must compare equal whatever
/// their memo state).
#[derive(Debug, Default)]
pub struct CraftHashMemo(std::sync::atomic::AtomicU64);

impl CraftHashMemo {
    /// The cached hash, or `None` when uncomputed.
    pub(crate) fn get(&self) -> Option<u64> {
        std::num::NonZeroU64::new(self.0.load(std::sync::atomic::Ordering::Relaxed))
            .map(|v| v.get().wrapping_sub(1))
    }

    /// Store a freshly computed hash (`0`-safe via the +1 encoding).
    pub(crate) fn set(&self, hash: u64) {
        self.0
            .store(hash.wrapping_add(1), std::sync::atomic::Ordering::Relaxed);
    }
}

// See the type doc: cloning clears the memo — the clone-modify-
// reassign convention makes a carried-over memo a stale-hit risk.
impl Clone for CraftHashMemo {
    fn clone(&self) -> Self {
        Self::default()
    }
}

// See the type doc: a cache is not content — memo state never
// affects `CraftDef` equality.
impl PartialEq for CraftHashMemo {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// Where a vessel lives (§2.4/§2.6.2): on rails, physics-resident in
/// the bubble, or surface-latched (§2.6.4's "surface rails").
///
/// Discriminants are the §7.1 rule-7 hash encoding — explicit, stable,
/// append-only.
///
/// `InBubble` deliberately carries the `WeldedBody` by value — the
/// vessel *is* the body while resident; boxing it would add a level of
/// indirection on the hot path for a lint that reads size asymmetry,
/// not a correctness risk.
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
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
/// `(lat, lon, alt)`. Velocity is the surface co-rotation of the
/// **centre of mass** — `body_spin × (pos + rot·com_local −
/// body_center)` — because `WeldedBody::vel` is the CoM slot (§2.9's
/// integrator advances `x̄`, not the origin): stamping the *origin's*
/// co-rotation would silently drop `ω × rot·com_local` for any weld
/// with `com_local ≠ 0` — the same `v + ω×r` material-point convention
/// `csp_phys::fragment_velocity` applies. Angular velocity is the
/// co-rotation expressed in body frame — the state a body-fixed object
/// actually has, so an unlatched body re-enters physics mid-rotation
/// with no impulse.
///
/// `com_local` is the vessel weld's [`WeldedBody::com_local`]; callers
/// re-weld the stored craft anyway, so it is handed straight through.
///
/// Returns `(pos, rot, vel, ang_vel)` in the bubble frame (ang_vel in
/// body frame, matching `WeldedBody`'s convention).
pub fn surface_pose_restore(
    env: &Environment,
    pose: &SurfacePose,
    com_local: DVec3,
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
    // `vel` stamps the CoM slot: the co-rotation velocity of
    // `com_world = pos + rot·com_local`, not of the origin `pos`.
    let vel = env
        .body_spin
        .cross(pos + rot.rotate(com_local) - env.body_center);
    let ang_vel = rot.conjugate().rotate(env.body_spin);
    (pos, rot, vel, ang_vel)
}

/// The upright pad orientation at `(lat_rad, lon_rad)` expressed in the
/// body's *fixed* frame — feed it to [`surface_pose_restore`] as
/// `orientation` to get the inertial rot at `tick`.
///
/// The mapping is a level ENU pose: the craft's stack axis (body +Z —
/// the VAB's build direction) points along local up `û`, body +Y faces
/// local north `n̂`, and body +X faces local east `ê` (the direction a
/// parked craft's nose pitches toward when it lifts off east). Built as
/// `yaw(φ about û) ∘ tilt(ẑ→û)` so the pole degenerate (`cl → 0`, where
/// east/north are undefined) still yields a finite upright pose.
///
/// Convention check at `lat = lon = 0`: `û = +x`, `tilt = R(ŷ, π/2)` —
/// the pose the original pad spawn wrote by hand.
pub fn surface_upright(lat_rad: f64, lon_rad: f64) -> DQuat {
    let (sl, cl) = (libm::sin(lat_rad), libm::cos(lat_rad));
    let (so, co) = (libm::sin(lon_rad), libm::cos(lon_rad));
    let up = DVec3::new(cl * co, cl * so, sl);
    // tilt: ẑ → û about the hinge axis ẑ × û = (−u_y, u_x, 0); the angle
    // is acos(ẑ·û) = acos(sl) = π/2 − lat.
    let tilt = if cl.abs() > 1.0e-12 {
        DQuat::from_axis_angle(
            DVec3::new(-up.y, up.x, 0.0),
            libm::acos(sl.clamp(-1.0, 1.0)),
        )
    } else if sl > 0.0 {
        DQuat::IDENTITY // north pole: ẑ already IS up
    } else {
        DQuat::from_axis_angle(DVec3::Y, core::f64::consts::PI)
    };
    // Level frame: ê is the eastward direction on the horizon plane;
    // n̂ = û × ê completes it. At the poles ê degenerates with `cl` —
    // pick +y as the zero-lat reference east there (any level choice
    // is equally honest at a pole).
    let east = if cl.abs() > 1.0e-12 {
        DVec3::new(-so, co, 0.0)
    } else {
        DVec3::Y
    };
    let north = up.cross(east);
    // yaw: rotate the tilted +Y about û onto n̂ — the signed angle
    // measured against û.
    let y_tilted = tilt.rotate(DVec3::Y);
    let phi = libm::atan2(y_tilted.cross(north).dot(up), y_tilted.dot(north));
    (DQuat::from_axis_angle(up, phi) * tilt).normalize()
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
    /// PLAN §2.3's bubble radius (balance.ron's `bubble_radius_m`,
    /// 20 km authored): the altitude at which vessels go on rails —
    /// the rail floor the auto bubble→rails edge and `ToRails` read
    /// as `radius_m + max(atmosphere.top_m, bubble_radius_m)`, so an
    /// airless body's low ballistic hop stays physical while an
    /// atmosphere-having world's floor is the atmosphere top.
    /// Construction config — never hashed; `serde(default)` keeps
    /// §7.6 artifacts recorded before the field existed decodable.
    #[serde(default)]
    pub bubble_radius_m: f64,
}
