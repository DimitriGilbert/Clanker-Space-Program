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
    MapCameraF64, MapInput, MapMode, body_system_pos, mat4_mul, perspective_revz, project_into,
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

/// The render classification of a resolved part — which module
/// family it belongs to. Drives the flight view's livery palette so a
/// stack reads as distinct parts (tank / engine / pod / structural)
/// rather than one grey lathe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    /// Engine — a `Thrust` force site or resolved `EngineSpec`.
    Engine,
    /// Crew pod.
    Crew,
    /// Mass-carrying propellant tank.
    Tank,
    /// Massless-resource store (a battery's `ElectricCharge` tank).
    Battery,
    /// Science instrument or lab.
    Science,
    /// Docking port.
    Port,
    /// Everything else — nose cones, adapters, decouplers, hull
    /// fallbacks.
    Structural,
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
    /// The module-family classification — livery tint + banding key.
    pub kind: PartKind,
}

/// One propellant resource's aggregate fill on a vessel — units and
/// capacity summed over every live node carrying it (the HUD's fuel
/// gauge; the `csp-ui` plain-data type so `UiBuild` passes the staged
/// slice straight to `flight_hud`).
pub type FlightTank = csp_ui::PropellantGauge;

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
    /// Propellant per resource — the HUD's fuel gauges. Empty for
    /// craft-less or tank-less vessels.
    pub tanks: Vec<FlightTank>,
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

/// Pass-1 dead-stick recovery: the vessel the flight focus should pin
/// to once it is staged (set by `View::recover_control` alongside the
/// `ToBubble` command). The pin is NOT forever — it releases when the
/// pinned vessel leaves the staged set after having been seen there
/// (`seen` flips on its first staged frame; a leave after that is a
/// `ToRails`/removal — the Presentation half of the release rule),
/// when it leaves the sim's vessel arena while still pending
/// (`View::end_frame`'s sim-side half — a pending pin can never stage
/// if the vessel is gone), or when a fresh launch claims the focus
/// under the documented newest-spawn rule (`View::launch_craft`).
#[derive(Resource, Default)]
pub(crate) struct FlightFocusOverride {
    /// The pinned vessel, if any.
    pub target: Option<csp_sim::VesselId>,
    /// `true` once `target` has appeared in the staged vessel set —
    /// what distinguishes "the `ToBubble` is still draining" (the pin
    /// must survive the gap) from "it staged and left again" (release).
    seen: bool,
}

impl FlightFocusOverride {
    /// Pin the flight focus to `vessel` once it stages.
    pub(crate) fn set(&mut self, vessel: csp_sim::VesselId) {
        self.target = Some(vessel);
        self.seen = false;
    }

    /// Release the pin — the newest-spawn focus rule applies again.
    pub(crate) fn clear(&mut self) {
        self.target = None;
        self.seen = false;
    }
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
    /// Osculating apoapsis altitude (m) — `None` open/latched/no vessel.
    pub apo_alt_m: Option<f64>,
    /// Osculating periapsis altitude (m).
    pub pe_alt_m: Option<f64>,
    /// Time to apoapsis (s).
    pub t_apo_s: Option<f64>,
    /// Time to periapsis (s).
    pub t_pe_s: Option<f64>,
    /// Orbital period (s).
    pub period_s: Option<f64>,
    /// The focused vessel's propellant (resource, units, capacity) —
    /// the HUD's fuel gauges; empty for tank-less vessels.
    pub propellant: Vec<FlightTank>,
    /// Stage firings left in the launched craft's [`StagePlan`] —
    /// the HUD's "STG" readout. `0` = nothing left; `stages_total`
    /// carries the single-stage-vs-expended distinction.
    pub stages_left: usize,
    /// Firings the launched craft's plan captured at launch — `0` =
    /// a genuinely single-stage craft (or no plan), `> 0` with
    /// `stages_left == 0` = an expended multi-stage craft.
    pub stages_total: usize,
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
pub(crate) const SFX_TTL_FRAMES: u32 = 90;

/// The RenderExtract product — `None` while no flight scene exists.
#[derive(Resource, Default)]
pub struct FlightFrame(pub Option<csp_render::flight::FlightPacket>);

/// The launched craft's remaining stage firings (§2.2-C session
/// state — captured at [`crate::View::launch_craft`] from the VAB
/// session's editor staging, which the spawned `PartGraph` itself
/// does not carry). Each entry is one firing's drop set: the part
/// nodes that leave the sim when it fires — every node in the subtree
/// below each decoupler assigned to that stage (the session's
/// "severs all its child edges" rule; the decoupler itself stays
/// with the parent side, the KSP convention). Entries are in firing
/// order (the session's descending `stage_order`). Empty stages
/// (no decoupler, or a decoupler severing nothing) are dropped at
/// capture, so `stages.len()` counts real separations only.
///
/// `View::fire_stage` pops the front set and issues one logged
/// `JettisonPart` per node — the sim's per-node jettison is the
/// command surface v1 staging maps onto (a jettisoned part leaves
/// the sim outright; KSP-style fragment spawn is not in the v1
/// command vocabulary).
///
/// The plan belongs to the vessel its launch spawned — NOT the
/// flight focus: a newer spawn or a breakup fragment can take the
/// focus rule's "newest staged vessel" slot without ever retargeting
/// this plan. The `VesselId` cannot be predicted at capture (the
/// enqueued `SpawnPhysicalVessel` allocates it inside `step_frame`,
/// and queued producers/event firings may burn ids first), so the
/// plan binds *lazily* — the first `fire_stage`/`vessel_for` call
/// that sees a post-launch vessel carrying the captured craft
/// fingerprint keeps it.
#[derive(Resource, Default)]
pub struct StagePlan {
    /// Remaining stage drop-sets, firing order.
    pub stages: Vec<Vec<csp_phys::PartNodeId>>,
    /// Firings the plan held at launch — `stages.is_empty()` with
    /// `total > 0` means "expended", not "single stage" (the HUD's
    /// `stages_total` mirror).
    pub total: usize,
    /// The vessel this plan fires at — `None` until [`Self::vessel_for`]
    /// binds it; a bound id is never re-pointed at another vessel.
    pub vessel: Option<VesselId>,
    /// Binding bound: the lowest `VesselId` a post-launch spawn can
    /// mint — `vessels().next_back() + 1` at capture (ids are
    /// monotonic and never reused, so nothing already live can meet
    /// it, and any id at/above it was necessarily allocated after the
    /// launch was enqueued).
    min_id: u64,
    /// `craft_fingerprint` of the launched craft — the binding match.
    /// Two launches of the same session craft produce identical
    /// fingerprints; `min_id` keeps the newest launch's plan from
    /// binding a still-live older copy.
    craft_key: u64,
}

impl StagePlan {
    /// The launch-time capture — [`crate::View::launch_craft`] fills
    /// this after resolving the craft and before enqueueing the
    /// spawn. `min_id`/`craft_key` are what [`Self::vessel_for`]
    /// binds against once the spawn lands.
    pub(crate) fn for_launch(
        stages: Vec<Vec<csp_phys::PartNodeId>>,
        sim: &World,
        craft: &csp_sim::CraftDef,
    ) -> Self {
        Self {
            total: stages.len(),
            stages,
            vessel: None,
            min_id: sim.vessels().keys().next_back().map_or(0, |id| id.0 + 1),
            craft_key: craft_fingerprint(craft),
        }
    }

    /// A plan pre-bound to a known vessel — for tests and scripted
    /// producers that mint the `VesselId` themselves. `min_id`/
    /// `craft_key` stay inert because [`Self::vessel`] is already set.
    pub fn bound_to(stages: Vec<Vec<csp_phys::PartNodeId>>, vessel: VesselId) -> Self {
        Self {
            total: stages.len(),
            stages,
            vessel: Some(vessel),
            min_id: 0,
            craft_key: 0,
        }
    }

    /// The vessel this plan fires at, binding on first call: the
    /// newest vessel minted at-or-after [`Self::min_id`] whose stored
    /// craft matches the launch's fingerprint. An unbound plan stays
    /// unbound (never caches `None`) so a not-yet-landed spawn binds
    /// on a later press; a bound id is returned verbatim — the caller
    /// re-checks it against the live arena (the vessel may be gone).
    pub(crate) fn vessel_for(&mut self, sim: &World) -> Option<VesselId> {
        if let Some(v) = self.vessel {
            return Some(v);
        }
        let found = sim
            .vessels()
            .iter()
            .rev()
            .find(|(id, v)| {
                id.0 >= self.min_id
                    && v.craft
                        .as_ref()
                        .is_some_and(|c| craft_fingerprint(c) == self.craft_key)
            })
            .map(|(id, _)| *id);
        if found.is_some() {
            self.vessel = found;
        }
        found
    }
}

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
                    tanks: vessel_tanks(vessel),
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
        tanks: vessel_tanks(vessel),
    })
}

/// One vessel's per-resource propellant totals — capacities summed
/// over each live node's `WeldPart::tanks`, contents over the sim's
/// `tank_contents` ledger (the same `(node, resource)` map the
/// transfer/feed paths maintain). Sorted by resource id so the HUD
/// order is stable; a tank-less vessel returns empty.
fn vessel_tanks(vessel: &csp_sim::Vessel) -> Vec<FlightTank> {
    let Some(craft) = vessel.craft.as_ref() else {
        return Vec::new();
    };
    let mut totals: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for (id, node) in &craft.graph.nodes {
        let Some(part) = craft.parts.get(node.effective_index as usize) else {
            continue;
        };
        let contents = vessel.tank_contents.get(id);
        for tank in &part.tanks {
            let e = totals.entry(tank.resource.as_str().to_owned()).or_default();
            e.1 += tank.capacity;
            e.0 += contents
                .and_then(|m| m.get(&tank.resource))
                .copied()
                .unwrap_or(0.0);
        }
    }
    totals
        .into_iter()
        .map(|(resource, (units, capacity))| FlightTank {
            resource,
            units,
            capacity,
        })
        .collect()
}

/// Classify a resolved part into its render [`PartKind`] — the same
/// precedence `part_mesh::classify` uses for silhouettes (engine and
/// crew beat the collider), then the carried inventories: propellant
/// tanks (mass-bearing `TankSpec`s) vs batteries (massless stores),
/// science payloads, docking ports, and everything else structural.
fn part_kind(wp: &csp_phys::WeldPart) -> PartKind {
    if wp.engine.is_some()
        || wp
            .force_sites
            .iter()
            .any(|s| s.kind == csp_phys::ForceSiteKind::Thrust)
    {
        return PartKind::Engine;
    }
    if wp.crew_seats > 0 {
        return PartKind::Crew;
    }
    if !wp.tanks.is_empty() {
        return if wp.tanks.iter().any(|t| t.mass_per_unit_kg > 0.0) {
            PartKind::Tank
        } else {
            PartKind::Battery
        };
    }
    if !wp.experiments.is_empty() || wp.lab.is_some() {
        return PartKind::Science;
    }
    if !wp.dock_ports.is_empty() {
        return PartKind::Port;
    }
    PartKind::Structural
}

/// Per-kind base albedo (linear RGB) — the livery palette that keeps
/// a stack from reading as one grey lathe: propellant tanks run a
/// light warm stage-white, capsules a slightly creamier off-white,
/// engines dark nozzle steel, batteries darker still.
fn kind_tint(kind: PartKind) -> [f32; 3] {
    match kind {
        PartKind::Engine => [0.23, 0.24, 0.28],
        PartKind::Crew => [0.72, 0.70, 0.64],
        PartKind::Tank => [0.80, 0.78, 0.71],
        PartKind::Battery => [0.19, 0.23, 0.26],
        PartKind::Science => [0.44, 0.46, 0.50],
        PartKind::Port => [0.30, 0.32, 0.35],
        PartKind::Structural => [0.60, 0.63, 0.69],
    }
}

/// Painted-band accents along the part's local z — `u` is the
/// vertex's height normalized over the shape's z range (0 = the −z
/// end). Tanks carry an engine-skirt band and a mid-stage ring (the
/// classic stage livery), pods a dark window ring; everything else
/// keeps its flat tint.
fn livery(kind: PartKind, tint: [f32; 3], u: f64) -> [f32; 3] {
    let band = |acc: [f32; 3], k: f32| {
        [
            tint[0] + (acc[0] - tint[0]) * k,
            tint[1] + (acc[1] - tint[1]) * k,
            tint[2] + (acc[2] - tint[2]) * k,
        ]
    };
    match kind {
        PartKind::Tank if u < 0.09 || (0.58..0.66).contains(&u) => band([0.68, 0.34, 0.14], 0.85),
        PartKind::Crew if (0.30..0.44).contains(&u) => band([0.08, 0.10, 0.15], 0.85),
        _ => tint,
    }
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
                    kind: PartKind::Structural,
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
            kind: part_kind(wp),
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
                // ~89° — the camera may go nearly under or directly
                // overhead; a hard ±π/2 flips the up vector instead.
                input.pitch = (input.pitch + dpitch).clamp(-1.55, 1.55);
            }
            FlightEvent::Zoom { factor } => {
                input.distance_m = (input.distance_m * factor).clamp(3.0, 1.0e6);
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

/// The flight chase-camera solve: an orbit camera in the *local-level
/// frame* at the focus (up = radial at the CoM, north = spin axis
/// projected level, east completes it), so the horizon stays level
/// while the camera is dragged — the shared world-frame
/// `solve_orbit_camera` puts the orbit basis on the ecliptic normal,
/// which renders a pad away from the pole tilted (the "can't place
/// the camera" complaint). `input.yaw` is the azimuth about local up,
/// `input.pitch` the elevation above the horizon; the eye is clamped
/// just above the *terrain* surface (the baked heightfield rises up
/// to `amplitude_m` above `radius_m`, so a datum-only floor lets a
/// downward drag bury the camera inside a hill). Falls back to the
/// world-frame basis when there is no environment.
fn solve_chase_camera(
    input: &FlightInput,
    env: Option<&FlightEnv>,
    focus: &FlightBody,
    terrain_cfg: &TerrainConfig,
    terrain_cache: &mut TerrainCache,
) -> MapCameraF64 {
    let mut up = DVec3::Z;
    let mut axis = DVec3::Z;
    if let Some(env) = env {
        axis = env.spin_axis;
        let rel = focus.com - env.body_center;
        if rel.norm_squared() > 1.0e-12 {
            up = rel / rel.norm();
        }
    }
    let mut north = axis - up * axis.dot(up);
    if north.norm_squared() < 1.0e-12 {
        north = DVec3::Z - up * up.z;
        if north.norm_squared() < 1.0e-12 {
            north = DVec3::X - up * up.x;
        }
    }
    let north = north / north.norm();
    let east = north.cross(up);
    let (sy, cy) = input.yaw.sin_cos();
    let (sp, cp) = input.pitch.sin_cos();
    let dir = north * (cy * cp) + east * (sy * cp) + up * sp;
    let mut eye = focus.com + dir * input.distance_m;
    // Keep the eye out of the ground: a drag pitching under the
    // horizon stops above the local terrain surface (the baked
    // heightfield's neighbourhood ceiling), not the bare datum —
    // procedural relief rises up to `amplitude_m` over `radius_m`.
    if let Some(env) = env {
        let rel = eye - env.body_center;
        let r = rel.norm();
        if r > 0.0 {
            let floor =
                crate::terrain::surface_floor_radius(terrain_cfg, terrain_cache, env, rel / r);
            let min_r = (env.radius_m + 1.5).max(floor + 1.5);
            if r < min_r {
                eye = env.body_center + rel * (min_r / r);
            }
        }
    }
    // Camera basis (columns = view axes in world, c0×c1 = c2): z_out
    // points focus → eye, "up" is the local surface up.
    let z_out = {
        let d = eye - focus.com;
        if d.norm_squared() > 1.0e-12 {
            d / d.norm()
        } else {
            up
        }
    };
    let right = {
        let r = up.cross(z_out);
        if r.norm_squared() > 1.0e-12 {
            r / r.norm()
        } else {
            east
        }
    };
    let upv = z_out.cross(right);
    let view = [
        [right.x, right.y, right.z, 0.0],
        [upv.x, upv.y, upv.z, 0.0],
        [z_out.x, z_out.y, z_out.z, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let aspect = (input.viewport_px.0 / input.viewport_px.1.max(1.0)).max(1e-6);
    let proj = perspective_revz(FLIGHT_FOV_Y, aspect, 0.1);
    MapCameraF64 {
        view_proj: mat4_mul(&proj, &view),
        eye,
        distance_m: input.distance_m,
        valid: true,
    }
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
#[allow(clippy::too_many_arguments)] // the camera solve carries the terrain floor
pub(crate) fn flight_present(
    input: Res<FlightInput>,
    snap: Res<FlightSnapshot>,
    plan: Res<StagePlan>,
    terrain_cfg: Res<TerrainConfig>,
    mut terrain_cache: ResMut<TerrainCache>,
    mut cam: ResMut<FlightCameraF64>,
    mut ui: ResMut<FlightUiData>,
    mut sfx: ResMut<FlightSfx>,
    clock: Res<crate::Clock>,
    mut focus_override: ResMut<FlightFocusOverride>,
) {
    ui.vessel_count = snap.vessels.len();
    ui.debris_count = snap.vessels.iter().filter(|v| v.debris).count();
    // §3.7 edge detection on the staged snapshot — cue pushes and
    // TTL aging live in `sfx_edges` (kept out of this system under
    // the line budget).
    ui.sfx = sfx_edges(&mut sfx, &snap.vessels, ui.debris_count);

    // Pin lifecycle: a pinned vessel seen staged releases the override
    // the frame it leaves the set (a `ToRails` conversion or a
    // removal — the "was there, now gone" edge). A pin whose vessel
    // was never staged is a `ToBubble` still draining and survives;
    // `View::end_frame` clears the sub-case where the vessel left the
    // sim's arena entirely (it can never stage).
    if let Some(id) = focus_override.target {
        if snap.vessels.iter().any(|b| b.id == id.0) {
            focus_override.seen = true;
        } else if focus_override.seen {
            focus_override.clear();
        }
    }
    let focus = focus_override
        .target
        .and_then(|id| snap.vessels.iter().find(|b| b.id == id.0))
        .or_else(|| focus_vessel(&snap.vessels));
    let Some(focus) = focus else {
        cam.0.valid = false;
        ui.has_vessel = false;
        ui.focus = None;
        ui.propellant.clear();
        ui.stages_left = 0;
        ui.stages_total = plan.total;
        return;
    };
    cam.0 = solve_chase_camera(
        &input,
        snap.env.as_ref(),
        focus,
        &terrain_cfg,
        &mut terrain_cache,
    );
    ui.has_vessel = true;
    ui.focus = Some(focus.id);
    ui.latched = focus.latched;
    ui.propellant = focus.tanks.clone();
    ui.stages_left = plan.stages.len();
    ui.stages_total = plan.total;
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
    ui.apo_alt_m = None;
    ui.pe_alt_m = None;
    ui.t_apo_s = None;
    ui.t_pe_s = None;
    ui.period_s = None;
    if let Some(env) = snap.env.as_ref() {
        let rel = focus.com - env.body_center;
        let r = rel.norm();
        if r > 0.0 {
            ui.alt_m = r - env.radius_m;
            ui.vspeed_mps = focus.vel.dot(rel / r);
            stage_attitude(&mut ui, env, focus, rel / r);
        }
        // Pass-1 orbit readout: osculating elements at the CoM →
        // Ap/Pe altitudes, times-to-apsis, period. Latched bodies
        // ride pure co-rotation — no honest conic to report.
        if !focus.latched
            && let Ok(el) = csp_sim::orbit::cartesian_to_elements(rel, focus.vel, env.mu_m3_per_s2)
        {
            let (a, e) = (el.semi_major_axis_m, el.eccentricity);
            // The parabolic sliver: `cartesian_to_elements` encodes
            // |e − 1| ≤ `E_PARABOLIC_TOL` as `a := −p < 0`, so a raw
            // `e < 1.0` gate reports AP = a(1 + e) < 0 and T = ∞ on a
            // conic that has neither. Pe/apo readouts publish only
            // outside the band — a closed branch needs `a > 0` too.
            if (e - 1.0).abs() > crate::map::E_PARABOLIC_TOL {
                ui.pe_alt_m = Some(a * (1.0 - e) - env.radius_m);
            }
            if a > 0.0 && e < 1.0 - crate::map::E_PARABOLIC_TOL {
                ui.apo_alt_m = Some(a * (1.0 + e) - env.radius_m);
                ui.period_s = Some(el.period_s);
                let now = clock.0.ticks();
                let t = |apo: bool| {
                    csp_sim::scripts::resolve_apsis_ticks(&el, env.mu_m3_per_s2, now, now, apo)
                        .ok()
                        .flatten()
                        .map(|tk| (tk - now) as f64 / csp_core::TICK_HZ as f64)
                };
                ui.t_apo_s = t(true);
                ui.t_pe_s = t(false);
            }
        }
    }
}

/// The §3.7 edge detector — the named transitions (ignition/cut,
/// jettison, contact, latch, breakup, removal) detected on the staged
/// snapshot, folded into [`FlightSfx`] cue pushes plus the TTL
/// age-out, and the live stack mirrored for the HUD. Per-focus edges
/// apply only while the SAME vessel stays focused; a focus switch
/// re-baselines instead of firing a spurious cut/contact edge. The
/// focus rule is `focus_vessel`'s — the newest staged vessel.
fn sfx_edges(sfx: &mut FlightSfx, vessels: &[FlightBody], debris_count: usize) -> Vec<String> {
    let cur = focus_vessel(vessels).map(|f| SfxPrev {
        focus: f.id,
        throttle_on: f.throttle > 0.0,
        nodes: f.nodes,
        debris: debris_count,
        vessels: vessels.len(),
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
    sfx.prev = cur;
    sfx.cues.iter().map(|c| c.text.clone()).collect()
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

/// All-or-nothing emission of one world-space triangle into the
/// clip-space stream: every corner is projected first and the tri is
/// pushed whole or dropped whole — the same rule the part pass and
/// the VAB packet use. A per-vertex `push` would let one behind-eye
/// corner shift every later triangle in the shared unindexed
/// `TriangleList` stream.
fn emit_tri(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    a: DVec3,
    b: DVec3,
    c: DVec3,
    color: [f32; 4],
) {
    let [Some(c0), Some(c1), Some(c2)] = [a, b, c].map(|q| project_into(vp, eye, q)) else {
        return;
    };
    for pos in [c0, c1, c2] {
        v.push(csp_render::flight::FlightVertex { pos, color });
    }
}

/// [`emit_tri`] with a per-corner color — the plume and plasma-sheath
/// gradient streams.
fn emit_tri3(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    a: DVec3,
    b: DVec3,
    c: DVec3,
    colors: [[f32; 4]; 3],
) {
    let [Some(c0), Some(c1), Some(c2)] = [a, b, c].map(|q| project_into(vp, eye, q)) else {
        return;
    };
    for (pos, color) in [(c0, colors[0]), (c1, colors[1]), (c2, colors[2])] {
        v.push(csp_render::flight::FlightVertex { pos, color });
    }
}

/// One vessel's engine plumes — a two-shell tapered column along
/// `−local_dir` per thrust site: a bright core inside a wider
/// translucent mantle. Rings widen downstream (under-expanded
/// expansion grows with altitude, `expand` ≥ 1), alpha fades
/// tip→tail, and a slow sim-clock flicker keeps the column alive.
fn emit_plumes(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    b: &FlightBody,
    expand: f64,
    t_s: f64,
) {
    let throttle = b.throttle.clamp(0.0, 1.0);
    const PLUME_RINGS: usize = 6;
    const PLUME_SEGS: usize = 16;
    for (site, (p0, dir)) in b.plumes.iter().enumerate() {
        let dir_w = {
            let d = b.rot.rotate(*dir);
            d / d.norm()
        };
        let tip = b.pos + b.rot.rotate(*p0);
        let len = 4.5 + 22.0 * throttle;
        let exit_r = 0.30 + 0.38 * throttle;
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
        // Two concentric shells: bright core + outer mantle. The
        // INNER core emits first: the flight pass writes depth with
        // `GreaterEqual`, so a mantle drawn first would leave its
        // front wall's depth in the buffer and the core — behind it
        // along the ray — would fail the test and never render
        // (depth-culled dead geometry inside its own plume).
        for &(r_scale, l_scale, hot, a_gain) in &[
            (0.55, 0.70, [1.0f64, 0.94, 0.78], 0.95f64),
            (1.0, 1.0, [1.0, 0.72, 0.32], 0.70),
        ] {
            let n_len = len * l_scale;
            for k in 0..PLUME_RINGS {
                let t0 = k as f64 / PLUME_RINGS as f64;
                let t1 = (k + 1) as f64 / PLUME_RINGS as f64;
                let ring = |t: f64, i: usize| {
                    let ang = (i as f64) * 2.0 * core::f64::consts::PI / PLUME_SEGS as f64;
                    let wob = 1.0
                        + 0.06 * (t_s * 9.0 + site as f64 * 2.3 + t * 11.0 + i as f64 * 0.7).sin();
                    let r = exit_r * r_scale * (1.0 + 0.85 * expand * t.powf(1.5)) * wob;
                    tip - dir_w * (t * n_len) + u1 * (ang.cos() * r) + u2 * (ang.sin() * r)
                };
                let col = |t: f64| -> [f32; 4] {
                    let fade = (1.0 - t).powf(1.7);
                    let cool = [0.85f64, 0.28, 0.08];
                    let rgb = [
                        hot[0] + (cool[0] - hot[0]) * t,
                        hot[1] + (cool[1] - hot[1]) * t,
                        hot[2] + (cool[2] - hot[2]) * t,
                    ];
                    [
                        rgb[0] as f32,
                        rgb[1] as f32,
                        rgb[2] as f32,
                        (a_gain * fade * (0.25 + 0.75 * throttle)) as f32,
                    ]
                };
                for i in 0..PLUME_SEGS {
                    let j = (i + 1) % PLUME_SEGS;
                    let (a0, b0) = (ring(t0, i), ring(t0, j));
                    let (a1, b1) = (ring(t1, i), ring(t1, j));
                    let (c0, c1) = (col(t0), col(t1));
                    emit_tri3(v, vp, eye, a0, a1, b1, [c0, c1, c1]);
                    emit_tri3(v, vp, eye, a0, b1, b0, [c0, c1, c0]);
                }
            }
        }
    }
}

/// One vessel's §3.7 reentry plasma — an emissive sheath grown off
/// the hull: each part's silhouette re-emitted inflated by an
/// intensity-scaled margin and shaded by how much each vertex faces
/// the flow, so the leading side burns white-orange while the wake
/// side stays a faint red rim. Alpha-blended over the hull, the craft
/// reads as glowing inside its shock layer. `plasma_i` is the staged
/// convective input normalized through the display window; the shell
/// is flat-lit (emissive).
fn emit_sheath(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    b: &FlightBody,
    flow: DVec3,
    plasma_i: f64,
    tris: &mut Vec<crate::part_mesh::SmoothTri>,
) {
    for p in &b.parts {
        let (lo, hi) = p.shape.local_aabb();
        let size = ((hi - lo).norm() * 0.5).max(0.05);
        let shell = crate::part_mesh::inflated(&p.shape, size * (0.04 + 0.08 * plasma_i) + 0.015);
        tris.clear();
        crate::part_mesh::emit_smooth(&shell, tris);
        for t in tris.iter() {
            let world = |q: DVec3| b.pos + b.rot.rotate(p.transform.apply(q));
            let cols = [(t.a, t.na), (t.b, t.nb), (t.c, t.nc)].map(|(_q, n)| {
                let n_w = b.rot.rotate(p.transform.apply_dir(n));
                let facing = if flow == DVec3::ZERO {
                    // Coasting: wrap the glow evenly rather than
                    // guess a leading side.
                    0.5
                } else {
                    n_w.dot(flow).max(0.0).powf(0.6)
                };
                // Wrap term keeps the shaded side a faint ember
                // rather than switching off.
                let glow = 0.12 + 0.88 * facing;
                [
                    1.0,
                    (0.16 + 0.58 * facing) as f32,
                    (0.03 + 0.28 * facing) as f32,
                    ((0.05 + 0.42 * plasma_i) * glow) as f32,
                ]
            });
            emit_tri3(v, vp, eye, world(t.a), world(t.b), world(t.c), cols);
        }
    }
}

/// `RenderExtract` — emit the scene's clip-space triangles + the R2
/// terrain packet (quadtree selection runs here, in the extract, from
/// the solved camera — §2.5's "LOD selection" lives upstream in
/// Presentation; the leaf walk itself is extract-time derivation).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
pub(crate) fn extract_flight(
    input: Res<FlightInput>,
    map_input: Res<MapInput>,
    snap: Res<FlightSnapshot>,
    cam: Res<FlightCameraF64>,
    terrain_cfg: Res<TerrainConfig>,
    mut terrain_cache: ResMut<TerrainCache>,
    clock: Res<crate::Clock>,
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
            input.viewport_px,
        ));
    }
    let v = &mut packet.vertices;
    if packet.terrain.is_none() {
        // Sky fallback: a far-depth fullscreen space backdrop (two
        // tris at NDC z≈0 — reverse-Z far). Near-black with a faint
        // cool gradient so the frame isn't a dead flat void; the
        // scattering sky replaces it whenever terrain runs.
        let zenith = [0.004, 0.006, 0.015, 1.0];
        let nadir = [0.014, 0.017, 0.034, 1.0];
        let q = [
            ([-1.0f32, -1.0, 0.0], nadir),
            ([1.0, -1.0, 0.0], nadir),
            ([1.0, 1.0, 0.0], zenith),
            ([-1.0, -1.0, 0.0], nadir),
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

    // R2: the quadtree terrain + ocean in `packet.terrain` replaced
    // M4c's flat ground disc — nothing else to emit for the surface.

    // Craft: per-part kitbash silhouettes, smooth-shaded per vertex
    // against the staged sun direction (the same vector that lights
    // the terrain) plus a faint view-half-vector sheen; livery tint
    // and painted bands come from the part's module family. Debris
    // tints charred orange and takes no sheen.
    let sun = snap
        .env
        .as_ref()
        .map(|e| e.sun_dir)
        .filter(|s| s.norm_squared() > 0.0)
        .unwrap_or_else(|| {
            let s = DVec3::new(0.4, 0.3, 0.86);
            s / s.norm()
        });
    let mut tris = Vec::new();
    for b in &snap.vessels {
        for p in &b.parts {
            let base: [f32; 3] = if b.debris {
                [0.72, 0.38, 0.16]
            } else {
                kind_tint(p.kind)
            };
            let (lo, hi) = p.shape.local_aabb();
            let z_span = (hi.z - lo.z).max(1e-9);
            tris.clear();
            crate::part_mesh::emit_smooth(&p.shape, &mut tris);
            for t in tris.iter() {
                // A tri with any corner behind the eye plane drops
                // whole — the same all-or-nothing rule the VAB packet
                // uses — rather than emitting a corrupted fan.
                let [Some(c0), Some(c1), Some(c2)] = [t.a, t.b, t.c]
                    .map(|q| project_into(vp, eye, b.pos + b.rot.rotate(p.transform.apply(q))))
                else {
                    continue;
                };
                for ((local, nrm), pos) in [(t.a, t.na), (t.b, t.nb), (t.c, t.nc)]
                    .into_iter()
                    .zip([c0, c1, c2])
                {
                    let n_w = b.rot.rotate(p.transform.apply_dir(nrm));
                    let lam = n_w.dot(sun).max(0.0);
                    let mut shade = 0.36 + 0.64 * lam;
                    // Faint metallic sheen toward the eye — charred
                    // debris stays matte.
                    if !b.debris {
                        let to_eye = eye - b.pos - b.rot.rotate(p.transform.apply(local));
                        let d2 = to_eye.norm_squared();
                        if d2 > 1e-6 {
                            let half = sun + to_eye / d2.sqrt();
                            let hl = half.norm();
                            if hl > 1e-9 {
                                shade += 0.20 * n_w.dot(half / hl).max(0.0).powi(12);
                            }
                        }
                    }
                    let u = (local.z - lo.z) / z_span;
                    let tint = if b.debris {
                        base
                    } else {
                        livery(p.kind, base, u)
                    };
                    let shade = shade.min(1.0) as f32;
                    v.push(csp_render::flight::FlightVertex {
                        pos,
                        color: [
                            (tint[0] * shade).min(1.0),
                            (tint[1] * shade).min(1.0),
                            (tint[2] * shade).min(1.0),
                            1.0,
                        ],
                    });
                }
            }
        }
        // Engine plume: a two-shell tapered column along −local_dir
        // per thrust site — a bright core inside a wider translucent
        // mantle. Rings widen downstream (under-expanded expansion
        // grows with altitude), alpha fades tip→tail, and a slow
        // sim-clock flicker keeps the column alive.
        if b.throttle > 0.0 {
            // Ambient expansion: vacuum plumes flare far wider than
            // sea-level ones (ratio keyed to the atmosphere top).
            let expand = snap.env.as_ref().map_or(1.0, |e| {
                if e.atm_top_m > 0.0 {
                    let alt = ((b.com - e.body_center).norm() - e.radius_m).max(0.0);
                    1.0 + 1.8 * (alt / e.atm_top_m).clamp(0.0, 1.0)
                } else {
                    2.6
                }
            });
            let t_s = clock.0.ticks() as f64 / csp_core::TICK_HZ as f64;
            emit_plumes(v, vp, eye, b, expand, t_s);
        }

        // §3.7 reentry plasma: an emissive sheath grown off the hull —
        // the leading side burns white-orange, the wake side a faint
        // red rim (`emit_sheath` carries the details).
        let plasma_i = ((b.plasma_w_m2 - PLASMA_ONSET_W_M2)
            / (PLASMA_FULL_W_M2 - PLASMA_ONSET_W_M2))
            .clamp(0.0, 1.0);
        if plasma_i > 0.0 {
            // Stagnation side = the surface-relative velocity
            // direction (the sim's own `v_rel` convention).
            let v_rel = snap
                .env
                .as_ref()
                .map_or(b.vel, |e| b.vel - e.body_spin.cross(b.com - e.body_center));
            let flow = if v_rel.norm_squared() > 1.0 {
                v_rel / v_rel.norm()
            } else {
                DVec3::ZERO
            };
            emit_sheath(v, vp, eye, b, flow, plasma_i, &mut tris);
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
                emit_tri(v, vp, eye, base, p0, p1, col);
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
            focus_vessel: None,
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
        world.insert_resource(crate::Clock::default());
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
                kind: PartKind::Structural,
            }],
            // Thrust site at the body origin, dir +Z: the fan tip sits
            // at the vessel, the 1.5 m-radius base ring 36 m along −Z.
            plumes: vec![(DVec3::ZERO, DVec3::Z)],
            com: DVec3::ZERO,
            latched: false,
            tanks: Vec::new(),
        };
        let snap = FlightSnapshot {
            env: None,
            vessels: vec![vessel],
        };
        // pitch = π puts the eye 36 m along −Z from the origin focus,
        // looking back along +Z: the plume tip is 36 m in front, the
        // ring corners sit at view depth ≈ 0 (`project_into` → `None`).
        let cam = crate::map::solve_orbit_camera(
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
