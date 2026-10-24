//! M4c near-flight view — PLAN §3.7's presentation floor over the
//! real sim: the InBubble craft (and its breakup debris) rendered as
//! silhouette-correct kitbash part meshes ([`crate::part_mesh`]), an
//! engine plume, a **reentry plasma sheath** driven by the sim's own
//! convective-heating flux sample, **ground-contact dust**, and the
//! HUD mirrors. Same §2.5 schedule shape as the map and VAB:
//! `end_frame` stages sim reads into [`FlightSyncInput`],
//! `sync_flight` mirrors them, `flight_present` solves the chase
//! camera + the SFX cue edges, `extract_flight` emits clip-space
//! vertices for `csp_render::flight::FlightRenderer`.
//!
//! **SFX (§3.7 "basic SFX").** M4c ships the cue channel, not an
//! audio device: [`FlightSfx`] edge-detects the sim transitions the
//! SFX row names (engine ignition/cut, staging/jettison, breakup,
//! contact/impact) and flashes them in the flight HUD. The same
//! transition stream is the seam a real audio backend consumes —
//! sound output is deferred to M10's own exit row (no audio stack in
//! the tree; adding one pulls a dependency through §3.7's license
//! audit for zero gate-visible fidelity). PLAN §3.7 carries the
//! deferral note.
//!
//! Determinism: all f64 world math runs on staged values in the
//! schedules; the only f32 is the clip-space cast at emission, like
//! the map projection. Cue TTLs count presentation frames — session
//! state, never sim state.

use std::collections::{BTreeMap, VecDeque};

use bevy_ecs::prelude::*;
use csp_core::{DQuat, DVec3};
use csp_sim::{VesselId, VesselState, World};

use crate::map::{
    MapCameraF64, MapInput, MapMode, body_system_pos, project_into, solve_orbit_camera,
};
use crate::terrain::{TerrainCache, TerrainConfig};

/// Chase-camera FOV (radians, vertical).
pub(crate) const FLIGHT_FOV_Y: f64 = 0.9;
/// Default camera distance (m) — reframed on focus changes.
const FLIGHT_DEFAULT_DIST: f64 = 60.0;
/// Plasma sheath display window (W/m² of convective input,
/// `FlightBody::plasma_w_m2`): first glow ≈ a fast low pass, full
/// saturation at mild-reentry flux. Display normalization only — the
/// sim's lethality is the M4b temperature record, not this curve.
const PLASMA_ONSET_W_M2: f64 = 5.0e3;
/// See [`PLASMA_ONSET_W_M2`].
const PLASMA_FULL_W_M2: f64 = 8.0e4;

/// One chase-camera input event — `csp-app` translates drags/wheel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FlightEvent {
    /// Orbit delta (radians).
    Orbit {
        /// Yaw increment.
        dyaw: f64,
        /// Pitch increment.
        dpitch: f64,
    },
    /// Multiplicative zoom.
    Zoom {
        /// `> 1` zooms out, `< 1` in.
        factor: f64,
    },
}

/// Session-state chase camera (§2.2-C).
#[derive(Resource)]
pub struct FlightInput {
    /// Orbit yaw (radians).
    pub yaw: f64,
    /// Orbit pitch (radians).
    pub pitch: f64,
    /// Eye–focus distance (m).
    pub distance_m: f64,
    /// Viewport in physical pixels.
    pub viewport_px: (f64, f64),
}

impl Default for FlightInput {
    fn default() -> Self {
        Self {
            yaw: 0.7,
            pitch: 0.25,
            distance_m: FLIGHT_DEFAULT_DIST,
            viewport_px: (1.0, 1.0),
        }
    }
}

/// Drained each frame by `apply_flight_input`.
#[derive(Resource, Default)]
pub struct FlightInputEvents {
    /// Queued events.
    pub pending: Vec<FlightEvent>,
}

/// The environment slice the flight view needs — staged sim data.
#[derive(Clone, Debug)]
pub struct FlightEnv {
    /// Planet centre (bubble frame).
    pub body_center: DVec3,
    /// Surface radius (m).
    pub radius_m: f64,
    /// Atmosphere top altitude (m), `0` when airless.
    pub atm_top_m: f64,
    /// Atmosphere scale height (m) — the R2 scattering LUT's `H`; `0`
    /// when airless.
    pub atm_scale_height_m: f64,
    /// The dominant body's gravitational parameter (m³/s²) — the
    /// tutorial's osculating-apsides derivation needs it (the same
    /// μ the environment's force model integrates against).
    pub mu_m3_per_s2: f64,
    /// The dominant body's id — keys the terrain heightfield/biome
    /// cache (SOI transfer swaps it).
    pub body: Option<csp_data::BodyId>,
    /// Unit sun direction, bubble frame (bubble = translated system
    /// frame, so directions carry over) — lights the terrain, the
    /// scattering LUT lookups, and the craft lambert alike.
    pub sun_dir: DVec3,
    /// Dominant-body spin axis (unit, bubble frame) — `DVec3::Z` on a
    /// non-rotating environment. With `spin_phase` this is the same
    /// `R(axis, ω·t)` the §2.6.4 surface-latch record co-rotates
    /// latched poses by.
    pub spin_axis: DVec3,
    /// Spin phase `ω·t` (rad) at the staged sim tick: a body-fixed
    /// direction maps to the inertial bubble frame as
    /// `R(spin_axis, spin_phase)·fixed`. `0` when `body_spin` is zero.
    pub spin_phase: f64,
    /// The body's angular velocity vector (bubble frame, rad/s) —
    /// the HUD's surface-relative speed and the navball's level frame
    /// both need `ω × (com − body_center)`, not just the phase.
    pub body_spin: DVec3,
}

/// One part's staged render data — the §3.7 kitbash silhouette plus
/// the collider bounds the sheath/dust VFX still measure against.
#[derive(Clone, Debug)]
pub struct FlightPart {
    /// Body-frame AABB min corner of the part's collider (plasma
    /// sheath + dust extent — the honest contact/glow bounds).
    pub lo: DVec3,
    /// Body-frame AABB max corner.
    pub hi: DVec3,
    /// Part-local → body frame (the graph node's composed transform —
    /// the same fold the weld ran).
    pub transform: csp_phys::PartTransform,
    /// The classified silhouette in part-local space.
    pub shape: crate::part_mesh::PartShape,
}

/// One InBubble vessel's staged render data.
#[derive(Clone, Debug)]
pub struct FlightBody {
    /// The vessel's id (`VesselId.0` — no sim types at render time).
    pub id: u64,
    /// Body-origin position (bubble frame).
    pub pos: DVec3,
    /// Body→world rotation.
    pub rot: DQuat,
    /// CoM velocity (bubble frame).
    pub vel: DVec3,
    /// Throttle [0,1] — plume intensity.
    pub throttle: f64,
    /// `true` for breakup debris (orange tint).
    pub debris: bool,
    /// Whether the last step recorded ground contact — dust VFX flag.
    pub contact: bool,
    /// Convective heating input per unit area (W/m²) —
    /// `CONVECTIVE_HEAT_FRACTION · ½ρ|v_rel|³` from the same
    /// `flow_sample` the M4b thermal tick consumes; the plasma
    /// sheath's intensity driver.
    pub plasma_w_m2: f64,
    /// Live part-graph node count — the staging/jettison SFX edge.
    pub nodes: usize,
    /// Per-part render entries (silhouette + collider bounds),
    /// ascending `PartNodeId` — the M4c box tier's hull AABBs carried
    /// forward as `lo`/`hi` on each part.
    pub parts: Vec<FlightPart>,
    /// Thrust sites `(local_pos, local_dir)` — plume emitters.
    pub plumes: Vec<(DVec3, DVec3)>,
    /// World-frame CoM (camera focus + HUD altitude).
    pub com: DVec3,
    /// `true` for a §2.6.4 surface-latched vessel — staged with its
    /// restored inertial pose (a parked craft stays visible and
    /// focusable; a throttle command unlatches it). Its `vel` is the
    /// co-rotation velocity the restore reproduces.
    pub latched: bool,
}

/// What `end_frame` stages out of the sim for the flight view.
#[derive(Resource, Default)]
pub struct FlightSyncInput {
    /// The physics environment — `None` without physics.
    pub env: Option<FlightEnv>,
    /// Staged vessels (InBubble + surface-latched), ascending
    /// `VesselId` — `last()` is the most recently spawned, which is the
    /// flight focus rule (see `focus_vessel`).
    pub vessels: Vec<FlightBody>,
}

/// The `sync` schedule's mirror — identical data, presentation-owned.
#[derive(Resource, Default)]
pub struct FlightSnapshot {
    /// See [`FlightSyncInput::env`].
    pub env: Option<FlightEnv>,
    /// See [`FlightSyncInput::vessels`].
    pub vessels: Vec<FlightBody>,
}

/// HUD mirrors — written by `flight_present`, drawn by
/// `csp_ui::flight_hud`.
#[derive(Resource, Default)]
pub struct FlightUiData {
    /// Focused vessel exists.
    pub has_vessel: bool,
    /// Focused vessel id (for throttle commands), `None` otherwise —
    /// the most recently spawned staged vessel (see `focus_vessel`).
    /// Includes a surface-latched pad vessel — `SetThrottle` unlatches
    /// it (the sim's §2.6.4 control rule).
    pub focus: Option<u64>,
    /// The focused vessel is surface-latched (parked on rails) — the
    /// HUD reads "PAD" and the instruments show rest.
    pub latched: bool,
    /// Altitude above surface (m).
    pub alt_m: f64,
    /// Surface-relative CoM speed (m/s) — `‖v − ω × r‖`: what "speed
    /// over the ground" means. ~0 for a parked craft, NOT the ~175 m/s
    /// co-rotation the inertial frame carries on a spinning world.
    pub surface_speed_mps: f64,
    /// Inertial CoM speed (m/s) — the "orbital" readout.
    pub orbital_speed_mps: f64,
    /// Vertical speed (m/s), radial component of the inertial velocity
    /// (the `ω × r` term is horizontal, so this is identical
    /// surface-relative).
    pub vspeed_mps: f64,
    /// Throttle [0,1].
    pub throttle: f64,
    /// Live part count of the focused vessel — the staging readout.
    pub parts: usize,
    /// Compass heading of the nose (deg, 0–360, NaN while undefined —
    /// e.g. pointing straight up).
    pub heading_deg: f64,
    /// Nose pitch above the horizon (deg).
    pub pitch_deg: f64,
    /// Roll about the nose axis (deg) — 0 when the craft's dorsal +Y
    /// points "up-ish".
    pub roll_deg: f64,
    /// Navball: local up expressed in the focused vessel's *body* frame
    /// (`rot⁻¹·û`, unit). `[0,0,0]` when there is no reference (no
    /// vessel / degenerate radius).
    pub nav_up: [f64; 3],
    /// Navball: local level north in body frame (unit; falls back to
    /// the projection of world +Z, then +X).
    pub nav_north: [f64; 3],
    /// Navball: local level east in body frame (`n̂ × û`).
    pub nav_east: [f64; 3],
    /// Navball: surface-relative prograde in body frame (unit) —
    /// `None` while the vessel is surface-stationary (parked).
    pub nav_prograde: Option<[f64; 3]>,
    /// Total vessels in the world (bubble + latched).
    pub vessel_count: usize,
    /// Debris-flagged vessels.
    pub debris_count: usize,
    /// Live SFX cue lines (§3.7's visual SFX representation) —
    /// written from [`FlightSfx`], drawn as event flashes by
    /// `csp_ui::flight_hud`.
    pub sfx: Vec<String>,
}

/// One live cue in the §3.7 visual-SFX channel.
#[derive(Clone, Debug)]
pub struct SfxCue {
    /// The HUD line ("ENGINE IGNITION", "BREAKUP", …).
    pub text: String,
    /// Remaining presentation frames before the cue fades out.
    pub ttl: u32,
}

/// Last frame's edge-detection summary for [`FlightSfx`].
#[derive(Clone, Copy, Debug)]
struct SfxPrev {
    /// Focused vessel id (`vessels.last()` — the chase target).
    focus: u64,
    /// `throttle > 0` — the ignition/cut edge.
    throttle_on: bool,
    /// Focused craft's live node count — the jettison edge.
    nodes: usize,
    /// Debris-flagged vessel count — the breakup edge.
    debris: usize,
    /// InBubble vessel count — the impact-removal edge.
    vessels: usize,
    /// Focused vessel's contact flag — the touchdown edge.
    contact: bool,
    /// Focused vessel is surface-latched — the landing edge.
    latched: bool,
}

/// §3.7's "basic SFX" as a visual cue channel (module docs carry the
/// audio deferral): `flight_present` edge-detects the named
/// transitions on the staged snapshot and flashes them for ~1.5 s.
/// Session state only — TTLs count *presentation frames*, never sim
/// ticks, and nothing here feeds back into the sim.
#[derive(Resource, Default)]
pub struct FlightSfx {
    /// Live cue stack, oldest first.
    pub cues: VecDeque<SfxCue>,
    /// Last frame's edge state — `None` until the first present pass.
    prev: Option<SfxPrev>,
}

/// Cue lifetime in frames — ~1.5 s at 60 fps.
const SFX_TTL_FRAMES: u32 = 90;

/// The RenderExtract product — `None` while no flight scene exists.
#[derive(Resource, Default)]
pub struct FlightFrame(pub Option<csp_render::flight::FlightPacket>);

/// Session-state cache for the §2.6.4 latched-vessel weld. The
/// `SurfaceLatched` pose record stores no collider, so staging a
/// parked craft's render data needs a re-weld of its stored craft —
/// but a latched craft is immutable sim state (any command that
/// changes it unlatches or removes the vessel first), so the weld +
/// per-part staging is cached on `(vessel id, craft fingerprint)`
/// instead of running once per frame. Entries whose vessel is no
/// longer surface-latched are dropped at the next capture, keeping
/// the map bounded by the live latched count.
#[derive(Resource, Default)]
pub struct LatchedWeldCache {
    /// `VesselId.0` → the cached weld-derived render data.
    entries: BTreeMap<u64, LatchedWeld>,
}

/// One latched vessel's cached weld output — the render data
/// `capture_sync_input` would otherwise recompute every frame.
struct LatchedWeld {
    /// `craft_fingerprint` of the `(graph, parts)` this entry welded.
    key: u64,
    /// `WeldedBody::com_local` — `surface_pose_restore`'s CoM input.
    com_local: DVec3,
    /// Thrust sites `(local_pos, local_dir)` — the plume emitters.
    plumes: Vec<(DVec3, DVec3)>,
    /// `vessel_parts`' output over the welded hulls.
    parts: Vec<FlightPart>,
}

/// The [`LatchedWeldCache`] staleness key: FNV-1a over the
/// weld-relevant craft structure — node ids, parents, transforms, and
/// effective indices, then the part count and each part's mass/CoM —
/// so a same-id/different-craft hit re-welds rather than rendering a
/// stale parked shape. Presentation-only; not the §7.1 state hash.
fn craft_fingerprint(craft: &csp_sim::CraftDef) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    let mut mix = |bits: u64| {
        h ^= bits;
        h = h.wrapping_mul(FNV_PRIME);
    };
    mix(craft.graph.root.0);
    for (id, node) in &craft.graph.nodes {
        mix(id.0);
        mix(node.parent.map_or(u64::MAX, |p| p.0));
        for v in [
            node.transform.offset.x,
            node.transform.offset.y,
            node.transform.offset.z,
            node.transform.rotation.x,
            node.transform.rotation.y,
            node.transform.rotation.z,
            node.transform.rotation.w,
        ] {
            mix(v.to_bits());
        }
        mix(u64::from(node.effective_index));
    }
    mix(craft.parts.len() as u64);
    for p in &craft.parts {
        mix(p.mass_kg.to_bits());
        mix(p.com.x.to_bits());
        mix(p.com.y.to_bits());
        mix(p.com.z.to_bits());
    }
    h
}

/// Sun direction in the bubble frame: the bubble is a translated copy
/// of the system frame, so `star_pos − dominant_pos` carries over as a
/// direction. Falls back to the fixed shading default when the orbit
/// model can't place either body (e.g. a physics world with no system).
fn sun_direction(sim: &World) -> DVec3 {
    let fallback = || {
        let d = DVec3::new(0.4, 0.3, 0.86);
        d / d.norm()
    };
    let Some(dom) = sim.dominant_body() else {
        return fallback();
    };
    let Some(star) = sim
        .orbits()
        .bodies()
        .iter()
        .find(|(_, b)| b.parent.is_none())
        .map(|(id, _)| id.clone())
    else {
        return fallback();
    };
    if star == *dom {
        return fallback(); // the bubble is centred on the star itself
    }
    let t = sim.clock();
    let to_sun = (|| {
        let star_pos = body_system_pos(sim.orbits(), &star, t).ok()?;
        let dom_pos = body_system_pos(sim.orbits(), dom, t).ok()?;
        let d = star_pos - dom_pos;
        (d.norm_squared() > 0.0).then_some(d / d.norm())
    })();
    to_sun.unwrap_or_else(fallback)
}

/// `end_frame` staging: pull the flight view's slice out of `World`
/// (the only sim read this module does — §2.5). `cache` is the
/// session's [`LatchedWeldCache`] — surface-latched vessels reuse its
/// weld-derived render data instead of re-welding every frame.
pub fn capture_sync_input(sim: &World, cache: &mut LatchedWeldCache) -> FlightSyncInput {
    let env = sim.environment().map(|e| {
        // The §2.6.4 phase convention (`csp_sim::bubble`'s
        // `surface_pose_*`): inertial = R(axis, ω·t)·fixed at
        // `t = clock` seconds. The terrain pass needs it to slide the
        // surface under latched/landed craft at exactly the contact
        // surface velocity `body_spin × rel`.
        let omega = e.body_spin.norm();
        let (spin_axis, spin_phase) = if omega > 0.0 {
            (e.body_spin / omega, omega * sim.clock().seconds())
        } else {
            (DVec3::Z, 0.0)
        };
        FlightEnv {
            body_center: e.body_center,
            radius_m: e.radius_m,
            atm_top_m: e.atmosphere.as_ref().map_or(0.0, |a| a.top_m),
            atm_scale_height_m: e.atmosphere.as_ref().map_or(0.0, |a| a.scale_height_m),
            mu_m3_per_s2: e.mu_m3_per_s2,
            body: sim.dominant_body().cloned(),
            sun_dir: sun_direction(sim),
            spin_axis,
            spin_phase,
            body_spin: e.body_spin,
        }
    });
    let env_ref = sim.environment();
    // Bound the weld cache to the live latched set — an unlatched or
    // removed vessel's entry can never be hit again.
    cache.entries.retain(|vid, _| {
        matches!(
            sim.vessels().get(&VesselId(*vid)).map(|v| &v.state),
            Some(VesselState::SurfaceLatched(_))
        )
    });
    let mut vessels = Vec::new();
    for (id, vessel) in sim.vessels() {
        match &vessel.state {
            VesselState::InBubble(body) => {
                let parts = vessel_parts(vessel.craft.as_ref(), &body.collider.hulls);
                let plumes = body
                    .force_sites
                    .iter()
                    .filter(|s| s.kind == csp_phys::ForceSiteKind::Thrust)
                    .map(|s| (s.local_pos, s.local_dir))
                    .collect();
                // §3.7's plasma driver: the per-area convective input the
                // M4b thermal tick itself integrates — same `flow_sample`,
                // same `v_rel`, so the glow tracks the sim's heating, not
                // a lookalike.
                let plasma_w_m2 = env_ref.map_or(0.0, |e| {
                    let flow = csp_phys::flow_sample(e, body);
                    csp_phys::CONVECTIVE_HEAT_FRACTION
                        * 0.5
                        * flow.density_kg_m3
                        * flow.speed_mps.powi(3)
                });
                vessels.push(FlightBody {
                    id: id.0,
                    pos: body.pos,
                    rot: body.rot,
                    vel: body.vel,
                    throttle: body.throttle,
                    debris: vessel.debris_since.is_some(),
                    contact: sim.ground_contact(*id),
                    plasma_w_m2,
                    nodes: vessel.craft.as_ref().map_or(0, |c| c.graph.nodes.len()),
                    parts,
                    plumes,
                    com: body.com_world(),
                    latched: false,
                });
            }
            // §2.6.4 surface rails: a latched vessel is still *on the
            // pad* — stage its restored inertial pose so the parked
            // craft stays rendered and focusable (the throttle command
            // the HUD targets unlatches it). The restore needs the
            // environment; without one the record is uninterpretable —
            // skip honestly rather than guess a pose.
            VesselState::SurfaceLatched(pose) => {
                if let Some(body) = stage_latched(sim, env_ref, *id, vessel, pose, cache) {
                    vessels.push(body);
                }
            }
            VesselState::OnRails(_) => {}
        }
    }
    FlightSyncInput { env, vessels }
}

/// Stage one §2.6.4 surface-latched vessel. The restored inertial
/// pose is a function of the sim clock and is re-solved every frame
/// (the pad co-rotates); the weld-derived render data (`com_local`,
/// the plume sites, the per-part silhouettes) comes from
/// [`LatchedWeldCache`] — the latched craft is immutable, so the weld
/// runs only on a `(vessel id, craft fingerprint)` miss.
///
/// `None` for an unstageable record — no environment (the pose record
/// is uninterpretable without one), no stored craft, or a craft that
/// cannot weld. Skipping honestly matches the pre-cache behaviour:
/// degraded, never guessed.
fn stage_latched(
    sim: &World,
    env: Option<&csp_phys::Environment>,
    id: VesselId,
    vessel: &csp_sim::Vessel,
    pose: &csp_sim::SurfacePose,
    cache: &mut LatchedWeldCache,
) -> Option<FlightBody> {
    let (Some(env), Some(craft)) = (env, vessel.craft.as_ref()) else {
        return None;
    };
    let key = craft_fingerprint(craft);
    if cache.entries.get(&id.0).is_none_or(|e| e.key != key) {
        let welded = csp_phys::weld(&craft.graph, &craft.parts).ok()?;
        let plumes = welded
            .force_sites
            .iter()
            .filter(|s| s.kind == csp_phys::ForceSiteKind::Thrust)
            .map(|s| (s.local_pos, s.local_dir))
            .collect();
        cache.entries.insert(
            id.0,
            LatchedWeld {
                key,
                com_local: welded.com_local,
                plumes,
                parts: vessel_parts(vessel.craft.as_ref(), &welded.collider.hulls),
            },
        );
    }
    let entry = cache.entries.get(&id.0)?;
    // `vel` stamps the CoM slot: the co-rotation velocity of
    // `pos + rot·com_local`, not of the origin `pos`.
    let (pos, rot, vel, _ang_vel) =
        csp_sim::bubble::surface_pose_restore(env, pose, entry.com_local, sim.clock());
    Some(FlightBody {
        id: id.0,
        pos,
        rot,
        vel,
        throttle: 0.0,
        debris: vessel.debris_since.is_some(),
        // Parked, not impacting — the dust puff reads as a touchdown
        // event, not a resting state.
        contact: false,
        plasma_w_m2: 0.0,
        nodes: craft.graph.nodes.len(),
        parts: entry.parts.clone(),
        plumes: entry.plumes.clone(),
        com: pos + rot.rotate(entry.com_local),
        latched: true,
    })
}

/// One vessel's per-part render entries — the §3.7 kitbash upgrade
/// over the M4c hull-AABB stream. Each graph node contributes its
/// classified silhouette, its composed body-frame transform (the
/// same `PartTransform::compose` fold the weld ran), and the collider
/// AABB the sheath/dust VFX bound against.
///
/// A body whose craft data does not stage — an elements-only vessel
/// with `craft: None`, or a graph `body_transforms` rejects — keeps
/// the box-tier rendering over its collider hulls: degraded, never
/// absent (the §2.13 honest-channel precedent: `log::warn` on the
/// typed-error branch).
fn vessel_parts(
    craft: Option<&csp_sim::CraftDef>,
    hulls: &[csp_phys::ConvexHull],
) -> Vec<FlightPart> {
    let fallback = || {
        hulls
            .iter()
            .map(|h| {
                let mut lo = DVec3::new(f64::MAX, f64::MAX, f64::MAX);
                let mut hi = DVec3::new(-f64::MAX, -f64::MAX, -f64::MAX);
                for v in &h.vertices {
                    lo = DVec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
                    hi = DVec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
                }
                let center = (lo + hi) * 0.5;
                let half = (hi - lo) * 0.5;
                FlightPart {
                    lo,
                    hi,
                    transform: csp_phys::PartTransform::new(center, csp_core::DQuat::IDENTITY),
                    // The transform already carries `center` — the shape
                    // is part-LOCAL, so the box is centred on the local
                    // origin. Centring it again here would double the
                    // offset at emit (`transform.apply(shape_corner)`).
                    shape: crate::part_mesh::PartShape::Box {
                        lo: -half,
                        hi: half,
                    },
                }
            })
            .collect()
    };
    let Some(craft) = craft else {
        return fallback();
    };
    let Ok(transforms) = csp_phys::weld::body_transforms(&craft.graph, &craft.parts) else {
        log::warn!("flight: craft graph failed body_transforms — boxing hulls");
        return fallback();
    };
    let mut parts = Vec::with_capacity(craft.graph.nodes.len());
    for (id, node) in &craft.graph.nodes {
        let (Some(wp), Some(&t)) = (
            craft.parts.get(node.effective_index as usize),
            transforms.get(id),
        ) else {
            continue;
        };
        // The part's body-frame collider AABB — same corners the
        // weld's hull carries, recomputed from the collider vertex
        // set so the `part_index` map needn't be trusted.
        let mut lo = DVec3::new(f64::MAX, f64::MAX, f64::MAX);
        let mut hi = DVec3::new(-f64::MAX, -f64::MAX, -f64::MAX);
        for v in wp.collider.vertices() {
            let w = t.apply(v);
            lo = DVec3::new(lo.x.min(w.x), lo.y.min(w.y), lo.z.min(w.z));
            hi = DVec3::new(hi.x.max(w.x), hi.y.max(w.y), hi.z.max(w.z));
        }
        parts.push(FlightPart {
            lo,
            hi,
            transform: t,
            shape: crate::part_mesh::classify_weld_part(wp),
        });
    }
    if parts.is_empty() { fallback() } else { parts }
}

/// `Input` schedule — drain camera events into `FlightInput`.
pub(crate) fn apply_flight_input(
    mut events: ResMut<FlightInputEvents>,
    mut input: ResMut<FlightInput>,
) {
    for e in events.pending.drain(..) {
        match e {
            FlightEvent::Orbit { dyaw, dpitch } => {
                input.yaw += dyaw;
                input.pitch = (input.pitch + dpitch).clamp(-1.45, 1.45);
            }
            FlightEvent::Zoom { factor } => {
                input.distance_m = (input.distance_m * factor).clamp(5.0, 1.0e6);
            }
        }
    }
}

/// `Sync` schedule — copy the staged input into the presentation
/// mirror.
pub(crate) fn sync_flight(staged: Res<FlightSyncInput>, mut snap: ResMut<FlightSnapshot>) {
    snap.env = staged.env.clone();
    snap.vessels = staged.vessels.clone();
}

/// The solved chase camera — `MapCameraF64` reused (same orbit
/// convention). A *separate* resource from the map's camera:
/// `map_present` and `flight_present` share the unordered Presentation
/// tuple, so one `ResMut<MapCameraF64>` between them would be a
/// last-writer-wins race every frame (same newtype pattern as
/// `vab::VabCameraF64`).
#[derive(Resource, Default)]
pub struct FlightCameraF64(pub MapCameraF64);

/// `Presentation` schedule — chase-camera solve + HUD mirrors + the
/// §3.7 SFX cue edges (the schedule's "VFX, audio cues" slot).
pub(crate) fn flight_present(
    input: Res<FlightInput>,
    snap: Res<FlightSnapshot>,
    mut cam: ResMut<FlightCameraF64>,
    mut ui: ResMut<FlightUiData>,
    mut sfx: ResMut<FlightSfx>,
) {
    ui.vessel_count = snap.vessels.len();
    ui.debris_count = snap.vessels.iter().filter(|v| v.debris).count();

    // SFX edge detection — the §3.7 row's named transitions, detected
    // on the staged snapshot (never the sim). Per-focus edges apply
    // only while the SAME vessel stays focused; a focus switch
    // re-baselines instead of firing a spurious cut/contact edge.
    // The focus rule is `focus_vessel`'s — the newest staged vessel.
    let cur = focus_vessel(&snap.vessels).map(|f| SfxPrev {
        focus: f.id,
        throttle_on: f.throttle > 0.0,
        nodes: f.nodes,
        debris: ui.debris_count,
        vessels: snap.vessels.len(),
        contact: f.contact,
        latched: f.latched,
    });
    match (sfx.prev, cur) {
        (Some(p), Some(c)) => {
            let mut push = |text: &str| {
                sfx.cues.push_back(SfxCue {
                    text: text.to_owned(),
                    ttl: SFX_TTL_FRAMES,
                });
            };
            if c.focus == p.focus {
                if c.throttle_on && !p.throttle_on {
                    push("ENGINE IGNITION");
                } else if !c.throttle_on && p.throttle_on {
                    push("ENGINE CUT");
                }
                if c.nodes < p.nodes {
                    push("STAGE — PART JETTISONED");
                }
                if c.contact && !p.contact {
                    push("CONTACT");
                }
                // §2.6.4: the settle → surface-rails transition — the
                // "parked on the pad" edge.
                if c.latched && !p.latched {
                    push("LANDED — SURFACE RAILS");
                } else if !c.latched && p.latched {
                    push("LIFTOFF");
                }
            }
            if c.debris > p.debris {
                push("BREAKUP");
            }
            if c.debris < p.debris {
                // §2.9: in-bubble debris leaves only via impact removal
                // (it never latches).
                push("DEBRIS IMPACT");
            }
            if c.vessels < p.vessels && c.debris >= p.debris {
                push("VESSEL REMOVED");
            }
        }
        (Some(_), None) => sfx.cues.push_back(SfxCue {
            text: "VESSEL LEFT BUBBLE".to_owned(),
            ttl: SFX_TTL_FRAMES,
        }),
        (None, _) => {} // first frame — baseline only
    }
    for cue in &mut sfx.cues {
        cue.ttl = cue.ttl.saturating_sub(1);
    }
    while sfx.cues.front().is_some_and(|c| c.ttl == 0) {
        sfx.cues.pop_front();
    }
    ui.sfx = sfx.cues.iter().map(|c| c.text.clone()).collect();
    sfx.prev = cur;

    let Some(focus) = focus_vessel(&snap.vessels) else {
        cam.0.valid = false;
        ui.has_vessel = false;
        ui.focus = None;
        return;
    };
    cam.0 = solve_orbit_camera(
        input.yaw,
        input.pitch,
        input.distance_m,
        input.viewport_px,
        focus.com,
        FLIGHT_FOV_Y,
        0.1,
    );
    ui.has_vessel = true;
    ui.focus = Some(focus.id);
    ui.latched = focus.latched;
    ui.orbital_speed_mps = focus.vel.norm();
    ui.throttle = focus.throttle;
    ui.parts = focus.nodes;
    ui.nav_up = [0.0; 3];
    ui.nav_north = [0.0; 3];
    ui.nav_east = [0.0; 3];
    ui.nav_prograde = None;
    ui.heading_deg = f64::NAN;
    ui.pitch_deg = f64::NAN;
    ui.roll_deg = f64::NAN;
    ui.surface_speed_mps = 0.0;
    if let Some(env) = snap.env.as_ref() {
        let rel = focus.com - env.body_center;
        let r = rel.norm();
        if r > 0.0 {
            ui.alt_m = r - env.radius_m;
            ui.vspeed_mps = focus.vel.dot(rel / r);
            stage_attitude(&mut ui, env, focus, rel / r);
        }
    }
}

/// The chase target / HUD / throttle-command vessel: the most recently
/// spawned staged vessel. `vessels` is staged ascending `VesselId` and
/// the sim's `next_vessel_id` counter is monotonic, so `last()` is the
/// newest spawn — a fresh launch (or a new breakup fragment) takes the
/// camera, the HUD mirrors, and `SetThrottle` routing with it. When it
/// leaves the staging set the next-newest vessel inherits focus; an
/// empty scene has none.
pub(crate) fn focus_vessel(vessels: &[FlightBody]) -> Option<&FlightBody> {
    vessels.last()
}

/// The attitude/velocity half of the HUD mirrors: surface-relative
/// speed, the heading/pitch/roll readouts, and the navball's
/// body-frame direction set (up, north, east, prograde). `up` is the
/// unit radial at the CoM. All presentation-side derivation off the
/// staged snapshot — no sim reads here (§2.5).
fn stage_attitude(ui: &mut FlightUiData, env: &FlightEnv, focus: &FlightBody, up: DVec3) {
    // The level frame at the CoM: north is the spin axis projected on
    // the horizon plane (world +Z for a non-rotating or equatorial
    // degenerate), east completes it. Above the poles the projection
    // collapses — fall back deterministically.
    let mut north = env.spin_axis - up * env.spin_axis.dot(up);
    if north.norm_squared() < 1.0e-12 {
        north = DVec3::Z - up * up.z;
        if north.norm_squared() < 1.0e-12 {
            north = DVec3::X - up * up.x;
        }
    }
    let north = north / north.norm();
    let east = north.cross(up);

    // Surface-relative velocity — the inertial velocity minus the
    // co-rotation term `ω × r` (the same `v_rel` the bubble's aero and
    // contact solves measure against).
    let v_rel = focus.vel - env.body_spin.cross(rel_of(focus, env));
    let v_surf = v_rel.norm();
    ui.surface_speed_mps = v_surf;
    if v_surf > 0.1 {
        let p = v_rel / v_surf;
        ui.nav_prograde = Some(into_arr(focus.rot.conjugate().rotate(p)));
    }

    // Attitude: nose is the stack axis (body +Z — the VAB's build
    // direction); dorsal is +Y. Readouts in degrees.
    let nose = focus.rot.rotate(DVec3::Z);
    ui.pitch_deg = up.dot(nose).clamp(-1.0, 1.0).asin().to_degrees();
    let nose_level = nose - up * up.dot(nose);
    if nose_level.norm_squared() > 1.0e-6 {
        let h = nose_level / nose_level.norm();
        ui.heading_deg = h
            .dot(east)
            .atan2(h.dot(north))
            .to_degrees()
            .rem_euclid(360.0);
    }
    // Roll about the nose axis: signed angle from "up projected ⊥
    // nose" to "dorsal projected ⊥ nose".
    let dorsal = focus.rot.rotate(DVec3::Y);
    let up_perp = up - nose * up.dot(nose);
    let dorsal_perp = dorsal - nose * dorsal.dot(nose);
    if up_perp.norm_squared() > 1.0e-6 && dorsal_perp.norm_squared() > 1.0e-6 {
        let a = up_perp / up_perp.norm();
        let b = dorsal_perp / dorsal_perp.norm();
        ui.roll_deg = a.cross(b).dot(nose).atan2(a.dot(b)).to_degrees();
    }

    // Navball vectors in body frame — the widget rotates the world
    // under the fixed nose reticle.
    let conj = focus.rot.conjugate();
    ui.nav_up = into_arr(conj.rotate(up));
    ui.nav_north = into_arr(conj.rotate(north));
    ui.nav_east = into_arr(conj.rotate(east));
}

/// `focus.com − env.body_center` (the radial lever arm) — factored so
/// `flight_present` and `stage_attitude` agree on one expression.
fn rel_of(focus: &FlightBody, env: &FlightEnv) -> DVec3 {
    focus.com - env.body_center
}

/// `[x, y, z]` — the plain-data mirror `csp-ui` consumes (§2.1's
/// dependency rule keeps csp-core types out of the panel layer).
fn into_arr(v: DVec3) -> [f64; 3] {
    [v.x, v.y, v.z]
}

/// `RenderExtract` — emit the scene's clip-space triangles + the R2
/// terrain packet (quadtree selection runs here, in the extract, from
/// the solved camera — §2.5's "LOD selection" lives upstream in
/// Presentation; the leaf walk itself is extract-time derivation).
#[allow(clippy::too_many_lines)]
pub(crate) fn extract_flight(
    input: Res<FlightInput>,
    map_input: Res<MapInput>,
    snap: Res<FlightSnapshot>,
    cam: Res<FlightCameraF64>,
    terrain_cfg: Res<TerrainConfig>,
    mut terrain_cache: ResMut<TerrainCache>,
    mut frame: ResMut<FlightFrame>,
) {
    let mut packet = csp_render::flight::FlightPacket::default();
    // R2 surface pass: when a physical world is present and the chase
    // camera solved, the quadtree terrain + LUT sky replace the M4c
    // gradient/disc. Without an environment the flat gradient stays —
    // a flight view with no planet is a vacuum backdrop anyway. The
    // mode gate keeps map frames from paying the quadtree walk +
    // instance bake for a packet nothing draws.
    if map_input.mode == MapMode::Flight
        && cam.0.valid
        && let Some(env) = &snap.env
    {
        packet.terrain = Some(crate::terrain::build_packet(
            &terrain_cfg,
            &mut terrain_cache,
            env,
            &cam.0,
            input.yaw,
            input.pitch,
            input.viewport_px,
        ));
    }
    let v = &mut packet.vertices;
    if packet.terrain.is_none() {
        // Sky fallback: a far-depth fullscreen gradient (two tris at
        // NDC z≈0 — reverse-Z far), horizon blue fading up. The
        // scattering sky replaces it whenever terrain runs.
        let zenith = [0.05, 0.09, 0.22, 1.0];
        let horizon = [0.35, 0.5, 0.75, 1.0];
        let q = [
            ([-1.0f32, -1.0, 0.0], horizon),
            ([1.0, -1.0, 0.0], horizon),
            ([1.0, 1.0, 0.0], zenith),
            ([-1.0, -1.0, 0.0], horizon),
            ([1.0, 1.0, 0.0], zenith),
            ([-1.0, 1.0, 0.0], zenith),
        ];
        for (pos, color) in q {
            v.push(csp_render::flight::FlightVertex { pos, color });
        }
    }
    if !cam.0.valid {
        frame.0 = Some(packet);
        return;
    }
    let vp = &cam.0.view_proj;
    let eye = cam.0.eye;
    // All-or-nothing emission: every corner of a triangle is projected
    // first and the tri is pushed whole or dropped whole — the same
    // rule the part pass below and the VAB packet use. A per-vertex
    // `push` here would let one behind-eye corner shift every later
    // triangle in the shared unindexed `TriangleList` stream.
    let emit_tri = |v: &mut Vec<csp_render::flight::FlightVertex>,
                    a: DVec3,
                    b: DVec3,
                    c: DVec3,
                    color: [f32; 4]| {
        let [Some(c0), Some(c1), Some(c2)] = [a, b, c].map(|q| project_into(vp, eye, q)) else {
            return;
        };
        for pos in [c0, c1, c2] {
            v.push(csp_render::flight::FlightVertex { pos, color });
        }
    };

    // R2: the quadtree terrain + ocean in `packet.terrain` replaced
    // M4c's flat ground disc — nothing else to emit for the surface.

    // Craft: per-part kitbash silhouettes, flat lambert shading
    // against the staged sun direction (the same vector that lights
    // the terrain); debris tints orange.
    let sun = snap
        .env
        .as_ref()
        .map(|e| e.sun_dir)
        .filter(|s| s.norm_squared() > 0.0)
        .unwrap_or_else(|| {
            let s = DVec3::new(0.4, 0.3, 0.86);
            s / s.norm()
        });
    // Box corner indices per face — the plasma sheath still emits the
    // collider AABBs, so the table stays for it (and the dust extent).
    const FACES: [([usize; 4], DVec3); 6] = [
        ([0, 1, 2, 3], DVec3::new(0.0, 0.0, -1.0)),
        ([5, 4, 7, 6], DVec3::new(0.0, 0.0, 1.0)),
        ([1, 5, 6, 2], DVec3::new(1.0, 0.0, 0.0)),
        ([4, 0, 3, 7], DVec3::new(-1.0, 0.0, 0.0)),
        ([3, 2, 6, 7], DVec3::new(0.0, 1.0, 0.0)),
        ([4, 5, 1, 0], DVec3::new(0.0, -1.0, 0.0)),
    ];
    let mut tris = Vec::new();
    for b in &snap.vessels {
        let base: [f32; 4] = if b.debris {
            [0.85, 0.45, 0.2, 1.0]
        } else {
            [0.75, 0.8, 0.88, 1.0]
        };
        for p in &b.parts {
            tris.clear();
            crate::part_mesh::emit(&p.shape, &mut tris);
            for t in tris.iter() {
                // A tri with any corner behind the eye plane drops
                // whole — the same all-or-nothing rule the VAB packet
                // uses — rather than emitting a corrupted fan.
                let [Some(c0), Some(c1), Some(c2)] = [t.a, t.b, t.c]
                    .map(|q| project_into(vp, eye, b.pos + b.rot.rotate(p.transform.apply(q))))
                else {
                    continue;
                };
                let n_w = b.rot.rotate(p.transform.apply_dir(t.n));
                let shade = (0.45 + 0.55 * n_w.dot(sun).max(0.0)) as f32;
                let color = [base[0] * shade, base[1] * shade, base[2] * shade, base[3]];
                for pos in [c0, c1, c2] {
                    v.push(csp_render::flight::FlightVertex { pos, color });
                }
            }
        }
        // Engine plume: a cone fan along −local_dir per thrust site.
        if b.throttle > 0.0 {
            for (p0, dir) in &b.plumes {
                let dir_w = {
                    let d = b.rot.rotate(*dir);
                    d / d.norm()
                };
                let tip = b.pos + b.rot.rotate(*p0);
                let len = 6.0 + 30.0 * b.throttle;
                let base_p = tip - dir_w * len;
                let (u1, u2) = {
                    let a = if dir_w.y.abs() < 0.9 {
                        DVec3::Y
                    } else {
                        DVec3::X
                    };
                    let u1 = {
                        let u = dir_w.cross(a);
                        u / u.norm()
                    };
                    (u1, dir_w.cross(u1))
                };
                let r = 0.6 + 0.9 * b.throttle;
                let col = [1.0, 0.55 + 0.25 * b.throttle as f32, 0.15, 0.7];
                for i in 0..8 {
                    let (a0, a1) = (
                        (i as f64) * core::f64::consts::PI * 0.25,
                        ((i + 1) as f64) * core::f64::consts::PI * 0.25,
                    );
                    let q0 = base_p + u1 * (a0.cos() * r) + u2 * (a0.sin() * r);
                    let q1 = base_p + u1 * (a1.cos() * r) + u2 * (a1.sin() * r);
                    emit_tri(v, tip, q0, q1, col);
                }
            }
        }

        // §3.7 reentry plasma: an emissive sheath hugging the hull —
        // each box re-emitted expanded by an intensity-scaled margin,
        // alpha-blended over the hull so the craft reads as glowing
        // inside its shock layer. Intensity is the staged convective
        // input normalized through the display window; the shell is
        // flat-lit (emissive, no sun term).
        let plasma_i = ((b.plasma_w_m2 - PLASMA_ONSET_W_M2)
            / (PLASMA_FULL_W_M2 - PLASMA_ONSET_W_M2))
            .clamp(0.0, 1.0);
        if plasma_i > 0.0 {
            let m = 0.15 + 0.35 * plasma_i;
            let col: [f32; 4] = [
                1.0,
                (0.3 + 0.6 * plasma_i) as f32,
                (0.05 + 0.35 * plasma_i) as f32,
                (0.12 + 0.5 * plasma_i) as f32,
            ];
            let d = DVec3::new(m, m, m);
            for p in &b.parts {
                let lo = p.lo - d;
                let hi = p.hi + d;
                let corners: [DVec3; 8] = [
                    DVec3::new(lo.x, lo.y, lo.z),
                    DVec3::new(hi.x, lo.y, lo.z),
                    DVec3::new(hi.x, hi.y, lo.z),
                    DVec3::new(lo.x, hi.y, lo.z),
                    DVec3::new(lo.x, lo.y, hi.z),
                    DVec3::new(hi.x, lo.y, hi.z),
                    DVec3::new(hi.x, hi.y, hi.z),
                    DVec3::new(lo.x, hi.y, hi.z),
                ];
                for (idx, _) in FACES {
                    let w = idx.map(|i| b.pos + b.rot.rotate(corners[i]));
                    emit_tri(v, w[0], w[1], w[2], col);
                    emit_tri(v, w[0], w[2], w[3], col);
                }
            }
        }

        // §3.7 ground-contact dust: a translucent tan ring hugging
        // the surface under the vessel's lowest hull vertex.
        if b.contact
            && let Some(env) = snap.env.as_ref()
        {
            let mut low = b.com;
            let mut low_alt = f64::MAX;
            let mut extent = 1.0_f64;
            for p in &b.parts {
                for c in [
                    DVec3::new(p.lo.x, p.lo.y, p.lo.z),
                    DVec3::new(p.hi.x, p.lo.y, p.lo.z),
                    DVec3::new(p.hi.x, p.hi.y, p.lo.z),
                    DVec3::new(p.lo.x, p.hi.y, p.lo.z),
                    DVec3::new(p.lo.x, p.lo.y, p.hi.z),
                    DVec3::new(p.hi.x, p.lo.y, p.hi.z),
                    DVec3::new(p.hi.x, p.hi.y, p.hi.z),
                    DVec3::new(p.lo.x, p.hi.y, p.hi.z),
                ] {
                    let w = b.pos + b.rot.rotate(c);
                    extent = extent.max((w - b.pos).norm());
                    let alt = (w - env.body_center).norm() - env.radius_m;
                    if alt < low_alt {
                        low_alt = alt;
                        low = w;
                    }
                }
            }
            let dir = {
                let d = low - env.body_center;
                if d.norm() > 0.0 {
                    d / d.norm()
                } else {
                    DVec3::X
                }
            };
            // Puff sits a hair above the surface so depth ties resolve
            // for the dust.
            let base = env.body_center + dir * (env.radius_m + 0.3);
            let (t1, t2) = {
                let a = if dir.y.abs() < 0.9 {
                    DVec3::Y
                } else {
                    DVec3::X
                };
                let t1 = {
                    let t = dir.cross(a);
                    t / t.norm()
                };
                (t1, dir.cross(t1))
            };
            let dust_r = extent * 1.6 + 1.0;
            let col = [0.62, 0.55, 0.42, 0.35];
            for i in 0..12 {
                let (a0, a1) = (
                    (i as f64) * core::f64::consts::PI / 6.0,
                    ((i + 1) as f64) * core::f64::consts::PI / 6.0,
                );
                let p0 = base + t1 * (a0.cos() * dust_r) + t2 * (a0.sin() * dust_r);
                let p1 = base + t1 * (a1.cos() * dust_r) + t2 * (a1.sin() * dust_r);
                emit_tri(v, base, p0, p1, col);
            }
        }
    }
    frame.0 = Some(packet);
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Run `extract_flight` on a synthetic snapshot/camera through a
    /// real `Schedule` — the RenderExtract system's own resource
    /// plumbing, not a bare call.
    fn run_extract(snap: FlightSnapshot, cam: MapCameraF64) -> csp_render::flight::FlightPacket {
        let mut world = bevy_ecs::world::World::new();
        world.insert_resource(FlightInput::default());
        // extract_flight gates the terrain build on the map's mode —
        // Flight here, matching the synthetic chase camera below.
        world.insert_resource(MapInput {
            mode: MapMode::Flight,
            focus: 0,
            pan_offset: DVec3::ZERO,
            yaw_rad: 0.0,
            pitch_rad: 0.0,
            distance_m: 0.0,
            viewport_px: (1.0, 1.0),
        });
        world.insert_resource(snap);
        world.insert_resource(FlightCameraF64(cam));
        world.insert_resource(TerrainConfig::default());
        world.insert_resource(TerrainCache::default());
        world.insert_resource(FlightFrame::default());
        let mut sched = Schedule::default();
        sched.add_systems(extract_flight);
        sched.run(&mut world);
        world
            .resource_mut::<FlightFrame>()
            .0
            .take()
            .expect("extract produced a flight packet")
    }

    /// A throttle-open vessel whose plume base ring lies exactly on
    /// the eye plane: every fan triangle has its tip in front of the
    /// eye and both ring corners unprojectable. The pre-fix per-vertex
    /// `emit` pushed the lone tip vertices and shifted every later
    /// triangle in the shared `TriangleList` stream; all-or-nothing
    /// `emit_tri` drops each fan tri whole, so the stream stays a
    /// multiple of three — aligned.
    #[test]
    fn behind_eye_corners_keep_the_triangle_stream_aligned() {
        let vessel = FlightBody {
            id: 0,
            pos: DVec3::ZERO,
            rot: DQuat::IDENTITY,
            vel: DVec3::ZERO,
            throttle: 1.0,
            debris: false,
            contact: false,
            plasma_w_m2: 0.0,
            nodes: 1,
            parts: vec![FlightPart {
                lo: DVec3::new(-0.5, -0.5, -0.5),
                hi: DVec3::new(0.5, 0.5, 0.5),
                transform: csp_phys::PartTransform::IDENTITY,
                shape: crate::part_mesh::PartShape::Box {
                    lo: DVec3::new(-0.5, -0.5, -0.5),
                    hi: DVec3::new(0.5, 0.5, 0.5),
                },
            }],
            // Thrust site at the body origin, dir +Z: the fan tip sits
            // at the vessel, the 1.5 m-radius base ring 36 m along −Z.
            plumes: vec![(DVec3::ZERO, DVec3::Z)],
            com: DVec3::ZERO,
            latched: false,
        };
        let snap = FlightSnapshot {
            env: None,
            vessels: vec![vessel],
        };
        // pitch = π puts the eye 36 m along −Z from the origin focus,
        // looking back along +Z: the plume tip is 36 m in front, the
        // ring corners sit at view depth ≈ 0 (`project_into` → `None`).
        let cam = solve_orbit_camera(
            0.0,
            core::f64::consts::PI,
            36.0,
            (320.0, 240.0),
            DVec3::ZERO,
            FLIGHT_FOV_Y,
            0.1,
        );
        // The setup really is the bug's shape — the tip projects, a
        // ring corner does not.
        assert!(project_into(&cam.view_proj, cam.eye, DVec3::ZERO).is_some());
        assert!(project_into(&cam.view_proj, cam.eye, DVec3::new(1.5, 0.0, -36.0)).is_none());

        let packet = run_extract(snap, cam);
        assert_eq!(
            packet.vertices.len() % 3,
            0,
            "unindexed TriangleList stream misaligned: {} vertices",
            packet.vertices.len()
        );
    }

    /// The craftless `vessel_parts` fallback: the part-local box must
    /// be centred on the local origin — `transform` already carries
    /// the hull's body-frame centre, so centring the shape too would
    /// double the offset at emit.
    #[test]
    fn fallback_box_is_not_double_offset() {
        // One hull offset from the body origin: body-frame AABB
        // lo=(2,−3,0), hi=(4,−1,2).
        let hull = csp_phys::ConvexHull {
            vertices: vec![
                DVec3::new(2.0, -3.0, 0.0),
                DVec3::new(4.0, -3.0, 0.0),
                DVec3::new(4.0, -1.0, 0.0),
                DVec3::new(2.0, -1.0, 0.0),
                DVec3::new(2.0, -3.0, 2.0),
                DVec3::new(4.0, -3.0, 2.0),
                DVec3::new(4.0, -1.0, 2.0),
                DVec3::new(2.0, -1.0, 2.0),
            ],
        };
        let parts = vessel_parts(None, &[hull]);
        assert_eq!(parts.len(), 1);
        let p = &parts[0];
        let crate::part_mesh::PartShape::Box { lo, hi } = &p.shape else {
            panic!("fallback shape must be a box, got {:?}", p.shape);
        };
        // The emitted corners (`transform.apply(shape_corner)`) must
        // land on the body-frame AABB — the dyadic coordinates make
        // this exact.
        assert_eq!(p.transform.apply(*lo), p.lo);
        assert_eq!(p.transform.apply(*hi), p.hi);
    }
}
