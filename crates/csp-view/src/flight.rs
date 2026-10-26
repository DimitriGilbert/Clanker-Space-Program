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
    /// `PartNodeId.0` — the right-click part menu's pick key: the
    /// screen-space pick resolves *this* node and the emitted
    /// `RunExperiment`/`DeployPart` commands name it. `u64::MAX` for
    /// the craft-less hull fallback (no graph node to name).
    pub node: u64,
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
    /// The real baked model (procedural parts.csar)
    /// fitted to the shape's AABB at emit — `None` draws the kitbash
    /// silhouette fallback.
    pub model: Option<std::sync::Arc<crate::art::PartModel>>,
}

/// One propellant resource's aggregate fill on a vessel — units and
/// capacity summed over every live node carrying it (the HUD's fuel
/// gauge; the `csp-ui` plain-data type so `UiBuild` passes the staged
/// slice straight to `flight_hud`).
pub type FlightTank = csp_ui::PropellantGauge;

/// One graph node's propulsion-walk inputs — the staged data
/// `flight_present` turns into the live TWR/acceleration/burn-time/Δv
/// readouts (§2.5: the sim read happens in `capture_sync_input`; the
/// arithmetic is presentation-side, off this mirror). Craft-less
/// vessels stage an empty node list — their unmetered thrust still
/// shows via [`VesselPropulsion::thrust_live_n`], the propellant
/// figures honestly report "—".
#[derive(Clone, Debug)]
pub struct PropulsionNode {
    /// The graph node — the [`StagePlan`] drop sets key on these.
    pub node: csp_phys::PartNodeId,
    /// Live node mass (kg) — dry + current contents (the part's
    /// `mass_kg` is debited as propellant leaves).
    pub mass_kg: f64,
    /// Σ *craft-baseline* `rated_n` over the node's `Thrust` force
    /// sites (N) — pre-factor, pre-coverage; the walk applies the
    /// §3.4 derate and the fuel-coverage pool itself so a zero-throttle
    /// tick (whose live sites all read 0) still stages a real figure.
    pub thrust_base_n: f64,
    /// The §3.4 reliability thrust factor (`1.0` nominal, `0.0`
    /// misfired/ignition-dead) — `vessel.reliability`'s record.
    pub thrust_factor: f64,
    /// Resolved vacuum Isp (s) — `0` without an `EngineSpec` (an
    /// unmetered site: real thrust, no consumption model).
    pub isp_vac_s: f64,
    /// `(resource, mass-fraction)` consumption split — empty unmetered.
    pub propellants: Vec<(csp_data::ResourceId, f64)>,
    /// The node's declared tanks as `(resource, units remaining,
    /// mass_per_unit_kg)` — live `tank_contents` against the spec.
    pub tanks: Vec<(csp_data::ResourceId, f64, f64)>,
}

/// One vessel's propulsion walk table — the per-node inputs plus the
/// live body figures that need no craft.
#[derive(Clone, Debug, Default)]
pub struct VesselPropulsion {
    /// `VesselId.0` — the lookup key against the staged vessels.
    pub vessel: u64,
    /// Live wet mass (kg) — `WeldedBody::mass_kg` in the bubble, Σ
    /// part masses on a latched pose.
    pub mass_kg: f64,
    /// Σ live-scaled `rated_n` over the body's `Thrust` sites (N) —
    /// the base·factor·coverage product a full-throttle tick applies.
    /// `0` for latched vessels (a parked craft's commanded throttle is
    /// zero — its sites carry no live scaling; `nodes` still stages
    /// the baseline ratings the TWR/Δv walk needs).
    pub thrust_live_n: f64,
    /// Per-node walk rows, ascending `PartNodeId`.
    pub nodes: Vec<PropulsionNode>,
}

/// The propulsion walk mirror — a standalone resource for the same
/// reason [`LiveVesselCount`] is one: `FlightSnapshot`'s pub fields
/// are literal-constructed by `csp-app`'s audio test, so the struct
/// cannot grow. `sync_flight` writes it from the staged input;
/// `flight_present` resolves the focused vessel against it.
#[derive(Resource, Default)]
pub struct VesselPropulsionStaged(pub Vec<VesselPropulsion>);

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
    /// Live plume emitters `(local_pos, local_dir, delivered)` — only
    /// the Thrust sites the sim still rates: each tick rewrites a
    /// craft engine site's `rated_n` as `base·factor·coverage` (the
    /// §3.4 derate times the pooled propellant coverage), so a dry or
    /// misfired engine reads `0` and stages no emitter — a
    /// dry-but-throttled craft (the `throttle` command survives the
    /// dry-out) draws no exhaust. `delivered` is the site's live
    /// share of its baseline rating — `emit_plumes` dims the column
    /// by `throttle·delivered`, keying the effect to real delivered
    /// thrust rather than the throttle command alone.
    pub plumes: Vec<(DVec3, DVec3, f64)>,
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
    /// Non-debris vessels in the sim across ALL residences — `vessels`
    /// stages only InBubble + latched, so an OnRails vessel would
    /// otherwise read as destroyed (P12-F2: a burn completing back to
    /// rails read as "vessel lost" and rewound the tutorial).
    pub live_vessels: usize,
    /// Per-vessel propulsion walk tables — staged beside `vessels` (a
    /// parallel list keyed by id, not a `FlightBody` field: the render
    /// struct is literal-constructed by downstream tests and cannot
    /// grow either). Mirrored into [`VesselPropulsionStaged`].
    pub propulsion: Vec<VesselPropulsion>,
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
    pub(crate) seen: bool,
    /// `true` while the recovered vessel's `TakeControl{Player}` lease
    /// transfer is still owed — issued by `View::end_frame` only on
    /// the `seen` edge, so a `ToBubble` that drained and REJECTED
    /// never preempts the vessel's live `Script`/`Agent` lease
    /// (P04-F3: the press is not the conversion).
    pub(crate) lease_pending: bool,
}

impl FlightFocusOverride {
    /// Pin the flight focus to `vessel` once it stages, and arm the
    /// Player-lease transfer for the moment it proves staged (the
    /// recover-control press issues `ToBubble`; the lease follows the
    /// conversion, not the press).
    pub(crate) fn set(&mut self, vessel: csp_sim::VesselId) {
        self.target = Some(vessel);
        self.seen = false;
        self.lease_pending = true;
    }

    /// Release the pin — the newest-spawn focus rule applies again.
    pub(crate) fn clear(&mut self) {
        self.target = None;
        self.seen = false;
        self.lease_pending = false;
    }

    /// `true` while the pin is armed but its vessel has never staged
    /// — the `ToBubble` is either still draining or already rejected.
    /// `View::sim_step` uses this to release the dead pin on a
    /// drain-reject (P04-F2): a pending pin left armed would hijack
    /// the focus on any later staging of the same vessel.
    pub(crate) fn pending(&self) -> bool {
        self.target.is_some() && !self.seen
    }

    /// The vessel the Player-lease transfer is owed for — `Some`
    /// exactly once, on the frame `seen` first proves the `ToBubble`
    /// materialized the pinned vessel.
    pub(crate) fn lease_target(&self) -> Option<csp_sim::VesselId> {
        (self.seen && self.lease_pending)
            .then_some(self.target)
            .flatten()
    }

    /// The shared pin-aware focus resolver — the real focus is the
    /// pinned vessel when it is staged, else the newest staged vessel
    /// ([`focus_vessel`]'s rule). Every focus-sensitive consumer
    /// resolves through HERE — the chase camera/HUD, the SFX edges,
    /// the tutorial measures, and (via [`FlightSfx::focus_vessel`])
    /// the app's audio voices — so a `recover_control` pin cannot
    /// split them across two vessels (P04-F4/P07-F1/P11-F1/P12-F1).
    pub(crate) fn resolve<'a>(&self, vessels: &'a [FlightBody]) -> Option<&'a FlightBody> {
        self.target
            .and_then(|id| vessels.iter().find(|b| b.id == id.0))
            .or_else(|| focus_vessel(vessels))
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

/// The staged non-debris vessel count across ALL residences —
/// [`FlightSyncInput::live_vessels`] mirrored by `sync_flight`
/// (Appendix A item 1's "mirror writes are systems"). A standalone
/// resource rather than a `FlightSnapshot` field: `FlightSnapshot`'s
/// pub fields are literal-constructed by `csp-app`'s audio test, so
/// the struct cannot grow. The tutorial's `has_vessel` reads this —
/// an OnRails vessel is alive, not lost (P12-F2).
#[derive(Resource, Default)]
pub struct LiveVesselCount(pub usize);

/// HUD mirrors — written by `flight_present`, drawn by
/// `csp_ui::flight_hud`.
#[derive(Resource, Default)]
pub struct FlightUiData {
    /// Focused vessel exists.
    pub has_vessel: bool,
    /// Focused vessel id (for throttle commands), `None` otherwise —
    /// the pin-aware resolution ([`FlightFocusOverride::resolve`]):
    /// the pinned vessel when staged, else the most recently spawned
    /// staged vessel (see `focus_vessel`). Includes a surface-latched
    /// pad vessel — `SetThrottle` unlatches it (the sim's §2.6.4
    /// control rule).
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
    /// Live wet mass of the focused vessel (kg) — `0` unknown.
    pub mass_kg: f64,
    /// Thrust delivered at the commanded throttle (N) — the
    /// base·factor·coverage·throttle product over the live engines
    /// (the unmetered-craft fallback is the live site sum·throttle).
    pub thrust_n: f64,
    /// Full-throttle deliverable thrust (N) — the coverage pool at
    /// θ = 1; the figure `twr_surface` and the max-acceleration
    /// readouts hang off.
    pub thrust_full_n: f64,
    /// Thrust-to-weight at the commanded throttle vs *local* gravity —
    /// `None` with no environment, no propulsion data, or zero mass.
    pub twr: Option<f64>,
    /// `thrust_full_n / (mass·g_surface)` — the pad figure KSP pads
    /// the staging column with (body's own surface g, not G0).
    pub twr_surface: Option<f64>,
    /// Thrust acceleration at the commanded throttle (m/s²).
    pub accel_mps2: Option<f64>,
    /// `accel_mps2 / csp_data::G0` — the same in g's.
    pub accel_gee: Option<f64>,
    /// Mass-flow-weighted vacuum Isp over the metered engines (s) —
    /// `None` when none are aboard (unmetered craft or no engines).
    pub isp_s: Option<f64>,
    /// Propellant mass flow at the commanded throttle (kg/s) —
    /// `None` without metered engines.
    pub mass_flow_kg_s: Option<f64>,
    /// Seconds until the limiting propellant pool empties at the
    /// commanded throttle — `None` closed-throttle, no metered
    /// engines, or nothing drawing.
    pub fuel_depletion_s: Option<f64>,
    /// The same at full throttle (s) — the burn-time figure.
    pub burn_full_s: Option<f64>,
    /// Tsiolkovsky Δv still aboard: the attached set's burn plus the
    /// remaining stage drops' (m/s) — `None` with no metered engine.
    pub delta_v_mps: Option<f64>,
    /// The staged-drops portion of `delta_v_mps` (m/s) — `0` when the
    /// plan is unbound, expended, or not the focused vessel's.
    pub delta_v_stages_mps: f64,
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
    /// Focused vessel id — the pin-aware resolution
    /// ([`FlightFocusOverride::resolve`]) the caller staged the edges
    /// against, not a raw `vessels.last()`.
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
    /// The pin-aware flight-focus vessel id (`FlightFocusOverride`'s
    /// [`resolve`](FlightFocusOverride::resolve) result, `VesselId.0`)
    /// — mirrored here each `flight_present` so the audio backend's
    /// continuous voices follow the SAME vessel the cues and HUD do,
    /// not `vessels.last()` (P07-F1's wrong-vessel-audio defect).
    /// `None` when no vessel is staged.
    pub focus_vessel: Option<u64>,
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
    /// `true` once binding proved AMBIGUOUS — two or more live
    /// vessels minted at/above `min_id` share the launch fingerprint
    /// (P05-F1: a second same-craft mint interposed while unbound,
    /// e.g. the plan's own spawn rejected and a scripted mint of the
    /// same craft landed instead). `(min_id, fingerprint)` cannot
    /// name the vessel honestly, so the plan stays unbound rather
    /// than latching a permanent wrong-vessel bind — `fire_stage`
    /// surfaces the notice and retires the plan.
    pub ambiguous: bool,
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
            ambiguous: false,
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
            ambiguous: false,
        }
    }

    /// The vessel this plan fires at, binding on first call: the
    /// UNIQUE vessel minted at-or-after [`Self::min_id`] whose stored
    /// craft matches the launch's fingerprint (P05-F1 — the old
    /// "newest match" rule permanently bound a wrong vessel whenever
    /// a second same-craft mint interposed while the plan was
    /// unbound). An unbound plan stays unbound (never caches `None`)
    /// so a not-yet-landed spawn binds on a later press; a bound id
    /// is returned verbatim — the caller re-checks it against the
    /// live arena (the vessel may be gone). Two or more matching
    /// candidates mean `(min_id, fingerprint)` cannot disambiguate:
    /// [`Self::ambiguous`] latches and the plan never binds.
    pub(crate) fn vessel_for(&mut self, sim: &World) -> Option<VesselId> {
        if let Some(v) = self.vessel {
            return Some(v);
        }
        if self.ambiguous {
            return None;
        }
        let mut candidates = sim.vessels().iter().filter(|(id, v)| {
            id.0 >= self.min_id
                && v.craft
                    .as_ref()
                    .is_some_and(|c| craft_fingerprint(c) == self.craft_key)
        });
        let found = candidates.next().map(|(id, _)| *id);
        if candidates.next().is_some() {
            // A second same-fingerprint mint at/above the bound —
            // which one is OURS is unanswerable. Latch the flag:
            // honest "cannot stage" beats a silent wrong-vessel fire.
            self.ambiguous = true;
            return None;
        }
        if found.is_some() {
            self.vessel = found;
        }
        found
    }
}

/// Every live launch's [`StagePlan`] — per-VESSEL keyed, not a single
/// slot: a second launch used to `insert_resource` over the first
/// craft's unfired stages, making them permanently unfireable and
/// pointing the HUD at the wrong craft. Each launch pushes its own
/// plan; they bind their own vessels independently through
/// [`StagePlan::vessel_for`]. Most-recent launch LAST, so
/// [`Self::active`] prefers the focused vessel's plan and falls back
/// to the newest plan — the single-plan behaviour is unchanged.
#[derive(Resource, Default)]
pub struct StagePlans {
    /// Live plans in launch order (oldest first). Expended or
    /// orphaned plans are retired out of the vec by `fire_stage`.
    pub plans: Vec<StagePlan>,
}

impl StagePlans {
    /// One-plan convenience for tests and scripted producers that
    /// bind a plan themselves (`StagePlan::bound_to`).
    pub fn single(plan: StagePlan) -> Self {
        Self { plans: vec![plan] }
    }

    /// Push a launch's plan — never discards an incumbent.
    pub fn push(&mut self, plan: StagePlan) {
        self.plans.push(plan);
    }

    /// The plan a stage press should fire: the plan bound to `focus`
    /// when one exists, else the most recent plan still holding
    /// stages (or simply the newest — an expended newest plan is
    /// still the honest "nothing left" report). `None` when empty.
    fn active_index(&mut self, sim: &World, focus: Option<VesselId>) -> Option<usize> {
        // Bind every unbound plan first so focus matching reads
        // resolved vessels, not pending fingerprints.
        for p in self.plans.iter_mut() {
            let _ = p.vessel_for(sim);
        }
        if let Some(f) = focus
            && let Some(i) = self.plans.iter().position(|p| p.vessel == Some(f))
        {
            return Some(i);
        }
        self.plans
            .iter()
            .rposition(|p| !p.stages.is_empty())
            .or(if self.plans.is_empty() {
                None
            } else {
                Some(self.plans.len() - 1)
            })
    }

    /// Read-only view of the plan [`Self::active_index`] selects —
    /// for the HUD mirror (no binding side-effects; unbound plans
    /// still count their stored stages).
    pub fn hud_plan(&self, focus: Option<VesselId>) -> Option<&StagePlan> {
        if let Some(f) = focus
            && let Some(p) = self.plans.iter().find(|p| p.vessel == Some(f))
        {
            return Some(p);
        }
        self.plans
            .iter()
            .rev()
            .find(|p| !p.stages.is_empty())
            .or(self.plans.last())
    }

    /// Fire the active plan's next stage at `sim`: resolves binding,
    /// returns `(vessel, ambiguous, refuse_msg)`; `refuse_msg`
    /// `Some("")` means "plan exists but nothing to fire" — the
    /// caller composes the cue from `total`.
    pub(crate) fn select_for_fire(
        &mut self,
        sim: &World,
        focus: Option<VesselId>,
    ) -> Option<(usize, Option<VesselId>, bool)> {
        let i = self.active_index(sim, focus)?;
        let p = &mut self.plans[i];
        let v = p.vessel_for(sim);
        let ambiguous = p.ambiguous;
        Some((i, v, ambiguous))
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
    /// `VesselId.0` → `(craft_fingerprint, vessel_parts output)` for
    /// the InBubble vessels — the same craft-identity key, reusing
    /// the per-part derivation (the `body_transforms` fold, the
    /// per-collider-vertex AABB fold, `classify_weld_part`) across
    /// frames instead of redoing it for a craft that only moves
    /// (C12-F3). Entries drop the frame the vessel leaves the
    /// bubble; a staging/jettison/breakup re-graph changes the
    /// fingerprint and re-derives.
    bubble_parts: BTreeMap<u64, (u64, Vec<FlightPart>)>,
}

/// One latched vessel's cached weld output — the render data
/// `capture_sync_input` would otherwise recompute every frame.
struct LatchedWeld {
    /// `craft_fingerprint` of the `(graph, parts)` this entry welded.
    key: u64,
    /// `WeldedBody::com_local` — `surface_pose_restore`'s CoM input.
    com_local: DVec3,
    /// Live plume emitters `(local_pos, local_dir, delivered)` — see
    /// [`FlightBody::plumes`]. A fresh baseline weld stages `1.0`.
    plumes: Vec<(DVec3, DVec3, f64)>,
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
pub fn capture_sync_input(
    sim: &World,
    cache: &mut LatchedWeldCache,
    art: &crate::art::ArtAssets,
) -> FlightSyncInput {
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
    cache.bubble_parts.retain(|vid, _| {
        matches!(
            sim.vessels().get(&VesselId(*vid)).map(|v| &v.state),
            Some(VesselState::InBubble(_))
        )
    });
    let mut vessels = Vec::new();
    let mut propulsion = Vec::new();
    for (id, vessel) in sim.vessels() {
        match &vessel.state {
            VesselState::InBubble(body) => {
                // The per-part render data is a pure function of the
                // craft topology — cache it on the same fingerprint
                // the latched path uses (C12-F3). Craft-less bodies
                // stage the cheap hull-AABB fallback uncached.
                let parts = match vessel.craft.as_ref() {
                    Some(craft) => {
                        let key = craft_fingerprint(craft);
                        let entry = cache.bubble_parts.entry(id.0).or_insert_with(|| {
                            (key, vessel_parts(Some(craft), &body.collider.hulls, art))
                        });
                        if entry.0 != key {
                            *entry = (key, vessel_parts(Some(craft), &body.collider.hulls, art));
                        }
                        entry.1.clone()
                    }
                    None => vessel_parts(None, &body.collider.hulls, art),
                };
                let plumes = live_plumes(vessel.craft.as_ref(), body);
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
                propulsion.push(vessel_propulsion(*id, vessel, Some(body)));
            }
            // §2.6.4 surface rails: a latched vessel is still *on the
            // pad* — stage its restored inertial pose so the parked
            // craft stays rendered and focusable (the throttle command
            // the HUD targets unlatches it). The restore needs the
            // environment; without one the record is uninterpretable —
            // skip honestly rather than guess a pose.
            VesselState::SurfaceLatched(pose) => {
                if let Some(body) = stage_latched(sim, env_ref, *id, vessel, pose, cache, art) {
                    vessels.push(body);
                    propulsion.push(vessel_propulsion(*id, vessel, None));
                }
            }
            VesselState::OnRails(_) => {}
        }
    }
    // Every non-debris vessel counts as ALIVE regardless of residence —
    // the staged list above carries only InBubble + latched, so an
    // OnRails vessel (a completed burn, a converted flight) must still
    // register here or the tutorial's `has_vessel` conflates "on
    // rails" with "destroyed" (P12-F2).
    let live_vessels = sim
        .vessels()
        .values()
        .filter(|v| v.debris_since.is_none())
        .count();
    FlightSyncInput {
        env,
        vessels,
        live_vessels,
        propulsion,
    }
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
    art: &crate::art::ArtAssets,
) -> Option<FlightBody> {
    let (Some(env), Some(craft)) = (env, vessel.craft.as_ref()) else {
        return None;
    };
    let key = craft_fingerprint(craft);
    if cache.entries.get(&id.0).is_none_or(|e| e.key != key) {
        let welded = csp_phys::weld(&craft.graph, &craft.parts).ok()?;
        // A fresh weld reads baseline `rated_n` on every site —
        // `live_plumes` stages each at `delivered = 1` (and drops a
        // baseline-dead site honestly). The parked craft's staged
        // `throttle` is 0, so the emitters only matter the frame a
        // throttle command unlatches it into the InBubble arm anyway.
        let plumes = live_plumes(vessel.craft.as_ref(), &welded);
        cache.entries.insert(
            id.0,
            LatchedWeld {
                key,
                com_local: welded.com_local,
                plumes,
                parts: vessel_parts(vessel.craft.as_ref(), &welded.collider.hulls, art),
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

/// One vessel's live plume emitters — `(local_pos, local_dir,
/// delivered)` per Thrust site the sim still rates. Every tick the
/// propellant bookkeeping rewrites a craft engine site's `rated_n` as
/// `base·factor·coverage` (the §3.4 reliability derate times the
/// pooled `min(1, avail/Σdemand)` coverage), so `rated_n > 0` is
/// exactly "this site can deliver thrust now": a starved or misfired
/// engine reads `0` and stages no emitter — the dry-but-throttled
/// craft that used to keep its exhaust lit shows nothing. A
/// craft-less (unmetered) body's sites never rescale, so they all
/// stage — no consumption model exists to starve them.
///
/// `delivered` is the site's live share of its baseline rating — the
/// factor·coverage product — so partial coverage shrinks the column
/// instead of leaving it full-size. The baseline lookup walks
/// `part_index`: each node's body site span indexes
/// `craft.parts[eff].force_sites` in order, the same fold the sim's
/// `apply_fuel_scales` writes through. Sites with no baseline
/// (unmapped, or a baseline `rated_n` of `0`) stage at `1.0`.
fn live_plumes(
    craft: Option<&csp_sim::CraftDef>,
    body: &csp_phys::WeldedBody,
) -> Vec<(DVec3, DVec3, f64)> {
    let mut base = vec![None; body.force_sites.len()];
    if let Some(craft) = craft {
        for (nid, w) in &body.part_index {
            let Some(part) = craft
                .graph
                .nodes
                .get(nid)
                .and_then(|n| craft.parts.get(n.effective_index as usize))
            else {
                continue;
            };
            let start = w.site_start as usize;
            let end = ((w.site_start + w.site_len) as usize).min(body.force_sites.len());
            for (j, slot) in base[start..end].iter_mut().enumerate() {
                *slot = part.force_sites.get(j).map(|s| s.rated_n);
            }
        }
    }
    body.force_sites
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            if s.kind != csp_phys::ForceSiteKind::Thrust
                || !s.rated_n.is_finite()
                || s.rated_n <= 0.0
            {
                return None;
            }
            let delivered = match base[i] {
                Some(b) if b > 0.0 => (s.rated_n / b).clamp(0.0, 1.0),
                _ => 1.0,
            };
            Some((s.local_pos, s.local_dir, delivered))
        })
        .collect()
}

/// Stage one vessel's propulsion walk table — the per-node mass,
/// engine, and live tank data `flight_present` turns into the
/// TWR/acceleration/burn-time/Δv readouts. `body` is the live welded
/// body for an InBubble vessel (its Thrust sites carry this tick's
/// base·factor·coverage ratings — the `thrust_live_n` figure); `None`
/// for a latched pose, where the parked craft still stages its
/// baseline walk rows but no live scaling exists to read.
///
/// The walk deliberately stages *baseline* site ratings
/// (`craft.parts[].force_sites`), not the body's live-scaled ones:
/// the sim zeros every site's `rated_n` on a closed throttle (the
/// coverage pool has no demand to normalize), so live ratings would
/// report zero engine capability exactly when the pilot needs the
/// number. The presentation-side walk recomputes coverage at the
/// commanded throttle — the same pooled `min(1, avail/Σdemand)`
/// accounting `propellant_plans` runs per tick.
fn vessel_propulsion(
    id: VesselId,
    vessel: &csp_sim::Vessel,
    body: Option<&csp_phys::WeldedBody>,
) -> VesselPropulsion {
    let mut nodes = Vec::new();
    if let Some(craft) = vessel.craft.as_ref() {
        for (nid, node) in &craft.graph.nodes {
            let Some(part) = craft.parts.get(node.effective_index as usize) else {
                continue;
            };
            let thrust_base_n: f64 = part
                .force_sites
                .iter()
                .filter(|s| s.kind == csp_phys::ForceSiteKind::Thrust)
                .map(|s| s.rated_n)
                .sum();
            let (isp_vac_s, propellants) = part.engine.as_ref().map_or_else(
                || (0.0, Vec::new()),
                |e| (e.isp_vac_s, e.propellants.clone()),
            );
            let tanks = part
                .tanks
                .iter()
                .map(|t| {
                    let units = vessel
                        .tank_contents
                        .get(nid)
                        .and_then(|m| m.get(&t.resource))
                        .copied()
                        .unwrap_or(0.0);
                    (t.resource.clone(), units, t.mass_per_unit_kg)
                })
                .collect();
            nodes.push(PropulsionNode {
                node: *nid,
                mass_kg: part.mass_kg,
                thrust_base_n,
                thrust_factor: vessel.reliability.get(nid).map_or(1.0, |r| r.thrust_factor),
                isp_vac_s,
                propellants,
                tanks,
            });
        }
    }
    VesselPropulsion {
        vessel: id.0,
        mass_kg: body.map_or_else(|| nodes.iter().map(|n| n.mass_kg).sum(), |b| b.mass_kg),
        thrust_live_n: body.map_or(0.0, |b| {
            b.force_sites
                .iter()
                .filter(|s| s.kind == csp_phys::ForceSiteKind::Thrust)
                .map(|s| s.rated_n)
                .sum()
        }),
        nodes,
    }
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
    art: &crate::art::ArtAssets,
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
                    model: None,
                    node: u64::MAX,
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
        let shape = crate::part_mesh::classify_weld_part(wp);
        let kind = part_kind(wp);
        // The weld view carries no def id — key the model by module
        // family; a pointed lathe profile upgrades Structural → nose cone.
        let model = art.model(crate::art::model_key(
            None,
            kind,
            crate::part_mesh::is_pointed(&shape),
        ));
        parts.push(FlightPart {
            lo,
            hi,
            transform: t,
            shape,
            kind,
            model,
            node: id.0,
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
pub(crate) fn sync_flight(
    staged: Res<FlightSyncInput>,
    mut snap: ResMut<FlightSnapshot>,
    mut live: ResMut<LiveVesselCount>,
    mut prop: ResMut<VesselPropulsionStaged>,
) {
    snap.env.clone_from(&staged.env);
    snap.vessels.clone_from(&staged.vessels);
    live.0 = staged.live_vessels;
    prop.0.clone_from(&staged.propulsion);
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
        // The POST-clamp eye–focus distance, not the requested one:
        // `vessel_shadow` recovers the craft as `eye − z_out·d`, so a
        // stale pre-clamp `input.distance_m` slid the blob shadow and
        // its altitude term off the hull whenever the terrain clamp
        // engaged (C12-F4). This matches the map convention
        // (`distance_m` = actual |eye − focus|) — the no-clamp path
        // reduces to `input.distance_m` exactly.
        distance_m: (eye - focus.com).norm(),
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

/// What a flight-mode right-click resolved — the part-action menu's
/// target (§3.7's in-context surface: the menu emits the same
/// `RunExperiment`/`DeployPart`/data/recovery commands the Program
/// window's SCIENCE section does).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlightPick {
    /// A named part — `PartNodeId.0`, the menu's single-part target.
    Part(u64),
    /// The vessel's hull — the click sat on the craft's screen bounds
    /// (or near it) but on no particular part; the menu lists every
    /// actionable part instead.
    Vessel,
}

/// Screen padding (points) around a part's projected AABB that still
/// counts as a hit — thin parts (antennae, chute packs) project a few
/// pixels wide, and a click on their silhouette should not miss.
const PICK_PART_PAD_PX: f32 = 4.0;
/// Forgiving-pick radius (points) around a part's projected center —
/// a click between projected AABBs still names the nearest part.
const PICK_NEAR_PX: f32 = 48.0;

/// Right-click part picking: project `focus`'s part collider AABBs
/// through the solved chase camera into `content` (egui points) and
/// hit-test `cursor`. The frontmost containing part wins (nearest
/// projected corner depth — the same camera-relative math the emit
/// pass uses); a near miss picks the closest projected part center;
/// a click inside the hull's bounding box but on nothing resolves
/// [`FlightPick::Vessel`] (the whole-vessel action list). `None` = a
/// total miss — the caller closes any open menu.
///
/// All presentation-side — the staged [`FlightBody`] transforms plus
/// the presentation camera, never the sim.
pub(crate) fn pick_part(
    cam: &FlightCameraF64,
    focus: Option<&FlightBody>,
    content: egui::Rect,
    cursor: egui::Pos2,
) -> Option<FlightPick> {
    let focus = focus?;
    if !cam.0.valid || focus.parts.is_empty() {
        return None;
    }
    let to_screen = |w: DVec3| -> Option<egui::Pos2> {
        project_into(&cam.0.view_proj, cam.0.eye, w).map(|ndc| {
            egui::pos2(
                content.left() + (ndc[0] * 0.5 + 0.5) * content.width(),
                content.top() + (0.5 - ndc[1] * 0.5) * content.height(),
            )
        })
    };
    let mut hull = egui::Rect::NOTHING;
    let mut contained: Option<(f64, u64)> = None; // (nearest-corner depth², node)
    let mut nearest: Option<(f32, u64)> = None; // (screen px, node)
    for p in &focus.parts {
        // The collider's body-frame AABB → 8 world corners → the part's
        // screen-space bounding rect. A part entirely behind the eye
        // projects nothing and is skipped.
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(8);
        let mut depth2 = f64::INFINITY;
        for x in [p.lo.x, p.hi.x] {
            for y in [p.lo.y, p.hi.y] {
                for z in [p.lo.z, p.hi.z] {
                    let w = focus.pos + focus.rot.rotate(DVec3::new(x, y, z));
                    depth2 = depth2.min((w - cam.0.eye).norm_squared());
                    if let Some(s) = to_screen(w) {
                        pts.push(s);
                    }
                }
            }
        }
        if pts.is_empty() {
            continue;
        }
        let rect = egui::Rect::from_points(&pts);
        hull = hull.union(rect);
        let center_w = focus.pos + focus.rot.rotate((p.lo + p.hi) * 0.5);
        if let Some(cs) = to_screen(center_w) {
            let d = cs.distance(cursor);
            if nearest.is_none_or(|(bd, _)| d < bd) {
                nearest = Some((d, p.node));
            }
        }
        if rect.expand(PICK_PART_PAD_PX).contains(cursor)
            && contained.is_none_or(|(bd, _)| depth2 < bd)
        {
            contained = Some((depth2, p.node));
        }
    }
    let resolve = |node: u64| {
        if node == u64::MAX {
            FlightPick::Vessel
        } else {
            FlightPick::Part(node)
        }
    };
    if let Some((_, node)) = contained {
        return Some(resolve(node));
    }
    if let Some((d, node)) = nearest
        && d <= PICK_NEAR_PX
    {
        return Some(resolve(node));
    }
    (hull.is_positive() && hull.expand(PICK_NEAR_PX).contains(cursor)).then_some(FlightPick::Vessel)
}

/// `Presentation` schedule — chase-camera solve + HUD mirrors + the
/// §3.7 SFX cue edges (the schedule's "VFX, audio cues" slot).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // the camera solve carries the terrain floor; per-vessel stage plans add the hud_plan read
pub(crate) fn flight_present(
    input: Res<FlightInput>,
    snap: Res<FlightSnapshot>,
    plans: Res<StagePlans>,
    prop: Res<VesselPropulsionStaged>,
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
    // THE focus: pin first, newest staged vessel otherwise — one
    // resolver for the camera, the HUD mirrors, and the SFX edges.
    // (`sfx_edges` used to baseline on `vessels.last()` alone, so a
    // recover_control pin tracked cues on the wrong vessel — P04-F4.)
    let focus = focus_override.resolve(&snap.vessels);
    // §3.7 edge detection on the staged snapshot — cue pushes and
    // TTL aging live in `sfx_edges` (kept out of this system under
    // the line budget). The edges baseline on the same resolved
    // focus the camera follows.
    ui.sfx = sfx_edges(&mut sfx, &snap.vessels, ui.debris_count, focus);
    // The audio backend's focus mirror: voices key off this, not a
    // `vessels.last()` of their own (P07-F1).
    sfx.focus_vessel = focus.map(|f| f.id);
    let Some(focus) = focus else {
        cam.0.valid = false;
        ui.has_vessel = false;
        ui.focus = None;
        ui.propellant.clear();
        ui.stages_left = 0;
        ui.stages_total = plans.hud_plan(None).map_or(0, |p| p.total);
        stage_propulsion(&mut ui, None, &[], 0.0, None, None);
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
    let hud = plans.hud_plan(Some(VesselId(focus.id)));
    ui.stages_left = hud.map_or(0, |p| p.stages.len());
    ui.stages_total = hud.map_or(0, |p| p.total);
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
    // Weight figures need the environment's μ against the live radius;
    // without one the propulsion readouts stay honest ("—").
    let mut g_local = None;
    let mut g_surface = None;
    if let Some(env) = snap.env.as_ref() {
        if env.radius_m > 0.0 {
            g_surface = Some(env.mu_m3_per_s2 / (env.radius_m * env.radius_m));
        }
        let rel = focus.com - env.body_center;
        let r = rel.norm();
        if r > 0.0 {
            g_local = Some(env.mu_m3_per_s2 / (r * r));
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
    // Propulsion readouts — the staged walk table for the focused
    // vessel. The plan's remaining drop sets apply only while it is
    // bound to this vessel (a newer spawn or a fragment can hold the
    // focus without owning the plan — `vessel_for`'s binding rule).
    let drops: &[Vec<csp_phys::PartNodeId>] = plans
        .plans
        .iter()
        .find(|p| p.vessel == Some(VesselId(focus.id)))
        .map_or(&[][..], |p| &p.stages[..]);
    stage_propulsion(
        &mut ui,
        prop.0.iter().find(|p| p.vessel == focus.id),
        drops,
        focus.throttle,
        g_local,
        g_surface,
    );
}

/// The §3.7 edge detector — the named transitions (ignition/cut,
/// jettison, contact, latch, breakup, removal) detected on the staged
/// snapshot, folded into [`FlightSfx`] cue pushes plus the TTL
/// age-out, and the live stack mirrored for the HUD. Per-focus edges
/// apply only while the SAME vessel stays focused; a focus switch
/// re-baselines instead of firing a spurious cut/contact edge. The
/// `focus` is the caller's pin-aware resolution
/// ([`FlightFocusOverride::resolve`]) — pin target when staged, else
/// the newest staged vessel — so edges track the vessel the player
/// actually controls, not whichever staged last (P04-F4).
fn sfx_edges(
    sfx: &mut FlightSfx,
    vessels: &[FlightBody],
    debris_count: usize,
    focus: Option<&FlightBody>,
) -> Vec<String> {
    let cur = focus.map(|f| SfxPrev {
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

/// The propulsion walk's mutable node state — the burn loop shrinks
/// live mass as it draws contents.
struct WalkNode {
    /// Live mass (kg) — dry + remaining contents.
    mass_kg: f64,
    /// Σ Thrust rating at base·reliability-factor (N) — pre-coverage.
    thrust_n: f64,
    /// Vacuum Isp (s) — `0` on an unmetered node (real thrust, no
    /// consumption model; counted for TWR/accel, never for Δv).
    isp_s: f64,
    /// `(resource, mass fraction)` consumption split — empty unmetered.
    propellants: Vec<(csp_data::ResourceId, f64)>,
    /// `resource → units` remaining on this node.
    contents: BTreeMap<csp_data::ResourceId, f64>,
}

/// One attached-set burn evaluation at throttle `θ` — the shared core
/// of the TWR/acceleration/burn-time/Δv readouts.
struct BurnStep {
    /// Deliverable Σ thrust at `θ` (N) — metered and unmetered engines.
    thrust_n: f64,
    /// Mass-flow-weighted vacuum Isp over the metered engines (s);
    /// `0` when none deliver.
    isp_s: f64,
    /// Σ propellant mass flow at `θ` (kg/s).
    mass_flow_kg_s: f64,
    /// Seconds until the limiting pooled propellant empties at `θ` —
    /// `INFINITY` when nothing draws (idle throttle, dry tanks).
    burn_s: f64,
    /// Engines want propellant (`demand` non-empty) but nothing flows
    /// — the tanks are out *now*. The HUD's depletion readout renders
    /// `0:00` instead of an ambiguous "—" (a misfired engine carries
    /// `thrust_factor = 0` → no demand → still "—", correctly).
    deprived: bool,
    /// `(resource, units)` the burn draws — the walk's debit list.
    drawn: Vec<(csp_data::ResourceId, f64)>,
    /// Propellant mass the burn consumes (kg).
    burned_kg: f64,
}

/// Pooled-coverage burn accounting over the attached set — the same
/// `min(1, avail / Σ demand)` model the sim's `propellant_plans`
/// normalizes per tick, evaluated on the staged walk table at throttle
/// `θ`. Unmetered nodes (`EngineSpec: None`) deliver `base·factor·θ`
/// and claim nothing — the sim's free-thrust convention.
fn burn_step(
    attached: &BTreeMap<csp_phys::PartNodeId, WalkNode>,
    mpu: &BTreeMap<csp_data::ResourceId, f64>,
    throttle: f64,
) -> BurnStep {
    let mut step = BurnStep {
        thrust_n: 0.0,
        isp_s: 0.0,
        mass_flow_kg_s: 0.0,
        burn_s: f64::INFINITY,
        deprived: false,
        drawn: Vec::new(),
        burned_kg: 0.0,
    };
    let theta = throttle.clamp(0.0, 1.0);
    if theta <= 0.0 {
        // A closed throttle draws nothing — the sim's coverage pool
        // resolves empty and every metered site reads zero.
        return step;
    }
    // Pooled availability (units) and full-throttle demand (units/s)
    // per resource — the demand pool counts every claimant's needs,
    // starved or not (the sim's shared-denominator rule).
    let mut avail: BTreeMap<&csp_data::ResourceId, f64> = BTreeMap::new();
    let mut demand: BTreeMap<&csp_data::ResourceId, f64> = BTreeMap::new();
    for n in attached.values() {
        for (r, u) in &n.contents {
            *avail.entry(r).or_default() += u;
        }
        if n.isp_s > 0.0 && n.thrust_n > 0.0 {
            for (r, frac) in &n.propellants {
                let need = match mpu.get(r) {
                    Some(&m) if m > 0.0 => n.thrust_n * frac / (n.isp_s * csp_data::G0 * m),
                    // Un-holdable (or massless) propellant: infinite
                    // demand → zero coverage — the sim's starvation rule.
                    _ => f64::INFINITY,
                };
                if need > 0.0 {
                    *demand.entry(r).or_default() += need * theta;
                }
            }
        }
    }
    let cov_of = |r: &csp_data::ResourceId| -> f64 {
        let d = demand.get(r).copied().unwrap_or(0.0);
        if d.is_finite() && d > 0.0 {
            (avail.get(r).copied().unwrap_or(0.0) / d).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    // Per-engine thrust at `θ` + per-resource draw (units/s).
    let mut metered_f = 0.0;
    let mut metered_f_over_isp = 0.0;
    let mut flow: BTreeMap<&csp_data::ResourceId, f64> = BTreeMap::new();
    for n in attached.values() {
        if n.thrust_n <= 0.0 {
            continue;
        }
        if n.propellants.is_empty() || n.isp_s <= 0.0 {
            step.thrust_n += n.thrust_n * theta; // unmetered site
            continue;
        }
        let mut cov = 1.0f64;
        for (r, frac) in &n.propellants {
            if *frac > 0.0 {
                cov = cov.min(cov_of(r));
            }
        }
        let f = n.thrust_n * cov * theta;
        if f <= 0.0 {
            continue; // starved — contributes no thrust and draws nothing
        }
        step.thrust_n += f;
        metered_f += f;
        metered_f_over_isp += f / n.isp_s;
        for (r, frac) in &n.propellants {
            // `f > 0` already proved this resource's coverage finite,
            // so its mpu resolved — the lookup still degrades to 0
            // honestly rather than trusting the implication.
            let Some(&m) = mpu.get(r) else { continue };
            if m > 0.0 {
                *flow.entry(r).or_default() += f * frac / (n.isp_s * csp_data::G0 * m);
            }
        }
    }
    if metered_f_over_isp > 0.0 {
        step.isp_s = metered_f / metered_f_over_isp;
    }
    for (r, units_s) in &flow {
        if *units_s > 0.0 {
            let a = avail.get(r).copied().unwrap_or(0.0);
            step.burn_s = step.burn_s.min(a / units_s);
            step.mass_flow_kg_s += units_s * mpu.get(*r).copied().unwrap_or(0.0);
        }
    }
    if step.burn_s.is_finite() {
        for (r, units_s) in &flow {
            let draw = (units_s * step.burn_s).min(avail.get(r).copied().unwrap_or(0.0));
            if draw > 0.0 {
                step.burned_kg += draw * mpu.get(*r).copied().unwrap_or(0.0);
                step.drawn.push(((*r).clone(), draw));
            }
        }
    } else {
        // Demand exists but no flow — every metered engine is fuel-
        // starved right now: the limiting pool is already empty.
        step.deprived = !demand.is_empty();
    }
    step
}

/// Debit one burn's `drawn` list out of the attached set — ascending
/// `PartNodeId`, the fixed drain order every sim pass shares. Node
/// mass follows the drawn units' mass-per-unit.
fn consume_drawn(
    attached: &mut BTreeMap<csp_phys::PartNodeId, WalkNode>,
    mpu: &BTreeMap<csp_data::ResourceId, f64>,
    drawn: &[(csp_data::ResourceId, f64)],
) {
    for (r, units) in drawn {
        let mut rem = *units;
        let m = mpu.get(r).copied().unwrap_or(0.0);
        for n in attached.values_mut() {
            if rem <= 0.0 {
                break;
            }
            let Some(have) = n.contents.get_mut(r) else {
                continue;
            };
            let take = (*have).min(rem);
            *have -= take;
            n.mass_kg -= take * m;
            rem -= take;
        }
    }
}

/// The staged table → mutable walk state (`BTreeMap` keeps the
/// ascending-`PartNodeId` drain order) plus the resource→mpu map the
/// kg↔units conversions read.
fn build_walk(
    nodes: &[PropulsionNode],
) -> (
    BTreeMap<csp_phys::PartNodeId, WalkNode>,
    BTreeMap<csp_data::ResourceId, f64>,
) {
    let attached = nodes
        .iter()
        .map(|n| {
            (
                n.node,
                WalkNode {
                    mass_kg: n.mass_kg,
                    thrust_n: n.thrust_base_n * n.thrust_factor,
                    isp_s: n.isp_vac_s,
                    propellants: n.propellants.clone(),
                    contents: n.tanks.iter().map(|(r, u, _)| (r.clone(), *u)).collect(),
                },
            )
        })
        .collect();
    let mut mpu: BTreeMap<csp_data::ResourceId, f64> = BTreeMap::new();
    for n in nodes {
        for (r, _, m) in &n.tanks {
            mpu.entry(r.clone()).or_insert(*m);
        }
    }
    (attached, mpu)
}

/// The propulsion half of the HUD mirrors — TWR (current + surface),
/// thrust acceleration, mass flow, time-to-depletion, and Δv
/// remaining, all off the staged [`VesselPropulsion`] table plus the
/// launch plan's remaining drop sets. `g_local`/`g_surface` are the
/// environment-derived gravities (`None` with no env → the weight
/// figures honestly go blank). Every number derives from the live
/// staged state each Presentation frame — never a launch-time cache.
fn stage_propulsion(
    ui: &mut FlightUiData,
    prop: Option<&VesselPropulsion>,
    drops: &[Vec<csp_phys::PartNodeId>],
    throttle: f64,
    g_local: Option<f64>,
    g_surface: Option<f64>,
) {
    ui.mass_kg = 0.0;
    ui.thrust_n = 0.0;
    ui.thrust_full_n = 0.0;
    ui.twr = None;
    ui.twr_surface = None;
    ui.accel_mps2 = None;
    ui.accel_gee = None;
    ui.isp_s = None;
    ui.mass_flow_kg_s = None;
    ui.fuel_depletion_s = None;
    ui.burn_full_s = None;
    ui.delta_v_mps = None;
    ui.delta_v_stages_mps = 0.0;
    let Some(prop) = prop else { return };
    ui.mass_kg = prop.mass_kg;
    let (mut attached, mpu) = build_walk(&prop.nodes);
    let now = burn_step(&attached, &mpu, throttle);
    let full = burn_step(&attached, &mpu, 1.0);
    // A crafted vessel's thrust is the recomputed coverage figure;
    // a craft-less one (hand-authored debug body) falls back to the
    // live site sum the sim carries.
    let (thrust_now, thrust_full) = if attached.is_empty() {
        (prop.thrust_live_n * throttle, prop.thrust_live_n)
    } else {
        (now.thrust_n, full.thrust_n)
    };
    ui.thrust_n = thrust_now;
    ui.thrust_full_n = thrust_full;
    if prop.mass_kg > 0.0 {
        ui.accel_mps2 = Some(thrust_now / prop.mass_kg);
        ui.accel_gee = Some(thrust_now / (prop.mass_kg * csp_data::G0));
        if let Some(g) = g_local.filter(|g| *g > 0.0) {
            ui.twr = Some(thrust_now / (prop.mass_kg * g));
        }
        if let Some(g) = g_surface.filter(|g| *g > 0.0) {
            ui.twr_surface = Some(thrust_full / (prop.mass_kg * g));
        }
    }
    let metered = attached
        .values()
        .any(|n| n.isp_s > 0.0 && n.thrust_n > 0.0 && !n.propellants.is_empty());
    if !metered {
        return;
    }
    ui.isp_s = (full.isp_s > 0.0).then_some(full.isp_s);
    ui.mass_flow_kg_s = Some(now.mass_flow_kg_s);
    ui.fuel_depletion_s = if now.burn_s.is_finite() {
        Some(now.burn_s)
    } else {
        now.deprived.then_some(0.0)
    };
    ui.burn_full_s = if full.burn_s.is_finite() {
        Some(full.burn_s)
    } else {
        full.deprived.then_some(0.0)
    };
    let (dv, dv_staged) = delta_v_walk_from(&mut attached, &mpu, drops);
    ui.delta_v_mps = Some(dv + dv_staged);
    ui.delta_v_stages_mps = dv_staged;
}

/// [`delta_v_walk`] over a pre-built attached set — `stage_propulsion`
/// constructs the walk table once for both its burn evaluations and
/// the Δv pass.
fn delta_v_walk_from(
    attached: &mut BTreeMap<csp_phys::PartNodeId, WalkNode>,
    mpu: &BTreeMap<csp_data::ResourceId, f64>,
    drops: &[Vec<csp_phys::PartNodeId>],
) -> (f64, f64) {
    let mut current = 0.0;
    let mut staged = 0.0;
    for step in 0..=drops.len() {
        // Burn the attached set to depletion — each pass drains at
        // least one pooled resource, so a few iterations cover
        // multi-resource craft; the bound is defence, not a limit.
        for _ in 0..64 {
            let b = burn_step(attached, mpu, 1.0);
            if !(b.burn_s.is_finite() && b.burn_s > 0.0 && b.isp_s > 0.0 && b.burned_kg > 0.0) {
                break;
            }
            let wet: f64 = attached.values().map(|n| n.mass_kg).sum();
            let dry_end = (wet - b.burned_kg).max(1e-9);
            let ratio = wet / dry_end;
            if ratio > 1.0 {
                let dv = b.isp_s * csp_data::G0 * ratio.ln();
                if step == 0 {
                    current += dv;
                } else {
                    staged += dv;
                }
            }
            consume_drawn(attached, mpu, &b.drawn);
        }
        if step < drops.len() {
            for id in &drops[step] {
                attached.remove(id);
            }
        }
    }
    (current, staged)
}

/// `DVec3` → f32 triple — the camera-relative world position and
/// world normal channels of the lit vertex format.
fn f3(v: DVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

/// An unlit-stream vertex — sky quads, plumes, the plasma sheath, the
/// contact-dust ring: `nrm` ≈ 0 marks the fragment-shader passthrough
/// so `color` lands unchanged.
fn unlit_v(pos: [f32; 3], color: [f32; 4]) -> csp_render::flight::FlightVertex {
    csp_render::flight::FlightVertex {
        pos,
        world: [0.0; 3],
        nrm: [0.0; 3],
        color,
        mat: [0.0; 3],
    }
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
        v.push(unlit_v(pos, color));
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
        v.push(unlit_v(pos, color));
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
    for (site, (p0, dir, delivered)) in b.plumes.iter().enumerate() {
        // Delivered-thrust fraction: the throttle command times the
        // site's live share of rated force (factor·coverage — see
        // [`FlightBody::plumes`]). A partially starved or derated
        // engine reads a shorter, dimmer column; a fully dry one
        // never staged an emitter at all.
        let drive = (throttle * *delivered).clamp(0.0, 1.0);
        if drive <= 0.0 {
            continue;
        }
        let dir_w = {
            let d = b.rot.rotate(*dir);
            d / d.norm()
        };
        let tip = b.pos + b.rot.rotate(*p0);
        let len = 4.5 + 22.0 * drive;
        let exit_r = 0.30 + 0.38 * drive;
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
                        (a_gain * fade * (0.25 + 0.75 * drive)) as f32,
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

/// Per-kind material terms for the kitbash fallback — `[spec strength,
/// spec exponent, occlusion]`: engines and structural parts shade as
/// bare/machined metal, tanks and pods as painted hull, batteries and
/// ports as duller hardware. (The real models carry their own baked
/// per-material terms — this table is the procedural tier's stand-in.)
fn kind_mat(kind: PartKind) -> [f32; 3] {
    match kind {
        PartKind::Engine => [0.62, 56.0, 1.0],
        PartKind::Crew => [0.55, 64.0, 1.0],
        PartKind::Tank => [0.38, 40.0, 1.0],
        PartKind::Battery => [0.45, 36.0, 1.0],
        PartKind::Science => [0.50, 44.0, 1.0],
        PartKind::Port => [0.42, 36.0, 1.0],
        PartKind::Structural => [0.52, 48.0, 1.0],
    }
}

/// Emit one part's real baked model (procedural parts.csar) — fitted to the
/// collider AABB. The vertex stream carries the authored albedo, the
/// smoothing-wedge normal, and the baked material terms; per-pixel
/// lighting happens in the fragment shader under `packet.light`.
/// Debris chars the albedo and kills the specular sheen.
#[allow(clippy::too_many_arguments)]
fn emit_model_part(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    b: &FlightBody,
    p: &FlightPart,
    m: &crate::art::PartModel,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    mtris: &mut Vec<crate::part_mesh::ModelTri>,
) {
    let (mlo, mhi) = p.shape.local_aabb();
    mtris.clear(); // scratch — the sibling kitbash path reuses `tris` the same way (C12-F3)
    crate::part_mesh::emit_model(m, mlo, mhi, mtris);
    for t in mtris.iter() {
        let world = [t.a, t.b, t.c].map(|q| b.pos + b.rot.rotate(p.transform.apply(q)));
        let [Some(c0), Some(c1), Some(c2)] = world.map(|q| project_into(vp, eye, q)) else {
            continue;
        };
        for k in 0..3 {
            let (nrm, alb, mt) = [(t.na, t.ca, t.ma), (t.nb, t.cb, t.mb), (t.nc, t.cc, t.mc)][k];
            let n_w = b.rot.rotate(p.transform.apply_dir(nrm));
            let rel = world[k] - eye;
            let (color, mat) = if b.debris {
                (
                    [alb[0] * 0.72, alb[1] * 0.38, alb[2] * 0.16, alb[3]],
                    [0.0, mt[1], mt[2]],
                )
            } else {
                (alb, mt)
            };
            v.push(csp_render::flight::FlightVertex {
                pos: [c0, c1, c2][k],
                world: f3(rel),
                nrm: f3(n_w),
                color,
                mat,
            });
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
    sites: Res<crate::structures::SiteTable>,
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
            clock.0.ticks() as f64 / csp_core::TICK_HZ as f64,
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
            v.push(unlit_v(pos, color));
        }
    }
    if !cam.0.valid {
        frame.0 = Some(packet);
        return;
    }
    let vp = &cam.0.view_proj;
    let eye = cam.0.eye;

    // The space center: pad/mount, gantry, VAB, crawlerway, propellant
    // farm and support buildings at the authored launch site — lit
    // geometry sharing the frame's staged sun via `packet.light`, with
    // unlit contact-shadow quads, anchored to the same rendered
    // terrain the craft rests on (the extract's `packet.terrain` gate
    // is exactly "env + solved camera + flight mode").
    if packet.terrain.is_some()
        && let Some(env) = &snap.env
        && let Some(body) = &env.body
        && let Some(site) = sites.launch_site(body)
    {
        crate::structures::emit_launch_complex(
            v,
            vp,
            eye,
            env,
            site,
            &terrain_cfg,
            &mut terrain_cache,
        );
    }

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
    // The part-lighting uniform for this frame: the staged sun plus a
    // hemisphere ambient keyed to the focus vessel's local up — sky
    // bounce above it, dim ground bounce below — so hull shading reads
    // like the scene the terrain pass draws (same sun direction).
    packet.light = {
        let up = snap
            .env
            .as_ref()
            .and_then(|e| {
                snap.vessels.first().map(|b| {
                    let d = b.com - e.body_center;
                    let l = d.norm();
                    if l > 1.0 { d / l } else { DVec3::Z }
                })
            })
            .unwrap_or(DVec3::Z);
        // C11-F2 — couple the craft's sun to the atmosphere: the same
        // T(h, μ) the terrain pass applies, evaluated on the focus
        // vessel's own altitude and sun-zenith angle. The old constant
        // [0.95,0.92,0.85] left the hull glowing identically in thick
        // air, at sunset, and in vacuum; now it reddens/dims with the
        // local transmittance exactly like the ground under it.
        // The base color mirrors fs_terrain's `sun_lum` ([1.0,0.98,0.92]).
        let sun_col = snap.env.as_ref().map_or([1.0, 0.98, 0.92], |e| {
            let alt = snap
                .vessels
                .first()
                .map(|b| ((b.com - e.body_center).norm() - e.radius_m).max(0.0))
                .unwrap_or(0.0);
            if e.atm_top_m <= 0.0 {
                [1.0, 0.98, 0.92] // vacuum — full sun
            } else {
                let t = csp_render::terrain::transmittance_to_space(
                    csp_render::terrain::AtmoDesc {
                        radius_m: e.radius_m,
                        top_m: e.atm_top_m,
                        scale_height_m: e.atm_scale_height_m.max(1.0),
                    },
                    csp_render::terrain::RAYLEIGH_BETA,
                    alt,
                    sun.dot(up),
                );
                [t[0], 0.98 * t[1], 0.92 * t[2]]
            }
        });
        csp_render::PartLight {
            sun_dir: [sun.x as f32, sun.y as f32, sun.z as f32, 0.0],
            sun_col: [sun_col[0], sun_col[1], sun_col[2], 0.0],
            amb_up: [0.16, 0.20, 0.27, 0.0],
            amb_dn: [0.065, 0.060, 0.055, 0.0],
            hemi: [up.x as f32, up.y as f32, up.z as f32, 0.10],
        }
    };
    let mut tris = Vec::new();
    let mut mtris = Vec::new();
    for b in &snap.vessels {
        for p in &b.parts {
            let base: [f32; 3] = if b.debris {
                [0.72, 0.38, 0.16]
            } else {
                kind_tint(p.kind)
            };
            let (lo, hi) = p.shape.local_aabb();
            let z_span = (hi.z - lo.z).max(1e-9);
            if let Some(m) = &p.model {
                emit_model_part(v, b, p, m, vp, eye, &mut mtris);
                continue;
            }
            tris.clear();
            crate::part_mesh::emit_smooth(&p.shape, &mut tris);
            let mat = if b.debris {
                [0.0, 32.0, 1.0]
            } else {
                kind_mat(p.kind)
            };
            for t in tris.iter() {
                // A tri with any corner behind the eye plane drops
                // whole — the same all-or-nothing rule the VAB packet
                // uses — rather than emitting a corrupted fan.
                let world = [t.a, t.b, t.c].map(|q| b.pos + b.rot.rotate(p.transform.apply(q)));
                let [Some(c0), Some(c1), Some(c2)] = world.map(|q| project_into(vp, eye, q)) else {
                    continue;
                };
                for (k, &(local, nrm)) in [(t.a, t.na), (t.b, t.nb), (t.c, t.nc)].iter().enumerate()
                {
                    let n_w = b.rot.rotate(p.transform.apply_dir(nrm));
                    let u = (local.z - lo.z) / z_span;
                    let tint = if b.debris {
                        base
                    } else {
                        livery(p.kind, base, u)
                    };
                    v.push(csp_render::flight::FlightVertex {
                        pos: [c0, c1, c2][k],
                        world: f3(world[k] - eye),
                        nrm: f3(n_w),
                        color: [tint[0], tint[1], tint[2], 1.0],
                        mat,
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
            // Puff sits a hair above the LOCAL terrain surface — the
            // baked heightfield's floor radius under `dir`, not the
            // bare datum: a datum-anchored ring buries under up to
            // `amplitude_m` of positive relief and floats over dips
            // (P07-F2/P08-F3; `surface_floor_radius` is the same tap
            // the camera floor uses).
            let floor =
                crate::terrain::surface_floor_radius(&terrain_cfg, &mut terrain_cache, env, dir);
            let base = env.body_center + dir * (floor.max(env.radius_m) + 0.3);
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
            cursor_px: None,
        });
        world.insert_resource(snap);
        world.insert_resource(FlightCameraF64(cam));
        world.insert_resource(TerrainConfig::default());
        world.insert_resource(TerrainCache::default());
        world.insert_resource(crate::structures::SiteTable::default());
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
                model: None,
                node: 0,
            }],
            // Thrust site at the body origin, dir +Z: the fan tip sits
            // at the vessel, the 1.5 m-radius base ring 36 m along −Z.
            plumes: vec![(DVec3::ZERO, DVec3::Z, 1.0)],
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
        let parts = vessel_parts(None, &[hull], &Default::default());
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

    /// A [`PropulsionNode`] fixture — one graph node's staged
    /// mass/engine/tank row.
    fn prop_node(
        id: u64,
        mass_kg: f64,
        thrust_n: f64,
        isp_s: f64,
        mix: &[(&str, f64)],
        tanks: &[(&str, f64, f64)],
    ) -> PropulsionNode {
        PropulsionNode {
            node: csp_phys::PartNodeId(id),
            mass_kg,
            thrust_base_n: thrust_n,
            thrust_factor: 1.0,
            isp_vac_s: isp_s,
            propellants: mix
                .iter()
                .map(|(r, f)| (csp_data::ResourceId::from(*r), *f))
                .collect(),
            tanks: tanks
                .iter()
                .map(|(r, u, m)| (csp_data::ResourceId::from(*r), *u, *m))
                .collect(),
        }
    }

    /// The HUD's live figures on a pod + tank + engine vessel: the
    /// burn time, Δv, and TWR all land on the hand-computed values —
    /// and a closed throttle honestly reports "—" for the burn while
    /// keeping the surface-TWR pad figure.
    #[test]
    fn propulsion_walk_reports_real_numbers() {
        // 100 u fuel @ 5 kg/u = 500 kg; 1 kN engine, isp 300 s;
        // wet 710 kg → ṁ = 1/(300·g₀) kN⁻¹… ṁ = F/(isp·g₀) kg/s.
        let g0 = csp_data::G0;
        let prop = VesselPropulsion {
            vessel: 1,
            mass_kg: 710.0,
            thrust_live_n: 1000.0,
            nodes: vec![
                prop_node(1, 60.0, 0.0, 0.0, &[], &[]),
                prop_node(2, 600.0, 0.0, 0.0, &[], &[("fuel", 100.0, 5.0)]),
                prop_node(3, 50.0, 1000.0, 300.0, &[("fuel", 1.0)], &[]),
            ],
        };
        let mut ui = FlightUiData::default();
        stage_propulsion(&mut ui, Some(&prop), &[], 1.0, Some(9.81), Some(9.81));
        let mdot = 1000.0 / (300.0 * g0);
        assert!((ui.mass_flow_kg_s.unwrap() - mdot).abs() < 1e-9);
        // burn = propellant_kg / ṁ = 500/ṁ ≈ 1471 s.
        assert!((ui.fuel_depletion_s.unwrap() - 500.0 / mdot).abs() < 1e-6);
        // Δv = isp·g₀·ln(710/210) ≈ 3583 m/s.
        let dv = 300.0 * g0 * (710.0_f64 / 210.0).ln();
        assert!((ui.delta_v_mps.unwrap() - dv).abs() < 1e-6);
        assert_eq!(ui.delta_v_stages_mps, 0.0);
        // TWR = F/(m·g) at both readouts (g = 9.81 staged both ways).
        let twr = 1000.0 / (710.0 * 9.81);
        assert!((ui.twr.unwrap() - twr).abs() < 1e-9);
        assert!((ui.twr_surface.unwrap() - twr).abs() < 1e-9);
        assert!((ui.accel_mps2.unwrap() - 1000.0 / 710.0).abs() < 1e-9);
        assert!((ui.isp_s.unwrap() - 300.0).abs() < 1e-9);

        // Half throttle: thrust halves, depletion doubles — the
        // same live-state arithmetic the tick's coverage pool runs.
        stage_propulsion(&mut ui, Some(&prop), &[], 0.5, Some(9.81), Some(9.81));
        assert!((ui.thrust_n - 500.0).abs() < 1e-9);
        assert!((ui.fuel_depletion_s.unwrap() - 1000.0 / mdot).abs() < 1e-6);

        // Closed throttle: the burn figures go "—" but the surface
        // TWR pad figure stays (it is a capability, not a burn).
        stage_propulsion(&mut ui, Some(&prop), &[], 0.0, Some(9.81), Some(9.81));
        assert_eq!(ui.thrust_n, 0.0);
        assert!(ui.fuel_depletion_s.is_none());
        assert!((ui.twr_surface.unwrap() - twr).abs() < 1e-9);
    }

    /// Dry tanks with the throttle open read `0:00` (deprived), not
    /// an ambiguous "—"; a spent drop-set feeds the staged-Δv figure.
    #[test]
    fn propulsion_walk_marks_depletion_and_stage_drops() {
        let mut dry = VesselPropulsion {
            vessel: 1,
            mass_kg: 710.0,
            thrust_live_n: 0.0,
            nodes: vec![
                prop_node(1, 60.0, 0.0, 0.0, &[], &[]),
                prop_node(2, 100.0, 0.0, 0.0, &[], &[("fuel", 0.0, 5.0)]),
                prop_node(3, 50.0, 1000.0, 300.0, &[("fuel", 1.0)], &[]),
                // Upper stage: own tank + engine still attached.
                prop_node(4, 540.0, 0.0, 0.0, &[], &[("fuel", 100.0, 5.0)]),
                prop_node(5, 20.0, 400.0, 350.0, &[("fuel", 1.0)], &[]),
            ],
        };
        let mut ui = FlightUiData::default();
        // The pooled fuel on node 4 still feeds both engines — the
        // v1 crossfeed model. Burn drains it; the drop set then cuts
        // nodes 2+3's dead mass — but nothing remains to burn, so the
        // staged share is 0 (the conservative, sim-literal figure).
        stage_propulsion(
            &mut ui,
            Some(&dry),
            &[vec![csp_phys::PartNodeId(2), csp_phys::PartNodeId(3)]],
            1.0,
            Some(9.81),
            Some(9.81),
        );
        assert!(ui.delta_v_mps.unwrap() > 0.0);
        // Now with node 4's tank dry too: the depletion readout is
        // "0:00" — tanks are out NOW, not merely unreportable.
        for n in &mut dry.nodes {
            for t in &mut n.tanks {
                t.1 = 0.0;
            }
        }
        stage_propulsion(&mut ui, Some(&dry), &[], 1.0, Some(9.81), Some(9.81));
        assert_eq!(ui.fuel_depletion_s, Some(0.0));
        assert_eq!(ui.delta_v_mps, Some(0.0));
        assert_eq!(ui.thrust_n, 0.0);
    }

    /// The right-click pick: the frontmost part under the cursor
    /// names its node, a near miss picks the closest projected
    /// center, a hull-edge click resolves the whole vessel, and a
    /// true miss returns `None` (the caller closes the menu).
    #[test]
    fn pick_part_names_the_clicked_node() {
        let mk = |node: u64, lo: DVec3, hi: DVec3| FlightPart {
            node,
            lo,
            hi,
            transform: csp_phys::PartTransform::IDENTITY,
            shape: crate::part_mesh::PartShape::Box { lo, hi },
            kind: PartKind::Structural,
            model: None,
        };
        let vessel = FlightBody {
            id: 0,
            pos: DVec3::ZERO,
            rot: DQuat::IDENTITY,
            vel: DVec3::ZERO,
            throttle: 0.0,
            debris: false,
            contact: false,
            plasma_w_m2: 0.0,
            nodes: 2,
            // Node 1 straddles the origin (screen center); node 2
            // sits 3 m above it (up-screen).
            parts: vec![
                mk(1, DVec3::new(-1.0, -1.0, -1.0), DVec3::new(1.0, 1.0, 1.0)),
                mk(2, DVec3::new(-1.0, 2.0, -1.0), DVec3::new(1.0, 4.0, 1.0)),
            ],
            plumes: Vec::new(),
            com: DVec3::ZERO,
            latched: false,
            tanks: Vec::new(),
        };
        // Eye on −Z looking back at the origin (the established
        // `solve_orbit_camera` convention — see the plume test above).
        let cam = FlightCameraF64(crate::map::solve_orbit_camera(
            0.0,
            core::f64::consts::PI,
            36.0,
            (320.0, 240.0),
            DVec3::ZERO,
            FLIGHT_FOV_Y,
            0.1,
        ));
        let content = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 240.0));
        let to_screen = |w: DVec3| {
            let ndc = project_into(&cam.0.view_proj, cam.0.eye, w).unwrap();
            egui::pos2(
                content.left() + (ndc[0] * 0.5 + 0.5) * content.width(),
                content.top() + (0.5 - ndc[1] * 0.5) * content.height(),
            )
        };
        let pick = |p: egui::Pos2| pick_part(&cam, Some(&vessel), content, p);
        // Dead center = node 1's AABB.
        assert_eq!(pick(to_screen(DVec3::ZERO)), Some(FlightPick::Part(1)));
        // Node 2's projected center is up-screen → its menu.
        assert_eq!(
            pick(to_screen(DVec3::new(0.0, 3.0, 0.0))),
            Some(FlightPick::Part(2))
        );
        // The forgiving fallback: ~35 px off node 2's projected
        // center still names it.
        let near = to_screen(DVec3::new(0.0, 3.0, 0.0)) + egui::vec2(35.0, 0.0);
        assert_eq!(pick(near), Some(FlightPick::Part(2)));
        // Beside the hull, outside every part's box but within the
        // padded bounds → the whole-vessel menu.
        let hull_side = to_screen(DVec3::new(7.3, 0.0, 0.0));
        assert_eq!(pick(hull_side), Some(FlightPick::Vessel));
        // Far away → nothing (menu closes).
        assert_eq!(pick(to_screen(DVec3::new(0.0, -20.0, 0.0))), None);
        // No staged camera or vessel → nothing.
        assert_eq!(
            pick_part(
                &FlightCameraF64::default(),
                Some(&vessel),
                content,
                to_screen(DVec3::ZERO)
            ),
            None
        );
        assert_eq!(pick_part(&cam, None, content, to_screen(DVec3::ZERO)), None);
    }
}
