//! M4c near-flight view — PLAN §3.7's presentation floor over the
//! real sim: the InBubble craft (and its breakup debris) rendered as
//! silhouette-correct kitbash part meshes ([`crate::part_mesh`]), an
//! engine plume, a **reentry plasma sheath** driven by the sim's own
//! convective-heating flux sample, **ground-contact dust**, and the
//! HUD mirrors. Same §2.5 schedule shape as the map and VAB:
//! `end_frame` stages sim reads into [`FlightSyncInput`],
//! `sync_flight` mirrors them, `flight_interp` draws each staged pose
//! at the frame's fractional tick ([`DrawnScene`]), `flight_camera`
//! steps the chase camera from that pose, `flight_present` derives the
//! HUD mirrors + the SFX cue edges, `extract_flight` emits clip-space
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

use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use bevy_ecs::prelude::*;
use csp_core::{DQuat, DVec3};
use csp_sim::{VesselId, VesselState, World};

use crate::map::{
    MapCameraF64, MapInput, MapMode, body_system_pos, mat4_mul, perspective_revz, project_into,
};
use crate::terrain::{TerrainCache, TerrainConfig};

/// Chase-camera FOV (radians, vertical).
pub(crate) const FLIGHT_FOV_Y: f64 = 0.9;
/// The chase camera's reverse-Z near plane (m, §2.3) — the near pass
/// of the two-pass depth partition (the far pass's near is
/// [`csp_render::flight::FAR_SPLIT_M`]). The only two places that
/// bake `n` into a matrix or an NDC z must agree bit-for-bit, hence
/// one constant.
pub(crate) const FLIGHT_NEAR_M: f64 = 0.1;
/// Where the chase camera sits before anything frames it (m). A focus
/// frames itself the frame it stages ([`frame_distance`], C19-F4: a kept
/// zoom framed the new focus at the old vessel's range); this is only
/// the distance the first frame eases from.
const FLIGHT_DEFAULT_DIST: f64 = 60.0;
/// Half-life (s) of the chase camera's eased zoom. A frame closes
/// `1 − exp2(−dt/half)` of what is left, so the same wall time closes
/// the same share at any frame rate and nothing overshoots. A travel to
/// a new focus starts from here and is shortened by
/// [`travel_half_life`].
const EASE_HALF_LIFE_S: f64 = 0.080;
/// The orbit pitch limits (rad): 89° — nearly straight down or up,
/// never the flip a hard ±π/2 would give the up vector.
const PITCH_LIMIT: f64 = 89.0 * core::f64::consts::PI / 180.0;
/// Nearest the eye may sit to the focus (m).
const MIN_DISTANCE_M: f64 = 3.0;
/// Farthest the eye may sit from the focus (m).
const MAX_DISTANCE_M: f64 = 1.0e6;
/// The distance floor as a multiple of the focus's bounding radius.
const MIN_RADIUS_MULTIPLE: f64 = 1.5;
/// A focus is framed at this multiple of its bounding radius.
const FRAME_RADIUS_MULTIPLE: f64 = 4.0;
/// The eye's clearance over the terrain under it (m) — the ground on
/// land, the seabed under an ocean, whose surface is no floor.
const EYE_GROUND_CLEARANCE_M: f64 = 0.3;
/// A focus travel shorter than this (m) is done: the anchor is the
/// focus's resting anchor exactly from that frame on.
const TRAVEL_DONE_M: f64 = 1.0e-3;
/// How long (s) a focus travel of ANY length may take to be under
/// [`TRAVEL_DONE_M`].
const TRAVEL_SETTLE_S: f64 = 0.5;
/// Plasma sheath display window (W/m² of convective input,
/// `FlightBody::plasma_w_m2`) — the flux half of
/// [`plasma_intensity`]. C19-F1 recalibration: a plasma sheath is
/// shock-layer ionization, whose onset is keyed to *speed*, not raw
/// heat flux — and the verified Terrin table forces the two-factor
/// shape, since a normal mid-ascent carries MORE flux than a high
/// reentry leg (12 km @ 450 m/s → η·½ρv³ ≈ 1.8e5 W/m² vs 40 km @
/// 2200 m/s → ≈ 1.2e5). No monotone flux window can keep that ascent
/// dark while lighting the reentry, so:
///
/// ```text
/// i = clamp((flux − ONSET)/(FULL − ONSET))      <- window
///   · clamp((v_rel − GATE_ON)/(GATE_FULL − GATE_ON))  <- speed gate
/// ```
///
/// `ONSET` 4.0e4 / `FULL` 1.6e5 reads the 40 km @ 2200 leg at
/// i ≈ 0.66 and saturates the deep corridor (35 km @ 2300 → flux
/// ≈ 3.4e5); the gate keeps every stock-ascent row (≤450 m/s through
/// 7–24 km) at zero and opens fully only in the near-orbital regime
/// (Terrin LEO ≈ 2287 m/s) a sheath belongs to. Display
/// normalization only — the sim's lethality is the M4b temperature
/// record, not this curve. (The window rode the C14-F3 η calibration
/// ×4 with the authored value it normalizes — same legs, same i's.)
const PLASMA_ONSET_W_M2: f64 = 4.0e4;
/// See [`PLASMA_ONSET_W_M2`].
const PLASMA_FULL_W_M2: f64 = 1.6e5;
/// Speed-gate lower edge (surface-relative m/s) — see
/// [`PLASMA_ONSET_W_M2`]. Below this the sheath is dark regardless
/// of flux: a dense slow pass heats but does not ionize.
const PLASMA_GATE_ONSET_MPS: f64 = 1.4e3;
/// See [`PLASMA_GATE_ONSET_MPS`] — the gate is fully open here, a
/// margin under circular-orbit speed so a decaying entry still
/// burns at full weight.
const PLASMA_GATE_FULL_MPS: f64 = 2.0e3;

/// The §3.7 plasma intensity — `window(flux) · gate(speed)`, the
/// calibration documented at [`PLASMA_ONSET_W_M2`]. `speed_mps` is
/// the surface-relative flow speed (the same `|v_rel|` the staged
/// `plasma_w_m2` was sampled at). Shared with `csp-app`'s whoosh
/// voice so the audible and visible plasma never drift — the old
/// mirrored constants did (C19-F1). Non-finite inputs yield NaN,
/// which the callers' `> 0`/`is_finite` handling rejects — the same
/// semantics the inline window had.
pub fn plasma_intensity(flux_w_m2: f64, speed_mps: f64) -> f64 {
    let window =
        ((flux_w_m2 - PLASMA_ONSET_W_M2) / (PLASMA_FULL_W_M2 - PLASMA_ONSET_W_M2)).clamp(0.0, 1.0);
    let gate = ((speed_mps - PLASMA_GATE_ONSET_MPS)
        / (PLASMA_GATE_FULL_MPS - PLASMA_GATE_ONSET_MPS))
        .clamp(0.0, 1.0);
    window * gate
}

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
    /// Multiplies the TARGET distance (`> 1` zooms out, `< 1` in); the
    /// camera eases to the target ([`FlightInput`]).
    Zoom {
        /// The factor.
        factor: f64,
    },
}

/// Session-state chase camera (§2.2-C). The angles follow the pointer
/// with nothing in between; the distance EASES — the wheel moves
/// [`Self::distance_target_m`] and every frame steps
/// [`Self::distance_m`] toward it ([`ease_fraction`]).
#[derive(Resource)]
pub struct FlightInput {
    /// Orbit yaw (radians).
    pub yaw: f64,
    /// Orbit pitch (radians), within ±[`PITCH_LIMIT`]. The player's
    /// intent, like the yaw: the camera step never rewrites it — the
    /// floor moves the eye, not the angles — and neither does a focus
    /// change.
    pub pitch: f64,
    /// Where the eye is this frame (m).
    distance_m: f64,
    /// Where the wheel asked the eye to go (m), within the limits.
    distance_target_m: f64,
    /// Viewport in physical pixels.
    pub viewport_px: (f64, f64),
    /// What makes a focus change continuous.
    travel: FocusTravel,
}

impl Default for FlightInput {
    fn default() -> Self {
        Self {
            yaw: 0.7,
            pitch: 0.25,
            distance_m: FLIGHT_DEFAULT_DIST,
            distance_target_m: FLIGHT_DEFAULT_DIST,
            viewport_px: (1.0, 1.0),
            travel: FocusTravel::default(),
        }
    }
}

impl FlightInput {
    /// The eye–anchor distance this frame (m) — the orbit the camera
    /// solves with, before any lift off the floor.
    pub fn distance_m(&self) -> f64 {
        self.distance_m
    }

    /// The distance the camera is easing to (m).
    pub fn distance_target_m(&self) -> f64 {
        self.distance_target_m
    }

    /// Place the eye at `m` outright — current and target together, so
    /// nothing eases afterward. A non-finite or non-positive `m` changes
    /// nothing.
    pub fn set_distance(&mut self, m: f64) {
        if m.is_finite() && m > 0.0 {
            self.distance_m = m;
            self.distance_target_m = m;
        }
    }

    /// Forget where the camera orbited: the next focus is a first one,
    /// with no travel to it. For a world replaced wholesale — the
    /// anchor belonged to the discarded session, and a travel from it
    /// would swoop in from wherever that session's craft flew.
    pub(crate) fn forget_anchor(&mut self) {
        self.travel.last = None;
    }
}

/// Last frame's camera, as the next frame's focus edge reads it.
#[derive(Clone, Copy)]
struct Last {
    /// The focused vessel.
    id: u64,
    /// The anchor the eye orbited.
    anchor: DVec3,
    /// The point the camera looked at.
    look_at: DVec3,
    /// How far the keep-out had moved the eye off its lifted orbit point —
    /// at rest the least-distance rule ([`skirt_eye`]), and on a travel
    /// whatever was still carried of the focus before's skirt as well.
    skirt: DVec3,
    /// The eye itself — what the next focus's least distance is read
    /// against when the focus changes ([`FocusTravel::began_inside`]),
    /// and where the keep-out on a travel finds it.
    eye: DVec3,
    /// The orbit point the keep-out read: lifted, and with whatever was
    /// still carried of the focus before's skirt. Where it was on the
    /// frame before, for the keep-out on a travel ([`KeptOut::ride`]).
    carried: DVec3,
    /// How far the eye moved over that frame (m): `None` where no frame
    /// time was put in, so nothing moved.
    moved_m: Option<f64>,
    /// The travel's share of what is left that that frame closed
    /// ([`ease_fraction`] of its half-life): what reads a move made in
    /// one frame time in another ([`Pace::cap_m`]).
    travelled: f64,
}

/// The eye's ride round a focus's ball while a travel carries its orbit
/// point across it ([`KeptOut::ride`]) — what has come of it so far.
#[derive(Clone, Copy)]
struct Ride {
    /// The way the eye set off round the ball: a unit vector, tangent to
    /// it. Decides a diameter alone — a way whose two ways round are as
    /// long as each other ([`KeptOut::round_way`]).
    sets_off: DVec3,
    /// Which way round the floor's circle the eye took where the way is
    /// under the floor: `1` with the bearings, `-1` against, `0` until it
    /// rode the circle.
    floor_side: f64,
    /// How much of its way the eye had left on the frame before (m).
    left_m: f64,
    /// How much of its own way inside the ball the orbit point had left
    /// on the frame before, to where its way leaves the ball (m): `None`
    /// where the orbit point comes to rest inside it, is outside it, or
    /// that frame put no time in.
    ahead_m: Option<f64>,
}

/// The camera's memory of where it orbited. At rest the anchor is the
/// focus's resting anchor ([`resting_anchor`]); when the focus moves to
/// another vessel the anchor starts where the last frame's eye orbited
/// and eases to the new craft's own (at [`travel_half_life`]), so the eye
/// carries no jump. The memory is also the focus edge: a frame whose
/// focus is not last frame's is a first focus or a change.
#[derive(Clone, Copy, Default)]
struct FocusTravel {
    /// The anchor's offset from the focus's resting anchor this frame
    /// (m) — zero at rest.
    offset: DVec3,
    /// The half-life (s) the travel in hand eases at — taken once when
    /// it starts and fixed for its whole run. `None` at rest.
    half_life_s: Option<f64>,
    /// The travel's length when it started (m) — `0` at rest.
    start_m: f64,
    /// What the previous focus left the camera looking at, as an offset
    /// from the anchor the eye orbited when the travel started (m): the
    /// look-at is handed over from it ([`travel_blend`]).
    look_carry: DVec3,
    /// The previous focus's skirt, handed over the same way.
    skirt_carry: DVec3,
    /// The eye's ride round the new focus's ball while the travel
    /// carries its orbit point across it ([`KeptOut::ride`]) — in hand
    /// or `None`. Nothing at rest.
    ride: Option<Ride>,
    /// The eye began this travel inside the new focus's least distance.
    /// The orbit point's own way may pass through the craft, so the eye
    /// does not follow it: it leaves the ball by the nearest way out and
    /// makes for the orbit point round the craft instead
    /// ([`KeptOut::outward`], [`KeptOut::chase`]) — its distance to the
    /// craft's centre never falls, on any frame. `false` at rest and on
    /// every other travel.
    began_inside: bool,
    /// Last frame — `None` while nothing is focused, so the next focus
    /// is a first one.
    last: Option<Last>,
}

impl FocusTravel {
    /// Start a focus's memory over on a frame whose focus is not last
    /// frame's: nothing in hand and — for a change, `last` being the frame
    /// before — a travel from where that frame's eye orbited to the new
    /// craft's resting anchor `rest`, carrying what the previous focus
    /// left in hand; unless that already is the anchor, when there is no
    /// travel at all. A first focus (`None`) has no earlier eye to be
    /// continuous with. `com` and `rmin` are the new focus's.
    fn begin(&mut self, last: Option<Last>, rest: DVec3, com: DVec3, rmin: f64) {
        *self = Self {
            last,
            ..Self::default()
        };
        let Some(last) = last else {
            return;
        };
        let change = last.anchor - rest;
        let d0_m = change.norm();
        if d0_m.is_finite() && d0_m > TRAVEL_DONE_M {
            self.offset = change;
            self.half_life_s = Some(travel_half_life(d0_m));
            self.start_m = d0_m;
            self.look_carry = last.look_at - last.anchor;
            self.skirt_carry = last.skirt;
            // The eye itself, skirt and all: that is what the travel
            // starts from — on the frame of the change the orbit point,
            // carry and all, is exactly this.
            self.began_inside = (last.eye - com).norm() < rmin;
        }
    }

    /// The travel is done: nothing is in hand any more, and nothing is
    /// left of the orbit point's way round the focus's zone.
    fn settle(&mut self) {
        *self = Self {
            last: self.last,
            ..Self::default()
        };
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
    /// Deployable decelerator — the part resolves a `ParachuteSpec`
    /// (the recovery canister the `"chute"` model keys).
    Chute,
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
    /// `ShapeKey::of(&shape)` folded ONCE at part staging (P1): the
    /// geometry cache's silhouette lookup used to rebuild this key per
    /// part per frame — a `Vec<(u64, u64)>` allocation per lathe
    /// profile on every frame's hit path. Cached here the lookup is
    /// allocation-free; the key still IS the shape's exact bit pattern
    /// (C1's identity rule — no hash folding, no collision class).
    /// Private: it is a cache token for `FlightGeomCache`, not part of
    /// the part's render data; construction sites all live in this
    /// module (`vessel_parts` and the tests).
    shape_key: ShapeKey,
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
    /// forward as `lo`/`hi` on each part. `Arc`-shared with the
    /// fingerprint caches that produced it: the vec is immutable per
    /// craft revision (a re-graph bumps the fingerprint and
    /// re-derives), so staging clones the pointer, not the parts —
    /// C19-F5 ended the per-frame deep clone.
    pub parts: Arc<Vec<FlightPart>>,
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
    /// Ids in `vessels` staged from `VesselState::OnRails` — the C19-F2
    /// handoff set (at most the pinned/focused ids). Mirrored into
    /// [`RailsStaged`]: a rails-staged focus is NOT a materialized
    /// one, and `flight_present` needs the distinction for the pin's
    /// `seen` edge and the SFX conversion cue.
    pub rails: Vec<u64>,
    /// `(VesselId.0, bounding radius about the CoM)` for every entry of
    /// `vessels` — the chase camera's framing and distance-limit input
    /// ([`bounding_radius`]), folded once per craft revision where the
    /// parts are staged. Mirrored into [`CraftRadii`].
    pub radii: Vec<(u64, f64)>,
}

/// [`FlightSyncInput::rails`] mirrored — a standalone resource for the
/// same reason [`LiveVesselCount`] is one (the audio test literal-
/// constructs `FlightSnapshot`, so that struct cannot grow).
#[derive(Resource, Default)]
pub struct RailsStaged(pub Vec<u64>);

/// [`FlightSyncInput::radii`] mirrored (same standalone-resource
/// reason as [`RailsStaged`]); `sync_flight` writes it, the chase
/// camera reads the focus's row.
#[derive(Resource, Default)]
pub struct CraftRadii(pub Vec<(u64, f64)>);

/// The §3.4 flight display's g-load input (C10-F2/C22-F5): sustained
/// acceleration per vessel, measured the way the sim's per-tick
/// `RegimeMetrics.g_load` is — `‖Δv‖/dt` over the inertial CoM
/// velocity — but over the staged frame window, since the tick's own
/// reading never persists outside the step. `stage_program` refreshes
/// it before the RELIABILITY rows read it; a paused frame (no elapsed
/// ticks) keeps the last reading, and a vanished vessel drops out.
#[derive(Resource, Default)]
pub struct RegimeLoads {
    /// `VesselId.0` → `(last staged inertial CoM velocity, its sim
    /// tick)` — the window's near edge.
    prev_vel: BTreeMap<u64, (DVec3, u64)>,
    /// `VesselId.0` → the sustained g over the last measured window.
    g_load: BTreeMap<u64, f64>,
}

impl RegimeLoads {
    /// Refresh from the freshly staged `(VesselId.0, vel)` rows —
    /// non-latched bodies only (the caller filters): a parked pose's
    /// co-rotation velocity would report phantom centripetal load,
    /// and a latched vessel's records are dormant-class anyway.
    pub fn stage(&mut self, rows: &[(u64, DVec3)], now_ticks: u64) {
        self.prev_vel
            .retain(|id, _| rows.iter().any(|(v, _)| v == id));
        let mut next = BTreeMap::new();
        for (id, vel) in rows {
            let g = match self.prev_vel.insert(*id, (*vel, now_ticks)) {
                // The windowed ‖Δv‖/dt — the same inertial measure
                // the sim samples per tick (the norm is
                // rotation-invariant, so the frame choice is moot).
                Some((v0, t0)) if now_ticks > t0 => {
                    (*vel - v0).norm()
                        / ((now_ticks - t0) as f64 / csp_core::TICK_HZ as f64)
                        / csp_data::G0
                }
                // A paused frame — keep the last reading; a first
                // sighting has no window to measure over yet.
                Some(_) => self.g_load.get(id).copied().unwrap_or(0.0),
                None => 0.0,
            };
            next.insert(*id, g);
        }
        self.g_load = next;
    }

    /// The vessel's last measured sustained g (`0` before its first
    /// measured window).
    pub fn g_for(&self, vessel: u64) -> f64 {
        self.g_load.get(&vessel).copied().unwrap_or(0.0)
    }
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

    /// Pin the flight focus to `vessel` WITHOUT arming the lease
    /// transfer — the `rebuild_from_snapshot` restore path, where the
    /// loaded world's `active_vessel` reclaims the camera and the
    /// restored record already carries its own leases (C02-F2: a
    /// load must not re-preempt control). A rails-resident target
    /// stages through the `capture_sync_input` handoff hint, so the
    /// pin can hold across a rails residence — `seen` still waits
    /// for a non-rails staging (the lease edge's materialization).
    pub(crate) fn pin(&mut self, vessel: csp_sim::VesselId) {
        self.target = Some(vessel);
        self.seen = false;
        self.lease_pending = false;
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
    /// resolves through HERE — the chase camera/HUD, the SFX edges, the
    /// extract's blob shadow, the tutorial measures, and (via
    /// [`FlightSfx::focus_vessel`]) the app's audio voices — so a
    /// `recover_control` pin cannot split them across two vessels
    /// (P04-F4/P07-F1/P11-F1/P12-F1).
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

/// A vessel's pose as a frame draws it — the same origin / orientation /
/// CoM triple [`FlightBody`] stages, at the instant the frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawnPose {
    /// The vessel's id (`VesselId.0`).
    pub id: u64,
    /// Body-origin position (bubble frame).
    pub pos: DVec3,
    /// Body→world rotation.
    pub rot: DQuat,
    /// CoM (bubble frame) — the chase camera's focus.
    pub com: DVec3,
}

impl DrawnPose {
    /// Exactly as staged — no interpolation.
    fn staged(b: &FlightBody) -> Self {
        Self {
            id: b.id,
            pos: b.pos,
            rot: b.rot,
            com: b.com,
        }
    }
}

/// One staged pose at one whole tick — a sample on the path a vessel is
/// drawn along.
#[derive(Clone, Copy, Debug)]
struct PoseSample {
    /// The sim tick the pose was staged at.
    tick: u64,
    pose: DrawnPose,
}

/// The last two distinct-tick samples of one vessel.
#[derive(Clone, Copy, Debug)]
struct SamplePair {
    /// The sample before `cur`'s tick — `None` until the vessel has
    /// been staged across a tick boundary. A vessel with no previous
    /// pose (freshly spawned, just delivered, a breakup fragment) is
    /// drawn at its current pose.
    prev: Option<PoseSample>,
    /// The latest staged sample.
    cur: PoseSample,
}

/// §2.5's interpolation memory — per vessel, the poses its last two
/// distinct ticks staged. Session state only: nothing here is sim state
/// or reaches a hash. A `BTreeMap`, so every walk is ascending
/// `VesselId` — the staged list's own order.
#[derive(Resource, Default)]
pub struct PoseHistory {
    samples: BTreeMap<u64, SamplePair>,
}

impl PoseHistory {
    /// Age the history by one frame's staging and return the pose every
    /// staged vessel is drawn at, in `vessels`' order. `tick` is the
    /// clock the poses were staged at; `frac` the frame's fractional
    /// tick remainder past it, in `[0, 1)` — at `0` every pose is the
    /// staged one, unchanged.
    fn advance(&mut self, vessels: &[FlightBody], tick: u64, frac: f64) -> Vec<DrawnPose> {
        // A vessel absent from this frame's staging has left the path —
        // a rails vessel outside the focus hint, a removal. Its next
        // sighting starts over.
        self.samples
            .retain(|id, _| vessels.iter().any(|b| b.id == *id));
        let mut poses = Vec::with_capacity(vessels.len());
        for b in vessels {
            let staged = PoseSample {
                tick,
                pose: DrawnPose::staged(b),
            };
            let pair = self.samples.entry(b.id).or_insert(SamplePair {
                prev: None,
                cur: staged,
            });
            match tick.cmp(&pair.cur.tick) {
                Ordering::Greater => pair.prev = Some(pair.cur),
                // The clock went backwards — a world restored at an
                // earlier tick: the samples describe a discarded
                // timeline.
                Ordering::Less => pair.prev = None,
                // A zero-tick frame restages the same instant.
                Ordering::Equal => {}
            }
            pair.cur = staged;
            poses.push(match pair.prev {
                Some(prev) if frac > 0.0 => pose_between(&prev, &pair.cur, frac),
                _ => pair.cur.pose,
            });
        }
        poses
    }
}

/// The point on `prev → cur` at the frame's drawn instant, `(cur.tick −
/// 1) + frac` ticks — one tick of latency, so nothing is ever
/// extrapolated: exactly `frac` of the way along a one-tick step, and a
/// frame that spanned several ticks interpolates across that whole
/// span. `frac < 1` keeps the result short of `cur`.
fn pose_between(prev: &PoseSample, cur: &PoseSample, frac: f64) -> DrawnPose {
    let span = (cur.tick - prev.tick) as f64;
    let t = ((cur.tick - 1 - prev.tick) as f64 + frac) / span;
    DrawnPose {
        id: cur.pose.id,
        pos: prev.pose.pos + (cur.pose.pos - prev.pose.pos) * t,
        rot: rot_between(prev.pose.rot, cur.pose.rot, t),
        com: prev.pose.com + (cur.pose.com - prev.pose.com) * t,
    }
}

/// Shortest-arc interpolation of two unit rotations at `t`: the
/// renormalized lerp, whose only transcendental is `DQuat::normalize`'s
/// libm sqrt (`csp-view` has no `libm` edge for a constant-rate slerp's
/// `acos`). It travels the arc a slerp would, at a rate that departs
/// from constant by under 0.001 rad across a step turning up to 18° —
/// a 64 Hz tick of any real tumble turns far less.
fn rot_between(a: DQuat, b: DQuat, t: f64) -> DQuat {
    // `q` and `−q` are the same rotation — flip to the shorter arc.
    let flip = if a.dot(b) < 0.0 { -1.0 } else { 1.0 };
    DQuat::new(
        a.x + (b.x * flip - a.x) * t,
        a.y + (b.y * flip - a.y) * t,
        a.z + (b.z * flip - a.z) * t,
        a.w + (b.w * flip - a.w) * t,
    )
    .normalize()
}

/// What a frame draws, all at one instant — §2.5's "never extrapolates"
/// presentation time, `(clock − 1) + remainder` ticks: every staged
/// vessel's pose, and the scene values that otherwise ride the integer
/// clock (the dominant body's spin phase; the seconds the plume flicker
/// and the ocean wave scroll read). [`flight_interp`] writes it every
/// frame ahead of `flight_camera`; the chase solve, the part pick and
/// `extract_flight` read it. The default — nothing drawn — reads as the
/// staged snapshot, so a rig that runs the extract alone draws exactly
/// what it stages.
#[derive(Resource, Default)]
pub struct DrawnScene {
    /// The environment at the drawn instant: the staged one with its
    /// spin phase rolled back to it. `None` mirrors a snapshot without
    /// physics.
    pub(crate) env: Option<FlightEnv>,
    /// The drawn instant in seconds — `None` until a frame has been
    /// drawn.
    pub(crate) time_s: Option<f64>,
    /// One pose per staged vessel, ascending id.
    poses: Vec<DrawnPose>,
}

impl DrawnScene {
    /// The pose `body` is drawn at this frame — its staged pose when
    /// this frame drew none.
    pub(crate) fn pose_of(&self, body: &FlightBody) -> DrawnPose {
        self.poses
            .iter()
            .find(|p| p.id == body.id)
            .map_or_else(|| DrawnPose::staged(body), |p| *p)
    }

    /// The pose drawn for vessel `id` this frame — `None` when the frame
    /// drew none (the vessel is not staged, or interpolation has not
    /// run).
    pub fn drawn(&self, id: u64) -> Option<DrawnPose> {
        self.poses.iter().find(|p| p.id == id).copied()
    }

    /// The environment at the drawn instant — its spin phase is the
    /// ground's, the drawn poses ride. `None` without physics or until
    /// a frame has been drawn.
    pub fn env(&self) -> Option<&FlightEnv> {
        self.env.as_ref()
    }

    /// The drawn instant in seconds — `None` until a frame has been
    /// drawn.
    pub fn time_s(&self) -> Option<f64> {
        self.time_s
    }
}

/// HUD mirrors — written by `flight_present`, drawn by
/// `csp_ui::flight_hud`.
#[derive(Resource, Default)]
pub struct FlightUiData {
    /// P1's FPS readout — the app's smoothed wall-clock frame timing
    /// (`View::set_frame_timing`, the §2.5-legal Instant site), NOT a
    /// sim product: 0.0 until the first window of frames lands, which
    /// the HUD reads as "hide the row".
    pub fps: f64,
    /// The same window's mean frame time (ms).
    pub frame_ms: f64,
    /// The frame-phase split's cpu half (ms) — the loop iteration's
    /// wall time minus the acquire/present wait (`View::set_frame_phase`,
    /// same §2.5-legal Instant site as [`Self::fps`]). 0.0 until the
    /// first submitted frame lands, which the F3 debug panel reads as
    /// "hide the split row". Present-dominated ⇒ GPU/vsync;
    /// cpu-dominated ⇒ our code or the player's CPU.
    pub cpu_ms: f64,
    /// The split's GPU-wait half (ms): `get_current_texture` acquire +
    /// submit/present, the spans where vsync or a saturated GPU park
    /// the CPU thread.
    pub present_ms: f64,
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
    /// In-flight manufacture orders — the FACILITIES `BuildRow`s
    /// mirrored from [`crate::ProgramMirror`]'s staged table so the
    /// `!has_vessel` HUD branch shows the delivery countdown and the
    /// no-focus branch below can tell "arena empty mid-build" (hold
    /// the camera; the pad the launch pressed from is still the
    /// scene) from "arena empty, nothing coming" (the dead-scene
    /// void). Session data only — never read back into the sim.
    pub builds: Vec<csp_ui::program::BuildRow>,
    /// Total vessels in the world (bubble + latched).
    pub vessel_count: usize,
    /// Debris-flagged vessels.
    pub debris_count: usize,
    /// Live SFX cue lines (§3.7's visual SFX representation) —
    /// written from [`FlightSfx`], drawn as event flashes by
    /// `csp_ui::flight_hud`.
    pub sfx: Vec<String>,
    /// The dominant body's heightfield bake is still in flight — the
    /// frame's terrain packet is sky-only (no displaced surface drawn,
    /// M5 §5.2-R2 arrival polish). Written by `extract_flight` (the
    /// extract is where the cache is polled); the HUD reads it one
    /// frame later because UiBuild runs before RenderExtract (§2.5).
    pub terrain_pending: bool,
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
    /// The vessel the DISPLAYED stage plan is bound to when it isn't
    /// the focus — `hud_plan` falls back to the newest live plan when
    /// the focused vessel holds none, so the readout can otherwise
    /// pass another vessel's firings off as this craft's (C19-F6).
    /// `Some(id)` labels the readout "vessel {id}'s plan"; `None` =
    /// the plan is the focus's own, unbound, or absent.
    pub stages_plan_vessel: Option<u64>,
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
    /// Non-debris vessel count across ALL residences — the removal
    /// edge (a bubble→rails conversion must NOT read as removal;
    /// [`LiveVesselCount`] exists exactly for this distinction —
    /// C19-F2/P12-F2).
    live: usize,
    /// Focused vessel staged from `OnRails` — the conversion edge.
    rails: bool,
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

/// One stage firing's actions — what KSP's SPACE press does: the
/// stage's decouplers sever their drop subtrees AND its deployables
/// (the `ModuleDef::Parachute` parts — v1's only deployable) flip
/// armed → deployed. Either list may be empty; a stage holding only
/// a parachute is still a real firing — it is exactly how a chute
/// stages in KSP.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StageActions {
    /// The nodes that leave the sim — every node in the subtree below
    /// each decoupler assigned to the stage (the session's "severs
    /// all its child edges" rule; the decoupler itself stays
    /// parent-side, the KSP convention), `PartNodeId`-ascending.
    pub jettisons: Vec<csp_phys::PartNodeId>,
    /// The stage's deployable nodes — every assigned part whose def
    /// carries a `Parachute` module, `PartNodeId`-ascending.
    /// `fire_stage` issues their `DeployPart`s BEFORE the jettisons
    /// so a chute staged alongside the decoupler dropping its own
    /// subtree still lands its deploy record (a `DeployPart` naming
    /// an already-jettisoned node would just reject).
    pub deploys: Vec<csp_phys::PartNodeId>,
}

/// The launched craft's remaining stage firings (§2.2-C session
/// state — captured at [`crate::View::launch_craft`] from the VAB
/// session's editor staging, which the spawned `PartGraph` itself
/// does not carry). Each entry is one firing's [`StageActions`] —
/// decoupler drop-sets plus staged deployables. Entries are in firing
/// order (the session's descending `stage_order`). Empty stages
/// (no decoupler, no deployable, or a decoupler severing nothing)
/// are dropped at capture, so `stages.len()` counts real firings
/// only.
///
/// `View::fire_stage` pops the front set and issues the logged
/// per-node commands — `DeployPart` for each deployable, then one
/// `JettisonPart` per drop-set node — the sim's v1 command surface
/// staging maps onto (a jettisoned part leaves the sim outright;
/// KSP-style fragment spawn is not in the v1 command vocabulary).
///
/// The plan belongs to the vessel its launch spawned — NOT the
/// flight focus: a newer spawn or a breakup fragment can take the
/// focus rule's "newest staged vessel" slot without ever retargeting
/// this plan. The `VesselId` cannot be predicted at capture (the
/// enqueued `SpawnPhysicalVessel` allocates it inside `step_frame`,
/// and queued producers/event firings may burn ids first), so the
/// plan binds *lazily* — the first `fire_stage`/`vessel_for` call
/// that sees a post-launch vessel carrying the captured craft's
/// launch key keeps it.
#[derive(Resource, Default)]
pub struct StagePlan {
    /// Remaining stage firings' action sets, firing order.
    pub stages: Vec<StageActions>,
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
    /// [`craft_launch_key`] of the launched craft — the binding match.
    /// Two launches of the same session craft produce identical keys;
    /// `min_id` keeps the newest launch's plan from binding a
    /// still-live older copy.
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
        stages: Vec<StageActions>,
        sim: &World,
        craft: &csp_sim::CraftDef,
    ) -> Self {
        Self {
            total: stages.len(),
            stages,
            vessel: None,
            min_id: sim.vessels().keys().next_back().map_or(0, |id| id.0 + 1),
            craft_key: craft_launch_key(craft),
            ambiguous: false,
        }
    }

    /// A plan pre-bound to a known vessel — for tests and scripted
    /// producers that mint the `VesselId` themselves. `min_id`/
    /// `craft_key` stay inert because [`Self::vessel`] is already set.
    pub fn bound_to(stages: Vec<StageActions>, vessel: VesselId) -> Self {
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
    /// craft matches the launch's key (P05-F1 — the old
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
                    .is_some_and(|c| craft_launch_key(c) == self.craft_key)
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
    /// `VesselId.0` → the staged craft for the InBubble vessels — the
    /// same craft-identity key, reusing the per-part derivation (the
    /// `body_transforms` fold, the per-collider-vertex AABB fold,
    /// `classify_weld_part`) across frames instead of redoing it for a
    /// craft that only moves (C12-F3). Entries drop the frame the vessel
    /// leaves the bubble; a staging/jettison/breakup re-graph changes the
    /// fingerprint and re-derives. The parts vec is `Arc`-shared into
    /// each staged [`FlightBody`] — the cache entry is immutable per
    /// fingerprint, so the share is honest (C19-F5).
    bubble_parts: BTreeMap<u64, StagedCraft>,
}

/// [`vessel_parts`]' output with what it bounds — derived once per
/// craft revision, never per frame.
struct StagedCraft {
    /// `craft_fingerprint` of the craft these entries describe.
    key: u64,
    /// The per-part render entries — `Arc`-shared into the staged
    /// bodies (immutable per fingerprint, C19-F5).
    parts: Arc<Vec<FlightPart>>,
    /// [`bounding_radius`] of `parts` about the craft's CoM (m).
    radius_m: f64,
}

impl StagedCraft {
    /// Derive the entries and the radius they bound for one craft
    /// revision, under its weld's body-frame CoM.
    fn stage(
        key: u64,
        craft: Option<&csp_sim::CraftDef>,
        hulls: &[csp_phys::ConvexHull],
        com_local: DVec3,
        art: &crate::art::ArtAssets,
    ) -> Self {
        let parts = vessel_parts(craft, hulls, art);
        Self {
            key,
            radius_m: bounding_radius(&parts, com_local),
            parts,
        }
    }
}

/// One latched vessel's cached weld output — the render data
/// `capture_sync_input` would otherwise recompute every frame.
struct LatchedWeld {
    /// The staged craft — its fingerprint, parts and bounding radius.
    craft: StagedCraft,
    /// `WeldedBody::com_local` — `surface_pose_restore`'s CoM input.
    com_local: DVec3,
    /// Live plume emitters `(local_pos, local_dir, delivered)` — see
    /// [`FlightBody::plumes`]. A fresh baseline weld stages `1.0`.
    plumes: Vec<(DVec3, DVec3, f64)>,
}

/// The smallest sphere about a craft's body-frame CoM holding every
/// part's collider AABB — the farthest AABB corner from `com_local`. The
/// baked models are fitted to those AABBs at emit, so it bounds what is
/// drawn. A corner at no finite distance (an empty hull's inverted
/// extents) bounds nothing.
fn bounding_radius(parts: &[FlightPart], com_local: DVec3) -> f64 {
    let mut far_sq: f64 = 0.0;
    for p in parts {
        for x in [p.lo.x, p.hi.x] {
            for y in [p.lo.y, p.hi.y] {
                for z in [p.lo.z, p.hi.z] {
                    let d_sq = (DVec3::new(x, y, z) - com_local).norm_squared();
                    if d_sq.is_finite() {
                        far_sq = far_sq.max(d_sq);
                    }
                }
            }
        }
    }
    far_sq.sqrt()
}

/// FNV-1a over a craft — the mixer [`craft_fingerprint`] and
/// [`craft_launch_key`] share.
struct CraftHash(u64);

impl CraftHash {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn mix(&mut self, bits: u64) {
        self.0 ^= bits;
        self.0 = self.0.wrapping_mul(Self::FNV_PRIME);
    }

    /// The mixer opened over `craft`'s shape: its root, then every
    /// node ascending — id, parent, transform, effective index — then
    /// the part count.
    fn over(craft: &csp_sim::CraftDef) -> Self {
        let mut h = Self(Self::FNV_OFFSET);
        h.mix(craft.graph.root.0);
        for (id, node) in &craft.graph.nodes {
            h.mix(id.0);
            h.mix(node.parent.map_or(u64::MAX, |p| p.0));
            for v in [
                node.transform.offset.x,
                node.transform.offset.y,
                node.transform.offset.z,
                node.transform.rotation.x,
                node.transform.rotation.y,
                node.transform.rotation.z,
                node.transform.rotation.w,
            ] {
                h.mix(v.to_bits());
            }
            h.mix(u64::from(node.effective_index));
        }
        h.mix(craft.parts.len() as u64);
        h
    }

    fn finish(self) -> u64 {
        self.0
    }
}

/// The [`LatchedWeldCache`] staleness key: FNV-1a over the
/// weld-relevant craft structure — node ids, parents, transforms, and
/// effective indices, then the part count and each part's mass/CoM —
/// so a same-id/different-craft hit re-welds rather than rendering a
/// stale parked shape. Presentation-only; not the §7.1 state hash.
fn craft_fingerprint(craft: &csp_sim::CraftDef) -> u64 {
    let mut h = CraftHash::over(craft);
    for p in &craft.parts {
        h.mix(p.mass_kg.to_bits());
        h.mix(p.com.x.to_bits());
        h.mix(p.com.y.to_bits());
        h.mix(p.com.z.to_bits());
    }
    h.finish()
}

/// The key a [`StagePlan`] binds its vessel by: the craft's shape and
/// its catalog part ids — deliberately not the masses
/// [`craft_fingerprint`] also folds in. A tanked craft's part masses
/// move with the fuel bucket the first tick its engine burns, and the
/// plan binds on its first stage press: keyed on mass, a craft the
/// player flew before staging never matched its plan, and the press
/// reported a spawn that had long since landed.
fn craft_launch_key(craft: &csp_sim::CraftDef) -> u64 {
    let mut h = CraftHash::over(craft);
    for id in &craft.part_ids {
        for byte in id.as_str().bytes() {
            h.mix(u64::from(byte));
        }
        h.mix(u64::MAX);
    }
    h.finish()
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
/// session's [`LatchedWeldCache`]: non-`InBubble` vessels (latched,
/// and the rails-staged focus below) reuse its weld-derived render
/// data instead of re-welding every frame.
///
/// `rails_focus` is the C19-F2 handoff hint — the vessel ids the
/// chase camera may need to FOLLOW through a bubble→rails conversion
/// (the armed focus pin's target plus the last-resolved focus). A
/// vessel in `rails_focus` that is `VesselState::OnRails` still
/// stages — pose evaluated off its conic — so the camera/HUD hold it
/// rather than popping to the next staged vessel or a dead frame.
/// Only the hinted vessels stage this way: staging every rails conic
/// would flood the staged list AND corrupt the newest-spawn focus
/// rule (`focus_vessel` = `vessels.last()`).
#[allow(clippy::too_many_lines)] // one staging arm per vessel state — splitting the loop mid-scan would scatter the order guarantees
pub fn capture_sync_input(
    sim: &World,
    cache: &mut LatchedWeldCache,
    art: &crate::art::ArtAssets,
    rails_focus: &[VesselId],
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
        ) || rails_focus.contains(&VesselId(*vid))
    });
    cache.bubble_parts.retain(|vid, _| {
        matches!(
            sim.vessels().get(&VesselId(*vid)).map(|v| &v.state),
            Some(VesselState::InBubble(_))
        )
    });
    let mut vessels = Vec::new();
    let mut propulsion = Vec::new();
    let mut rails = Vec::new();
    let mut radii = Vec::new();
    for (id, vessel) in sim.vessels() {
        match &vessel.state {
            VesselState::InBubble(body) => {
                // The per-part render data is a pure function of the
                // craft topology — cache it on the same fingerprint
                // the latched path uses (C12-F3); the bounding radius
                // is a function of the same revision, so it rides
                // along. Craft-less bodies stage the cheap hull-AABB
                // fallback uncached.
                let (parts, radius_m) = match vessel.craft.as_ref() {
                    Some(craft) => {
                        let key = craft_fingerprint(craft);
                        let derive = || {
                            StagedCraft::stage(
                                key,
                                Some(craft),
                                &body.collider.hulls,
                                body.com_local,
                                art,
                            )
                        };
                        let entry = cache.bubble_parts.entry(id.0).or_insert_with(derive);
                        if entry.key != key {
                            *entry = derive();
                        }
                        (Arc::clone(&entry.parts), entry.radius_m)
                    }
                    None => {
                        let staged =
                            StagedCraft::stage(0, None, &body.collider.hulls, body.com_local, art);
                        (staged.parts, staged.radius_m)
                    }
                };
                let plumes = live_plumes(vessel.craft.as_ref(), body);
                // §3.7's plasma driver: the per-area convective input the
                // M4b thermal tick itself integrates — same `flow_sample`,
                // same `v_rel`, so the glow tracks the sim's heating, not
                // a lookalike.
                let plasma_w_m2 = env_ref.map_or(0.0, |e| {
                    let flow = csp_phys::flow_sample(e, body);
                    flow.convective_fraction * 0.5 * flow.density_kg_m3 * flow.speed_mps.powi(3)
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
                radii.push((id.0, radius_m));
                propulsion.push(vessel_propulsion(*id, vessel, Some(body)));
            }
            // §2.6.4 surface rails: a latched vessel is still *on the
            // pad* — stage its restored inertial pose so the parked
            // craft stays rendered and focusable (the throttle command
            // the HUD targets unlatches it). The restore needs the
            // environment; without one the record is uninterpretable —
            // skip honestly rather than guess a pose.
            VesselState::SurfaceLatched(pose) => {
                if let Some((body, radius_m)) =
                    stage_latched(sim, env_ref, *id, vessel, pose, cache, art)
                {
                    vessels.push(body);
                    radii.push((id.0, radius_m));
                    propulsion.push(vessel_propulsion(*id, vessel, None));
                }
            }
            // C19-F2: the focus handoff — a vessel that converted to
            // rails while it held (or pins) the focus keeps staging
            // off its conic, so `resolve` still returns it. Entries
            // keep arena order (ascending `VesselId`), so a hinted
            // newest vessel stays `vessels.last()` — the same
            // resolver picks it again. Unhinted rails vessels stage
            // nothing, exactly as before.
            VesselState::OnRails(orbit) => {
                if rails_focus.contains(id)
                    && let Some((body, prop, radius_m)) =
                        stage_rails(sim, env_ref, *id, vessel, orbit, cache, art)
                {
                    rails.push(id.0);
                    vessels.push(body);
                    radii.push((id.0, radius_m));
                    propulsion.push(prop);
                }
            }
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
        rails,
        radii,
    }
}

/// Stage the focus-hinted `OnRails` vessel for the C19-F2 handoff.
/// `state_at` is parent-centered inertial; the bubble frame is the
/// system frame translated to the dominant body, so the pose composes
/// through the parent↔dominant offset (identity when the conic's
/// parent IS the dominant — the common case). Attitude is the weld
/// default (identity): the stamped conic anchors the CoM only — the
/// same fields `apply_to_bubble` leaves at defaults on a real
/// re-materialization, so the render cannot claim an attitude the sim
/// never tracked. `None` when the conic fails to evaluate or no
/// environment exists to express the frame — unstageable reads as
/// absent, never guessed (the `stage_latched` convention).
fn stage_rails(
    sim: &World,
    env: Option<&csp_phys::Environment>,
    id: VesselId,
    vessel: &csp_sim::Vessel,
    orbit: &csp_sim::orbit::OrbitState,
    cache: &mut LatchedWeldCache,
    art: &crate::art::ArtAssets,
) -> Option<(FlightBody, VesselPropulsion, f64)> {
    let env = env?;
    let dominant = sim.dominant_body()?.clone();
    let t = sim.clock();
    let (r, v) = csp_sim::orbit::OrbitModel::state_at(sim.orbits(), orbit, t).ok()?;
    let (pos_off, vel_off) = if orbit.parent == dominant {
        (DVec3::ZERO, DVec3::ZERO)
    } else {
        (
            body_system_pos(sim.orbits(), &orbit.parent, t).ok()?
                - body_system_pos(sim.orbits(), &dominant, t).ok()?,
            crate::map::body_system_vel(sim.orbits(), &orbit.parent, t).ok()?
                - crate::map::body_system_vel(sim.orbits(), &dominant, t).ok()?,
        )
    };
    let com = env.body_center + pos_off + r;
    let vel = vel_off + v;
    // Weld-derived render data — the latched path's cache (the craft
    // is immutable while railed, so the fingerprint key holds).
    let (parts, com_local, plumes, nodes, radius_m) = match vessel.craft.as_ref() {
        Some(craft) => {
            let key = craft_fingerprint(craft);
            if cache.entries.get(&id.0).is_none_or(|e| e.craft.key != key) {
                let welded = csp_phys::weld(&craft.graph, &craft.parts).ok()?;
                let plumes = live_plumes(vessel.craft.as_ref(), &welded);
                cache.entries.insert(
                    id.0,
                    LatchedWeld {
                        com_local: welded.com_local,
                        plumes,
                        craft: StagedCraft::stage(
                            key,
                            vessel.craft.as_ref(),
                            &welded.collider.hulls,
                            welded.com_local,
                            art,
                        ),
                    },
                );
            }
            let entry = cache.entries.get(&id.0)?;
            (
                Arc::clone(&entry.craft.parts),
                entry.com_local,
                entry.plumes.clone(),
                craft.graph.nodes.len(),
                entry.craft.radius_m,
            )
        }
        // Craft-less (elements-only) vessel: the hull-AABB fallback has
        // no hulls to box — an empty part list still leaves the staged
        // pose/HUD honest (the camera and telemetry keep working).
        None => (Arc::new(Vec::new()), DVec3::ZERO, Vec::new(), 0, 0.0),
    };
    let rot = DQuat::IDENTITY;
    Some((
        FlightBody {
            id: id.0,
            pos: com - rot.rotate(com_local),
            rot,
            vel,
            // A railed vessel coasts — the stamped conic carries no
            // throttle, contact, or heating state to mirror.
            throttle: 0.0,
            debris: vessel.debris_since.is_some(),
            contact: false,
            plasma_w_m2: 0.0,
            nodes,
            parts,
            plumes,
            com,
            latched: false,
            tanks: vessel_tanks(vessel),
        },
        vessel_propulsion(id, vessel, None),
        radius_m,
    ))
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
/// degraded, never guessed. Returns the body and its bounding radius (m).
fn stage_latched(
    sim: &World,
    env: Option<&csp_phys::Environment>,
    id: VesselId,
    vessel: &csp_sim::Vessel,
    pose: &csp_sim::SurfacePose,
    cache: &mut LatchedWeldCache,
    art: &crate::art::ArtAssets,
) -> Option<(FlightBody, f64)> {
    let (Some(env), Some(craft)) = (env, vessel.craft.as_ref()) else {
        return None;
    };
    let key = craft_fingerprint(craft);
    if cache.entries.get(&id.0).is_none_or(|e| e.craft.key != key) {
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
                com_local: welded.com_local,
                plumes,
                craft: StagedCraft::stage(
                    key,
                    vessel.craft.as_ref(),
                    &welded.collider.hulls,
                    welded.com_local,
                    art,
                ),
            },
        );
    }
    let entry = cache.entries.get(&id.0)?;
    // `vel` stamps the CoM slot: the co-rotation velocity of
    // `pos + rot·com_local`, not of the origin `pos`.
    let (pos, rot, vel, _ang_vel) =
        csp_sim::bubble::surface_pose_restore(env, pose, entry.com_local, sim.clock());
    Some((
        FlightBody {
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
            parts: Arc::clone(&entry.craft.parts),
            plumes: entry.plumes.clone(),
            com: pos + rot.rotate(entry.com_local),
            latched: true,
            tanks: vessel_tanks(vessel),
        },
        entry.craft.radius_m,
    ))
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
/// science payloads, docking ports, parachutes, and everything else
/// structural. **Shared with the VAB** (`vab.rs` runs it over
/// `EffectivePart::weld_input`): one classifier feeding `model_key`'s
/// kind arm is what keeps the editor↔flight model keys in lockstep
/// (C20-F1 — this function used to omit `parachute` entirely).
pub(crate) fn part_kind(wp: &csp_phys::WeldPart) -> PartKind {
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
    // A chute claims the part only when no functional identity above
    // did — a tank that also packs a chute still reads as a tank.
    if wp.parachute.is_some() {
        return PartKind::Chute;
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
        // Safety-orange — the packed-canopy recovery class reads
        // warm against the stage-white tanks.
        PartKind::Chute => [0.78, 0.38, 0.22],
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
) -> Arc<Vec<FlightPart>> {
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
                let shape = crate::part_mesh::PartShape::Box {
                    lo: -half,
                    hi: half,
                };
                FlightPart {
                    lo,
                    hi,
                    transform: csp_phys::PartTransform::new(center, csp_core::DQuat::IDENTITY),
                    // The transform already carries `center` — the shape
                    // is part-LOCAL, so the box is centred on the local
                    // origin. Centring it again here would double the
                    // offset at emit (`transform.apply(shape_corner)`).
                    shape_key: ShapeKey::of(&shape),
                    shape,
                    kind: PartKind::Structural,
                    model: None,
                    node: u64::MAX,
                }
            })
            .collect()
    };
    let Some(craft) = craft else {
        return Arc::new(fallback());
    };
    let Ok(transforms) = csp_phys::weld::body_transforms(&craft.graph, &craft.parts) else {
        log::warn!("flight: craft graph failed body_transforms — boxing hulls");
        return Arc::new(fallback());
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
        // The node's recorded def id — `part_ids` parallels `parts`
        // (empty on hand-built craft, then the kind arm decides) —
        // feeds the same id-first `model_key` the VAB resolves, so
        // editor and flight agree on one model per part (C20-F1).
        let model = art.model(crate::art::model_key(
            craft
                .part_ids
                .get(node.effective_index as usize)
                .map(|id| id.as_str()),
            kind,
            crate::part_mesh::is_pointed(&shape),
        ));
        parts.push(FlightPart {
            lo,
            hi,
            transform: t,
            shape_key: ShapeKey::of(&shape),
            shape,
            kind,
            model,
            node: id.0,
        });
    }
    if parts.is_empty() {
        Arc::new(fallback())
    } else {
        Arc::new(parts)
    }
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
                input.pitch = (input.pitch + dpitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
            }
            // The craft-radius half of the limit lands in
            // `step_chase_camera`, where the focus is known.
            FlightEvent::Zoom { factor } if factor.is_finite() => {
                input.distance_target_m =
                    (input.distance_target_m * factor).clamp(MIN_DISTANCE_M, MAX_DISTANCE_M);
            }
            FlightEvent::Zoom { .. } => {}
        }
    }
}

/// `Sync` schedule — move the staged input into the presentation
/// mirror. `FlightSyncInput` is rebuilt wholesale at the next
/// `end_frame` and read by nothing after `stage_program` (its sole
/// consumer runs before this schedule), so the mirror takes the
/// staged buffers outright instead of `clone_from`-ing the whole
/// fleet a second time (C19-F5).
pub(crate) fn sync_flight(
    mut staged: ResMut<FlightSyncInput>,
    mut snap: ResMut<FlightSnapshot>,
    mut live: ResMut<LiveVesselCount>,
    mut prop: ResMut<VesselPropulsionStaged>,
    mut rails: ResMut<RailsStaged>,
    mut radii: ResMut<CraftRadii>,
) {
    snap.env.clone_from(&staged.env);
    snap.vessels = std::mem::take(&mut staged.vessels);
    live.0 = staged.live_vessels;
    prop.0 = std::mem::take(&mut staged.propulsion);
    rails.0 = std::mem::take(&mut staged.rails);
    radii.0 = std::mem::take(&mut staged.radii);
}

/// The share of what is left that one frame of `dt_s` closes at a
/// half-life of `half_life_s` — `1 − exp2(−dt / half)`. Frame-rate
/// independent (two half-length frames close exactly what one does) and
/// never more than 1, which is what keeps an eased value short of its
/// target: no overshoot.
fn ease_fraction(dt_s: f64, half_life_s: f64) -> f64 {
    1.0 - (-dt_s / half_life_s).exp2()
}

/// The half-life (s) a focus travel of `d0_m` eases at: the zoom's
/// ([`EASE_HALF_LIFE_S`]), shortened for a long travel so that
/// `half_life · log2(d0_m / TRAVEL_DONE_M)` — the time to be done —
/// never exceeds [`TRAVEL_SETTLE_S`]. At the zoom's own rate a travel
/// from 850 km away crawled for over a second; at this one every travel
/// from a metre to a thousand kilometres is done in half a second.
fn travel_half_life(d0_m: f64) -> f64 {
    EASE_HALF_LIFE_S.min(TRAVEL_SETTLE_S / (d0_m / TRAVEL_DONE_M).log2())
}

/// The nearest the eye may sit to a focus of bounding radius `radius_m`
/// (m): the larger of [`MIN_DISTANCE_M`] and [`MIN_RADIUS_MULTIPLE`]
/// radii, itself within the far limit.
fn min_distance(radius_m: f64) -> f64 {
    MIN_DISTANCE_M
        .max(MIN_RADIUS_MULTIPLE * radius_m)
        .min(MAX_DISTANCE_M)
}

/// The distance a focus of bounding radius `radius_m` is framed at (m):
/// [`FRAME_RADIUS_MULTIPLE`] radii, clamped to the limits.
fn frame_distance(radius_m: f64) -> f64 {
    (FRAME_RADIUS_MULTIPLE * radius_m).clamp(min_distance(radius_m), MAX_DISTANCE_M)
}

/// The least radius the eye may have where the heightfield's surface is
/// `ground_m` from the body centre: [`EYE_GROUND_CLEARANCE_M`] over that
/// surface, on land and under an ocean alike. Over water the eye goes
/// under the surface freely, down to the clearance over the seabed.
fn eye_floor(ground_m: f64) -> f64 {
    ground_m + EYE_GROUND_CLEARANCE_M
}

/// The local-level frame at the camera's anchor — the basis the orbit
/// angles are read in, so the horizon stays level while the camera is
/// dragged (the shared world-frame `solve_orbit_camera` puts the orbit
/// basis on the ecliptic normal, which renders a pad away from the pole
/// tilted).
struct LevelFrame {
    /// Local up — the radial at the anchor.
    up: DVec3,
    /// Local north — the spin axis projected level.
    north: DVec3,
    /// Local east — completes the right-handed frame.
    east: DVec3,
}

impl LevelFrame {
    /// The unit direction from the anchor toward the eye: `yaw` about
    /// `up` from north, `pitch` above the horizon.
    fn dir(&self, yaw: f64, pitch: f64) -> DVec3 {
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        self.north * (cy * cp) + self.east * (sy * cp) + self.up * sp
    }
}

/// `anchor`'s level frame on the body centred at `centre` spinning
/// about `axis`. Without a body, or over a degenerate anchor, up is
/// world +Z. Over a pole the axis projection collapses — world +Z, then
/// +X, is projected instead.
fn level_frame(anchor: DVec3, centre: Option<DVec3>, axis: DVec3) -> LevelFrame {
    let up = centre.map_or(DVec3::Z, |c| {
        let rel = anchor - c;
        if rel.norm_squared() > 1.0e-12 {
            rel / rel.norm()
        } else {
            DVec3::Z
        }
    });
    let mut north = axis - up * axis.dot(up);
    if north.norm_squared() < 1.0e-12 {
        north = DVec3::Z - up * up.z;
        if north.norm_squared() < 1.0e-12 {
            north = DVec3::X - up * up.x;
        }
    }
    let north = north / north.norm();
    LevelFrame {
        up,
        north,
        east: north.cross(up),
    }
}

/// An orbit about the camera's anchor.
struct Orbit {
    /// Azimuth about local up (rad).
    yaw: f64,
    /// Elevation above the horizon (rad).
    pitch: f64,
    /// Eye–anchor distance (m).
    distance_m: f64,
}

/// The chase-camera solve, pure: the eye for one `orbit` about
/// `anchor`, and whether the floor lifted it. The intent's orbit point —
/// `anchor + dir(yaw, pitch)·d` — is the eye whenever it clears the floor
/// (`floor` gives the least radius the eye may have from `centre` along a
/// unit direction: [`eye_floor`] over the heightfield). Under the floor
/// the eye is that same point lifted along the local up, radially out from
/// `centre`, onto the floor — sampled along the point's own direction, so
/// the lifted eye sits on the floor exactly, whatever the ground does.
///
/// Nothing else happens here: no pitch is searched for and no angle is
/// handed back, because where the floor had to put the eye must never
/// rewrite the player's orbit. The eye is a continuous function of the
/// intent alone — the lift shrinks to nothing as the point reaches the
/// floor. What the lift keeps of a drag is the orbit point's share along
/// the floor, `d·cos(pitch)`, whose slope in pitch, `d·sin(pitch)`, is
/// zero at the anchor's own level. Two rules stand outside the solve for
/// what that leaves: a craft under the floor has its anchor lifted over
/// the ground ([`resting_anchor`]), and an eye that would come nearer
/// than the focus's least distance is kept out of its ball ([`skirt_eye`]
/// at rest, [`KeptOut::ride`] on a focus travel).
fn solve_orbit(
    orbit: &Orbit,
    anchor: DVec3,
    level: &LevelFrame,
    centre: DVec3,
    floor: &mut dyn FnMut(DVec3) -> f64,
) -> (DVec3, bool) {
    let asked = anchor
        + level.dir(orbit.yaw, orbit.pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT)) * orbit.distance_m;
    let rel = asked - centre;
    let r = rel.norm();
    if r > 0.0 {
        let dir = rel / r;
        let min_r = floor(dir);
        if r < min_r {
            return (centre + dir * min_r, true);
        }
    }
    (asked, false)
}

/// The least-distance rule's edges along the floor are placed to this (m).
const SKIRT_PRECISION_M: f64 = 1.0e-9;

/// A way into a craft's ball whose tangent share where it enters is within
/// this of nothing passes within `rmin` times this of the CoM: a diameter
/// for the keep-out on a travel, whose two ways round are as long as each
/// other.
const DIAMETER_SHARE: f64 = 1.0e-3;

/// Two points of a craft's ball this close together, as a share of its
/// radius, are the same point: nothing for a way to go.
const SAME_POINT_SHARE: f64 = 1.0e-12;

/// How many samples along the way round a craft's ball the keep-out on a
/// travel looks for the way being under the floor at, and how finely each
/// edge of that is then bisected.
const ROUND_SAMPLES: usize = 256;
const ROUND_BISECTIONS: usize = 40;

/// The share of the way across the zone — from the turn point to the far
/// edge, `2` whole in [`skirt_azimuth`]'s measure — over which the eye
/// settles from the drag's own pace along the floor onto the circle's
/// ([`skirt_plateau`]).
const SKIRT_ONRAMP: f64 = 0.5;

/// How many times the orbit point's slide along the floor the eye covers
/// along the circle once it has settled ([`skirt_azimuth`]). The circle's
/// half circumference is `π·h*` and the slide straight across the zone
/// `2·h*`, `h*` the turn: the eye must average `π/2` times the slide to be
/// round by the time the orbit point leaves, and it spends its first
/// [`SKIRT_ONRAMP`] of the way at the slide itself — so it settles at
/// this.
fn skirt_plateau() -> f64 {
    (core::f64::consts::PI - 0.5 * SKIRT_ONRAMP) / (2.0 - 0.5 * SKIRT_ONRAMP)
}

/// Where the eye is round the circle: the angle (rad) from the turn point
/// toward the yaw's own side, `π` at the far edge, for an orbit point `u`
/// of the way across the zone — `0` at the turn point, `2` at the far
/// edge. Its slope is how many metres of circle the eye covers for a
/// metre of the orbit point's slide: the slide itself where the eye
/// enters (at the turn point), so the frame that enters the zone moves no
/// further than the drag's own step, settling smoothly over
/// [`SKIRT_ONRAMP`] onto [`skirt_plateau`] times it. It rises all the way,
/// and depends on `u` alone: dragging down or zooming in carries the eye
/// on round the craft, dragging up or zooming out brings it back along
/// the same circle, and a reversed drag retraces the path.
fn skirt_azimuth(u: f64) -> f64 {
    let plateau = skirt_plateau();
    let v = u / SKIRT_ONRAMP;
    if v < 1.0 {
        // `u`, plus the on-ramp's gain integrated under a smooth step.
        u + 0.5 * (plateau - 1.0) * SKIRT_ONRAMP * v.powi(3) * (2.0 - v)
    } else {
        0.5 * SKIRT_ONRAMP * (1.0 + plateau) + plateau * (u - SKIRT_ONRAMP)
    }
}

/// A line along the floor from the point under a craft — where
/// [`skirt_eye`] moves the eye.
struct FloorRay<'a> {
    /// The body's centre.
    centre: DVec3,
    /// The craft's local up.
    up: DVec3,
    /// Across the floor, away from the point under the craft.
    along: DVec3,
    /// The floor's radius under the craft (m).
    floor_r: f64,
    floor: &'a mut dyn FnMut(DVec3) -> f64,
}

impl FloorRay<'_> {
    /// The floor point `x` metres from the point under the craft.
    fn at(&mut self, x: f64) -> DVec3 {
        let (sin, cos) = (x / self.floor_r).sin_cos();
        let dir = self.up * cos + self.along * sin;
        self.centre + dir * (self.floor)(dir)
    }
}

/// The first point from `from_m` out along the ray that is `rmin` or
/// more from `com` — `from_m` itself being nearer. No floor point further
/// than `rmin` along the ray can be nearer, so the edge is bracketed by
/// doubling steps inside that and bisected to [`SKIRT_PRECISION_M`].
fn clear_from(ray: &mut FloorRay, com: DVec3, rmin: f64, from_m: f64) -> f64 {
    let clear = |ray: &mut FloorRay, x: f64| (ray.at(x) - com).norm() >= rmin;
    let (mut inside, mut step, mut edge) = (from_m, 0.01 * rmin, from_m + 0.01 * rmin);
    while !clear(ray, edge) && edge < from_m + 4.0 * rmin {
        inside = edge;
        step *= 2.0;
        edge += step;
    }
    for _ in 0..64 {
        if edge - inside <= SKIRT_PRECISION_M {
            break;
        }
        let mid = 0.5 * (inside + edge);
        if clear(ray, mid) {
            edge = mid;
        } else {
            inside = mid;
        }
    }
    edge
}

/// How close to the floor a point of the craft's ball counts as on it.
/// A way that sets off from the floor's least-distance circle, or comes
/// in to land on it, is on the circle there: one built on the sphere a
/// hair over it would leave the ground.
const FLOOR_HAIR_M: f64 = 1.0e-7;

/// A point this close to the craft's ball's surface counts as on it (m).
const ON_BALL_M: f64 = 1.0e-7;

/// How many bisections find the first point along a way on the craft's
/// ball from which the straight line to a point outside it clears it.
const CLEAR_BISECTIONS: usize = 24;

/// How many bisections find how far along its way the eye's pace carries
/// it, where the way asked more of it ([`paced`]).
const PACE_BISECTIONS: usize = 24;

/// The most one frame's move of the eye may exceed the one before's on a
/// focus travel, over the frames moving more than a millimetre: a tenth,
/// less a hair, so that rounding cannot carry a frame over it.
const PACE_GAIN: f64 = 1.099;

/// A ride is done within this of where it is going (m): a tenth of the
/// millimetre the travel itself snaps at its end, so nothing shows.
const RIDE_DONE_M: f64 = 1.0e-4;

/// A great circle of a craft's ball — a leg of the eye's way along it
/// on a focus travel ([`Leg::Sphere`]).
#[derive(Clone, Copy)]
struct GreatCircle {
    /// The CoM's direction to where the leg starts.
    out: DVec3,
    /// The second axis, at right angles to the first: the way sets off
    /// along it.
    aside: DVec3,
    /// How far the leg sweeps along the circle (rad).
    swept: f64,
}

impl GreatCircle {
    /// The offset from the CoM to the point of the ball `angle` (rad)
    /// along the circle, the ball's radius being `rmin`.
    fn offset(&self, rmin: f64, angle: f64) -> DVec3 {
        (self.out * angle.cos() + self.aside * angle.sin()) * rmin
    }
}

/// One leg of the eye's way ([`Way`]).
#[derive(Clone, Copy)]
enum Leg {
    /// Straight from `from` to `to`.
    Line { from: DVec3, to: DVec3 },
    /// Along a great circle of the sphere, from `from` (rad) for `span`
    /// (rad) — the ball's radius being the craft's least distance.
    Sphere {
        circle: GreatCircle,
        from: f64,
        span: f64,
    },
    /// Along the floor's least-distance circle, from bearing `from` (rad,
    /// [`KeptOut::bearing`]) for `round` (rad, signed).
    Floor { from: f64, round: f64 },
}

impl Leg {
    /// The point `share` of the way along the leg: `0` where it starts,
    /// `1` where it ends.
    fn at(&self, kept_out: &mut KeptOut, share: f64) -> DVec3 {
        match *self {
            Self::Line { from, to } => from + (to - from) * share,
            Self::Sphere { circle, from, span } => kept_out.on_ball(&circle, from + span * share),
            Self::Floor { from, round } => kept_out.on_circle(from + round * share),
        }
    }
}

/// The eye's way from where it is to where it is going on a focus
/// travel: legs end to end, each by its own length, walked from `0` to
/// [`Way::total_m`]. Built afresh every frame from where the eye is.
#[derive(Default)]
struct Way {
    /// The legs, in order, each with its length (m).
    legs: Vec<(Leg, f64)>,
}

impl Way {
    /// Add `leg` of `length_m` on the end; nothing that has no length.
    fn push(&mut self, leg: Leg, length_m: f64) {
        if length_m > 0.0 {
            self.legs.push((leg, length_m));
        }
    }

    /// The way's whole length (m).
    fn total_m(&self) -> f64 {
        self.legs.iter().map(|(_, length_m)| *length_m).sum()
    }

    /// The point `arc_m` (m) along the way, its end at or past the whole
    /// of it; `None` for no way at all.
    fn at(&self, kept_out: &mut KeptOut, arc_m: f64) -> Option<DVec3> {
        let mut left_m = arc_m;
        for &(leg, length_m) in &self.legs {
            if left_m <= length_m {
                return Some(leg.at(kept_out, left_m / length_m));
            }
            left_m -= length_m;
        }
        self.legs.last().map(|&(leg, _)| leg.at(kept_out, 1.0))
    }

    /// Keep the first `arc_m` (m) of the way and no more.
    fn cut(&mut self, arc_m: f64) {
        let mut left_m = arc_m;
        for (i, &(leg, length_m)) in self.legs.iter().enumerate() {
            if left_m < length_m {
                let share = left_m / length_m;
                let leg = match leg {
                    Leg::Line { from, to } => Leg::Line {
                        from,
                        to: from + (to - from) * share,
                    },
                    Leg::Sphere { circle, from, span } => Leg::Sphere {
                        circle,
                        from,
                        span: span * share,
                    },
                    Leg::Floor { from, round } => Leg::Floor {
                        from,
                        round: round * share,
                    },
                };
                self.legs.truncate(i);
                self.legs.push((leg, left_m));
                return;
            }
            left_m -= length_m;
        }
    }

    /// How far along the way — all of it on the craft's ball — the first
    /// point is from which the straight line to `target`, a point
    /// outside the ball, does not enter it ([`KeptOut::clears`]), and
    /// that point. The way runs along the ball up to there and straight
    /// on from it.
    fn clear_at(&self, kept_out: &mut KeptOut, target: DVec3) -> (f64, DVec3) {
        let mut gone_m = 0.0;
        for &(leg, length_m) in &self.legs {
            let end = leg.at(kept_out, 1.0);
            if !kept_out.clears(end, target) {
                gone_m += length_m;
                continue;
            }
            let start = leg.at(kept_out, 0.0);
            if kept_out.clears(start, target) {
                return (gone_m, start);
            }
            // Between one that is blocked and one that clears: where it
            // turns.
            let (mut blocked, mut clear) = (0.0, 1.0);
            for _ in 0..CLEAR_BISECTIONS {
                let mid = 0.5 * (blocked + clear);
                let at = leg.at(kept_out, mid);
                if kept_out.clears(at, target) {
                    clear = mid;
                } else {
                    blocked = mid;
                }
            }
            let at = leg.at(kept_out, clear);
            return (gone_m + length_m * clear, at);
        }
        let end_m = self.total_m();
        (end_m, self.at(kept_out, end_m).unwrap_or(target))
    }
}

/// How much of what it has left the eye keeps of a frame riding toward a
/// place it must be at by the time the travel settles: the travel's own
/// share squared — its remainder falls off twice as fast as the
/// travel's — where that is there in time, and faster where it would not
/// be. `left_m` is what the eye has left, `remaining_m` what the travel
/// has, and a frame of the travel closes `share` of what is left.
fn ride_taper(left_m: f64, remaining_m: f64, share: f64) -> f64 {
    let keeps = 1.0 - share;
    let mut taper = keeps * keeps;
    if left_m > RIDE_DONE_M && remaining_m > TRAVEL_DONE_M && keeps > 0.0 {
        // Frames until the travel is under a millimetre:
        // `remaining_m · keeps^n` drops under `TRAVEL_DONE_M` at `n`.
        let frames = (TRAVEL_DONE_M / remaining_m).ln() / keeps.ln();
        // The most a frame may keep and be within `RIDE_DONE_M` of the
        // end by then: `left_m · taper^frames` under it.
        if frames.is_finite() && frames > 0.0 {
            taper = taper.min(((RIDE_DONE_M / left_m).ln() / frames).exp());
        }
    }
    taper.clamp(0.0, 1.0)
}

/// What paces the eye on a frame of a focus travel ([`KeepFrame::eye`]).
#[derive(Clone, Copy)]
struct Pace {
    /// The eye's move on the frame before (m): `None` where that frame
    /// put no time in or was the focus before's — the frame of a change
    /// is a travel's first, and what the eye moved on it was the last
    /// focus's.
    moved_m: Option<f64>,
    /// The travel's share of what is left that the frame before closed
    /// and that this frame closes.
    share: (f64, f64),
    /// How far the anchor still has to travel (m).
    remaining_m: f64,
    /// How far the anchor moved this frame (m).
    anchor_step_m: f64,
}

impl Pace {
    /// How far the eye may move this frame (m), as the eye itself moves:
    /// a tenth beyond its move on the frame before — the same move read at
    /// this frame's time, by the two frames' travel shares — or, where
    /// that frame was no frame of this travel's and so bounds nothing,
    /// beyond the travel's own step this frame.
    /// A frame that moved less than a millimetre bounds nothing
    /// ([`TRAVEL_DONE_M`]).
    fn cap_m(&self, step_m: f64) -> f64 {
        let base_m = self
            .moved_m
            .unwrap_or(step_m.max(self.anchor_step_m))
            .max(TRAVEL_DONE_M);
        let (was, now) = self.share;
        let read_m = if was > 0.0 && now > 0.0 {
            base_m * (now / was).clamp(1.0e-3, 1.0e3)
        } else {
            base_m
        };
        PACE_GAIN * read_m
    }

    /// How much of what it has left the eye keeps of this frame, to be
    /// within [`RIDE_DONE_M`] of where it is going by the time the travel
    /// settles ([`ride_taper`]).
    fn taper(&self, left_m: f64) -> f64 {
        ride_taper(left_m, self.remaining_m, self.share.1)
    }
}

/// The craft an eye is kept out of: the least-distance rule's measure of
/// the ball about it — its sphere, and the floor under that sphere: a
/// line along the floor from the point under the craft, out along the
/// yaw's side of it, and the yaw's own side across that line.
struct KeptOut<'a> {
    /// The craft's CoM.
    com: DVec3,
    /// The craft's least distance ([`min_distance`]) (m).
    rmin: f64,
    /// Out along the yaw's side of the craft — at rest the side of it the
    /// eye orbits on.
    along: DVec3,
    /// The yaw's own side, `along × up`: an increasing yaw moves the eye
    /// this way.
    side: DVec3,
    ray: FloorRay<'a>,
}

impl KeptOut<'_> {
    /// The measure for a craft whose CoM is `com`, on the floor under it.
    /// `None` without a usable floor there.
    fn under(
        com: DVec3,
        rmin: f64,
        yaw_dir: DVec3,
        centre: DVec3,
        floor: &mut dyn FnMut(DVec3) -> f64,
    ) -> Option<KeptOut<'_>> {
        let rel = com - centre;
        if rel.norm_squared() <= 0.0 {
            return None;
        }
        let up = rel / rel.norm();
        let floor_r = floor(up);
        if !floor_r.is_finite() || floor_r <= 0.0 {
            return None;
        }
        let along = [yaw_dir, DVec3::Z, DVec3::X]
            .into_iter()
            .map(|v| v - up * v.dot(up))
            .find(|v| v.norm_squared() > 1.0e-24)
            .unwrap_or(DVec3::X);
        let along = along / along.norm();
        Some(KeptOut {
            com,
            rmin,
            side: along.cross(up),
            along,
            ray: FloorRay {
                centre,
                up,
                along,
                floor_r,
                floor,
            },
        })
    }

    /// How far `point` is out along the yaw's side of the craft and how
    /// far to the yaw's own side of that line (m): its angles from the
    /// vertical through the craft, times the floor's radius under it.
    fn place(&self, point: DVec3) -> (f64, f64) {
        let out = point - self.ray.centre;
        let over = out.dot(self.ray.up);
        (
            out.dot(self.along).atan2(over) * self.ray.floor_r,
            out.dot(self.side).atan2(over) * self.ray.floor_r,
        )
    }

    /// `point`'s bearing round the craft (rad): from the yaw's side of it
    /// toward the yaw's own — the angle [`Self::on_circle`] takes.
    fn bearing(&self, point: DVec3) -> f64 {
        let (out, aside) = self.place(point);
        aside.atan2(out)
    }

    /// Whether `point` is inside the craft's ball: nearer than the least
    /// distance from the CoM.
    fn inside_ball(&self, point: DVec3) -> bool {
        (point - self.com).norm() < self.rmin
    }

    /// Whether `point` is on the craft's ball — its surface — to within
    /// [`ON_BALL_M`]: a way along the ball starts from such a point.
    fn is_on_ball(&self, point: DVec3) -> bool {
        ((point - self.com).norm() - self.rmin).abs() <= ON_BALL_M
    }

    /// Whether `point` is under the floor, or on it to within
    /// [`FLOOR_HAIR_M`].
    fn under_floor(&mut self, point: DVec3) -> bool {
        let out = point - self.ray.centre;
        let r = out.norm();
        r > 0.0 && r < (self.ray.floor)(out / r) + FLOOR_HAIR_M
    }

    /// Whether the straight line from `from` to `to` stays out of the
    /// craft's ball: neither end inside it, and the closest the line
    /// comes to the CoM no nearer than the least distance — as positions
    /// measure it, a point of the ball itself being on it. For a line off
    /// the ball toward a point outside it, that is setting off along the
    /// ball's surface or away from its centre, never in toward it.
    fn clears(&self, from: DVec3, to: DVec3) -> bool {
        let (line, at) = (to - from, from - self.com);
        let closest = if line.norm_squared() > 0.0 {
            (-at.dot(line) / line.norm_squared()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (at + line * closest).norm() >= self.rmin - ON_BALL_M
    }

    /// The point `offset` from the CoM gives — the offset lengthened by
    /// [`SKIRT_PRECISION_M`] at a time until that point is the least
    /// distance or more from the CoM as positions measure it: a point
    /// built on the ball can round a hair inside it.
    fn clear_of(&self, mut offset: DVec3) -> DVec3 {
        let step_m = SKIRT_PRECISION_M.max(f64::EPSILON * 8.0 * self.ray.floor_r);
        let mut point = self.com + offset;
        for _ in 0..16 {
            if (point - self.com).norm() >= self.rmin {
                break;
            }
            offset = offset + offset * (step_m / offset.norm());
            point = self.com + offset;
        }
        point
    }

    /// The point of the ball `angle` (rad) along `circle` from the entry.
    fn on_ball(&self, circle: &GreatCircle, angle: f64) -> DVec3 {
        self.clear_of(circle.offset(self.rmin, angle))
    }

    /// The point of the craft's ball on the way out from the CoM
    /// through `point`: on the sphere where that is over the floor, on
    /// the floor's least-distance circle at `point`'s bearing where it is
    /// under it ([`Self::on_circle`]). For a point inside the ball it is
    /// its nearest way out — the eye's distance to the CoM grows all
    /// along it, and nothing of the craft is in the way; for one outside
    /// it, the nearest point of the ball. A point on the vertical through
    /// the CoM leaves toward the yaw's side.
    fn outward(&mut self, point: DVec3) -> DVec3 {
        let to_ball = point - self.com;
        let to_ball = if to_ball.norm_squared() > (1.0e-9 * self.rmin).powi(2) {
            to_ball
        } else {
            self.along * self.rmin
        };
        let out = to_ball / to_ball.norm();
        if self.under_floor(self.com + out * self.rmin) {
            let (along, aside) = self.place(point);
            let bearing = if along.hypot(aside) > 1.0e-9 * self.ray.floor_r {
                aside.atan2(along)
            } else {
                0.0
            };
            self.on_circle(bearing)
        } else {
            self.clear_of(out * self.rmin)
        }
    }

    /// Where the straight step `from`…`to` crosses into the craft's
    /// ball — `from` outside it and `to` inside: the share of the step
    /// that is outside, and the crossing, a point of the ball.
    fn entry(&self, from: DVec3, to: DVec3) -> (f64, DVec3) {
        let step = to - from;
        let (mut outside, mut inside) = (0.0, 1.0);
        for _ in 0..64 {
            let mid = 0.5 * (outside + inside);
            if self.inside_ball(from + step * mid) {
                inside = mid;
            } else {
                outside = mid;
            }
        }
        (inside, from + step * inside)
    }

    /// The zone's edges along the yaw's line (m): how far out on the yaw's
    /// side the floor first is `rmin` from the CoM — the turn — and as far
    /// on the far side. The point under the craft is inside the zone.
    fn edges(&mut self) -> (f64, f64) {
        let turn_m = clear_from(&mut self.ray, self.com, self.rmin, 0.0);
        self.ray.along = -self.along;
        let far_m = clear_from(&mut self.ray, self.com, self.rmin, 0.0);
        (turn_m, far_m)
    }

    /// The first point along the floor on bearing `bearing` from the turn
    /// point toward the yaw's own side that is `rmin` or more from the
    /// CoM — the circle's own point there.
    fn on_circle(&mut self, bearing: f64) -> DVec3 {
        self.ray.along = self.along * bearing.cos() + self.side * bearing.sin();
        let reach_m = clear_from(&mut self.ray, self.com, self.rmin, 0.0);
        self.ray.at(reach_m)
    }

    /// [`skirt_eye`]'s rule for `lifted`: the lifted orbit point itself
    /// while it is `rmin` or more from the CoM, and the eye that goes
    /// round the craft for one that is nearer.
    fn rule(&mut self, lifted: DVec3) -> DVec3 {
        if (lifted - self.com).norm() >= self.rmin {
            return lifted;
        }
        self.ray.along = self.along;
        let h = self.place(lifted).0;
        if (self.ray.at(0.0) - self.com).norm() >= self.rmin {
            // The band's far edge, from where the eye is.
            let turn_m = clear_from(&mut self.ray, self.com, self.rmin, h);
            let mut x = 2.0 * turn_m - h;
            if (self.ray.at(x) - self.com).norm() < self.rmin {
                x = clear_from(&mut self.ray, self.com, self.rmin, x);
            }
            return self.ray.at(x);
        }
        // The zone's two edges and the circle between them are the craft's
        // and the floor's alone — the same for every eye: each the first
        // point along its own bearing from the craft's ground point that
        // is `rmin` from the CoM.
        let (turn_m, far_m) = self.edges();
        let u = (2.0 * (turn_m - h) / (turn_m + far_m)).clamp(0.0, 2.0);
        self.on_circle(skirt_azimuth(u))
    }

    /// The way the eye sets off round the ball from `entered`, a point of
    /// its surface, for a way that travelled `step` into it: the share of
    /// the step tangent to the ball there. It decides a diameter alone
    /// ([`Self::round_way`]) — a way whose two ways round are as long as
    /// each other; every other way goes round by the nearer. A way aimed
    /// within [`DIAMETER_SHARE`] of the CoM is a diameter and takes the
    /// yaw's own side; so does one that runs along that side itself, over
    /// the top.
    fn way_round(&self, entered: DVec3, step: DVec3) -> DVec3 {
        let to_ball = entered - self.com;
        let out = to_ball / to_ball.norm();
        let mut way = step - out * step.dot(out);
        if way.norm() > DIAMETER_SHARE * step.norm() {
            return way / way.norm();
        }
        way = self.side - out * self.side.dot(out);
        if way.norm_squared() <= 1.0e-24 {
            way = self.ray.up - out * self.ray.up.dot(out);
        }
        if way.norm_squared() <= 1.0e-24 {
            way = self.along - out * self.along.dot(out);
        }
        way / way.norm()
    }

    /// The way along the craft's ball from `from` to `to` — both points
    /// of the ball — going round it by the NEARER way, always: the great
    /// circle through both, from the one toward the other, for an arc of
    /// half a turn at most. Only a true diameter — its two ways round as
    /// long as each other — is decided by the way the eye set off
    /// (`sets_off`, [`Self::way_round`]), and a way that ends where it
    /// starts has nothing to go round. Where the great circle is under
    /// the floor the eye rides the floor's least-distance circle instead:
    /// down the sphere to the floor, along the circle, and up the sphere
    /// on to `to` ([`Self::under_the_floor`]) — so a way from the floor's
    /// circle to it, as the eye's is wherever the orbit point's own way
    /// is on the ground, is the circle's the whole way. `floor_side` is
    /// the ride's memory of which way round the circle the eye took, to
    /// go on round it. The eye covers the same length of the way for the
    /// same progress, whichever leg it is on.
    fn round_way(&mut self, from: DVec3, sets_off: DVec3, to: DVec3, floor_side: &mut f64) -> Way {
        let to_ball = from - self.com;
        let to_target = to - self.com;
        let (out, reach) = (to_ball / to_ball.norm(), to_target / to_target.norm());
        let facing = reach.dot(out);
        let mut aside = reach - out * facing;
        if facing > 0.0 {
            // The same point twice has nothing to go round; next to it,
            // the short arc — which way it lies is plain.
            if aside.norm_squared() <= SAME_POINT_SHARE.powi(2) {
                return Way::default();
            }
        } else if aside.norm() <= DIAMETER_SHARE {
            aside = sets_off;
        }
        let aside = aside / aside.norm();
        let swept = reach.dot(aside).atan2(facing);
        let circle = GreatCircle {
            out,
            aside,
            swept: if swept < 0.0 {
                swept + 2.0 * core::f64::consts::PI
            } else {
                swept
            },
        };
        let Some((into_floor, off_floor)) = self.under_the_floor(&circle) else {
            let mut way = Way::default();
            way.push(
                Leg::Sphere {
                    circle,
                    from: 0.0,
                    span: circle.swept,
                },
                self.rmin * circle.swept,
            );
            return way;
        };
        let (full, turn) = (core::f64::consts::PI, 2.0 * core::f64::consts::PI);
        let (enters_floor, leaves_floor) = (
            self.bearing(self.com + circle.offset(self.rmin, into_floor)),
            self.bearing(self.com + circle.offset(self.rmin, off_floor)),
        );
        let mut round = wrap_angle(leaves_floor - enters_floor);
        if *floor_side == 0.0 {
            // Two ways round as long as each other — a diameter: the
            // yaw's own side is the one taken.
            if round.abs() >= full - 1.0e-9 && (enters_floor + 0.5 * round).sin() < 0.0 {
                round -= turn * round.signum();
            }
            *floor_side = round.signum();
        } else if round * *floor_side < 0.0 {
            // The way round was taken where the eye came to the circle:
            // it goes on round that way to wherever it leaves.
            round -= turn * round.signum();
        }
        let from_floor = self.on_circle(enters_floor);
        let to_floor = self.on_circle(enters_floor + round);
        let half = 0.5 * round;
        let along_m = (to_floor - from_floor).norm()
            * if half.sin().abs() > 1.0e-9 {
                half / half.sin()
            } else {
                1.0
            };
        let mut way = Way::default();
        way.push(
            Leg::Sphere {
                circle,
                from: 0.0,
                span: into_floor,
            },
            self.rmin * into_floor,
        );
        way.push(
            Leg::Floor {
                from: enters_floor,
                round,
            },
            along_m,
        );
        way.push(
            Leg::Sphere {
                circle,
                from: off_floor,
                span: circle.swept - off_floor,
            },
            self.rmin * (circle.swept - off_floor),
        );
        way
    }

    /// Whether the point of the great circle `circle` — of the craft's
    /// ball — `angle` (rad) along it from its start is under the floor.
    fn under_at(&mut self, circle: &GreatCircle, angle: f64) -> bool {
        self.under_floor(self.com + circle.offset(self.rmin, angle))
    }

    /// Where the great circle `circle` is under the floor, sampled along
    /// the way and each edge of that then bisected: the angle from the
    /// start at which it goes under and the angle at which it comes up
    /// again. `None` where the whole way is over the sphere. A way that
    /// starts on the floor is on the circle from there, and one that ends
    /// on it to the end.
    fn under_the_floor(&mut self, circle: &GreatCircle) -> Option<(f64, f64)> {
        // The first and the last sample along the way that is under it.
        let (mut first, mut last) = (None, 0);
        for i in 0..=ROUND_SAMPLES {
            if self.under_at(circle, circle.swept * (i as f64 / ROUND_SAMPLES as f64)) {
                if first.is_none() {
                    first = Some(i);
                }
                last = i;
            }
        }
        let first = first?;
        let sample = |i: f64| circle.swept * (i / ROUND_SAMPLES as f64);
        // The angle between one that is under the floor and one that is
        // over it where it crosses: the last that is under.
        let edge = |kept_out: &mut Self, under_at: f64, over: f64| {
            let (mut under_at, mut over) = (under_at, over);
            for _ in 0..ROUND_BISECTIONS {
                let mid = 0.5 * (under_at + over);
                if kept_out.under_at(circle, mid) {
                    under_at = mid;
                } else {
                    over = mid;
                }
            }
            under_at
        };
        Some((
            // Where the way goes under: between the first sample that is
            // and the one before it — nothing, where it starts there.
            if first == 0 {
                0.0
            } else {
                edge(self, sample(first as f64), sample(first as f64 - 1.0))
            },
            // Where it comes up again: between the last sample that is
            // under and the one after it — the end, where it ends there.
            if last == ROUND_SAMPLES {
                circle.swept
            } else {
                edge(self, sample(last as f64), sample(last as f64 + 1.0))
            },
        ))
    }

    /// The eye's way to `target` — a carried point outside the craft's
    /// ball — from an eye at `eye` that is not at it. Straight there
    /// where that line stays out of the ball; else out to the ball first
    /// while the eye is not on it ([`Self::outward`]), along it by the
    /// nearer way to the first point from which the straight line to
    /// the target clears it ([`Way::clear_at`]), and straight on from
    /// there. `floor_side` is the ride's memory of the way round the
    /// floor's circle.
    fn way_to(&mut self, eye: DVec3, target: DVec3, floor_side: &mut f64) -> Way {
        let mut way = Way::default();
        if self.clears(eye, target) {
            way.push(
                Leg::Line {
                    from: eye,
                    to: target,
                },
                (target - eye).norm(),
            );
            return way;
        }
        let from = if self.is_on_ball(eye) {
            eye
        } else {
            self.outward(eye)
        };
        way.push(
            Leg::Line {
                from: eye,
                to: from,
            },
            (from - eye).norm(),
        );
        let mut straight_from = from;
        if !self.clears(from, target) {
            let shadow = self.outward(target);
            let mut round =
                self.round_way(from, self.way_round(from, self.side), shadow, floor_side);
            let (clear_m, at) = round.clear_at(self, target);
            round.cut(clear_m);
            way.legs.append(&mut round.legs);
            straight_from = at;
        }
        way.push(
            Leg::Line {
                from: straight_from,
                to: target,
            },
            (target - straight_from).norm(),
        );
        way
    }

    /// The orbit point at rest — about `rest`, a focus's resting anchor,
    /// on the body spinning about `axis` — lifted onto the floor, and the
    /// eye the rule at rest puts for it.
    fn at_rest(
        &mut self,
        orbit: &Orbit,
        rest: DVec3,
        centre: DVec3,
        axis: DVec3,
    ) -> (DVec3, DVec3) {
        let rest_level = level_frame(rest, Some(centre), axis);
        let (at_rest, _) = solve_orbit(orbit, rest, &rest_level, centre, &mut *self.ray.floor);
        (at_rest, self.rule(at_rest))
    }
}

/// An angle in radians wound into `(-π, π]`.
fn wrap_angle(radians: f64) -> f64 {
    let turn = 2.0 * core::f64::consts::PI;
    radians - turn * ((radians - core::f64::consts::PI) / turn).ceil()
}

/// The lifted eye kept out of the craft. The floor lift can bring the
/// eye nearer than `rmin` — the craft's least distance ([`min_distance`])
/// — to the craft's CoM `com`: dragged straight down, the lifted eye ends
/// over the craft's own ground point, a stone's throw from the CoM or
/// inside the craft. So the eye goes round the craft instead, along the
/// floor.
///
/// From the floor point `n` under the craft, along `yaw_dir`'s horizontal
/// — at rest the direction from `n` to the eye, the side of the craft it
/// orbits on — the orbit point is `h` metres along the floor, signed. The
/// zone is the stretch of that line nearer than `rmin` from the CoM: from
/// the turn point on the yaw's side to the far edge on the other. Outside
/// it the eye is the orbit point. Inside it the eye rides the floor at
/// exactly the least distance — the circle around `n` where the floor is
/// `rmin` from the CoM — entering at the turn point, going round the
/// yaw's own side (the way an increasing yaw moves the eye) and leaving
/// at the far edge. Its place on the circle is [`skirt_azimuth`]'s of its
/// place in the zone, a monotonic function of the intent: the eye never
/// turns back on a drag that goes on the same way, and zooming in never
/// carries it away from the craft. On flat ground the turn is
/// `sqrt(rmin² − H²)` out, `H` the CoM's signed height over `n`; here both
/// edges and the circle itself are measured on the real floor, so the
/// rule holds on a slope too. An eye already `rmin` or more from the
/// craft is handed back.
///
/// A floor whose point under the craft already clears it — a craft high
/// over rising ground — has no zone about the craft. The eye is on a band
/// of that ground instead, and is put past the band's far edge.
fn skirt_eye(
    lifted: DVec3,
    com: DVec3,
    rmin: f64,
    yaw_dir: DVec3,
    centre: DVec3,
    floor: &mut dyn FnMut(DVec3) -> f64,
) -> DVec3 {
    if (lifted - com).norm() >= rmin {
        return lifted;
    }
    KeptOut::under(com, rmin, yaw_dir, centre, floor)
        .map_or(lifted, |mut kept_out| kept_out.rule(lifted))
}

/// The focus as its keep-out reads it, for an orbit point on the floor or
/// in the air, and the frame the keep-out is read on.
struct KeepFrame<'a> {
    /// The focus's CoM, its least distance and the body's centre.
    com: DVec3,
    rmin: f64,
    centre: DVec3,
    axis: DVec3,
    /// The orbit — the angles and the distance — and the focus's resting
    /// anchor.
    orbit: &'a Orbit,
    rest: DVec3,
    /// The yaw's horizontal.
    yaw_dir: DVec3,
    /// The eye on the frame before, and the carried point on it: `None`
    /// on a first focus, and on the frame of a change the eye itself —
    /// where the travel starts from.
    before: DVec3,
    previous: Option<DVec3>,
    /// What paces the eye this frame.
    pace: Pace,
}

/// How far the eye goes along `way` this frame, setting out from `from`:
/// of the `asked_m` of the way — never more than the `left_m` of it there
/// is — as much as its pace allows, read as the eye itself moves, in a
/// straight line from where it was ([`Pace::cap_m`]); nothing at all
/// where the frame put no time in. Where the way bends more is gone
/// along it for the same move.
fn paced(
    way: &Way,
    kept_out: &mut KeptOut,
    from: DVec3,
    asked_m: f64,
    left_m: f64,
    step_m: f64,
    pace: &Pace,
) -> f64 {
    if pace.share.1 <= 0.0 {
        return 0.0;
    }
    let asked_m = asked_m.max(0.0).min(left_m);
    let cap_m = pace.cap_m(step_m);
    let move_m = |kept_out: &mut KeptOut, arc_m: f64| {
        way.at(kept_out, arc_m).map_or(0.0, |at| (at - from).norm())
    };
    if move_m(kept_out, asked_m) <= cap_m {
        return asked_m;
    }
    // Between how far it is and how far it may: the furthest that stays
    // within its pace. Nothing is always within it.
    let (mut within, mut beyond) = (0.0, asked_m);
    for _ in 0..PACE_BISECTIONS {
        let mid = 0.5 * (within + beyond);
        if move_m(kept_out, mid) > cap_m {
            beyond = mid;
        } else {
            within = mid;
        }
    }
    within
}

impl KeepFrame<'_> {
    /// The eye for `carried` — the orbit point about the anchor, lifted
    /// onto the floor while it is under it, with whatever is still carried
    /// of the previous focus's skirt — under the focus's keep-out.
    ///
    /// - At rest: the rule itself ([`KeptOut::rule`]) wherever the orbit
    ///   point was lifted into the craft's zone. A free eye at rest is
    ///   never nearer than the least distance, and is handed back.
    /// - On a travel while the carried point is inside the craft's ball:
    ///   the eye rides the ball to where the carried point's way leads
    ///   ([`KeptOut::ride`]) — from the air as from the floor.
    /// - On a travel while the carried point is outside the ball and the
    ///   eye is not at it — it began the travel inside the ball, or was
    ///   still riding it when the carried point left — the eye makes for
    ///   the carried point round the craft ([`KeptOut::chase`]).
    /// - Everywhere else on a travel, and wherever there is no usable
    ///   floor to measure the ball on: the carried point itself.
    fn eye(
        &self,
        carried: DVec3,
        travel: &mut FocusTravel,
        floor: &mut dyn FnMut(DVec3) -> f64,
    ) -> DVec3 {
        let on_travel = travel.began_inside || travel.half_life_s.is_some();
        let (eye, ride) = if !on_travel {
            travel.ride = None;
            return skirt_eye(
                carried,
                self.com,
                self.rmin,
                self.yaw_dir,
                self.centre,
                floor,
            );
        } else if let Some(mut kept_out) =
            KeptOut::under(self.com, self.rmin, self.yaw_dir, self.centre, floor)
        {
            let step_m = self.previous.map_or(0.0, |p| (carried - p).norm());
            if kept_out.inside_ball(carried) {
                kept_out.ride(carried, step_m, self, travel.ride)
            } else if travel.ride.is_some() || travel.began_inside {
                kept_out.chase(carried, step_m, self, travel.ride)
            } else {
                (carried, None)
            }
        } else {
            (carried, None)
        };
        if ride.is_none() {
            travel.began_inside = false;
        }
        travel.ride = ride;
        eye
    }
}

impl KeptOut<'_> {
    /// The eye for a carried point that is INSIDE the craft's ball —
    /// `carried`, having moved `step_m` this frame — on a focus travel,
    /// and the ride's memory (`ride`: what has come of it so far, `None`
    /// setting out).
    ///
    /// The eye's way is from where it is: out to the ball while it is not
    /// on it — where the carried point's step crossed in, for an eye that
    /// was at the carried point; the eye's nearest way out
    /// ([`Self::outward`]), for one that began the travel inside — and
    /// then along the ball by the nearer way ([`Self::round_way`]) to
    /// where the carried point's way leads: out of the ball, or its rest
    /// and the rule's own eye there. Before it is on the ball the eye
    /// only moves away from the CoM; on it, it is exactly the least
    /// distance out.
    ///
    /// What it covers of the way this frame is the lesser of what its
    /// pace allows ([`Pace`]) and what the carried point's own way asks:
    ///
    /// - The way leaves the ball: the eye keeps its share of the carried
    ///   point's remaining run to where the way leaves — for the carried
    ///   point's step, as much further of its own way as it has left to
    ///   the carried point's — so it is at the way out about when the
    ///   carried point is, moving no faster than it. Behind, from a frame
    ///   its pace capped, it goes that much faster until it is not.
    /// - The way comes to rest inside the ball: the eye's remainder keeps
    ///   [`Pace::taper`] of a frame, so it is at the at-rest eye before
    ///   the travel settles.
    fn ride(
        &mut self,
        carried: DVec3,
        step_m: f64,
        frame: &KeepFrame,
        ride: Option<Ride>,
    ) -> (DVec3, Option<Ride>) {
        let (at_rest, rest_eye) = self.at_rest(frame.orbit, frame.rest, frame.centre, frame.axis);
        // What lies ahead for the carried point: where its way leaves the
        // ball — the first point along it that is the least distance or
        // more from the CoM — or, coming to rest inside it, the rest
        // itself and the rule's own eye there.
        let to_rest = at_rest - carried;
        let (ahead_m, target, leaves) = if self.inside_ball(at_rest) {
            (to_rest.norm(), rest_eye, false)
        } else {
            let step = carried - frame.previous.unwrap_or(carried);
            let way = if step_m > 0.0 {
                step / step_m
            } else if to_rest.norm() > 0.0 {
                to_rest / to_rest.norm()
            } else {
                self.along
            };
            let (mut inside, mut outside) = (0.0, 4.0 * self.rmin);
            for _ in 0..64 {
                let mid = 0.5 * (inside + outside);
                if self.inside_ball(carried + way * mid) {
                    inside = mid;
                } else {
                    outside = mid;
                }
            }
            (outside, carried + way * outside, true)
        };
        // Where the way takes off from: the eye itself while it is on the
        // ball; where the line on to the carried point crosses into it,
        // for an eye outside it; the eye's nearest way out, for one inside.
        let eye = frame.before;
        let take_off = if self.is_on_ball(eye) {
            eye
        } else if self.inside_ball(eye) {
            self.outward(eye)
        } else {
            self.clear_of(self.entry(eye, carried).1 - self.com)
        };
        // A ride setting out takes the way the line on to the carried
        // point sets off; one in hand goes on as it began.
        let mut ride = ride.unwrap_or(Ride {
            sets_off: self.way_round(take_off, carried - eye),
            floor_side: 0.0,
            left_m: 0.0,
            ahead_m: None,
        });
        let mut way = Way::default();
        way.push(
            Leg::Line {
                from: eye,
                to: take_off,
            },
            (take_off - eye).norm(),
        );
        let round = self.round_way(take_off, ride.sets_off, target, &mut ride.floor_side);
        way.legs.extend(round.legs);
        let left_m = way.total_m();
        // What the eye had left of its way and the carried point of its
        // run on the frame before: what the ride remembers of a frame that
        // put time in, or — setting out — the eye's way as it stands now
        // and the carried point's step this frame and what it has left.
        let was_run = ride.ahead_m.filter(|_| frame.pace.share.0 > 0.0);
        let (was_left_m, was_ahead_m) =
            was_run.map_or((left_m, step_m + ahead_m), |ahead| (ride.left_m, ahead));
        let asked_m = if leaves {
            if was_ahead_m > 0.0 {
                was_left_m * step_m / was_ahead_m
            } else {
                0.0
            }
        } else {
            left_m * (1.0 - frame.pace.taper(left_m))
        };
        let moved_m = paced(&way, self, eye, asked_m, left_m, step_m, &frame.pace);
        // Where nothing is gone, nothing moves: the eye stays exactly.
        let eye = if moved_m > 0.0 {
            way.at(self, moved_m).unwrap_or(eye)
        } else {
            eye
        };
        ride.left_m = left_m - moved_m;
        ride.ahead_m = (leaves && frame.pace.share.1 > 0.0).then_some(ahead_m);
        (eye, Some(ride))
    }

    /// The eye for a carried point that is OUTSIDE the craft's ball while
    /// the eye is not at it — it began the travel inside the ball, or was
    /// still riding the ball when the carried point left it — and the
    /// ride's memory, `None` once the eye is at the carried point. The
    /// eye makes for the carried point along [`Self::way_to`]: out of the
    /// ball by the nearest way, along it to where a straight line to the
    /// carried point clears it, and straight on — never through the
    /// craft, and never nearer to its centre than it was. It goes the
    /// carried point's own step and closes [`Pace::taper`] of what it had
    /// left, so it is at the carried point before the travel settles —
    /// and no faster than its pace allows.
    fn chase(
        &mut self,
        carried: DVec3,
        step_m: f64,
        frame: &KeepFrame,
        ride: Option<Ride>,
    ) -> (DVec3, Option<Ride>) {
        let eye = frame.before;
        let gap_m = ride.map_or((carried - eye).norm(), |ride| ride.left_m);
        let (sets_off, mut floor_side) =
            ride.map_or((self.side, 0.0), |ride| (ride.sets_off, ride.floor_side));
        let way = self.way_to(eye, carried, &mut floor_side);
        let left_m = way.total_m();
        let asked_m = step_m + (1.0 - frame.pace.taper(gap_m)) * gap_m;
        let moved_m = paced(&way, self, eye, asked_m, left_m, step_m, &frame.pace);
        let ride = Ride {
            sets_off,
            floor_side,
            left_m: left_m - moved_m,
            ahead_m: None,
        };
        if ride.left_m <= RIDE_DONE_M {
            return (carried, None);
        }
        let eye = if moved_m > 0.0 {
            way.at(self, moved_m).unwrap_or(carried)
        } else {
            eye
        };
        (eye, Some(ride))
    }
}

/// The solved chase camera for an eye at `eye` looking at `look_at` — the
/// view basis (columns = view axes in world, c0×c1 = c2): z_out points
/// look_at → eye, "up" is the local surface up, so the horizon stays
/// level wherever the camera sits.
fn chase_camera(
    eye: DVec3,
    look_at: DVec3,
    level: &LevelFrame,
    viewport_px: (f64, f64),
) -> MapCameraF64 {
    let z_out = {
        let d = eye - look_at;
        if d.norm_squared() > 1.0e-12 {
            d / d.norm()
        } else {
            level.up
        }
    };
    let right = {
        let r = level.up.cross(z_out);
        if r.norm_squared() > 1.0e-12 {
            r / r.norm()
        } else {
            level.east
        }
    };
    let upv = z_out.cross(right);
    let view = [
        [right.x, right.y, right.z, 0.0],
        [upv.x, upv.y, upv.z, 0.0],
        [z_out.x, z_out.y, z_out.z, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let aspect = (viewport_px.0 / viewport_px.1.max(1.0)).max(1e-6);
    let proj = perspective_revz(FLIGHT_FOV_Y, aspect, FLIGHT_NEAR_M);
    MapCameraF64 {
        view_proj: mat4_mul(&proj, &view),
        eye,
        // The POST-floor eye–look-at distance, not the eased one — the
        // map convention (`distance_m` = actual |eye − focus|); with the
        // camera looking at what it orbits it is the eased distance
        // exactly. `cascade_uniforms`' no-focus fallback recovers the
        // look-at as `eye − z_out·distance_m` from it.
        distance_m: (eye - look_at).norm(),
        valid: true,
    }
}

/// What one frame's chase-camera step ([`step_chase_camera`]) is told.
struct ChaseFrame {
    /// The focused vessel's id.
    id: u64,
    /// The focus's CoM as this frame draws it — what the camera looks
    /// at, and the anchor it rests on at or above the floor.
    com: DVec3,
    /// The focus's bounding radius about its CoM (m).
    radius_m: f64,
    /// The body's centre — `None` without physics (and then no floor).
    centre: Option<DVec3>,
    /// The body's spin axis — world +Z without physics.
    axis: DVec3,
    /// The frame's wall time (ms) — what the easing advances by.
    dt_wall_ms: f64,
}

/// One frame's chase-camera step, everything that moves the camera
/// besides the pointer. `floor` gives the least radius the eye may have
/// from the body's centre along a unit direction ([`eye_floor`] over the
/// heightfield); it is the only terrain the step sees, so the step is
/// pure in it. The focus edge is the camera's own memory of last frame
/// ([`FocusTravel`]), so the step never depends on the order the
/// Presentation systems run in.
///
/// - **Framing.** The frame a vessel takes the focus, its target
///   distance is [`frame_distance`] — the old snap to a fixed range put
///   a new focus at the old vessel's distance (C19-F4).
/// - **Zoom.** The target is held inside the limits and the distance
///   eases toward it by this frame's share.
/// - **Anchor.** The focus's resting anchor ([`resting_anchor`]): its
///   CoM, lifted over the ground while the craft is under the floor.
/// - **Travel.** On a focus CHANGE (not a first focus) the anchor starts
///   where the last frame's eye orbited and eases to the new craft's
///   resting anchor by the share of its own half-life
///   ([`travel_half_life`]), held above the ground while it crosses the
///   planet ([`travel_anchor`]). What the previous focus left the camera
///   looking at, and its eye's offset from its own orbit point, is
///   carried along and handed over by [`travel_blend`] — so the frame of
///   the change moves neither the eye nor the view.
/// - **Look-at.** The craft, never the lifted anchor: the camera looks
///   at what the player is flying.
/// - **Solve.** The eye from the angles and the distance about the
///   anchor — the intent's orbit point, lifted onto the floor while that
///   point is under it ([`solve_orbit`]). The angles are left exactly as
///   the player set them.
/// - **Keep-out.** A focus's keep-out is a ball of its least distance
///   ([`min_distance`]) about its CoM. At rest only the floor lift can
///   take the orbit point into it, and a lifted eye goes round the craft
///   along the floor ([`skirt_eye`]); a free eye at rest is never inside
///   the ball.
/// - **Keep-out on a travel.** On a travel the new craft's keep-out acts
///   at once on the travelling orbit point — with whatever is still
///   carried of the previous craft's skirt, which is kept out of the new
///   craft as well — whether it rides the floor or is in the air. Carried
///   into the ball, the eye rides the ball from where it enters to where
///   the point's own way leaves it ([`KeptOut::ride`],
///   [`KeptOut::round_way`]): the sphere wherever that is above the
///   floor, the floor's least-distance circle wherever it is under it, by
///   the nearer way round and along a diameter by the yaw's own side — so
///   the eye passes over or round the craft, never through it — and is
///   the orbit point itself again on the frame that leaves. Wherever the
///   orbit point comes to rest is where the rule at rest puts the eye,
///   and the eye is there before the travel settles: it ends on the
///   at-rest keep-out exactly.
/// - **Pace.** No frame moves the eye more than a tenth beyond the one
///   before ([`Pace::cap_m`]), over the frames above a millimetre. The
///   ride keeps the orbit point's own pace where it may and, held back,
///   catches up as its pace allows — it is at the way out about when the
///   orbit point is — and what it has left of a ride to a place the eye
///   is to be at falls off by [`ride_taper`] of a frame.
/// - **The eye began inside the ball.** Where the eye itself begins the
///   travel inside the new craft's ball ([`FocusTravel::began_inside`]),
///   the orbit point's own way may run through the craft, so the eye does
///   not follow it: it leaves the ball by the nearest way out
///   ([`KeptOut::outward`]) — its distance to the craft's centre never
///   falls — and, once the orbit point's way has left the ball, makes for
///   the orbit point round the craft ([`KeptOut::chase`]). The frame of
///   the change still moves nothing.
///
/// A harness that supplies no frame time (the render gates) steps
/// nothing: the camera stays exactly where it was put.
fn step_chase_camera(
    input: &mut FlightInput,
    frame: &ChaseFrame,
    floor: &mut dyn FnMut(DVec3) -> f64,
) -> MapCameraF64 {
    // The frame loop's 100 ms clamp (`MAX_FRAME_DT`); a non-finite
    // frame time carries no step, as it carries no interpolation.
    let dt_s = (frame.dt_wall_ms / 1000.0).clamp(0.0, 0.1);
    let dt_s = if dt_s.is_finite() { dt_s } else { 0.0 };
    let k = ease_fraction(dt_s, EASE_HALF_LIFE_S);
    let radius_m = if frame.radius_m.is_finite() {
        frame.radius_m.max(0.0)
    } else {
        0.0
    };
    let rmin = min_distance(radius_m);

    let last = input.travel.last;
    let changed = last.map(|l| l.id) != Some(frame.id);
    if changed {
        input.distance_target_m = frame_distance(radius_m);
    }
    input.distance_target_m = input
        .distance_target_m
        .clamp(min_distance(radius_m), MAX_DISTANCE_M);
    input.distance_m += (input.distance_target_m - input.distance_m) * k;
    let eye_m = input.distance_m;

    let rest = resting_anchor(frame.com, eye_m, frame.centre, floor);
    let travel = &mut input.travel;
    if changed {
        travel.begin(last, rest, frame.com, rmin);
    }
    let travelled = travel
        .half_life_s
        .map_or(0.0, |half_life_s| ease_fraction(dt_s, half_life_s));
    travel.offset = travel.offset * (1.0 - travelled);
    let mut remaining_m = travel.offset.norm();
    if remaining_m < TRAVEL_DONE_M {
        travel.settle();
        remaining_m = 0.0;
    }
    let blend = travel_blend(remaining_m, travel.start_m, eye_m);
    let anchor = travel_anchor(
        rest + travel.offset,
        remaining_m,
        eye_m,
        frame.centre,
        floor,
    );
    let level = level_frame(anchor, frame.centre, frame.axis);
    let look_at = frame.com + (anchor - frame.com + travel.look_carry) * blend;
    // Without a body there is no ground to stay above.
    let mut floored = |dir: DVec3| frame.centre.map_or(f64::NEG_INFINITY, |_| floor(dir));
    let orbit = Orbit {
        yaw: input.yaw,
        pitch: input.pitch,
        distance_m: eye_m,
    };
    let (lifted, on_floor) = solve_orbit(
        &orbit,
        anchor,
        &level,
        frame.centre.unwrap_or(anchor),
        &mut floored,
    );
    // The orbit point with whatever is still carried of the previous
    // focus's skirt — the eye, wherever the craft's keep-out leaves it
    // alone. The focus's keep-out reads that, not the bare orbit point: a
    // carried skirt must not carry the eye into the new craft either. At
    // rest it is the lifted orbit point itself, and a free eye is left
    // alone; on a travel it is kept out of the craft's ball from the air
    // as from the floor. The kept-out eye itself is carried on, so a point
    // the rule puts on the circle is on it to the last digit and not a
    // round trip through the orbit point.
    let carried = lifted + travel.skirt_carry * blend;
    let on_travel = travel.began_inside || travel.half_life_s.is_some();
    // The carried point on the frame before — on the frame of a change
    // the eye itself, where the travel starts from — and the eye on it.
    let previous = last.map(|l| if changed { l.eye } else { l.carried });
    let before = last.map_or(carried, |l| l.eye);
    let eye = if let Some(centre) = frame.centre
        && (on_floor || on_travel)
    {
        KeepFrame {
            com: frame.com,
            rmin,
            centre,
            axis: frame.axis,
            orbit: &orbit,
            rest,
            yaw_dir: level.dir(input.yaw, 0.0),
            before,
            previous,
            pace: Pace {
                // The frame of a change is a travel's first: what the eye
                // moved on it belongs to the focus before.
                moved_m: last.and_then(|l| l.moved_m).filter(|_| !changed),
                share: (last.map_or(0.0, |l| l.travelled), travelled),
                remaining_m,
                anchor_step_m: last.map_or(0.0, |l| (anchor - l.anchor).norm()),
            },
        }
        .eye(carried, travel, floor)
    } else {
        travel.ride = None;
        carried
    };
    travel.last = Some(Last {
        id: frame.id,
        anchor,
        look_at,
        skirt: eye - lifted,
        eye,
        carried,
        moved_m: (dt_s > 0.0 && last.is_some()).then_some((eye - before).norm()),
        travelled,
    });
    chase_camera(eye, look_at, &level, input.viewport_px)
}

/// The anchor a focus rests on: its drawn CoM, unless that is under the
/// floor. A craft `depth` under the floor has every orbit point lifted
/// onto it, and the lift keeps of a drag only its share along the floor
/// — nothing at the craft's own level. So the anchor rises along the
/// local up to `depth + min(depth, eye_m)` above the CoM: a craft at
/// least an eye distance down has its whole orbit sphere over the floor
/// and a drag moves the eye as freely as in flight. That is the travel
/// lift ([`travel_anchor`]) at its full margin, and it never fades: a
/// shallower craft keeps `depth` of clearance. A craft at or above the
/// floor rests on its CoM. `floor` is sampled only with a body to sample
/// it on.
fn resting_anchor(
    com: DVec3,
    eye_m: f64,
    centre: Option<DVec3>,
    floor: &mut dyn FnMut(DVec3) -> f64,
) -> DVec3 {
    let Some(centre) = centre else {
        return com;
    };
    let rel = com - centre;
    let r = rel.norm();
    if !(r > 0.0 && eye_m > 0.0) {
        return com;
    }
    let dir = rel / r;
    let floor_r = floor(dir);
    let depth = floor_r - r;
    if depth > 0.0 {
        centre + dir * (floor_r + depth.min(eye_m))
    } else {
        com
    }
}

/// How much of what the previous focus left in hand a travel still
/// carries: nothing once it is done, everything while more than an eye
/// distance of it is left, and between those the share of that last eye
/// distance which is left. A travel shorter than an eye distance fades
/// over its whole length, so it starts carrying everything.
fn travel_blend(remaining_m: f64, start_m: f64, eye_m: f64) -> f64 {
    let reach_m = eye_m.min(start_m);
    if reach_m <= TRAVEL_DONE_M {
        return 0.0;
    }
    ((remaining_m - TRAVEL_DONE_M) / (reach_m - TRAVEL_DONE_M)).clamp(0.0, 1.0)
}

/// The travelling anchor: `anchor` itself — the straight line from the
/// previous focus's resting anchor to this one — unless it has gone under
/// the floor. A long line crosses the planet; an anchor below the floor
/// is lifted radially to the floor plus a margin of its depth up to one
/// eye distance `eye_m`. At the full margin the whole orbit sphere clears
/// the ground, so the floor has nothing to clamp mid-travel; the margin
/// shrinks with the travel that is left and is zero one eye distance
/// out, and both ends of the line are resting anchors, at or above the
/// floor: the hand-over is continuous. `floor` is sampled only with a
/// body to sample it on.
fn travel_anchor(
    anchor: DVec3,
    remaining_m: f64,
    eye_m: f64,
    centre: Option<DVec3>,
    floor: &mut dyn FnMut(DVec3) -> f64,
) -> DVec3 {
    let Some(centre) = centre else {
        return anchor;
    };
    let rel = anchor - centre;
    let r = rel.norm();
    if !(r > 0.0 && remaining_m > 0.0 && eye_m > 0.0) {
        return anchor;
    }
    let dir = rel / r;
    let floor_r = floor(dir);
    let depth = floor_r - r;
    if depth > 0.0 {
        let margin = depth.min(eye_m) * (remaining_m / eye_m).min(1.0);
        centre + dir * (floor_r + margin)
    } else {
        anchor
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

/// The R1 cascade arm threshold (terrain-overhaul 3b): the flight
/// extract solves cascades only while the sun clears the focused
/// craft's local horizon by more than this (sun·up). 0.06 ≈ 3.4°
/// elevation sits just above the two neighboring cutoffs —
/// `sun_occlusion`'s ray-march full-occludes below sun·up 0.035 and
/// the vessel blob disables at 0.04 — so there is no band where the
/// cascade replaces a dimmer fallback; below it an ortho sun-fit
/// degenerates into shadows hundreds of km long that no 20 km
/// cascade box contains (at 0.06, the M5 relief ceiling of ~2.4 km
/// throws a ~40 km penumbra — exactly the grazing-sun regime the
/// ray-march's long soft shadows own). Night and the terminator keep
/// the pre-cascade look bit-for-bit.
pub const SHADOW_ARM_SUN_UP: f64 = 0.06;

/// The 3b cascade arm predicate: sun above the local horizon by
/// [`SHADOW_ARM_SUN_UP`]. Pure — pinned by unit test at the
/// threshold, consumed by [`cascade_uniforms`] and by the render
/// gate's hand-built surface probe (which reconstructs the solve
/// outside the real extract).
pub fn cascades_armed(sun_dir: DVec3, up: DVec3) -> bool {
    sun_dir.dot(up) > SHADOW_ARM_SUN_UP
}

/// 3b: solve the frame's R1 cascade uniforms — pure presentation
/// math over the SOLVED chase camera, the focused craft's drawn CoM
/// (`craft_com`, bubble frame; `None` with nothing focused) and the
/// staged `FlightEnv` sun (§2.5: RenderExtract owns presentation
/// derivation; nothing here reads the sim). The camera's forward is
/// recovered from `view_proj` exactly like `sky_view_rot`: the
/// reverse-Z projection's last row is `[0,0,−1,0]`, so `view_proj`'s
/// row 3 is `−z_out`.
///
/// The local-level up is the CRAFT's — the direction of its drawn CoM
/// from the body centre, the blob shadow's anchor — never the camera's:
/// after a focus change the camera's anchor is still travelling to the
/// craft, and an up read off it rolled the frustum to the local level
/// of wherever the anchor happened to be — hundreds of kilometres from
/// the craft mid-travel — so the shadows did not stay with it. With
/// nothing focused the camera's own anchor (`eye − z_out·distance`)
/// supplies the up, so the frame a craft leaves keeps the one it had.
/// Returns the all-zero DISARMED row when the sun sits at or below
/// the arm threshold; the solve itself is `csp_render::shadow::
/// solve_cascades` (f64, texel-snapped — see its docs).
fn cascade_uniforms(
    cam: &MapCameraF64,
    craft_com: Option<DVec3>,
    env: &FlightEnv,
    viewport_px: (f64, f64),
) -> csp_render::shadow::ShadowUniforms {
    let z_out = DVec3::new(
        -cam.view_proj[3][0],
        -cam.view_proj[3][1],
        -cam.view_proj[3][2],
    );
    let unit = |rel: DVec3| {
        let l = rel.norm();
        if l > env.radius_m * 1.0e-6 {
            rel / l
        } else {
            DVec3::Z
        }
    };
    let up = match craft_com {
        Some(com) => unit(com - env.body_center),
        None => unit((cam.eye - env.body_center) - z_out * cam.distance_m),
    };
    if !cascades_armed(env.sun_dir, up) {
        return csp_render::shadow::ShadowUniforms::default();
    }
    let forward = {
        let f = -z_out;
        let n = f.norm();
        if n > 1.0e-12 { f / n } else { DVec3::X }
    };
    let aspect = (viewport_px.0 / viewport_px.1.max(1.0)).max(1e-6);
    csp_render::shadow::solve_cascades(
        &csp_render::shadow::ShadowCamera {
            sun_dir: env.sun_dir,
            forward,
            up,
            fov_y: FLIGHT_FOV_Y,
            aspect,
            near_m: 0.1,
        },
        &csp_render::shadow::CascadeConfig::default(),
    )
    .uniforms
}

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
/// All presentation-side — the staged [`FlightBody`] parts placed at the
/// pose the frame DRAWS the focus at ([`DrawnPose`], the one under the
/// solved chase camera), never the sim.
pub(crate) fn pick_part(
    cam: &FlightCameraF64,
    focus: Option<(&FlightBody, DrawnPose)>,
    content: egui::Rect,
    cursor: egui::Pos2,
) -> Option<FlightPick> {
    let (focus, pose) = focus?;
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
    for p in focus.parts.iter() {
        // The collider's body-frame AABB → 8 world corners → the part's
        // screen-space bounding rect. A part entirely behind the eye
        // projects nothing and is skipped.
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(8);
        let mut depth2 = f64::INFINITY;
        for x in [p.lo.x, p.hi.x] {
            for y in [p.lo.y, p.hi.y] {
                for z in [p.lo.z, p.hi.z] {
                    let w = pose.pos + pose.rot.rotate(DVec3::new(x, y, z));
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
        let center_w = pose.pos + pose.rot.rotate((p.lo + p.hi) * 0.5);
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

/// `Presentation` schedule — §2.5's interpolation, declared
/// `.before(flight_camera)`: age every staged vessel's pose history and
/// solve what the frame draws.
///
/// The snapshot holds each vessel at its LAST simulation tick; drawn
/// there, a 60 fps frame steps one tick of motion fifteen frames in
/// sixteen and two in the sixteenth — the view hitch-steps. With the
/// frame's fractional tick remainder each vessel is instead drawn one
/// tick of latency back, between its previous and current pose, so
/// consecutive frames advance evenly. [`crate::TickFraction`] is the
/// remainder — handed over by `csp-app`'s warp controller, never read
/// off the clock here — and `0` (a harness that supplies none) draws
/// every pose exactly as staged.
pub(crate) fn flight_interp(
    snap: Res<FlightSnapshot>,
    clock: Res<crate::Clock>,
    frac: Res<crate::TickFraction>,
    mut history: ResMut<PoseHistory>,
    mut scene: ResMut<DrawnScene>,
) {
    let tick = clock.0.ticks();
    // Into `[0, 1)`, so the drawn instant never passes the clock's own
    // pose. A non-finite remainder carries no interpolation: 0.
    let frac = if frac.0.is_finite() {
        frac.0.clamp(0.0, 1.0 - f64::EPSILON)
    } else {
        0.0
    };
    scene.poses = history.advance(&snap.vessels, tick, frac);
    let drawn = if frac > 0.0 {
        (tick.saturating_sub(1) as f64) + frac
    } else {
        tick as f64
    };
    scene.time_s = Some(drawn / csp_core::TICK_HZ as f64);
    // The staged spin phase is `ω·t` at the clock's tick. Rolled back
    // to the drawn instant, the ground turns under the craft at the
    // rate the craft is drawn moving — a latched hull stays on its pad.
    scene.env = snap.env.as_ref().map(|staged| {
        let mut env = staged.clone();
        env.spin_phase -=
            staged.body_spin.norm() * (tick as f64 - drawn) / csp_core::TICK_HZ as f64;
        env
    });
}

/// The chase camera's frame inputs, kept as one SystemParam so
/// `flight_camera` fits the argument budget (the `FlightScene`
/// precedent in `lib.rs`).
#[derive(bevy_ecs::system::SystemParam)]
pub(crate) struct ChaseInputs<'w> {
    /// The camera's session state — the angles, the eased distance,
    /// the travel memory.
    input: ResMut<'w, FlightInput>,
    /// The mirrored flight scene — where the focus is resolved from.
    snap: Res<'w, FlightSnapshot>,
    /// The poses that scene is drawn at this frame.
    scene: Res<'w, DrawnScene>,
    /// The pin-aware focus resolver.
    focus_override: Res<'w, FlightFocusOverride>,
    /// The body's terrain table and heightfield cache — the floor
    /// under the eye.
    terrain_cfg: Res<'w, TerrainConfig>,
    terrain_cache: ResMut<'w, TerrainCache>,
    /// Every staged vessel's bounding radius — the focus's framing input.
    radii: Res<'w, CraftRadii>,
    /// The frame's wall time — what the easing advances by.
    session: Res<'w, crate::SessionMirror>,
}

/// `Presentation` schedule — the chase-camera step for the frame's
/// focus ([`step_chase_camera`]), solved from the pose the craft is
/// DRAWN at: the interpolated CoM under the drawn spin phase, so the
/// floor it clamps against is the terrain the frame shows. Independent
/// of `flight_present`: nothing in Presentation reads the camera, and
/// the focus edge is the camera's own memory.
pub(crate) fn flight_camera(inputs: ChaseInputs, mut cam: ResMut<FlightCameraF64>) {
    let ChaseInputs {
        mut input,
        snap,
        scene,
        focus_override,
        terrain_cfg,
        mut terrain_cache,
        radii,
        session,
    } = inputs;
    // THE focus — the pin-aware resolver `flight_present`, the SFX
    // edges and the extract's blob shadow share. A pin whose vessel is
    // not staged falls through to the newest staged vessel here, exactly
    // the vessel it resolves to once `flight_present` releases it later
    // this frame.
    let Some(focus) = focus_override.resolve(&snap.vessels) else {
        // Nothing to orbit: whatever takes the focus next is a first
        // focus, with no earlier eye to be continuous with.
        input.travel.last = None;
        return;
    };
    let env = scene.env.as_ref().or(snap.env.as_ref());
    // The floor — under the eye and under the craft: the heightfield's
    // own surface there (a point sample — `ground_radius` with no ring),
    // plus the clearance; under an ocean that is the seabed.
    let mut floor = |dir: DVec3| match env {
        Some(e) => eye_floor(crate::terrain::ground_radius(
            &terrain_cfg,
            &mut terrain_cache,
            e,
            dir,
            0.0,
        )),
        None => f64::NEG_INFINITY,
    };
    cam.0 = step_chase_camera(
        &mut input,
        &ChaseFrame {
            id: focus.id,
            com: scene.pose_of(focus).com,
            radius_m: radii
                .0
                .iter()
                .find(|(id, _)| *id == focus.id)
                .map_or(0.0, |(_, r)| *r),
            centre: env.map(|e| e.body_center),
            axis: env.map_or(DVec3::Z, |e| e.spin_axis),
            dt_wall_ms: session.dt_wall_ms,
        },
        &mut floor,
    );
}

/// `Presentation` schedule — the HUD mirrors + the §3.7 SFX cue edges
/// (the schedule's "VFX, audio cues" slot). The chase camera is
/// [`flight_camera`]'s.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // every HUD mirror is its own resource; per-vessel stage plans add the hud_plan read
pub(crate) fn flight_present(
    snap: Res<FlightSnapshot>,
    live: Res<LiveVesselCount>,
    rails: Res<RailsStaged>,
    plans: Res<StagePlans>,
    prop: Res<VesselPropulsionStaged>,
    mut cam: ResMut<FlightCameraF64>,
    mut ui: ResMut<FlightUiData>,
    mut sfx: ResMut<FlightSfx>,
    clock: Res<crate::Clock>,
    mut focus_override: ResMut<FlightFocusOverride>,
    mirror: Res<crate::ProgramMirror>,
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
            // `seen` marks the materialization the lease edge waits
            // on — a rails-staged target (the C19-F2 handoff) is
            // staged but not yet `InBubble`, so the lease still owes
            // a real conversion (P04-F3).
            if !rails.0.contains(&id.0) {
                focus_override.seen = true;
            }
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
    ui.sfx = sfx_edges(&mut sfx, ui.debris_count, live.0, &rails.0, focus);
    // The audio backend's focus mirror: voices key off this, not a
    // `vessels.last()` of their own (P07-F1).
    sfx.focus_vessel = focus.map(|f| f.id);
    // The staged FACILITIES rows — the HUD's build-status surface and
    // the no-focus branch's "delivery in flight" signal.
    ui.builds.clone_from(&mirror.input.builds);
    let Some(focus) = focus else {
        // An empty arena with an order on the books is a delivery in
        // flight, not a dead scene: keep the last solved chase pose
        // (the pad the launch pressed from — the camera framed it
        // while the occupant was still staged) so the terrain pass +
        // launch complex keep drawing for the build's duration
        // instead of dropping to the fallback void. The pose
        // re-solves the frame the delivery stages a vessel; a
        // build-free empty arena still invalidates (the truly dead
        // scene `sim_step`'s VAB bounce covers).
        if ui.builds.is_empty() {
            cam.0.valid = false;
        }
        ui.has_vessel = false;
        ui.focus = None;
        ui.propellant.clear();
        ui.stages_left = 0;
        ui.stages_total = plans.hud_plan(None).map_or(0, |p| p.total);
        ui.stages_plan_vessel = None;
        stage_propulsion(&mut ui, None, &[], 0.0, None, None);
        return;
    };
    ui.has_vessel = true;
    ui.focus = Some(focus.id);
    ui.latched = focus.latched;
    ui.propellant = focus.tanks.clone();
    let hud = plans.hud_plan(Some(VesselId(focus.id)));
    ui.stages_left = hud.map_or(0, |p| p.stages.len());
    ui.stages_total = hud.map_or(0, |p| p.total);
    // C19-F6: name the plan's owner when the fallback borrowed another
    // vessel's plan — the fire press lands on THAT vessel's parts, not
    // the craft the camera is on, so the readout says whose it is.
    ui.stages_plan_vessel = hud
        .and_then(|p| p.vessel)
        .filter(|v| *v != VesselId(focus.id))
        .map(|v| v.0);
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
    let drops: &[StageActions] = plans
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
    debris_count: usize,
    live_vessels: usize,
    rails: &[u64],
    focus: Option<&FlightBody>,
) -> Vec<String> {
    let cur = focus.map(|f| SfxPrev {
        focus: f.id,
        throttle_on: f.throttle > 0.0,
        nodes: f.nodes,
        debris: debris_count,
        live: live_vessels,
        rails: rails.contains(&f.id),
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
                // C19-F2: the bubble↔rails conversion edge — the vessel
                // is alive and still focused (it keeps staging through
                // the handoff); the cue is the honest "now on rails"
                // mark where "VESSEL REMOVED"/"VESSEL LEFT BUBBLE"
                // used to lie.
                if c.rails && !p.rails {
                    push("ON RAILS");
                } else if !c.rails && p.rails {
                    push("IN BUBBLE — CONTROL");
                }
            }
            if c.debris > p.debris {
                push("BREAKUP");
            }
            if c.debris < p.debris {
                // §3.5: in-bubble debris leaves only via impact removal
                // (it never latches).
                push("DEBRIS IMPACT");
            }
            if c.live < p.live && c.debris >= p.debris {
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
/// spawned NON-DEBRIS staged vessel. `vessels` is staged ascending
/// `VesselId` and the sim's `next_vessel_id` counter is monotonic, so
/// `last()` is the newest spawn — a fresh launch takes the camera, the
/// HUD mirrors, and `SetThrottle` routing with it. The debris filter is
/// C19-F7: `apply_breakup` mints its fragments at HIGHER ids than the
/// retained hull, so a bare `last()` handed focus to an uncommandable
/// fragment by construction on every breakup. The newest live vessel
/// wins instead; a scene whose only staged vessels are debris (the
/// hull is gone entirely) still falls back to `last()` — the newest
/// wreck reads better than a dead frame. When the focus leaves the
/// staging set the next-newest vessel inherits it; an empty scene has
/// none.
pub(crate) fn focus_vessel(vessels: &[FlightBody]) -> Option<&FlightBody> {
    vessels
        .iter()
        .rev()
        .find(|b| !b.debris)
        .or_else(|| vessels.last())
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
///
/// C13-F3: this is a *display mirror*, not a third authority — the
/// sim's `propellant_plans` stays the consumption walk of record
/// (it sees reliability `thrust_factor`s and debits per `PHYSICS_DT`;
/// `csp-phys::readout` deliberately walks per-stage ignition, and this
/// HUD copy evaluates the same coverage rule per second over a staged
/// `WalkNode` table). Sharing the walk body would couple the HUD to
/// sim-internal types (`CraftDef`/`WeldedBody`/reliability records)
/// for no behavioral gain; the coverage rule the three agree on is
/// documented on each side.
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
    drops: &[StageActions],
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
    drops: &[StageActions],
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
            // The Δv walk cares about departing mass only — staged
            // deploys keep their part aboard, so the drop set is the
            // jettison half of the firing.
            for id in &drops[step].jettisons {
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
/// so `color` lands unchanged, save for water in front of it, which
/// `flight.wgsl` measures from `world_rel` — the vertex's place relative
/// to the eye. A vertex with no place of its own (the clip-space sky
/// quad) passes `DVec3::ZERO`: it sits at the eye, where no water lies.
fn unlit_v(pos: [f32; 3], world_rel: DVec3, color: [f32; 4]) -> csp_render::flight::FlightVertex {
    csp_render::flight::FlightVertex {
        pos,
        world: f3(world_rel),
        nrm: [0.0; 3],
        color,
        mat: [0.0; 3],
    }
}

/// All-or-nothing emission of one world-space triangle into the
/// clip-space stream: every corner is projected first and the tri is
/// pushed whole or dropped whole — the same rule the part pass and the
/// VAB packet use. A per-vertex `push` would let one behind-eye
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
    for (pos, q) in [(c0, a), (c1, b), (c2, c)] {
        v.push(unlit_v(pos, q - eye, color));
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
    for (pos, q, color) in [(c0, a, colors[0]), (c1, b, colors[1]), (c2, c, colors[2])] {
        v.push(unlit_v(pos, q - eye, color));
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
    pose: &DrawnPose,
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
            let d = pose.rot.rotate(*dir);
            d / d.norm()
        };
        let tip = pose.pos + pose.rot.rotate(*p0);
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
#[allow(clippy::too_many_arguments)] // the drawn pose rides beside the staged body it places
fn emit_sheath(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    b: &FlightBody,
    pose: &DrawnPose,
    flow: DVec3,
    plasma_i: f64,
    tris: &mut Vec<crate::part_mesh::SmoothTri>,
) {
    for p in b.parts.iter() {
        let (lo, hi) = p.shape.local_aabb();
        let size = ((hi - lo).norm() * 0.5).max(0.05);
        let shell = crate::part_mesh::inflated(&p.shape, size * (0.04 + 0.08 * plasma_i) + 0.015);
        tris.clear();
        crate::part_mesh::emit_smooth(&shell, tris);
        for t in tris.iter() {
            let world = |q: DVec3| pose.pos + pose.rot.rotate(p.transform.apply(q));
            let cols = [(t.a, t.na), (t.b, t.nb), (t.c, t.nc)].map(|(_q, n)| {
                let n_w = pose.rot.rotate(p.transform.apply_dir(n));
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
        // Packed fabric + canister: matte, not nozzle steel.
        PartKind::Chute => [0.25, 14.0, 1.0],
        PartKind::Structural => [0.52, 48.0, 1.0],
    }
}

/// Emit one part's real baked model (procedural parts.csar) — fitted to the
/// collider AABB. The vertex stream carries the authored albedo, the
/// smoothing-wedge normal, and the baked material terms; per-pixel
/// lighting happens in the fragment shader under `packet.light`.
/// Debris chars the albedo and kills the specular sheen.
///
/// C1: `tris` is the CACHED `emit_model` product for the part's
/// (model, AABB) — the transform/projection arithmetic below is the
/// exact per-frame code the fresh-emit path ran.
fn emit_model_part(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    b: &FlightBody,
    pose: &DrawnPose,
    p: &FlightPart,
    tris: &[crate::part_mesh::ModelTri],
    vp: &[[f64; 4]; 4],
    eye: DVec3,
) {
    for t in tris.iter() {
        let world = [t.a, t.b, t.c].map(|q| pose.pos + pose.rot.rotate(p.transform.apply(q)));
        let [Some(c0), Some(c1), Some(c2)] = world.map(|q| project_into(vp, eye, q)) else {
            continue;
        };
        for k in 0..3 {
            let (nrm, alb, mt) = [(t.na, t.ca, t.ma), (t.nb, t.cb, t.mb), (t.nc, t.cc, t.mc)][k];
            let n_w = pose.rot.rotate(p.transform.apply_dir(nrm));
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

/// The bit-pattern key of a [`crate::part_mesh::PartShape`] —
/// `emit_smooth` is a pure function of the shape, so the bits ARE the
/// cache identity (C1). Authored shapes never carry NaN.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
enum ShapeKey {
    /// Box collider corners.
    Box {
        /// Min corner bits.
        lo: [u64; 3],
        /// Max corner bits.
        hi: [u64; 3],
    },
    /// Lathe stations.
    Lathe {
        /// Axis bits.
        axis: [u64; 2],
        /// `(z, r)` station bits, ascending.
        profile: Vec<(u64, u64)>,
    },
}

impl ShapeKey {
    fn of(shape: &crate::part_mesh::PartShape) -> Self {
        let v3 = |v: DVec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
        match shape {
            crate::part_mesh::PartShape::Box { lo, hi } => Self::Box {
                lo: v3(*lo),
                hi: v3(*hi),
            },
            crate::part_mesh::PartShape::Lathe { axis, profile } => Self::Lathe {
                axis: [axis[0].to_bits(), axis[1].to_bits()],
                profile: profile
                    .iter()
                    .map(|&(z, r)| (z.to_bits(), r.to_bits()))
                    .collect(),
            },
        }
    }
}

/// One baked kitbash triangle — part-local corners/normals plus the
/// livery-baked per-corner color and the kind's material row (C1).
struct BakedTri {
    /// Corners, part-local.
    a: DVec3,
    /// Corner b.
    b: DVec3,
    /// Corner c.
    c: DVec3,
    /// Normal at `a`.
    na: DVec3,
    /// Normal at `b`.
    nb: DVec3,
    /// Normal at `c`.
    nc: DVec3,
    /// Livery color per corner (alpha 1).
    col: [[f32; 4]; 3],
    /// `[spec strength, exponent, occlusion]` — per-kind, shared by
    /// the triangle's corners.
    mat: [f32; 3],
}

/// The silhouette table's inner map: `(kind tag, debris)` → the
/// variant's baked tris — nested under [`ShapeKey`] so lookups borrow
/// the caller's key (see `FlightGeomCache::smooth_tris`).
type SmoothVariants = BTreeMap<(u8, bool), Arc<Vec<BakedTri>>>;

/// The model cache's key: the fit AABB's bit pattern plus the model
/// `Arc`'s address (pinned by the entry holding its own clone).
type ModelKey = ([u64; 3], [u64; 3], usize);

/// The model cache's entry — the pinning `Arc` beside its baked tris.
type ModelEntry = (
    Arc<crate::art::PartModel>,
    Arc<Vec<crate::part_mesh::ModelTri>>,
);

/// `PartKind`'s cache tag — the livery/material tables key on it.
const fn kind_tag(kind: PartKind) -> u8 {
    match kind {
        PartKind::Engine => 0,
        PartKind::Crew => 1,
        PartKind::Tank => 2,
        PartKind::Battery => 3,
        PartKind::Science => 4,
        PartKind::Port => 5,
        PartKind::Chute => 6,
        PartKind::Structural => 7,
    }
}

/// The LOCAL half of the kitbash part emit — `emit_smooth` plus the
/// per-corner livery tint and material, every step the call the
/// fresh-emit path made (C1; bits identical by construction).
fn bake_smooth(shape: &crate::part_mesh::PartShape, kind: PartKind, debris: bool) -> Vec<BakedTri> {
    let base: [f32; 3] = if debris {
        [0.72, 0.38, 0.16]
    } else {
        kind_tint(kind)
    };
    let mat = if debris {
        [0.0, 32.0, 1.0]
    } else {
        kind_mat(kind)
    };
    let (lo, hi) = shape.local_aabb();
    let z_span = (hi.z - lo.z).max(1e-9);
    let mut tris = Vec::new();
    crate::part_mesh::emit_smooth(shape, &mut tris);
    tris.iter()
        .map(|t| {
            let tint = |q: DVec3| -> [f32; 4] {
                if debris {
                    [base[0], base[1], base[2], 1.0]
                } else {
                    let c = livery(kind, base, (q.z - lo.z) / z_span);
                    [c[0], c[1], c[2], 1.0]
                }
            };
            BakedTri {
                a: t.a,
                b: t.b,
                c: t.c,
                na: t.na,
                nb: t.nb,
                nc: t.nc,
                col: [tint(t.a), tint(t.b), tint(t.c)],
                mat,
            }
        })
        .collect()
}

/// The extract's geometry cache (C1): the launch complex's baked local
/// triangles plus the craft's baked part silhouettes/models, keyed by
/// what actually invalidates them — the static layout version for the
/// structures, the shape/model bits (with kind and debris folding in
/// the livery/material rows) for the parts. Position/orientation stay
/// per-frame and never enter the keys. Presentation data only.
///
/// Bounds: the silhouette table caps at 256 entries and the model
/// table at 128 — the parts catalog is far smaller, and a long
/// session of craft edits (fresh collider AABBs) can't grow either
/// unboundedly; on overflow the table simply clears (a re-bake, never
/// a wrong answer). The model key is the model `Arc`'s address PINNED
/// by the entry holding its own clone, so a freed-and-reused address
/// can never alias a live key.
#[derive(Resource, Default)]
pub(crate) struct FlightGeomCache {
    /// The launch complex's baked local tris (layout-version keyed).
    pub(crate) space_center: crate::structures::SpaceCenterBake,
    /// Kitbash silhouettes: shape bits → `(kind, debris)` → tris
    /// (nested so lookups borrow the caller's key — see
    /// [`Self::smooth_tris`]).
    smooth: BTreeMap<ShapeKey, SmoothVariants>,
    /// Baked models: `(fit-lo bits, fit-hi bits, model address)` →
    /// the entry holds the model `Arc` (the address PIN — it can't be
    /// reused while the entry lives) beside its tris.
    models: BTreeMap<ModelKey, ModelEntry>,
    /// Bake counters (the invalidation tests' observability).
    pub(crate) smooth_bakes: u32,
    /// Model bake counter (same).
    pub(crate) model_bakes: u32,
    /// Scratch tri stream reused across frames (`emit_sheath`'s
    /// target — the old per-extract `Vec::new()` pair).
    pub(crate) sheath_tris: Vec<crate::part_mesh::SmoothTri>,
    /// Last frame's vertex count — the capacity hint that spares the
    /// packet's stream its growth-realloc chain (allocation only;
    /// no output effect).
    vert_hint: usize,
}

impl FlightGeomCache {
    /// The part's baked kitbash triangles, baking once per key.
    fn smooth_tris(
        &mut self,
        shape: &crate::part_mesh::PartShape,
        key: &ShapeKey,
        kind: PartKind,
        debris: bool,
    ) -> Arc<Vec<BakedTri>> {
        // Nested so the HIT path borrows the caller's key: a flat
        // `(ShapeKey, tag)` tuple key would clone the lathe profile
        // Vec on every lookup (P1 — the allocation this cache exists
        // to avoid). The clone happens only on a miss, beside the
        // bake it amortizes.
        let tag = (kind_tag(kind), debris);
        if let Some(inner) = self.smooth.get(key)
            && let Some(t) = inner.get(&tag)
        {
            return Arc::clone(t);
        }
        if self.smooth.len() >= 256 {
            self.smooth.clear();
        }
        let baked = Arc::new(bake_smooth(shape, kind, debris));
        self.smooth
            .entry(key.clone())
            .or_default()
            .insert(tag, Arc::clone(&baked));
        self.smooth_bakes += 1;
        baked
    }

    /// The part's baked model triangles fitted to `lo`/`hi`, baking
    /// once per (model, fit).
    fn model_tris(
        &mut self,
        m: &Arc<crate::art::PartModel>,
        lo: DVec3,
        hi: DVec3,
    ) -> Arc<Vec<crate::part_mesh::ModelTri>> {
        let v3 = |v: DVec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
        let key = (v3(lo), v3(hi), Arc::as_ptr(m) as usize);
        if let Some((_, t)) = self.models.get(&key) {
            return Arc::clone(t);
        }
        if self.models.len() >= 128 {
            self.models.clear();
        }
        let mut tris = Vec::new();
        crate::part_mesh::emit_model(m, lo, hi, &mut tris);
        let tris = Arc::new(tris);
        self.models.insert(key, (Arc::clone(m), Arc::clone(&tris)));
        self.model_bakes += 1;
        tris
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
    scene: Res<DrawnScene>,
    focus_override: Res<FlightFocusOverride>,
    cam: Res<FlightCameraF64>,
    terrain_cfg: Res<TerrainConfig>,
    mut terrain_cache: ResMut<TerrainCache>,
    mut flight_ui: ResMut<FlightUiData>,
    sites: Res<crate::structures::SiteTable>,
    clock: Res<crate::Clock>,
    mut geom: ResMut<FlightGeomCache>,
    mut frame: ResMut<FlightFrame>,
) {
    let mut packet = csp_render::flight::FlightPacket::default();
    // C1: pre-size the stream from the last frame's count — a pure
    // allocation hint (the emit arithmetic is untouched), sparing the
    // ~76 k-vertex stream its doubling-growth memcpy chain.
    if geom.vert_hint > 0 {
        packet.vertices = Vec::with_capacity(geom.vert_hint);
    }
    // Every clock-driven quantity is taken at the drawn instant. The
    // default scene — nothing drawn — reads as the staged snapshot at
    // the clock's own tick.
    let env = scene.env.as_ref().or(snap.env.as_ref());
    let time_s = scene
        .time_s
        .unwrap_or_else(|| clock.0.ticks() as f64 / csp_core::TICK_HZ as f64);
    // The blob shadow and the cascade's local up are the FOCUS's: its
    // drawn CoM goes to both by name, never recovered from the camera
    // matrix — whose anchor is still travelling to the craft for the
    // first frames after a focus change, which is where the blob used
    // to ride and the shadow frustum used to turn.
    let focus_com = focus_override
        .resolve(&snap.vessels)
        .map(|f| scene.pose_of(f).com);
    // R2 surface pass: when a physical world is present and the chase
    // camera solved, the quadtree terrain + LUT sky replace the M4c
    // gradient/disc. Without an environment the flat gradient stays —
    // a flight view with no planet is a vacuum backdrop anyway. The
    // mode gate keeps map frames from paying the quadtree walk +
    // instance bake for a packet nothing draws.
    //
    // While the body's heightfield bakes on the background thread the
    // packet comes back sky-only (M5 §5.2-R2 arrival polish) — the
    // HUD mirrors that wait below.
    let mut terrain_pending = false;
    if map_input.mode == MapMode::Flight
        && cam.0.valid
        && let Some(env) = env
    {
        let tp = crate::terrain::build_packet(
            &terrain_cfg,
            &mut terrain_cache,
            env,
            &cam.0,
            focus_com,
            input.viewport_px,
            time_s,
        );
        terrain_pending = tp.heightfield.is_none();
        packet.terrain = Some(tp);
        // R1/3b: solve the cascade uniforms beside the packet they
        // shadow — the app feeds this row to `draw_shadow` before the
        // scene pass. Same gate as the terrain build (a flight scene
        // with an environment and a solved camera); the arm predicate
        // inside additionally demands the sun clear the local horizon.
        //
        // Sky-only wait frames DISARM the row: the cascade passes
        // early-return on the empty terrain instance list, and an
        // armed SU would drive the craft's cascade lookup into the
        // never-cleared depth array (zeros on a fresh arrival — every
        // comparison fails, a dull hull; the PREVIOUS body's stale map
        // after an SOI switch — splotch self-shadowing). The disarmed
        // row is both receivers' documented fully-lit fallback, and
        // `draw_shadow`'s every-call SU write keeps the GPU row
        // tracking the packet, so the armed solve resumes clean the
        // frame the field lands.
        packet.shadow = if terrain_pending {
            csp_render::shadow::ShadowUniforms::default()
        } else {
            cascade_uniforms(&cam.0, focus_com, env, input.viewport_px)
        };
    }
    flight_ui.terrain_pending = terrain_pending;
    let v = &mut packet.vertices;
    if packet.terrain.is_none() {
        // Sky fallback: a far-depth fullscreen space backdrop (two
        // tris at NDC z≈0 — reverse-Z far). Near-black with a faint
        // cool gradient so the frame isn't a dead flat void; the
        // scattering sky replaces it whenever terrain runs.
        // §2.3: it rides the FAR stream (`far_vertices`) — the far
        // pass owns the sky backdrop, drawn before the near pass
        // clears the depth and draws gameplay geometry over it.
        // (Its vertices are unlit, so the cascade cast degenerates
        // them either way — moving streams changes no cascade byte.)
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
            packet.far_vertices.push(unlit_v(pos, DVec3::ZERO, color));
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
        && let Some(env) = env
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
            &mut geom.space_center,
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
    // hemisphere ambient keyed to the lowest-id vessel's local up — sky
    // bounce above it, dim ground bounce below — so hull shading reads
    // like the scene the terrain pass draws (same sun direction).
    packet.light = {
        let first = snap.vessels.first().map(|b| scene.pose_of(b));
        let up = snap
            .env
            .as_ref()
            .and_then(|e| {
                first.map(|p| {
                    let d = p.com - e.body_center;
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
            let alt = first
                .map(|p| ((p.com - e.body_center).norm() - e.radius_m).max(0.0))
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
            // The water the hull is seen through: the eye from the body
            // centre and the ocean's surface — all zero without an
            // environment, the surface zero without an ocean.
            water: snap.env.as_ref().map_or([0.0; 4], |e| {
                let eye = cam.0.eye - e.body_center;
                [
                    eye.x as f32,
                    eye.y as f32,
                    eye.z as f32,
                    crate::terrain::ocean_datum_m(&terrain_cfg, e).unwrap_or(0.0) as f32,
                ]
            }),
        }
    };
    for b in &snap.vessels {
        let pose = scene.pose_of(b);
        for p in b.parts.iter() {
            // C1: the part's local silhouette comes from the geometry
            // cache — `emit_smooth` re-tessellation, the livery tint
            // and the material rows are pure functions of
            // (shape, kind, debris) and bake once; the world transform
            // and projection below are the exact per-frame arithmetic
            // the fresh-emit path ran.
            if let Some(m) = &p.model {
                let (mlo, mhi) = p.shape.local_aabb();
                let tris = geom.model_tris(m, mlo, mhi);
                emit_model_part(v, b, &pose, p, &tris, vp, eye);
                continue;
            }
            let baked = geom.smooth_tris(&p.shape, &p.shape_key, p.kind, b.debris);
            for t in baked.iter() {
                // A tri with any corner behind the eye plane drops
                // whole — the same all-or-nothing rule the VAB packet
                // uses — rather than emitting a corrupted fan.
                let world =
                    [t.a, t.b, t.c].map(|q| pose.pos + pose.rot.rotate(p.transform.apply(q)));
                let [Some(c0), Some(c1), Some(c2)] = world.map(|q| project_into(vp, eye, q)) else {
                    continue;
                };
                for (k, nrm) in [t.na, t.nb, t.nc].iter().enumerate() {
                    let n_w = pose.rot.rotate(p.transform.apply_dir(*nrm));
                    v.push(csp_render::flight::FlightVertex {
                        pos: [c0, c1, c2][k],
                        world: f3(world[k] - eye),
                        nrm: f3(n_w),
                        color: t.col[k],
                        mat: t.mat,
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
                    let alt = ((pose.com - e.body_center).norm() - e.radius_m).max(0.0);
                    1.0 + 1.8 * (alt / e.atm_top_m).clamp(0.0, 1.0)
                } else {
                    2.6
                }
            });
            emit_plumes(v, vp, eye, b, &pose, expand, time_s);
        }

        // §3.7 reentry plasma: an emissive sheath grown off the hull —
        // the leading side burns white-orange, the wake side a faint
        // red rim (`emit_sheath` carries the details). Stagnation side
        // = the surface-relative velocity direction (the sim's own
        // `v_rel` convention) — its magnitude is also the speed the
        // C19-F1 gate reads, so the sheath can't light on a slow
        // ascent pass however dense the air.
        let v_rel = snap.env.as_ref().map_or(b.vel, |e| {
            b.vel - e.body_spin.cross(pose.com - e.body_center)
        });
        let plasma_i = plasma_intensity(b.plasma_w_m2, v_rel.norm());
        if plasma_i > 0.0 {
            let flow = if v_rel.norm_squared() > 1.0 {
                v_rel / v_rel.norm()
            } else {
                DVec3::ZERO
            };
            emit_sheath(v, vp, eye, b, &pose, flow, plasma_i, &mut geom.sheath_tris);
        }

        // §3.7 ground-contact dust: a translucent tan ring hugging
        // the surface under the vessel's lowest hull vertex.
        if b.contact
            && let Some(env) = env
        {
            let mut low = pose.com;
            let mut low_alt = f64::MAX;
            let mut extent = 1.0_f64;
            for p in b.parts.iter() {
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
                    let w = pose.pos + pose.rot.rotate(c);
                    extent = extent.max((w - pose.pos).norm());
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
            // baked heightfield's neighbourhood ceiling under `dir`
            // (a ring sample, so the ring never hangs over an un-sampled
            // dip), not the bare datum: a datum-anchored ring buries
            // under up to `amplitude_m` of positive relief and floats
            // over dips (P07-F2/P08-F3). Under an ocean that surface is
            // the seabed the craft rests on: the ring stays on the
            // bottom, seen through the water, not on the surface above.
            let floor =
                crate::terrain::surface_floor_radius(&terrain_cfg, &mut terrain_cache, env, dir);
            let base = env.body_center + dir * (floor + 0.3);
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
    geom.vert_hint = packet.vertices.len();
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
    /// The extract's test rig — one world + the RenderExtract
    /// schedule. The terrain cache LIVES IN THE WORLD, so a caller can
    /// hold the rig across frames and idle-pump passes while the
    /// background heightfield bake runs (the sky-only wait state; the
    /// same poll pattern `render_gate::until_real_terrain` uses).
    fn extract_rig(snap: FlightSnapshot, cam: MapCameraF64) -> (bevy_ecs::world::World, Schedule) {
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
        // No interpolation has run in this rig — the extract reads the
        // default scene as the staged snapshot itself.
        world.insert_resource(DrawnScene::default());
        world.insert_resource(TerrainConfig::default());
        world.insert_resource(TerrainCache::default());
        // The extract resolves the focus for the blob shadow.
        world.insert_resource(FlightFocusOverride::default());
        // The HUD mirror the extract writes the terrain-bake wait to.
        world.insert_resource(FlightUiData::default());
        world.insert_resource(crate::structures::SiteTable::default());
        world.insert_resource(crate::Clock::default());
        world.insert_resource(FlightFrame::default());
        world.insert_resource(FlightGeomCache::default());
        let mut sched = Schedule::default();
        sched.add_systems(extract_flight);
        (world, sched)
    }

    /// Pump the rig until the background heightfield bake lands: the
    /// packet of the first pass that carries it.
    fn waited_pass(
        world: &mut bevy_ecs::world::World,
        sched: &mut Schedule,
    ) -> csp_render::flight::FlightPacket {
        for _ in 0..4_000 {
            let packet = extract_pass(world, sched);
            if packet
                .terrain
                .as_ref()
                .is_some_and(|t| t.heightfield.is_some())
            {
                return packet;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the background bake never landed for the rig");
    }

    /// The rig's terrain packet once the background heightfield bake has
    /// landed.
    fn waited_terrain(
        world: &mut bevy_ecs::world::World,
        sched: &mut Schedule,
    ) -> csp_render::terrain::TerrainPacket {
        waited_pass(world, sched)
            .terrain
            .expect("the packet carries terrain")
    }

    /// One extract pass over the rig; takes the frame's packet.
    fn extract_pass(
        world: &mut bevy_ecs::world::World,
        sched: &mut Schedule,
    ) -> csp_render::flight::FlightPacket {
        sched.run(world);
        world
            .resource_mut::<FlightFrame>()
            .0
            .take()
            .expect("extract produced a flight packet")
    }

    fn run_extract(snap: FlightSnapshot, cam: MapCameraF64) -> csp_render::flight::FlightPacket {
        let (mut world, mut sched) = extract_rig(snap, cam);
        extract_pass(&mut world, &mut sched)
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
            parts: Arc::new(vec![FlightPart {
                lo: DVec3::new(-0.5, -0.5, -0.5),
                hi: DVec3::new(0.5, 0.5, 0.5),
                transform: csp_phys::PartTransform::IDENTITY,
                shape_key: ShapeKey::of(&crate::part_mesh::PartShape::Box {
                    lo: DVec3::new(-0.5, -0.5, -0.5),
                    hi: DVec3::new(0.5, 0.5, 0.5),
                }),
                shape: crate::part_mesh::PartShape::Box {
                    lo: DVec3::new(-0.5, -0.5, -0.5),
                    hi: DVec3::new(0.5, 0.5, 0.5),
                },
                kind: PartKind::Structural,
                model: None,
                node: 0,
            }]),
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

    /// §2.3's far-field routing at the flight-stream level: the
    /// fallback sky gradient (no terrain packet) rides the FAR stream
    /// — the far pass owns the sky backdrop — while a craftless
    /// vacuum scene keeps the near stream empty. Both streams draw
    /// exactly where the two-pass frame expects them.
    #[test]
    fn fallback_sky_routes_to_the_far_stream() {
        let packet = run_extract(
            FlightSnapshot {
                env: None,
                vessels: Vec::new(),
            },
            crate::map::solve_orbit_camera(
                0.0,
                0.0,
                60.0,
                (320.0, 240.0),
                DVec3::ZERO,
                FLIGHT_FOV_Y,
                FLIGHT_NEAR_M,
            ),
        );
        assert!(
            packet.terrain.is_none(),
            "no environment ⇒ no terrain packet"
        );
        assert_eq!(
            packet.far_vertices.len(),
            6,
            "the two fallback-sky triangles"
        );
        assert!(
            packet.vertices.is_empty(),
            "a craftless vacuum scene has no near geometry"
        );
        assert!(
            packet.far_vertices.iter().all(|v| v.pos[2] == 0.0),
            "the backdrop sits on the reverse-Z far plane"
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
            &[StageActions {
                jettisons: vec![csp_phys::PartNodeId(2), csp_phys::PartNodeId(3)],
                ..StageActions::default()
            }],
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
        let mk = |node: u64, lo: DVec3, hi: DVec3| {
            let shape = crate::part_mesh::PartShape::Box { lo, hi };
            FlightPart {
                node,
                lo,
                hi,
                transform: csp_phys::PartTransform::IDENTITY,
                shape_key: ShapeKey::of(&shape),
                shape,
                kind: PartKind::Structural,
                model: None,
            }
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
            parts: Arc::new(vec![
                mk(1, DVec3::new(-1.0, -1.0, -1.0), DVec3::new(1.0, 1.0, 1.0)),
                mk(2, DVec3::new(-1.0, 2.0, -1.0), DVec3::new(1.0, 4.0, 1.0)),
            ]),
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
        let staged = DrawnPose::staged(&vessel);
        let pick = |p: egui::Pos2| pick_part(&cam, Some((&vessel, staged)), content, p);
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
                Some((&vessel, staged)),
                content,
                to_screen(DVec3::ZERO)
            ),
            None
        );
        assert_eq!(pick_part(&cam, None, content, to_screen(DVec3::ZERO)), None);
    }

    /// C19-F1's regression guard, over the verified altitude/speed
    /// table (verified-report-C19). Fluxes are re-derived here from
    /// the shipped Terrin atmosphere and the staged driver's own
    /// `η·½ρv³` formula rather than trusting the report's arithmetic:
    /// the flux rows genuinely overlap (mid-ascent 4.4e4 > 40 km
    /// reentry 3.0e4), so only the speed gate separates them — every
    /// ascent row must read dark while the reentry corridor lights.
    #[test]
    fn plasma_window_separates_ascent_from_reentry() {
        // `data/systems/home/terrin/body.ron`'s authored atmosphere,
        // mirrored literally so the table evaluates under the shipped
        // constants (p0 101325 Pa, H 5600 m, M 0.02897, γ 1.40, the
        // four temperature knots).
        let atm = csp_data::AtmosphereDef {
            sea_level_pressure_pa: 101325.0,
            scale_height_m: 5600.0,
            top_m: 70_000.0,
            molar_mass_kg_per_mol: 0.02897,
            heat_capacity_ratio: 1.40,
            temperature_knots: [
                (0.0, 288.15),
                (12_000.0, 216.65),
                (40_000.0, 250.35),
                (70_000.0, 200.15),
            ]
            .into_iter()
            .map(|(altitude_m, temperature_k)| csp_data::TemperatureKnot {
                altitude_m,
                temperature_k,
            })
            .collect(),
        };
        // The staging code's own driver (flight.rs `plasma_w_m2`).
        let flux = |alt_m: f64, v_mps: f64| {
            csp_phys::DEFAULT_CONVECTIVE_HEAT_FRACTION
                * 0.5
                * csp_phys::atmosphere_density_kg_m3(&atm, alt_m)
                * v_mps.powi(3)
        };
        let i = |alt_m: f64, v_mps: f64| plasma_intensity(flux(alt_m, v_mps), v_mps);

        // The stock-ascent band: the pre-fix window lit these to
        // i ≈ 0.25–0.51 — every launch carried a sheath. All must
        // read dark now (flux alone would still cross the window's
        // onset at 12 km @ 450; only the speed gate holds it off).
        for (alt, v) in [
            (0.0, 200.0),
            (8_000.0, 300.0),
            (10_000.0, 350.0),
            (12_000.0, 450.0),
        ] {
            assert!(
                i(alt, v) <= 0.1,
                "ascent {alt} m @ {v} m/s glowed: {}",
                i(alt, v)
            );
        }
        // The reentry corridor: partial burn on the thin 40 km pass,
        // full saturation in the deep corridor.
        assert!(
            i(40_000.0, 2_200.0) >= 0.5,
            "40 km @ 2200 reentry under-lit: {}",
            i(40_000.0, 2_200.0)
        );
        assert_eq!(
            i(35_000.0, 2_300.0),
            1.0,
            "35 km @ 2300 must saturate the sheath"
        );
        // The gate's edges: an orbital-speed pass in thin air is a
        // faint rim, not full burn (45 km @ 2400 → flux ≈ 1.6e4), and
        // no atmosphere means no sheath at any speed.
        let rim = i(45_000.0, 2_400.0);
        assert!(
            rim > 0.0 && rim < 0.5,
            "45 km @ 2400 should be a partial rim, got {rim}"
        );
        assert_eq!(plasma_intensity(0.0, 2_300.0), 0.0);
        // Speed alone is not enough either — below the gate onset no
        // flux level lights (a sea-level Mach-number edge case).
        assert_eq!(plasma_intensity(1.0e6, 200.0), 0.0);
    }

    /// A minimal `PartDef` for the readout-parity fixture — stack
    /// `top`/`bottom` attach nodes and one stock variant; `modules`
    /// carries whatever the part needs.
    fn readout_def(id: &str, mass: f64, modules: Vec<csp_data::ModuleDef>) -> csp_data::PartDef {
        csp_data::PartDef {
            schema_version: csp_data::PART_SCHEMA_VERSION,
            id: csp_data::PartId::from(id),
            family: csp_data::FamilyId::from("f"),
            display_name: "n".into(),
            category: csp_data::PartCategory::Utility,
            mass_dry_kg: mass,
            cost_funds: 1.0,
            size_m: DVec3::new(1.0, 1.0, 1.0),
            build_resources: Vec::new(),
            attach_nodes: vec![
                csp_data::AttachNode {
                    id: "top".into(),
                    position_m: DVec3::new(0.0, 0.0, 0.5),
                    direction: DVec3::Z,
                    diameter_m: 1.0,
                },
                csp_data::AttachNode {
                    id: "bottom".into(),
                    position_m: DVec3::new(0.0, 0.0, -0.5),
                    direction: DVec3::new(0.0, 0.0, -1.0),
                    diameter_m: 1.0,
                },
            ],
            scale: csp_data::ScaleRule::None,
            collider: csp_data::ColliderDef::Box {
                half_extents_m: DVec3::new(0.5, 0.5, 0.5),
            },
            aero: csp_data::AeroDef {
                cd_curve: Vec::new(),
                cl_curve: Vec::new(),
                ref_len_m: 1.0,
            },
            thermal: csp_data::ThermalDef {
                heat_capacity_j_per_k: 1.0,
                max_temp_k: 1.0,
                emissivity: 0.5,
                conduction_w_per_k: 1.0,
            },
            structural: csp_data::StructuralDef {
                max_axial_n: 1.0,
                max_shear_n: 1.0,
                max_torque_nm: 1.0,
            },
            variants: vec![csp_data::PartVariant {
                id: csp_data::VariantId::from("stock"),
                display_name: "v".into(),
                cost_multiplier: 1.0,
                stat_multipliers: Vec::new(),
            }],
            modules,
            reliability: csp_data::ReliabilityDef {
                base_mtbf_s: 1.0e9,
                infant_mortality: 0.0,
                qualification_curve: csp_data::QualCurve {
                    knots: vec![csp_data::QualKnot {
                        qualification_units: 0.0,
                        hazard_multiplier: 1.0,
                    }],
                },
                regime_stress: Vec::new(),
                dormant_stress: 0.02,
                failure_modes: vec![(csp_data::FailureMode::SensorFault, 1.0)],
            },
            tech_requirement: csp_data::TechNodeId::from("t"),
        }
    }

    /// The canonical two-engine stack's catalog + resource file —
    /// the same numbers the readout fixture uses (fuel at 5 kg/u).
    fn readout_catalog() -> (csp_data::LoadedParts, csp_data::ResourceFile) {
        let fuel = csp_data::ResourceId::from("fuel");
        let engine = |id: &str, thrust: f64, isp: f64| {
            readout_def(
                id,
                80.0,
                vec![csp_data::ModuleDef::Engine {
                    thrust_vac_n: thrust,
                    isp_vac_s: isp,
                    isp_sl_s: 250.0,
                    throttle_range: csp_data::ThrottleRange { min: 0.0, max: 1.0 },
                    gimbal_deg: 0.0,
                    ignitions: None,
                    rated_burn_s: 600.0,
                    propellants: vec![(fuel.clone(), 1.0)],
                    ullage_required: false,
                }],
            )
        };
        let defs = csp_data::LoadedParts {
            parts: vec![
                readout_def(
                    "r.hull",
                    100.0,
                    vec![csp_data::ModuleDef::Control {
                        torque_nm: 10.0,
                        power_w: 1.0,
                    }],
                ),
                readout_def(
                    "r.tank",
                    50.0,
                    vec![csp_data::ModuleDef::Tank {
                        capacity: vec![(fuel.clone(), 200.0)],
                    }],
                ),
                engine("r.eng", 10_000.0, 300.0),
                readout_def("r.dec", 20.0, vec![csp_data::ModuleDef::Decoupler]),
                engine("r.beng", 20_000.0, 280.0),
            ]
            .into_iter()
            .map(|def| csp_data::LoadedPart {
                def,
                path: "x".into(),
            })
            .collect(),
        };
        let res = csp_data::ResourceFile {
            schema_version: 1,
            resources: vec![csp_data::ResourceDef {
                id: fuel,
                display_name: "r.fuel".into(),
                mass_per_unit_kg: 5.0,
            }],
        };
        (defs, res)
    }

    /// C13-F1 parity: the VAB's stage rows must equal the figures the
    /// flight model delivers — the readout's first row is the pooled
    /// `burn_step` over the whole attached set (stage boundaries
    /// jettison, they do not ignite), and its post-drop row matches
    /// the Δv walk over the survivors. The canonical stack: hull
    /// 100 kg → tank 50 + 1000 kg fuel → sustainer 10 kN/300 s →
    /// decoupler 20 kg → booster tank → booster 20 kN/280 s.
    #[test]
    #[allow(clippy::too_many_lines)] // fixture + assertions read as one narrative
    fn vab_stage_readout_matches_flight_burn_step() {
        let (defs, res) = readout_catalog();
        let stock = |id: &str| {
            csp_phys::BuiltPart::stock(
                csp_data::PartId::from(id),
                csp_data::VariantId::from("stock"),
            )
        };
        let mut s = csp_phys::CraftSession::new();
        let hull = s.add_root(&defs, stock("r.hull")).unwrap();
        let tank = s
            .add_part(
                &defs,
                stock("r.tank"),
                csp_phys::AttachEdge::new(hull, "bottom", "top"),
            )
            .unwrap();
        let eng = s
            .add_part(
                &defs,
                stock("r.eng"),
                csp_phys::AttachEdge::new(tank, "bottom", "top"),
            )
            .unwrap();
        let dec = s
            .add_part(
                &defs,
                stock("r.dec"),
                csp_phys::AttachEdge::new(eng, "bottom", "top"),
            )
            .unwrap();
        let btank = s
            .add_part(
                &defs,
                stock("r.tank"),
                csp_phys::AttachEdge::new(dec, "bottom", "top"),
            )
            .unwrap();
        let beng = s
            .add_part(
                &defs,
                stock("r.beng"),
                csp_phys::AttachEdge::new(btank, "bottom", "top"),
            )
            .unwrap();
        s.set_stage(dec, 1).unwrap();
        s.set_stage(beng, 1).unwrap();
        let a = csp_phys::readout::analyze(&s, &defs, &res).unwrap();
        assert_eq!(a.stages.len(), 2);
        let (r1, r0) = (&a.stages[0], &a.stages[1]);
        assert_eq!(r1.stage, 1);

        // The flight side on the same attached set — node masses are
        // dry + contents, the `WalkNode` convention `build_walk` reads.
        let nodes = vec![
            prop_node(hull.0, 100.0, 0.0, 0.0, &[], &[]),
            prop_node(tank.0, 1050.0, 0.0, 0.0, &[], &[("fuel", 200.0, 5.0)]),
            prop_node(eng.0, 80.0, 10_000.0, 300.0, &[("fuel", 1.0)], &[]),
            prop_node(dec.0, 20.0, 0.0, 0.0, &[], &[]),
            prop_node(btank.0, 1050.0, 0.0, 0.0, &[], &[("fuel", 200.0, 5.0)]),
            prop_node(beng.0, 80.0, 20_000.0, 280.0, &[("fuel", 1.0)], &[]),
        ];
        let (attached, mpu) = build_walk(&nodes);
        let full = burn_step(&attached, &mpu, 1.0);
        assert!(
            (r1.thrust_vac_n - full.thrust_n).abs() < 1e-9,
            "{r1:?} vs {} N",
            full.thrust_n
        );
        assert!((r1.isp_vac_s - full.isp_s).abs() < 1e-9);
        assert!((r1.mass_flow_kg_s - full.mass_flow_kg_s).abs() < 1e-9);
        assert!((r1.burn_time_s - full.burn_s).abs() < 1e-9);
        let wet: f64 = attached.values().map(|n| n.mass_kg).sum();
        assert!((r1.wet_mass_kg - wet).abs() < 1e-9);
        assert!((r1.twr_surface - full.thrust_n / (wet * csp_data::G0)).abs() < 1e-9);
        // Δv: the walk's current segment is the stage-1 row; the row's
        // own severed set is the walk's drop list, so the post-drop
        // (starved) row must equal the staged remainder — 0 here.
        let (mut attached2, mpu2) = build_walk(&nodes);
        let drops = [StageActions {
            jettisons: r1.separated_ids.clone(),
            ..StageActions::default()
        }];
        let (dv_now, dv_staged) = delta_v_walk_from(&mut attached2, &mpu2, &drops);
        assert!(
            (r1.delta_v_m_per_s - dv_now).abs() < 1e-9,
            "{r1:?} vs {dv_now}"
        );
        assert!(
            (r0.delta_v_m_per_s - dv_staged).abs() < 1e-9,
            "{r0:?} vs {dv_staged}"
        );
        // The starved row agrees on the coverage-zeroed figures too:
        // no deliverable thrust, no burn — the "0:00" case.
        let post = burn_step(&attached2, &mpu2, 1.0);
        assert!(post.deprived && post.thrust_n == 0.0);
        assert_eq!(r0.thrust_vac_n, 0.0);
        assert_eq!(r0.burn_time_s, 0.0);
        assert_eq!(r0.twr_surface, 0.0);
    }

    /// C10-F2/C22-F5's staged input: the windowed `‖Δv‖/dt` g-load —
    /// first sighting reads 0, an advancing window divides the inertial
    /// Δv by the elapsed sim seconds, a paused frame keeps the last
    /// reading, and a vanished vessel drops its entry.
    #[test]
    fn regime_loads_measures_the_frame_window() {
        let hz = csp_core::TICK_HZ as f64;
        let mut loads = RegimeLoads::default();
        // First sighting — no window yet.
        loads.stage(&[(1, DVec3::ZERO)], 0);
        assert_eq!(loads.g_for(1), 0.0);
        // 64 ticks later the vessel gained 9.81 m/s → 1.0 g sustained.
        loads.stage(&[(1, DVec3::new(0.0, 9.81, 0.0))], 64);
        assert!((loads.g_for(1) - 1.0).abs() < 1e-9, "g {}", loads.g_for(1));
        // Another second at +19.62 m/s² over the window → 2.0 g.
        loads.stage(&[(1, DVec3::new(0.0, 9.81 * 3.0, 0.0))], 128);
        assert!((loads.g_for(1) - 2.0).abs() < 1e-9, "g {}", loads.g_for(1));
        // A paused frame (same tick) keeps the last reading.
        loads.stage(&[(1, DVec3::new(0.0, 9.81 * 3.0, 0.0))], 128);
        assert!((loads.g_for(1) - 2.0).abs() < 1e-9);
        // A second vessel measures its own window independently.
        loads.stage(
            &[(1, DVec3::new(0.0, 9.81 * 3.0, 0.0)), (2, DVec3::ZERO)],
            128 + (hz * 0.5) as u64,
        );
        assert_eq!(loads.g_for(2), 0.0);
        // Vessel 1 vanishes — its entry drops with it.
        loads.stage(&[(2, DVec3::new(0.0, 0.0, 9.81))], 128 + hz as u64);
        assert_eq!(loads.g_for(1), 0.0);
        // …and vessel 2's half-second window still reads its own Δv:
        // +9.81 m/s over 32 ticks = 2.0 g.
        assert!((loads.g_for(2) - 2.0).abs() < 1e-9, "g {}", loads.g_for(2));
    }

    /// A bare `FlightBody` for the focus-rule tests — every field the
    /// rule ignores sits at its zero value.
    fn bare_body(id: u64, debris: bool) -> FlightBody {
        FlightBody {
            id,
            pos: DVec3::ZERO,
            rot: DQuat::IDENTITY,
            vel: DVec3::ZERO,
            throttle: 0.0,
            debris,
            contact: false,
            plasma_w_m2: 0.0,
            nodes: 1,
            parts: Arc::new(Vec::new()),
            plumes: Vec::new(),
            com: DVec3::ZERO,
            latched: false,
            tanks: Vec::new(),
        }
    }

    /// C19-F7: breakup fragments mint higher ids than the retained
    /// hull, so a bare `last()` handed the camera to uncommandable
    /// debris by construction. `focus_vessel` must prefer the newest
    /// NON-DEBRIS staged vessel — and still follow a fresh launch
    /// (non-debris) or a debris-only scene (the newest wreck).
    #[test]
    fn focus_vessel_prefers_the_retained_hull_over_breakup_debris() {
        // The bug's shape: hull at id 1, two fragments minted after it.
        let scene = [bare_body(1, false), bare_body(2, true), bare_body(3, true)];
        assert_eq!(
            focus_vessel(&scene).map(|b| b.id),
            Some(1),
            "focus must land on the retained hull, not the newest fragment"
        );
        // A fresh launch is non-debris — the newest-spawn rule is
        // unchanged for live vessels.
        let launch = [bare_body(1, true), bare_body(2, false)];
        assert_eq!(focus_vessel(&launch).map(|b| b.id), Some(2));
        // Debris only: the hull is gone entirely — the newest wreck
        // reads better than a dead frame.
        let wreck = [bare_body(4, true), bare_body(5, true)];
        assert_eq!(focus_vessel(&wreck).map(|b| b.id), Some(5));
        assert!(focus_vessel(&[]).is_none());
    }

    /// C19-F5's sync half: `sync_flight` MOVES the staged buffers into
    /// the mirror — `FlightSyncInput` is rebuilt per `end_frame` and
    /// read by nothing after `stage_program`, so the mirror takes the
    /// vec outright instead of deep-cloning the fleet a second time.
    #[test]
    fn sync_flight_moves_the_staged_buffers() {
        let mut world = bevy_ecs::world::World::new();
        world.insert_resource(FlightSyncInput {
            env: None,
            vessels: vec![bare_body(7, false)],
            live_vessels: 1,
            propulsion: vec![VesselPropulsion {
                vessel: 7,
                ..VesselPropulsion::default()
            }],
            rails: vec![7],
            radii: vec![(7, 2.5)],
        });
        world.insert_resource(FlightSnapshot::default());
        world.insert_resource(LiveVesselCount::default());
        world.insert_resource(VesselPropulsionStaged::default());
        world.insert_resource(RailsStaged::default());
        world.insert_resource(CraftRadii::default());
        let mut sched = Schedule::default();
        sched.add_systems(sync_flight);
        sched.run(&mut world);
        assert_eq!(
            world.resource::<FlightSnapshot>().vessels[0].id,
            7,
            "the staged vessel must land in the mirror"
        );
        assert_eq!(world.resource::<LiveVesselCount>().0, 1);
        assert_eq!(world.resource::<VesselPropulsionStaged>().0[0].vessel, 7);
        assert_eq!(world.resource::<RailsStaged>().0, vec![7]);
        assert_eq!(world.resource::<CraftRadii>().0, vec![(7, 2.5)]);
        assert!(
            world.resource::<FlightSyncInput>().vessels.is_empty()
                && world.resource::<FlightSyncInput>().propulsion.is_empty()
                && world.resource::<FlightSyncInput>().rails.is_empty()
                && world.resource::<FlightSyncInput>().radii.is_empty(),
            "the staged input must be consumed, not copied"
        );
    }

    /// C19-F6's staging half: when `hud_plan`'s fallback borrows
    /// another vessel's plan, the mirror names the owner — and reads
    /// `None` while the plan IS the focused vessel's own.
    #[test]
    fn flight_present_names_the_borrowed_plan_owner() {
        let run = |plan_vessel: VesselId| -> Option<u64> {
            let mut world = bevy_ecs::world::World::new();
            world.insert_resource(FlightInput::default());
            world.insert_resource(FlightSnapshot {
                env: None,
                vessels: vec![bare_body(7, false)],
            });
            world.insert_resource(LiveVesselCount::default());
            world.insert_resource(RailsStaged::default());
            // The plan is bound to `plan_vessel`; the staged focus is
            // vessel 7 — `hud_plan` borrows the newest live plan.
            world.insert_resource(StagePlans::single(StagePlan::bound_to(
                vec![StageActions::default()],
                plan_vessel,
            )));
            world.insert_resource(VesselPropulsionStaged::default());
            world.insert_resource(TerrainConfig::default());
            world.insert_resource(TerrainCache::default());
            world.insert_resource(FlightCameraF64::default());
            world.insert_resource(DrawnScene::default());
            world.insert_resource(FlightUiData::default());
            world.insert_resource(FlightSfx::default());
            world.insert_resource(crate::Clock::default());
            world.insert_resource(FlightFocusOverride::default());
            world.insert_resource(crate::ProgramMirror::default());
            let mut sched = Schedule::default();
            sched.add_systems(flight_present);
            sched.run(&mut world);
            world.resource::<FlightUiData>().stages_plan_vessel
        };
        assert_eq!(
            run(VesselId(9)),
            Some(9),
            "a plan bound to another vessel must be labeled"
        );
        assert_eq!(
            run(VesselId(7)),
            None,
            "the focused vessel's own plan needs no owner label"
        );
    }

    /// The launch-UX camera hold: an empty arena with a build order
    /// on the books is a delivery in flight — the no-focus branch
    /// must KEEP the last solved pose (the pad the launch pressed
    /// from), so `extract_flight`'s terrain gate stays open for the
    /// whole build instead of dropping to the fallback void. The
    /// same empty arena with NO order still invalidates — the
    /// dead-scene path `sim_step` bounces out of.
    #[test]
    fn no_focus_holds_the_camera_while_a_build_is_pending() {
        let run = |builds: Vec<csp_ui::program::BuildRow>| -> (bool, usize) {
            let mut world = bevy_ecs::world::World::new();
            world.insert_resource(FlightInput::default());
            world.insert_resource(FlightSnapshot {
                env: None,
                vessels: vec![],
            });
            world.insert_resource(LiveVesselCount::default());
            world.insert_resource(RailsStaged::default());
            world.insert_resource(StagePlans::default());
            world.insert_resource(VesselPropulsionStaged::default());
            world.insert_resource(TerrainConfig::default());
            world.insert_resource(TerrainCache::default());
            // A previously solved pose — the pad camera from before
            // the launch's pad-clearing drain emptied the scene.
            let mut cam = FlightCameraF64::default();
            cam.0.valid = true;
            world.insert_resource(cam);
            world.insert_resource(DrawnScene::default());
            world.insert_resource(FlightUiData::default());
            world.insert_resource(FlightSfx::default());
            world.insert_resource(crate::Clock::default());
            world.insert_resource(FlightFocusOverride::default());
            world.insert_resource(crate::ProgramMirror {
                input: csp_ui::program::ProgramInput {
                    builds,
                    ..csp_ui::program::ProgramInput::default()
                },
                ..crate::ProgramMirror::default()
            });
            let mut sched = Schedule::default();
            sched.add_systems(flight_present);
            sched.run(&mut world);
            (
                world.resource::<FlightCameraF64>().0.valid,
                world.resource::<FlightUiData>().builds.len(),
            )
        };
        let pending = vec![csp_ui::program::BuildRow {
            id: 0,
            site: "terrin-launch".into(),
            label: "3-part craft".into(),
            eta_s: 100.0,
        }];
        assert_eq!(
            run(pending),
            (true, 1),
            "a pending order holds the camera + mirrors the row"
        );
        assert_eq!(
            run(vec![]),
            (false, 0),
            "a build-free empty arena is still the dead scene"
        );
    }

    // --- R1/3b: the cascade solve's arm contract ----------------------

    /// A test env over a body centred 600 km down −X (so the local up
    /// at a focus near the origin is +X), with the sun at
    /// `sun_x·X + 0.3·Y + 0.74·Z` normalized — `sun_x` is the sun's
    /// elevation knob at the focus.
    fn shadow_env(sun_x: f64) -> FlightEnv {
        let s = DVec3::new(sun_x, 0.3, 0.74);
        FlightEnv {
            body_center: DVec3::new(-600_000.0, 0.0, 0.0),
            radius_m: 600_000.0,
            atm_top_m: 70_000.0,
            atm_scale_height_m: 5_600.0,
            mu_m3_per_s2: 2.4e19,
            body: None,
            sun_dir: s / s.norm(),
            spin_axis: DVec3::Z,
            spin_phase: 0.0,
            body_spin: DVec3::ZERO,
        }
    }

    /// The arm predicate pins its threshold: strictly above arms, at
    /// and below do not (the boundary keeps the ray-march's grazing
    /// regime — see [`SHADOW_ARM_SUN_UP`]).
    #[test]
    fn cascade_arm_predicate_pins_the_threshold() {
        let up = DVec3::X;
        // A unit sun whose dot with `up` is exactly `d` (the y leg
        // completes the norm; the sun never points along ±X here).
        let sun_at = |d: f64| DVec3::new(d, (1.0 - d * d).max(0.0).sqrt(), 0.0);
        assert!(cascades_armed(sun_at(SHADOW_ARM_SUN_UP * 1.01), up));
        assert!(!cascades_armed(sun_at(SHADOW_ARM_SUN_UP), up));
        assert!(!cascades_armed(sun_at(SHADOW_ARM_SUN_UP * 0.99), up));
        assert!(!cascades_armed(sun_at(0.0), up));
        assert!(!cascades_armed(sun_at(-0.5), up), "night never arms");
    }

    /// The extract's cascade row flows armed by day: non-zero
    /// matrices, the documented split/far band constants, terrain
    /// packet present — and zeroed (the disarm row) at night. Run
    /// through `extract_rig`'s real Schedule so the wiring — not just
    /// the solve — is what's under test. The rig also pins the
    /// sky-only disarm (M5 §5.2-R2 arrival polish): while the bake is
    /// in flight the packet draws no terrain, and an armed row would
    /// drive the craft's cascade lookup into the never-cleared depth
    /// map — the row must be the disarmed fallback in those frames.
    #[test]
    fn extract_solves_armed_cascades_by_day_and_disarms_at_night() {
        // Deterministic reconstruction per use — `extract_pass` takes
        // the camera by value and `MapCameraF64` is not `Clone`.
        let cam = || {
            crate::map::solve_orbit_camera(
                0.7,
                0.25,
                60.0,
                (320.0, 240.0),
                DVec3::ZERO,
                FLIGHT_FOV_Y,
                0.1,
            )
        };
        // Day rig: sun 0.6 up the local (+X) horizon.
        let (mut world, mut sched) = extract_rig(
            FlightSnapshot {
                env: Some(shadow_env(0.6)),
                vessels: vec![],
            },
            cam(),
        );
        // First pass — fresh cache, the bake is in flight: the packet
        // is sky-only and the row must be DISARMED even in daylight.
        let pending = extract_pass(&mut world, &mut sched);
        assert!(
            pending
                .terrain
                .as_ref()
                .is_some_and(|t| t.heightfield.is_none()),
            "a fresh rig starts in the sky-only wait state"
        );
        assert!(
            !pending.shadow.armed(),
            "the sky-only wait must disarm the cascade row (an armed SU over \
             the never-cleared depth map kills the craft's sun term)"
        );
        // Idle-pump the SAME rig until the bake lands, then the day
        // row arms with the real field.
        let mut day = None;
        for _ in 0..4_000 {
            let p = extract_pass(&mut world, &mut sched);
            if p.terrain.as_ref().is_some_and(|t| t.heightfield.is_some()) {
                day = Some(p);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let day = day.expect("the background bake never landed for the rig");
        assert!(day.terrain.is_some(), "the env gate built a terrain packet");
        assert!(
            day.shadow.armed(),
            "a sun 0.6 above the focus horizon must arm the cascades"
        );
        // The solved band rows are the documented cascade budget
        // (2 km / 20 km defaults) and the matrices actually solved.
        assert_eq!(day.shadow.pars[1], 2_000.0);
        assert_eq!(day.shadow.pars[2], 20_000.0);
        assert!(
            day.shadow
                .vp
                .iter()
                .flatten()
                .any(|m| m.iter().any(|v| *v != 0.0))
        );
        // Night: same rig (the real field stays cached), sun down —
        // the all-zero DISARM row (a partial row would leave receivers
        // sampling dead matrices).
        world.insert_resource(FlightSnapshot {
            env: Some(shadow_env(-0.6)),
            vessels: vec![],
        });
        let night = extract_pass(&mut world, &mut sched);
        assert!(!night.shadow.armed());
        let zeroed = csp_render::shadow::ShadowUniforms::default();
        assert!(
            night.shadow.vp == zeroed.vp
                && night.shadow.cascade == zeroed.cascade
                && night.shadow.pars == zeroed.pars,
            "the disarm row must be all-zero, not merely flagged off"
        );
        // And the solve is presentation-stable: the same inputs
        // reproduce the same uniform bits (the texel-snap contract's
        // precondition on the view side).
        world.insert_resource(FlightSnapshot {
            env: Some(shadow_env(0.6)),
            vessels: vec![],
        });
        let again = extract_pass(&mut world, &mut sched);
        assert_eq!(day.shadow.vp, again.shadow.vp);
        assert_eq!(day.shadow.cascade, again.shadow.cascade);
        assert_eq!(day.shadow.pars, again.shadow.pars);
    }

    /// No environment (the vacuum/dead-scene fallback) leaves the
    /// cascade row disarmed — the pre-3b packet shape.
    #[test]
    fn extract_without_env_disarms_cascades() {
        let packet = run_extract(
            FlightSnapshot {
                env: None,
                vessels: vec![],
            },
            crate::map::solve_orbit_camera(
                0.7,
                0.25,
                60.0,
                (320.0, 240.0),
                DVec3::ZERO,
                FLIGHT_FOV_Y,
                0.1,
            ),
        );
        assert!(!packet.shadow.armed());
        assert!(packet.terrain.is_none());
    }

    /// The blob shadow lands under the FOCUSED CRAFT, not wherever the
    /// camera points. After a far focus change the camera's anchor is
    /// still hundreds of kilometres from the craft; the blob used to be
    /// read back out of the camera matrix and sat under the camera. Run
    /// through `extract_rig`'s real Schedule, over a flat body so the
    /// bake cannot move the surface under the assertion.
    #[test]
    fn extract_puts_the_blob_shadow_under_the_focused_craft() {
        let r = 600_000.0;
        let env = FlightEnv {
            body_center: DVec3::ZERO,
            radius_m: r,
            atm_top_m: 0.0,
            atm_scale_height_m: 0.0,
            mu_m3_per_s2: 1.0,
            body: Some(csp_data::BodyId::from("flat")),
            sun_dir: DVec3::new(0.8, 0.6, 0.0),
            spin_axis: DVec3::Z,
            spin_phase: 0.0,
            body_spin: DVec3::ZERO,
        };
        let flat = TerrainConfig {
            bodies: std::collections::BTreeMap::from([(
                "flat".to_owned(),
                crate::terrain::TerrainBody {
                    seed: 0,
                    amplitude_m: 0.0,
                    profile: csp_core::terrain::ReliefProfile::Legacy,
                    ocean_datum_m: None,
                    biome: None,
                },
            )]),
            ..TerrainConfig::default()
        };
        let craft = DVec3::new(r + 5.0, 0.0, 0.0);
        let mut body = bare_body(7, false);
        body.pos = craft;
        body.com = craft;
        // The camera orbits a point a quarter of the way round the
        // planet — 850 km from the craft, where a travel to it starts.
        let anchor = DVec3::new(0.0, r + 5.0, 0.0);
        let level = level_frame(anchor, Some(DVec3::ZERO), DVec3::Z);
        let cam = chase_camera(
            anchor + level.dir(0.7, 0.25) * 12.0,
            anchor,
            &level,
            (320.0, 240.0),
        );
        let (mut world, mut sched) = extract_rig(
            FlightSnapshot {
                env: Some(env.clone()),
                vessels: vec![body],
            },
            cam,
        );
        world.insert_resource(flat);
        let terrain = waited_terrain(&mut world, &mut sched);
        let blob = terrain.uniforms.shadow;
        let dir = DVec3::new(f64::from(blob[0]), f64::from(blob[1]), f64::from(blob[2]));
        assert!(blob[3] > 0.0, "no blob for a landed craft in daylight");
        assert!(
            dir.dot(DVec3::X) > 0.999_999,
            "the blob points {dir:?}, not up the craft's own radial"
        );
        assert!(
            dir.dot(DVec3::Y).abs() < 0.001,
            "the blob points at the camera's anchor, 850 km away"
        );
        assert_eq!(terrain.uniforms.atmo[2], 5.0, "the craft's altitude");

        // Nothing focused: no craft, no blob.
        world.insert_resource(FlightSnapshot {
            env: Some(env),
            vessels: Vec::new(),
        });
        let terrain = extract_pass(&mut world, &mut sched)
            .terrain
            .expect("the env gate built a terrain packet");
        assert_eq!(
            (terrain.uniforms.shadow, terrain.uniforms.atmo[2]),
            ([0.0; 4], 0.0)
        );
    }

    /// A rig over an ocean: Terrin's field at its authored amplitude on a
    /// 600 km body, the datum on the body's radius, no spin, no craft.
    /// Returns the rig, the environment, the level frame and the radius of
    /// the surface under +X as the extract samples it, with the bake
    /// landed. A bevy world lends one resource out at a time, so the
    /// cache leaves for that sample and goes straight back.
    fn ocean_rig() -> (bevy_ecs::world::World, Schedule, FlightEnv, LevelFrame, f64) {
        let r = 600_000.0;
        let env = FlightEnv {
            body_center: DVec3::ZERO,
            radius_m: r,
            atm_top_m: 0.0,
            atm_scale_height_m: 0.0,
            mu_m3_per_s2: 1.0,
            body: Some(csp_data::BodyId::from("sea")),
            sun_dir: DVec3::new(0.8, 0.6, 0.0),
            spin_axis: DVec3::Z,
            spin_phase: 0.0,
            body_spin: DVec3::ZERO,
        };
        let sea = TerrainConfig {
            bodies: std::collections::BTreeMap::from([(
                "sea".to_owned(),
                crate::terrain::TerrainBody {
                    seed: 601,
                    amplitude_m: 3_600.0,
                    profile: csp_core::terrain::ReliefProfile::Legacy,
                    ocean_datum_m: Some(r),
                    biome: None,
                },
            )]),
            ..TerrainConfig::default()
        };
        let level = level_frame(DVec3::X * r, Some(DVec3::ZERO), DVec3::Z);
        let nowhere = chase_camera(DVec3::X * (r + 50.0), DVec3::X * r, &level, (320.0, 240.0));
        let (mut world, mut sched) = extract_rig(
            FlightSnapshot {
                env: Some(env.clone()),
                vessels: Vec::new(),
            },
            nowhere,
        );
        world.insert_resource(sea);
        waited_pass(&mut world, &mut sched);
        let mut cache = world
            .remove_resource::<TerrainCache>()
            .expect("the terrain cache is installed");
        let cfg = world.resource::<TerrainConfig>();
        let floor_r = crate::terrain::surface_floor_radius(cfg, &mut cache, &env, DVec3::X);
        world.insert_resource(cache);
        (world, sched, env, level, floor_r)
    }

    /// The ground-contact dust ring of a craft resting on the seabed sits
    /// on the seabed — 0.3 m over it, seen through the water — not on the
    /// ocean's surface above. A craft in contact where Terrin's field lies
    /// deep under the datum gets its ring's every vertex 0.3 m over that
    /// ground, as far under the surface as it is, and the vertices carry
    /// their place relative to the eye, by which `flight.wgsl` measures
    /// the water in front of them. Run through `extract_rig`'s real
    /// Schedule, with the bake landed.
    #[test]
    fn the_dust_ring_of_a_craft_on_the_seabed_sits_on_the_seabed() {
        let (mut world, mut sched, env, level, floor_r) = ocean_rig();
        let r = env.radius_m;
        assert!(
            floor_r < r - 50.0,
            "the scene needs a seabed: the surface under +X is {:.1} m over the datum",
            floor_r - r
        );

        let com = DVec3::X * (floor_r + 2.0);
        let mut body = bare_body(7, false);
        body.pos = com;
        body.com = com;
        body.contact = true;
        // The camera 12 m from the craft, looking down at it.
        let eye = com + level.dir(0.7, 0.25) * 12.0;
        world.insert_resource(FlightSnapshot {
            env: Some(env),
            vessels: vec![body],
        });
        world.insert_resource(FlightCameraF64(chase_camera(
            eye,
            com,
            &level,
            (320.0, 240.0),
        )));
        let packet = extract_pass(&mut world, &mut sched);
        let dust = |v: &csp_render::flight::FlightVertex| {
            v.nrm == [0.0; 3] && v.color == [0.62, 0.55, 0.42, 0.35]
        };
        let ring: Vec<DVec3> = packet
            .vertices
            .iter()
            .filter(|v| dust(v))
            .map(|v| {
                eye + DVec3::new(
                    f64::from(v.world[0]),
                    f64::from(v.world[1]),
                    f64::from(v.world[2]),
                )
            })
            .collect();
        assert_eq!(ring.len(), 36, "the ring is twelve fan triangles");
        let ring_centre = DVec3::X * (floor_r + EYE_GROUND_CLEARANCE_M);
        for p in &ring {
            // The rim leaves the sphere's tangent: 2.6² / 2R = 5.6 μm out.
            close(
                p.norm(),
                floor_r + EYE_GROUND_CLEARANCE_M,
                1.0e-4,
                "a ring vertex's radius: on the seabed, not the surface",
            );
        }
        let on_rim = ring
            .iter()
            .filter(|p| ((**p - ring_centre).norm() - 2.6).abs() <= 1.0e-5)
            .count();
        assert_eq!(on_rim, 24, "the fan's rim: two rim corners per triangle");
        eprintln!(
            "dust ring on the seabed: the surface under the craft {:.1} m under the datum, the ring {:.3} m under the ocean's",
            r - floor_r,
            r - ring_centre.norm()
        );
    }

    // ==================== C1: extract geometry cache ================

    /// Raw bytes of one `repr(C)` Pod value — the packet-identity
    /// tests' comparison basis (byte equality, not field tolerance).
    #[allow(unsafe_code)] // a byte view of a Pod value — sound by the Pod contract
    fn pod_bytes<T: Copy>(v: &T) -> Vec<u8> {
        unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
            .to_vec()
    }
    fn pod_slice_bytes<T: Copy>(vs: &[T]) -> Vec<u8> {
        let mut out = Vec::with_capacity(std::mem::size_of_val(vs));
        for v in vs {
            out.extend_from_slice(&pod_bytes(v));
        }
        out
    }

    /// The C1 identity scene: Terrin (Legacy bake, ocean at the
    /// datum), a launch site, a spinning day, and a two-vessel craft
    /// (kitbash stack + debris cluster) at the pad.
    fn geom_cache_scene(tick: u64) -> FlightSnapshot {
        let omega = 2.0 * core::f64::consts::PI / 21_600.0;
        let env = FlightEnv {
            body_center: DVec3::ZERO,
            radius_m: 600_000.0,
            atm_top_m: 70_000.0,
            atm_scale_height_m: 5_600.0,
            mu_m3_per_s2: 3.5316e12,
            body: Some(csp_data::BodyId::from("terrin")),
            sun_dir: {
                let s = DVec3::new(0.4, 0.3, 0.86);
                s / s.norm()
            },
            spin_axis: DVec3::Z,
            spin_phase: omega * (tick as f64 / csp_core::TICK_HZ as f64),
            body_spin: DVec3::Z * omega,
        };
        let parts = Arc::new(vec![
            FlightPart {
                node: 0,
                lo: DVec3::new(-0.9, -0.9, 0.0),
                hi: DVec3::new(0.9, 0.9, 4.0),
                transform: csp_phys::PartTransform::IDENTITY,
                shape_key: ShapeKey::of(&crate::part_mesh::PartShape::Lathe {
                    axis: [0.0, 0.0],
                    profile: vec![(0.0, 0.9), (2.0, 0.9), (3.4, 0.6), (4.0, 0.2)],
                }),
                shape: crate::part_mesh::PartShape::Lathe {
                    axis: [0.0, 0.0],
                    profile: vec![(0.0, 0.9), (2.0, 0.9), (3.4, 0.6), (4.0, 0.2)],
                },
                kind: PartKind::Tank,
                model: None,
            },
            FlightPart {
                node: 1,
                lo: DVec3::new(-0.5, -0.5, -1.0),
                hi: DVec3::new(0.5, 0.5, 0.4),
                transform: csp_phys::PartTransform::new(
                    DVec3::new(0.0, 0.0, 4.2),
                    csp_core::DQuat::IDENTITY,
                ),
                shape_key: ShapeKey::of(&crate::part_mesh::PartShape::Box {
                    lo: DVec3::new(-0.5, -0.5, -1.0),
                    hi: DVec3::new(0.5, 0.5, 0.4),
                }),
                shape: crate::part_mesh::PartShape::Box {
                    lo: DVec3::new(-0.5, -0.5, -1.0),
                    hi: DVec3::new(0.5, 0.5, 0.4),
                },
                kind: PartKind::Crew,
                model: None,
            },
        ]);
        let site = crate::structures::SiteEntry {
            id: csp_data::SiteId::from("terrin-launch"),
            display_name: csp_data::LocKey::from("x"),
            body: csp_data::BodyId::from("terrin"),
            latitude_rad: 0.1,
            longitude_rad: 0.2,
            altitude_m: 0.0,
            launch: true,
        };
        let up = crate::structures::site_dir(&env, &site);
        let pos = up * (env.radius_m + 8.0);
        let mk = |debris: bool| FlightBody {
            id: debris as u64,
            pos: if debris { pos + DVec3::X * 30.0 } else { pos },
            rot: csp_core::DQuat::IDENTITY,
            vel: DVec3::ZERO,
            throttle: 0.0,
            debris,
            contact: false,
            plasma_w_m2: 0.0,
            nodes: 2,
            parts: parts.clone(),
            plumes: Vec::new(),
            com: pos,
            latched: false,
            tanks: Vec::new(),
        };
        FlightSnapshot {
            env: Some(env),
            vessels: vec![mk(false), mk(true)],
        }
    }

    /// The scene's world + a solved camera over the pad.
    fn geom_cache_rig(tick: u64) -> (bevy_ecs::world::World, Schedule) {
        let snap = geom_cache_scene(tick);
        let focus = snap.vessels[0].pos;
        let cam = crate::map::solve_orbit_camera(
            0.8,
            0.35,
            120.0,
            (1920.0, 1080.0),
            focus,
            FLIGHT_FOV_Y,
            0.1,
        );
        let (mut world, sched) = extract_rig(snap, cam);
        let mut bodies = std::collections::BTreeMap::new();
        bodies.insert(
            "terrin".to_owned(),
            crate::terrain::TerrainBody {
                seed: 601,
                amplitude_m: 3_600.0,
                profile: csp_core::terrain::ReliefProfile::Legacy,
                ocean_datum_m: Some(600_000.0),
                biome: None,
            },
        );
        world.insert_resource(crate::terrain::TerrainConfig {
            bodies,
            ..Default::default()
        });
        let mut sites = crate::structures::SiteTable::default();
        sites.sites.push(crate::structures::SiteEntry {
            id: csp_data::SiteId::from("terrin-launch"),
            display_name: csp_data::LocKey::from("x"),
            body: csp_data::BodyId::from("terrin"),
            latitude_rad: 0.1,
            longitude_rad: 0.2,
            altitude_m: 0.0,
            launch: true,
        });
        world.insert_resource(sites);
        (world, sched)
    }

    /// A packet's comparable bytes: the two vertex streams, the light
    /// and shadow rows, and the terrain packet's uniform + instance
    /// rows (the heightfield itself is keyed, not embedded).
    fn packet_bytes(p: &csp_render::flight::FlightPacket) -> Vec<u8> {
        let mut out = pod_slice_bytes(&p.vertices);
        out.extend_from_slice(&pod_slice_bytes(&p.far_vertices));
        out.extend_from_slice(&pod_bytes(&p.light));
        out.extend_from_slice(&pod_bytes(&p.shadow));
        if let Some(t) = &p.terrain {
            out.extend_from_slice(&pod_bytes(&t.uniforms));
            out.extend_from_slice(&pod_bytes(&t.uniforms_far));
            out.extend_from_slice(&pod_slice_bytes(&t.terrain));
            out.extend_from_slice(&pod_slice_bytes(&t.ocean));
            out.extend_from_slice(&t.terrain_far_start.to_le_bytes());
            out.extend_from_slice(&t.ocean_far_start.to_le_bytes());
            out.extend_from_slice(
                &t.heightfield
                    .as_ref()
                    .map_or(u64::MAX, |h| h.key)
                    .to_le_bytes(),
            );
        }
        out
    }

    /// C1's packet-identity contract: for the same scene state the
    /// emitted packet bytes are IDENTICAL cold vs warm (the caches
    /// change only where the work happens, never the output), and a
    /// frame sequence with the clock advancing reproduces byte-for-
    /// byte from a fresh world — cache state never leaks into the
    /// packet.
    #[test]
    fn extract_packets_are_byte_identical_cold_warm_and_across_runs() {
        let run_sequence = |cam_yaw: f64| -> Vec<Vec<u8>> {
            let (mut world, mut sched) = geom_cache_rig(64 * 300);
            // Pump the background bake (the arrival pattern).
            for _ in 0..4_000 {
                let p = extract_pass(&mut world, &mut sched);
                if p.terrain.as_ref().is_some_and(|t| t.heightfield.is_some()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let mut frames = Vec::new();
            for f in 0..8 {
                // One tick per frame — the spinning day advances, so
                // every frame's spin phase (and the probes' windows)
                // moves, exactly the live-flight pattern.
                let tick = 64 * 300 + f;
                let snap = geom_cache_scene(tick);
                let focus = snap.vessels[0].pos;
                world.insert_resource(FlightSnapshot {
                    env: snap.env,
                    vessels: snap.vessels,
                });
                world.insert_resource(FlightCameraF64(crate::map::solve_orbit_camera(
                    cam_yaw,
                    0.35,
                    120.0,
                    (1920.0, 1080.0),
                    focus,
                    FLIGHT_FOV_Y,
                    0.1,
                )));
                world.insert_resource(crate::Clock(csp_core::SimTime::from_ticks(tick)));
                let p = extract_pass(&mut world, &mut sched);
                assert!(
                    p.terrain.as_ref().is_some_and(|t| t.heightfield.is_some()),
                    "the field must stay live across the sequence"
                );
                // Warm re-extract at the SAME state: identical bytes.
                let warm = extract_pass(&mut world, &mut sched);
                assert_eq!(
                    packet_bytes(&p),
                    packet_bytes(&warm),
                    "frame {f}: cold and warm extracts must be byte-identical"
                );
                frames.push(packet_bytes(&p));
            }
            frames
        };
        let a = run_sequence(0.8);
        let b = run_sequence(0.8);
        assert!(
            a.iter().zip(&b).all(|(x, y)| x == y),
            "the same frame sequence from a fresh world must reproduce byte-for-byte"
        );
        assert!(
            a.windows(2).any(|w| w[0] != w[1]),
            "the sequence must actually advance (frames differ across ticks)"
        );
        assert!(
            a[0].len() > 10_000,
            "the scene must emit real geometry ({} bytes)",
            a[0].len()
        );
    }

    /// C1's craft-cache keys: the silhouette bake is keyed by (shape
    /// bits, kind, debris) — every distinct key bakes once, repeats
    /// hit — and the model bake is keyed by the model's PINNED Arc
    /// address + fit AABB (a clone hits; a same-content new Arc
    /// bakes; a different fit bakes).
    #[test]
    fn flight_geom_cache_keys_shape_kind_debris_and_pins_models() {
        let mut g = FlightGeomCache::default();
        let box_a = crate::part_mesh::PartShape::Box {
            lo: DVec3::new(-1.0, -1.0, -1.0),
            hi: DVec3::new(1.0, 1.0, 1.0),
        };
        let box_b = crate::part_mesh::PartShape::Box {
            lo: DVec3::new(-2.0, -1.0, -1.0),
            hi: DVec3::new(1.0, 1.0, 1.0),
        };
        // P1's cached-key contract: callers hand the key they folded
        // at part staging — the fresh `ShapeKey::of` of the SAME shape
        // must hit the entry it cached (bit-identity preserved, the
        // lathe profile's Vec included), and a part-cached key of a
        // DIFFERENT shape must miss. This is the identity the packet
        // rides on: the extract never re-folds, so a drift here would
        // silently serve one shape's silhouette under another's key.
        let lathe = crate::part_mesh::PartShape::Lathe {
            axis: [0.0, 0.0],
            profile: vec![(0.0, 0.9), (2.0, 0.9), (3.4, 0.6), (4.0, 0.2)],
        };
        let t = g.smooth_tris(&box_a, &ShapeKey::of(&box_a), PartKind::Tank, false);
        assert_eq!(g.smooth_bakes, 1);
        assert!(!t.is_empty());
        g.smooth_tris(&box_a, &ShapeKey::of(&box_a), PartKind::Tank, false);
        assert_eq!(g.smooth_bakes, 1, "an identical key must hit");
        g.smooth_tris(&box_a, &ShapeKey::of(&box_a), PartKind::Tank, true);
        assert_eq!(g.smooth_bakes, 2, "debris folds into the key");
        g.smooth_tris(&box_a, &ShapeKey::of(&box_a), PartKind::Engine, false);
        assert_eq!(g.smooth_bakes, 3, "kind folds into the key (livery)");
        g.smooth_tris(&box_b, &ShapeKey::of(&box_b), PartKind::Tank, false);
        assert_eq!(g.smooth_bakes, 4, "shape bits fold into the key");
        g.smooth_tris(&lathe, &ShapeKey::of(&lathe), PartKind::Tank, false);
        assert_eq!(g.smooth_bakes, 5, "a lathe profile is its own key");
        g.smooth_tris(&lathe, &ShapeKey::of(&lathe), PartKind::Tank, false);
        assert_eq!(
            g.smooth_bakes, 5,
            "the cached lathe key must hit (no re-fold)"
        );
        // The debris variant's colors differ from the clean one.
        let clean = g.smooth_tris(&box_a, &ShapeKey::of(&box_a), PartKind::Tank, false);
        let debris = g.smooth_tris(&box_a, &ShapeKey::of(&box_a), PartKind::Tank, true);
        assert_ne!(
            clean[0].col[0], debris[0].col[0],
            "debris charring must reach the baked colors"
        );
        // Models: pinned by the Arc the entry itself holds. A minimal
        // real model (two tris) exercises `emit_model`'s fit.
        let tiny_model = || {
            Arc::new(crate::art::PartModel {
                verts: vec![crate::art::ModelVert {
                    pos: [0.0, 0.0, 0.0],
                    nrm: [0.0, 0.0, 1.0],
                    col: [1.0, 1.0, 1.0, 1.0],
                    mat: [0.5, 8.0, 1.0],
                }],
                indices: vec![0, 0, 0],
                lo: DVec3::new(-1.0, -1.0, -1.0),
                hi: DVec3::new(1.0, 1.0, 1.0),
            })
        };
        let m = tiny_model();
        let lo = DVec3::new(-1.0, -1.0, -1.0);
        let hi = DVec3::new(1.0, 1.0, 1.0);
        g.model_tris(&m, lo, hi);
        assert_eq!(g.model_bakes, 1);
        g.model_tris(&m.clone(), lo, hi);
        assert_eq!(g.model_bakes, 1, "the same model Arc must hit");
        g.model_tris(&tiny_model(), lo, hi);
        assert_eq!(
            g.model_bakes, 2,
            "a same-content fresh Arc is a distinct (safe) key — it bakes"
        );
        g.model_tris(&m, lo, DVec3::new(2.0, 1.0, 1.0));
        assert_eq!(g.model_bakes, 3, "a different fit AABB bakes");
    }

    /// §2.5's pose history ages by tick, not by frame. A vessel first
    /// seen draws at its staged pose; across a one-tick step it draws
    /// the remainder of the way; a frame that carried several ticks
    /// draws across the whole span; a zero-tick frame keeps closing on
    /// the same current pose and never passes it; a clock that went
    /// backwards, or a vessel that left and returned, starts over; and
    /// at remainder 0 every pose is the staged one, exactly.
    #[test]
    fn pose_history_advances_by_tick_not_by_frame() {
        // Origin `x` metres along +X, CoM a metre above it.
        let body = |x: f64| FlightBody {
            pos: DVec3::new(x, 0.0, 0.0),
            com: DVec3::new(x, 0.0, 1.0),
            ..bare_body(7, false)
        };
        let near = |a: DVec3, b: DVec3| assert!((a - b).norm() < 1e-9, "{a:?} vs {b:?}");
        let mut h = PoseHistory::default();

        let drawn = h.advance(&[body(10.0)], 100, 0.5);
        assert_eq!(drawn, vec![DrawnPose::staged(&body(10.0))]);

        // Tick 101, a quarter past it: a quarter of the 100 → 101 step.
        let drawn = h.advance(&[body(10.4)], 101, 0.25)[0];
        near(drawn.pos, DVec3::new(10.1, 0.0, 0.0));
        near(drawn.com, DVec3::new(10.1, 0.0, 1.0));

        // A frame that carried four ticks (×4 warp at 60 fps): the span
        // is 101 → 105 and the drawn instant `(105 − 1) + 0.5`, so
        // 3.5 of the 4 ticks across.
        let drawn = h.advance(&[body(12.0)], 105, 0.5)[0];
        near(drawn.pos, DVec3::new(10.4 + 1.6 * 0.875, 0.0, 0.0));

        // A zero-tick frame: the same tick restaged, the remainder
        // grown — still closing on tick 105's pose.
        let drawn = h.advance(&[body(12.0)], 105, 0.75)[0];
        near(drawn.pos, DVec3::new(10.4 + 1.6 * 0.9375, 0.0, 0.0));
        // The largest remainder there is stops short of it — to within
        // a rounding step of the arithmetic.
        let drawn = h.advance(&[body(12.0)], 105, 1.0 - f64::EPSILON)[0];
        assert!(
            drawn.pos.x > 11.99 && drawn.pos.x <= 12.0,
            "{}",
            drawn.pos.x
        );

        // Remainder 0: the staged pose, bit for bit.
        assert_eq!(
            h.advance(&[body(12.0)], 106, 0.0)[0],
            DrawnPose::staged(&body(12.0))
        );

        // The clock went backwards — a world restored at an earlier
        // tick: the samples describe a discarded timeline.
        let drawn = h.advance(&[body(3.0)], 90, 0.5)[0];
        assert_eq!(drawn, DrawnPose::staged(&body(3.0)));

        // A vessel absent for a frame has left the path.
        assert!(h.advance(&[], 91, 0.5).is_empty());
        let drawn = h.advance(&[body(3.1)], 92, 0.5)[0];
        assert_eq!(drawn, DrawnPose::staged(&body(3.1)));
    }

    /// The drawn rotation is a unit rotation on the shorter arc between
    /// its endpoints — `q` and `−q` are the same rotation, so the
    /// negated end pose is not the long way round — exact at both ends
    /// and half-way between them at the middle.
    #[test]
    fn rot_between_takes_the_shortest_arc() {
        // The same rotation, to the last digit — whichever sign each
        // quaternion carries.
        let same = |a: DQuat, b: DQuat| {
            let flip = if a.dot(b) < 0.0 { -1.0 } else { 1.0 };
            assert!(
                [
                    a.x * flip - b.x,
                    a.y * flip - b.y,
                    a.z * flip - b.z,
                    a.w * flip - b.w
                ]
                .iter()
                .all(|d| d.abs() < 1e-12),
                "{a:?} vs {b:?}"
            );
        };
        let q0 = DQuat::from_axis_angle(DVec3::new(0.3, -0.5, 0.8), 0.4);
        let q1 = DQuat::from_axis_angle(DVec3::new(0.3, -0.5, 0.8), 0.5);
        let neg = DQuat::new(-q1.x, -q1.y, -q1.z, -q1.w);
        let mid = DQuat::from_axis_angle(DVec3::new(0.3, -0.5, 0.8), 0.45);
        for end in [q1, neg] {
            same(rot_between(q0, end, 0.0), q0);
            same(rot_between(q0, end, 1.0), end);
            same(rot_between(q0, end, 0.5), mid);
            assert!((rot_between(q0, end, 0.3).norm() - 1.0).abs() < 1e-12);
        }
    }

    // --- Chase camera (terrain-overhaul 3.2) --------------------------

    /// A chase frame in deep space — no body, so no floor — for vessel
    /// `id` at the origin.
    fn space_frame(id: u64, radius_m: f64, dt_ms: f64) -> ChaseFrame {
        ChaseFrame {
            id,
            com: DVec3::ZERO,
            radius_m,
            centre: None,
            axis: DVec3::Z,
            dt_wall_ms: dt_ms,
        }
    }

    fn no_floor(_: DVec3) -> f64 {
        f64::NEG_INFINITY
    }

    /// `input` came to rest on vessel `id` at the origin last frame, so a
    /// frame for `id` keeps its zoom instead of framing.
    fn held(input: &mut FlightInput, id: u64) {
        input.travel.last = Some(Last {
            id,
            anchor: DVec3::ZERO,
            look_at: DVec3::ZERO,
            skirt: DVec3::ZERO,
            eye: DVec3::ZERO,
            carried: DVec3::ZERO,
            moved_m: None,
            travelled: 0.0,
        });
    }

    /// A camera that holds a vessel of radius 2 m at a set distance.
    fn camera_at(distance_m: f64) -> FlightInput {
        let mut input = FlightInput::default();
        input.set_distance(distance_m);
        input
    }

    fn close(actual: f64, want: f64, tol: f64, what: &str) {
        assert!(
            (actual - want).abs() <= tol,
            "{what}: {actual} vs {want} (tol {tol})"
        );
    }

    /// R9: the render gates place the camera by setting its distance
    /// directly. That sets the current AND the target distance, so no
    /// frame eases it toward anything; an unusable value changes nothing.
    #[test]
    fn a_directly_set_distance_is_current_and_target() {
        let mut input = FlightInput::default();
        assert_eq!(
            (input.distance_m(), input.distance_target_m()),
            (60.0, 60.0)
        );
        input.set_distance(300.0);
        assert_eq!(
            (input.distance_m(), input.distance_target_m()),
            (300.0, 300.0)
        );
        // The gates set it before the first focus and hand over no
        // frame time: the first focus frames the target, the eye stays.
        step_chase_camera(&mut input, &space_frame(1, 10.0, 0.0), &mut no_floor);
        assert_eq!(input.distance_m(), 300.0);
        // On a held focus with frame time flowing, nothing eases either.
        input.set_distance(300.0);
        let cam = step_chase_camera(&mut input, &space_frame(1, 10.0, 16.0), &mut no_floor);
        assert_eq!(input.distance_m(), 300.0);
        assert_eq!(input.distance_target_m(), 300.0);
        assert_eq!(cam.distance_m, 300.0);
        for unusable in [f64::NAN, f64::INFINITY, 0.0, -5.0] {
            input.set_distance(unusable);
        }
        assert_eq!(input.distance_m(), 300.0);
        assert_eq!(input.distance_target_m(), 300.0);
    }

    /// R2: the wheel moves the target and the distance closes on it with
    /// an 80 ms half-life — the share closed depends on wall time alone,
    /// so 30, 60 and 144 fps land on the same figure and no frame steps
    /// past the target. After 8 1/3 half-lives (2/3 s) 0.31 % is left;
    /// after 6 1/4 (1/2 s) 1.33 % still is, so the 1 % bound has teeth.
    #[test]
    fn zoom_eases_with_an_80_ms_half_life_at_any_frame_rate() {
        for (fps, at_half_s, at_two_thirds_s) in [(30.0, 15, 20), (60.0, 30, 40), (144.0, 72, 96)] {
            for (start, target) in [(60.0, 20.0), (20.0, 300.0)] {
                let mut input = camera_at(start);
                held(&mut input, 1);
                input.distance_target_m = target;
                let dt_ms = 1000.0 / fps;
                let share = ease_fraction(dt_ms / 1000.0, EASE_HALF_LIFE_S);
                let mut left_at_half_s = f64::NAN;
                for frame in 1..=at_two_thirds_s {
                    let before = input.distance_m();
                    step_chase_camera(&mut input, &space_frame(1, 0.0, dt_ms), &mut no_floor);
                    let now = input.distance_m();
                    let closed = (now - before) * (target - start).signum();
                    assert!(closed > 0.0, "{fps} fps, frame {frame}: stalled");
                    assert!(
                        (now - target) * (start - target) > 0.0,
                        "{fps} fps, frame {frame}: overshot the target"
                    );
                    // Each frame closes exactly its share of what was left.
                    close(
                        closed,
                        share * (before - target).abs(),
                        1.0e-9,
                        "frame step",
                    );
                    let left = (now - target).abs() / (start - target).abs();
                    if frame == at_half_s {
                        left_at_half_s = left;
                    }
                }
                let left = (input.distance_m() - target).abs() / (start - target).abs();
                close(left, 2.0_f64.powf(-25.0 / 3.0), 1.0e-9, "2/3 s: share left");
                close(
                    left_at_half_s,
                    2.0_f64.powf(-6.25),
                    1.0e-9,
                    "1/2 s: share left",
                );
                assert!(left < 0.01, "{fps} fps: {left} left after 2/3 s");
                assert!(left_at_half_s > 0.01, "{fps} fps: 1/2 s is too early");
            }
        }
    }

    /// R3/R4: a vessel frames at four bounding radii the frame it takes
    /// the focus — first focus or change — clamped between 3 m (or 1.5
    /// radii, for a craft that big) and 1e6 m. The distance the camera
    /// is AT does not snap with it: the next frame eases from where it
    /// was.
    #[test]
    fn a_focus_frames_at_four_radii_clamped_to_the_limits() {
        assert_eq!(min_distance(0.0), 3.0);
        assert_eq!(min_distance(1.9), 3.0);
        assert_eq!(min_distance(10.0), 15.0);
        assert_eq!(frame_distance(10.0), 40.0);
        assert_eq!(frame_distance(0.5), 3.0);
        assert_eq!(frame_distance(1.0e7), MAX_DISTANCE_M);
        assert_eq!(min_distance(f64::INFINITY), MAX_DISTANCE_M);
        assert_eq!(min_distance(f64::NAN), 3.0);

        let mut input = camera_at(3.0);
        step_chase_camera(&mut input, &space_frame(2, 10.0, 0.0), &mut no_floor);
        assert_eq!(input.distance_target_m(), 40.0);
        assert_eq!(input.distance_m(), 3.0, "no frame time: nothing moves");
        step_chase_camera(
            &mut input,
            &space_frame(2, 10.0, 1000.0 / 60.0),
            &mut no_floor,
        );
        close(
            input.distance_m(),
            3.0 + 37.0 * ease_fraction(1.0 / 60.0, EASE_HALF_LIFE_S),
            1.0e-9,
            "eased",
        );
        // A focus change reframes again — a kept zoom never carries over.
        held(&mut input, 2);
        step_chase_camera(&mut input, &space_frame(3, 2.0, 0.0), &mut no_floor);
        assert_eq!(input.distance_target_m(), 8.0);
        // The same vessel keeps its zoom: only a change reframes.
        step_chase_camera(&mut input, &space_frame(3, 2.0, 0.0), &mut no_floor);
        assert_eq!(input.distance_target_m(), 8.0);
        input.distance_target_m = 1.0;
        step_chase_camera(&mut input, &space_frame(3, 2.0, 0.0), &mut no_floor);
        assert_eq!(
            input.distance_target_m(),
            3.0,
            "the floor holds every frame"
        );
    }

    /// R1/R6 through the Input schedule: an orbit event changes yaw and
    /// pitch by exactly its deltas — nothing smooths a drag — and pitch
    /// stops at ±89°. R2: a wheel event moves only the TARGET distance,
    /// held to 3 m…1e6 m.
    #[test]
    fn events_follow_the_pointer_and_the_wheel_moves_only_the_target() {
        let drain = |events: Vec<FlightEvent>| -> FlightInput {
            let mut world = bevy_ecs::world::World::new();
            world.insert_resource(FlightInputEvents { pending: events });
            world.insert_resource(FlightInput::default());
            let mut sched = Schedule::default();
            sched.add_systems(apply_flight_input);
            sched.run(&mut world);
            world
                .remove_resource::<FlightInput>()
                .expect("the camera state is a resource")
        };
        let (dyaw, dpitch) = (0.0137, -0.0221);
        let input = drain(vec![FlightEvent::Orbit { dyaw, dpitch }]);
        assert_eq!((input.yaw, input.pitch), (0.7 + dyaw, 0.25 + dpitch));
        let input = drain(vec![FlightEvent::Orbit {
            dyaw: 0.0,
            dpitch: 10.0,
        }]);
        assert_eq!(input.pitch, PITCH_LIMIT);
        close(PITCH_LIMIT.to_degrees(), 89.0, 1.0e-9, "the pitch limit");
        let input = drain(vec![FlightEvent::Orbit {
            dyaw: 0.0,
            dpitch: -10.0,
        }]);
        assert_eq!(input.pitch, -PITCH_LIMIT);

        let input = drain(vec![FlightEvent::Zoom { factor: 0.05 }]);
        assert_eq!(input.distance_target_m(), 3.0);
        assert_eq!(
            input.distance_m(),
            60.0,
            "the wheel does not move the camera"
        );
        let input = drain(vec![
            FlightEvent::Zoom { factor: 1.0e9 },
            FlightEvent::Zoom { factor: f64::NAN },
        ]);
        assert_eq!(input.distance_target_m(), MAX_DISTANCE_M);
    }

    /// A test body: centred at the origin, 600 km radius, spinning about
    /// +Z. A craft whose CoM is `height_m` above the ground sits on +X,
    /// so its local up is +X, north +Z and east +Y.
    const BODY_R: f64 = 600_000.0;

    fn landed(height_m: f64) -> (DVec3, LevelFrame) {
        let anchor = DVec3::new(BODY_R + height_m, 0.0, 0.0);
        let level = level_frame(anchor, Some(DVec3::ZERO), DVec3::Z);
        (anchor, level)
    }

    fn flat_floor(_: DVec3) -> f64 {
        BODY_R + EYE_GROUND_CLEARANCE_M
    }

    fn orbit(yaw: f64, pitch: f64, distance_m: f64) -> Orbit {
        Orbit {
            yaw,
            pitch,
            distance_m,
        }
    }

    /// A chase frame on the test body for a craft of bounding radius
    /// `radius_m` whose CoM is `com`.
    fn craft_frame(id: u64, com: DVec3, radius_m: f64, dt_ms: f64) -> ChaseFrame {
        ChaseFrame {
            id,
            com,
            radius_m,
            centre: Some(DVec3::ZERO),
            axis: DVec3::Z,
            dt_wall_ms: dt_ms,
        }
    }

    /// A chase frame on the test body for a craft of bounding radius
    /// 3 m whose CoM is `com`.
    fn body_frame(id: u64, com: DVec3, dt_ms: f64) -> ChaseFrame {
        craft_frame(id, com, 3.0, dt_ms)
    }

    /// A craft as high as one on +X, a chord of `d0_m` from it along
    /// the equator — for a long chord the straight line between the two
    /// crosses the planet.
    fn neighbour(d0_m: f64) -> DVec3 {
        let r = BODY_R + 5.0;
        let half = (d0_m / (2.0 * r)).asin();
        r * DVec3::new((2.0 * half).cos(), (2.0 * half).sin(), 0.0)
    }

    /// The share of a `d0_m` travel that one frame of `dt_s` closes.
    fn travel_share(d0_m: f64, dt_s: f64) -> f64 {
        ease_fraction(dt_s, travel_half_life(d0_m))
    }

    /// Two solved cameras showing the same thing — to a tenth of a
    /// micrometre, the round-trip of a planet-scale anchor through the
    /// travel offset at the last digit.
    fn same_view(a: &MapCameraF64, b: &MapCameraF64, what: &str) {
        close(
            (a.eye - b.eye).norm(),
            0.0,
            1.0e-9,
            &format!("{what}: the eye moved"),
        );
        for (x, y) in a
            .view_proj
            .iter()
            .flatten()
            .zip(b.view_proj.iter().flatten())
        {
            close(*x, *y, 1.0e-12, &format!("{what}: the view turned"));
        }
    }

    /// R5: a landed craft, its CoM 5 m up on flat ground. A level orbit
    /// puts the eye exactly at the CoM's height and a downward drag
    /// takes it below — to 1.454 m over the ground here. Above the
    /// floor the eye IS the intent's orbit point.
    #[test]
    fn a_landed_craft_is_seen_from_its_own_height_and_below() {
        let (anchor, level) = landed(5.0);
        let (level_view, lifted) = solve_orbit(
            &orbit(0.0, 0.0, 12.0),
            anchor,
            &level,
            DVec3::ZERO,
            &mut flat_floor,
        );
        assert!(!lifted, "nothing lifts a level eye");
        assert_eq!(level_view, anchor + DVec3::Z * 12.0);
        let (below, lifted) = solve_orbit(
            &orbit(0.0, -0.3, 12.0),
            anchor,
            &level,
            DVec3::ZERO,
            &mut flat_floor,
        );
        assert!(!lifted, "above the floor, nothing lifts the eye");
        assert_eq!(
            below,
            anchor + level.dir(0.0, -0.3) * 12.0,
            "above the floor, the eye is exactly where the angles put it"
        );
        close(below.x - BODY_R, 1.4538, 1.0e-3, "height over the ground");
        assert!(below.x < anchor.x, "the eye is below the craft's CoM");
    }

    /// How far the eye rides the floor sideways from the point under the
    /// craft: the craft's CoM `com_r` from the body centre, the eye
    /// asked `distance_m` out at `pitch` — the asked point's sideways
    /// offset `d·cos(pitch)`, scaled by its own lift onto the floor at
    /// `floor_r`.
    fn sideways_m(com_r: f64, floor_r: f64, distance_m: f64, pitch: f64) -> f64 {
        let (sideways, up) = (distance_m * pitch.cos(), distance_m * pitch.sin());
        sideways * floor_r / ((com_r + up).powi(2) + sideways * sideways).sqrt()
    }

    /// R6 at the solver: dragged under the ground the eye rides the
    /// floor — the asked point itself lifted radially onto it, 0.3 m
    /// over the ground and as far from the point under the craft as the
    /// asked point's sideways offset. From there a rising pitch slides
    /// the eye away from that point and a falling one toward it; nothing
    /// is rewritten, since the solve hands back an eye and nothing else.
    #[test]
    fn under_the_ground_the_eye_rides_the_floor() {
        let (anchor, level) = landed(5.0);
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let solve = |pitch: f64| {
            solve_orbit(
                &orbit(0.0, pitch, 12.0),
                anchor,
                &level,
                DVec3::ZERO,
                &mut flat_floor,
            )
            .0
        };
        let sideways = |pitch: f64| sideways_m(anchor.norm(), floor_r, 12.0, pitch);
        let pinned = solve(-0.5);
        close(pinned.norm(), floor_r, 1.0e-9, "riding the floor");
        close(
            pinned.z,
            sideways(-0.5),
            1.0e-9,
            "sideways from the point under the craft",
        );
        close(
            pinned.z,
            10.53101,
            1.0e-5,
            "sideways from the point under the craft",
        );

        // The pitch rising to −0.49: the eye slides AWAY, along the floor.
        let up = solve(-0.49);
        close(up.norm(), floor_r, 1.0e-9, "still on the floor");
        close(
            (up - pinned).norm(),
            sideways(-0.49) - sideways(-0.5),
            1.0e-9,
            "a rising pitch slides the eye away",
        );
        close(
            (up - pinned).norm(),
            0.05700,
            1.0e-5,
            "a rising pitch slides the eye away",
        );
        // The pitch falling to −0.51: the eye slides TOWARD the point under
        // the craft — it no longer holds.
        let down = solve(-0.51);
        close(down.norm(), floor_r, 1.0e-9, "still on the floor");
        close(
            (down - pinned).norm(),
            sideways(-0.5) - sideways(-0.51),
            1.0e-9,
            "a falling pitch slides the eye toward the point under the craft",
        );
        close(
            (down - pinned).norm(),
            0.05805,
            1.0e-5,
            "a falling pitch slides the eye toward the point under the craft",
        );

        // Walking the pitch up across the floor: the asked point reaches
        // it at −0.4024 rad. Under it the eye is exactly on the floor,
        // over it exactly the asked point, and no step moves the eye
        // further than it moved the asked point.
        let crossing = ((floor_r - anchor.norm()) / 12.0).asin();
        close(
            crossing,
            -0.40244,
            1.0e-5,
            "where the asked point reaches the floor",
        );
        let mut previous = solve(crossing - 0.01);
        let asked_step = 12.0 * 2.0 * (5.0e-5_f64).sin();
        for step in 1..=200 {
            let pitch = crossing - 0.01 + f64::from(step) * 1.0e-4;
            let (eye, lifted) = solve_orbit(
                &orbit(0.0, pitch, 12.0),
                anchor,
                &level,
                DVec3::ZERO,
                &mut flat_floor,
            );
            let asked = anchor + level.dir(0.0, pitch) * 12.0;
            let moved = (eye - previous).norm();
            assert!(moved > 0.0, "pitch {pitch}: the drag did not move the eye");
            assert!(
                moved <= asked_step * 1.000_001,
                "pitch {pitch}: the eye moved {moved} m for a drag of {asked_step} m"
            );
            if asked.norm() >= floor_r {
                assert!(!lifted, "pitch {pitch}: over the floor");
                assert_eq!(eye, asked, "pitch {pitch}: over the floor");
            } else {
                assert!(lifted, "pitch {pitch}: under the floor");
                close(
                    eye.norm(),
                    floor_r,
                    1.0e-9,
                    &format!("pitch {pitch}: under the floor"),
                );
            }
            previous = eye;
        }
    }

    /// R5 on a hillside: the floor is the ground under the EYE, not the
    /// highest ground around it. The craft lies on a 20 % slope with a
    /// 2 000 m ridge 3 km downhill; the eye looks downhill, where a
    /// neighbourhood ceiling would lift it kilometres. It rides its own
    /// ground instead: 0.3 m over it, wherever the ground is.
    #[test]
    fn on_a_slope_the_eye_follows_its_own_ground_not_the_ridge_above() {
        let (anchor, level) = landed(5.0);
        // Height over the datum along the slope (`y` = metres east),
        // with the ridge spanning 3…5 km west of the craft.
        let ground = |dir: DVec3| {
            let y = dir.y * BODY_R;
            BODY_R + 0.2 * y + 2000.0 * (1.0 - (y + 4000.0).abs() / 1000.0).max(0.0)
        };
        let mut floor = |dir: DVec3| ground(dir) + EYE_GROUND_CLEARANCE_M;
        let mut solve = |pitch: f64| {
            solve_orbit(
                &orbit(-core::f64::consts::FRAC_PI_2, pitch, 12.0),
                anchor,
                &level,
                DVec3::ZERO,
                &mut floor,
            )
            .0
        };
        let over_own_ground = |eye: DVec3| {
            let r = eye.norm();
            r - ground(eye / r)
        };
        assert_eq!(
            ground(DVec3::X),
            BODY_R,
            "the craft's ground is on the datum"
        );
        // Looking 11° down: the eye is above the floor and stays exactly
        // where the angles put it — 4.96 m over its own ground.
        let free = solve(-0.2);
        assert_eq!(
            free,
            anchor + level.dir(-core::f64::consts::FRAC_PI_2, -0.2) * 12.0,
            "above the floor, the eye is exactly where the angles put it"
        );
        close(
            over_own_ground(free),
            4.965,
            1.0e-2,
            "free eye over its ground",
        );
        // Looking 34° down: the floor catches it. The asked point is
        // lifted radially onto the floor sampled under the point ITSELF,
        // so the eye ends 0.3 m over its own ground — to the last digit.
        let pinned = solve(-0.6);
        close(
            over_own_ground(pinned),
            EYE_GROUND_CLEARANCE_M,
            1.0e-9,
            "lifted onto its own ground",
        );
        assert!(
            pinned.norm() < BODY_R + 100.0,
            "the ridge 3 km away must not lift the eye; it sits {} m up",
            pinned.norm() - BODY_R
        );
        // From there the pitch falling further slides the eye along that
        // ground toward the point under the craft; it does not hold.
        let further = solve(-0.62);
        close(
            over_own_ground(further),
            EYE_GROUND_CLEARANCE_M,
            1.0e-9,
            "still riding its own ground",
        );
        let slid = (further - pinned).norm();
        assert!(
            (0.13..=0.15).contains(&slid),
            "a falling pitch slid the eye {slid} m"
        );
    }

    /// R5: a capsized craft, its CoM 1 m up, can be viewed level — on a
    /// dry body and at an ocean's waterline alike, because the floor is
    /// 0.3 m over the ground — and, 0.5 m up, from below its CoM.
    #[test]
    fn a_capsized_craft_can_be_viewed_level() {
        let (anchor, level) = landed(1.0);
        let datum_ground = |_: DVec3| BODY_R;
        let mut shore = move |dir: DVec3| eye_floor(datum_ground(dir));
        let (view, lifted) = solve_orbit(
            &orbit(0.0, 0.0, 10.0),
            anchor,
            &level,
            DVec3::ZERO,
            &mut shore,
        );
        assert!(!lifted, "nothing lifts a level eye");
        assert_eq!(view, anchor + level.dir(0.0, 0.0) * 10.0);
        close(view.x, anchor.x, 1.0e-9, "level with the CoM");
        close(view.norm() - BODY_R, 1.0, 1.0e-3, "height over the ground");
        let (below, lifted) = solve_orbit(
            &orbit(0.0, -0.05, 10.0),
            anchor,
            &level,
            DVec3::ZERO,
            &mut shore,
        );
        assert!(!lifted, "nothing lifts this one either");
        assert_eq!(below, anchor + level.dir(0.0, -0.05) * 10.0);
        close(
            below.norm() - BODY_R,
            1.0 - 10.0 * 0.05_f64.sin(),
            1.0e-3,
            "height over the ground, below the CoM",
        );
    }

    /// The floor is 0.3 m over the ground: on land, at the waterline, and
    /// under an ocean's surface, where it keeps the eye down to the
    /// seabed. Walking the ground from 2 m above the datum to 4 m below,
    /// the floor moves exactly as the ground does — no cliff, no ramp, at
    /// the waterline.
    #[test]
    fn the_floor_is_the_ground_on_land_and_under_water() {
        let r = BODY_R;
        close(eye_floor(r + 40.0), r + 40.3, 1.0e-6, "land");
        close(eye_floor(r), r + 0.3, 1.0e-6, "the waterline");
        close(
            eye_floor(r - 0.5),
            r - 0.2,
            1.0e-6,
            "half a metre deep: under the surface",
        );
        close(
            eye_floor(r - 3000.0),
            r - 2999.7,
            1.0e-6,
            "3 000 m of water: 2 999.7 m under the surface",
        );
        for step in 0..=600 {
            let ground = r + 2.0 - step as f64 * 0.01;
            close(
                eye_floor(ground),
                ground + EYE_GROUND_CLEARANCE_M,
                1.0e-9,
                "the floor follows the ground",
            );
        }
    }

    /// Over water the eye goes under the surface, down to 0.3 m over the
    /// seabed. A craft afloat with its CoM on the surface of 200 m of
    /// water is framed at 300 m: dragged straight down, its eye is lifted
    /// onto the seabed's floor — 199.7 m under the surface — and 30 m
    /// under the surface at pitch −0.1 rad nothing lifts it at all. The
    /// surface is no floor; the seabed is the land's rule.
    #[test]
    fn over_water_the_eye_goes_down_to_the_seabed() {
        let com = DVec3::X * BODY_R;
        let seabed_r = BODY_R - 200.0;
        let frame = craft_frame(7, com, 1.0, 1000.0 / 60.0);
        let level = level_frame(com, Some(DVec3::ZERO), DVec3::Z);
        let seabed = |_: DVec3| seabed_r;
        let mut floor = |dir: DVec3| eye_floor(seabed(dir));
        let mut input = camera_at(300.0);
        held(&mut input, 7);
        input.yaw = 0.0;
        input.pitch = -PITCH_LIMIT;
        let down = step_chase_camera(&mut input, &frame, &mut floor);
        close(
            down.eye.norm(),
            seabed_r + EYE_GROUND_CLEARANCE_M,
            1.0e-9,
            "dragged straight down, the eye is 0.3 m over the seabed",
        );
        close(
            BODY_R - down.eye.norm(),
            199.7,
            1.0e-9,
            "…and so this far under the surface",
        );
        input.pitch = -0.1;
        let shallow = step_chase_camera(&mut input, &frame, &mut floor);
        let asked = com + level.dir(0.0, -0.1) * 300.0;
        close(
            (shallow.eye - asked).norm(),
            0.0,
            1.0e-9,
            "30 m under the surface, the eye is exactly where the angles put it",
        );
        assert!(
            shallow.eye.norm() < BODY_R && shallow.eye.norm() > seabed_r + 1.0,
            "the eye is under the surface and well over the seabed: {:.3} m under",
            BODY_R - shallow.eye.norm()
        );
    }

    /// R6 frame to frame, at 60 fps through the per-frame step, for a
    /// craft whose CoM is 2 m up: dragged under the ground the eye rides
    /// the floor 0.3 m over it and the stored pitch is the asked one. The
    /// eye is below the craft's level there, so the slide runs the other
    /// way round from a view from above: a drag that raises the pitch
    /// (−0.48) slides the eye away from the point under the craft, and
    /// the next frame's drag that lowers it slides it toward again —
    /// each on the very next frame, by the drag's sideways share, each
    /// reaching the pitch whole. A frame with no drag leaves both where
    /// they are.
    #[test]
    fn a_pinned_camera_reverses_on_the_next_frame() {
        let com = DVec3::new(BODY_R + 2.0, 0.0, 0.0);
        let frame = ChaseFrame {
            com,
            centre: Some(DVec3::ZERO),
            ..space_frame(7, 1.0, 1000.0 / 60.0)
        };
        let mut input = camera_at(10.0);
        held(&mut input, 7);
        input.yaw = 0.0;
        input.pitch = -0.5;
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let sideways = |pitch: f64| sideways_m(com.norm(), floor_r, 10.0, pitch);
        let pinned = step_chase_camera(&mut input, &frame, &mut flat_floor);
        assert_eq!(input.pitch, -0.5, "the floor rewrote the pitch");
        close(pinned.eye.norm(), floor_r, 1.0e-9, "riding the floor");

        // A raised pitch on the next frame (−0.48): the eye slides AWAY
        // along the floor — 9.4 cm, the drag's sideways share of its 20 cm.
        input.pitch = -0.48;
        let up = step_chase_camera(&mut input, &frame, &mut flat_floor);
        let slid = (up.eye - pinned.eye).norm();
        close(
            slid,
            sideways(-0.48) - sideways(-0.5),
            1.0e-9,
            "a raised pitch slides the eye away",
        );
        close(slid, 0.09412, 1.0e-5, "a raised pitch slides the eye away");
        assert!(
            slid > 0.0 && slid <= 10.0 * 2.0 * (0.01_f64).sin() * 1.000_1,
            "the eye moved {slid} m for a drag of {} m",
            10.0 * 2.0 * (0.01_f64).sin()
        );
        close(up.eye.norm(), floor_r, 1.0e-9, "still on the floor");

        // The pitch back down on the next frame (−0.5): slides toward
        // the point under the craft, onto where it started.
        input.pitch = -0.5;
        let back = step_chase_camera(&mut input, &frame, &mut flat_floor);
        assert_eq!((back.eye, back.view_proj), (pinned.eye, pinned.view_proj));

        // Lower still (−0.52): the eye used to hold here, dead. It slides
        // on toward the point under the craft, and the drag reaches the
        // pitch whole.
        input.pitch = -0.52;
        let down = step_chase_camera(&mut input, &frame, &mut flat_floor);
        let slid = (down.eye - back.eye).norm();
        close(
            slid,
            sideways(-0.5) - sideways(-0.52),
            1.0e-9,
            "a falling pitch slides the eye toward the point under the craft",
        );
        close(
            slid,
            0.09763,
            1.0e-5,
            "a falling pitch slides the eye toward the point under the craft",
        );
        assert_eq!(input.pitch, -0.52, "the drag reached the pitch whole");

        // A frame with no drag leaves both where they are.
        let still = step_chase_camera(&mut input, &frame, &mut flat_floor);
        assert_eq!(
            (still.eye, still.view_proj),
            (down.eye, down.view_proj),
            "stationary"
        );
    }

    /// The starter craft's bounding radius as first measured on the
    /// real pad, before it flew with its chute (M4c): 3.062 m — the
    /// least distance 4.593 m — framed at 12.248 m. The synthetic
    /// tests' stand-in; the shipped starter measures 3.329 m with the
    /// canister on the pod's top node
    /// (`on_the_real_pad_the_eye_goes_round_the_starter_craft`).
    const STARTER_RADIUS_M: f64 = 3.062;

    /// What [`sweep_the_zone`] saw.
    struct Zone {
        /// The pitch of the frame the eye entered the zone on (rad), and
        /// how far out along the yaw's line its orbit point was on the
        /// frame before and on that frame (m): the zone's edge lies
        /// between the two.
        entry: (f64, [f64; 2]),
        /// The eye at the pitch limit.
        at_limit: DVec3,
        /// The least and the most the eye moved on any frame outside the
        /// zone — the frame that entered it and the frame that left it
        /// included (m).
        moved_m: [f64; 2],
        /// The most the eye moved on any frame inside the zone, and the
        /// most that was for a metre of its orbit point's slide along the
        /// floor (m).
        inside_moved_m: [f64; 2],
    }

    /// One frame of a drag: the eye moved, by more than nothing and by no
    /// more than the drag's own step, and never came nearer than `rmin` to
    /// the craft's CoM. Returns the move (m).
    fn dragged(
        com: DVec3,
        rmin: f64,
        drag_step: f64,
        eye: DVec3,
        previous: DVec3,
        what: &str,
    ) -> f64 {
        let moved = (eye - previous).norm();
        assert!(moved > 0.0, "{what}: the drag did not move the eye");
        assert!(
            moved <= drag_step * 1.000_000_1,
            "{what}: the eye moved {moved} m for a drag of {drag_step} m"
        );
        let to_com = (eye - com).norm();
        assert!(
            to_com >= rmin - SKIRT_PRECISION_M,
            "{what}: the eye came {to_com} m from the CoM, inside the least distance {rmin}"
        );
        moved
    }

    /// How far a floor point is along the yaw's line from the point under
    /// the craft — the rule's own measure: its angle from the vertical
    /// through the CoM, times the floor's radius there. The body is
    /// centred on the origin.
    fn along_m(eye: DVec3, com: DVec3, along: DVec3, floor_r: f64) -> f64 {
        let up = com / com.norm();
        eye.dot(along).atan2(eye.dot(up)) * floor_r
    }

    /// The circle about a craft whose CoM is `com_r` from the body's
    /// centre, on a level floor of radius `floor_r`: its angular radius,
    /// where the floor is `rmin` from the CoM (rad).
    fn zone_angle(com_r: f64, floor_r: f64, rmin: f64) -> f64 {
        // From `1 − cos ρ`, which is all of two parts in 1e11 here: the
        // law of cosines itself would cancel down to its last digits.
        let sag = (rmin * rmin - (com_r - floor_r) * (com_r - floor_r)) / (2.0 * com_r * floor_r);
        2.0 * (0.5 * sag).sqrt().asin()
    }

    /// What one frame of [`sweep_the_zone`] did.
    struct Frame {
        /// The orbit point is inside the zone.
        inside: bool,
        /// How far the eye moved (m).
        moved: f64,
        /// How far that was for a metre of the orbit point's slide, on a
        /// frame that follows one inside the zone too (m).
        pace: f64,
    }

    /// Hold one frame of [`sweep_the_zone`] to the rule and report it.
    /// `shot` is the eye, its orbit point lifted and not kept out, and the
    /// frame before's eye; `before` the orbit point's place along the
    /// yaw's line on this frame and on the frame before, and whether it
    /// was inside the zone then; `bounds` the drag's own step and the most
    /// metres of eye a metre of slide may move inside the zone. Outside
    /// the zone the eye is the orbit point itself; inside it the eye is on
    /// the floor and exactly the least distance from the CoM. The eye
    /// moves, by no more than the drag's own step unless the frame before
    /// was inside the zone too.
    fn zone_frame(
        com: DVec3,
        rmin: f64,
        shot: (DVec3, DVec3, DVec3),
        before: ([f64; 2], bool),
        bounds: [f64; 2],
        floor: &mut dyn FnMut(DVec3) -> f64,
        what: &str,
    ) -> Frame {
        let (eye, point, previous) = shot;
        let inside = (point - com).norm() < rmin;
        let moved = (eye - previous).norm();
        if inside {
            close(
                eye.norm() - floor(eye / eye.norm()),
                0.0,
                1.0e-9,
                &format!("{what}: the eye is not on the floor"),
            );
            let to_com = (eye - com).norm();
            assert!(
                (-SKIRT_PRECISION_M..=1.0e-6).contains(&(to_com - rmin)),
                "{what}: the eye is {to_com} m from the CoM, not on the least distance {rmin}"
            );
        } else {
            assert_eq!(
                eye, point,
                "{what}: outside the zone, the eye is the lifted orbit point"
            );
        }
        if inside && before.1 {
            let slid = (before.0[0] - before.0[1]).abs();
            assert!(moved > 0.0, "{what}: the drag did not move the eye");
            let pace = moved / slid;
            assert!(
                pace <= bounds[1] * 1.000_001,
                "{what}: the eye moved {moved} m inside the zone for a slide of {slid} m"
            );
            Frame {
                inside,
                moved,
                pace,
            }
        } else {
            dragged(com, rmin, bounds[0], eye, previous, what);
            Frame {
                inside,
                moved,
                pace: 0.0,
            }
        }
    }

    /// A vertical drag, in steps of 0.0025 rad, from the level down to the
    /// pitch limit and back up again, about a craft of bounding radius
    /// `radius_m` whose CoM is `com`, the eye held at `eye_m` and yaw
    /// `yaw`, through the per-frame step on `floor` (the eye's least
    /// radius from the body's centre, centred on the origin, along a
    /// direction): what the least-distance rule did, and the eyes of the
    /// way down. `pace` is the most metres of eye a metre of the orbit
    /// point's slide may move inside the zone. Outside the zone the eye is
    /// the lifted orbit point itself, on it; inside it the eye is on the
    /// floor and exactly the least distance from the CoM. Every frame
    /// moves; a frame that enters or leaves the zone moves no further than
    /// the drag's own step, and a frame inside it no further than `pace`
    /// times the orbit point's slide along the floor. The way up retraces
    /// the way down, frame for frame.
    fn sweep_the_zone(
        com: DVec3,
        radius_m: f64,
        eye_m: f64,
        yaw: f64,
        pace: f64,
        floor: &mut dyn FnMut(DVec3) -> f64,
    ) -> (Zone, Vec<DVec3>) {
        let centre = DVec3::ZERO;
        let frame = craft_frame(7, com, radius_m, 1000.0 / 60.0);
        let rmin = min_distance(radius_m);
        let level = level_frame(com, Some(centre), DVec3::Z);
        let along = level.dir(yaw, 0.0);
        let floor_r = floor(level.up);
        let step_rad = 0.0025;
        let drag_step = full_step(eye_m, step_rad);
        let bounds = [drag_step, pace];
        let frames = (PITCH_LIMIT / step_rad).ceil() as usize;
        let pitch_at = |step: usize| (-(step as f64) * step_rad).max(-PITCH_LIMIT);
        // The intent's orbit point at `pitch`, lifted onto the floor and
        // not kept out.
        let point = |pitch: f64, floor: &mut dyn FnMut(DVec3) -> f64| {
            solve_orbit(&orbit(yaw, pitch, eye_m), com, &level, centre, floor).0
        };
        let place = |eye: DVec3| along_m(eye, com, along, floor_r);
        let mut input = camera_at(eye_m);
        held(&mut input, 7);
        input.yaw = yaw;
        input.pitch = 0.0;
        let mut cam = step_chase_camera(&mut input, &frame, floor);
        let mut path = Vec::with_capacity(frames);
        let (mut moved_m, mut inside_moved_m) = ([f64::MAX, 0.0_f64], [0.0_f64; 2]);
        let mut entry = None;
        let (mut previous, mut before) = (cam.eye, (place(cam.eye), false));
        for step in 1..=frames {
            input.pitch = pitch_at(step);
            cam = step_chase_camera(&mut input, &frame, floor);
            let what = format!("yaw {yaw}, pitch {}", input.pitch);
            let shot = (cam.eye, point(input.pitch, floor), previous);
            let now = place(shot.1);
            let seen = zone_frame(
                com,
                rmin,
                shot,
                ([now, before.0], before.1),
                bounds,
                floor,
                &what,
            );
            if seen.inside {
                inside_moved_m = [
                    inside_moved_m[0].max(seen.moved),
                    inside_moved_m[1].max(seen.pace),
                ];
            } else {
                moved_m = [moved_m[0].min(seen.moved), moved_m[1].max(seen.moved)];
            }
            if seen.inside && !before.1 {
                entry = Some((input.pitch, [before.0, now]));
            }
            path.push(cam.eye);
            (previous, before) = (cam.eye, (now, seen.inside));
        }
        let (at_limit, end_inside) = (cam.eye, before.1);
        let entry = entry.expect("the eye never entered the zone");
        // Reversed: the same pitches back up move the eye along the same
        // path, to the last digit.
        let mut was_inside = end_inside;
        for step in (1..frames).rev() {
            input.pitch = pitch_at(step);
            cam = step_chase_camera(&mut input, &frame, floor);
            assert_eq!(
                cam.eye,
                path[step - 1],
                "yaw {yaw}, pitch {}: a reversed drag left the path",
                input.pitch
            );
            let inside = (point(input.pitch, floor) - com).norm() < rmin;
            if was_inside && !inside {
                dragged(
                    com,
                    rmin,
                    drag_step,
                    cam.eye,
                    path[step],
                    &format!("yaw {yaw}, pitch {}: leaving the zone", input.pitch),
                );
            }
            was_inside = inside;
        }
        (
            Zone {
                entry,
                at_limit,
                moved_m,
                inside_moved_m,
            },
            path,
        )
    }

    /// The floor lift never takes the eye inside the craft. Dragged
    /// straight down, the starter craft's lifted eye used to end 2.636 m
    /// from its CoM — the least distance is 4.593 m — and a craft with its
    /// CoM 1 m up, 0.721 m: inside the craft. The eye goes round the craft
    /// on the floor instead. It slides toward the craft along the ground,
    /// enters the zone at the turn — where the floor first is the least
    /// distance from the CoM — and from there rides the circle at exactly
    /// that distance, round the yaw's own side: every frame moving, the
    /// frame that enters by no more than the drag's own step, no frame
    /// inside by more than [`skirt_plateau`] times the orbit point's
    /// slide, and never nearer than the least distance. At the pitch
    /// limit it sits where `skirt_azimuth` says, and a reversed drag
    /// retraces the path, frame for frame.
    #[test]
    fn the_lifted_eye_goes_round_the_craft_at_the_least_distance() {
        let radius_m = STARTER_RADIUS_M;
        let eye_m = 4.0 * radius_m;
        close(eye_m, 12.248, 1.0e-9, "the starter craft's frame distance");
        close(
            min_distance(radius_m),
            4.593,
            1.0e-9,
            "the starter craft's least distance",
        );
        let (floor_r, rmin) = (BODY_R + EYE_GROUND_CLEARANCE_M, min_distance(radius_m));
        let step_m = full_step(eye_m, 0.0025);
        for (what, com_height_m) in [
            (
                "the starter craft, its CoM 2.892 m over the ground as on the real pad",
                2.892,
            ),
            ("a craft with its CoM 1 m over the ground", 1.0),
        ] {
            let (com, level) = landed(com_height_m);
            let height_m = com_height_m - EYE_GROUND_CLEARANCE_M;
            let turn_m = (rmin * rmin - height_m * height_m).sqrt();
            for yaw in [0.0, 0.7, 2.6] {
                let (zone, _) =
                    sweep_the_zone(com, radius_m, eye_m, yaw, skirt_plateau(), &mut flat_floor);
                let (entry_pitch, [before, at]) = zone.entry;
                assert!(
                    before >= turn_m - 1.0e-4 && at <= turn_m + 1.0e-4,
                    "{what}, yaw {yaw}: the eye entered the zone between {before} and {at} m out, not at the turn {turn_m}"
                );
                // At the pitch limit: on the circle, where `skirt_azimuth` says.
                let along = level.dir(yaw, 0.0);
                let mut limit_floor = flat_floor;
                let point = solve_orbit(
                    &orbit(yaw, -PITCH_LIMIT, eye_m),
                    com,
                    &level,
                    DVec3::ZERO,
                    &mut limit_floor,
                )
                .0;
                let bearing =
                    skirt_azimuth((turn_m - along_m(point, com, along, floor_r)) / turn_m);
                let (reach, side) = (zone_angle(com.x, floor_r, rmin), along.cross(level.up));
                let wanted = level.up * (floor_r * reach.cos())
                    + (along * bearing.cos() + side * bearing.sin()) * (floor_r * reach.sin());
                close(
                    (zone.at_limit - wanted).norm(),
                    0.0,
                    1.0e-3,
                    &format!("{what}, yaw {yaw}: the eye at the pitch limit"),
                );
                close(
                    (zone.at_limit - com).norm(),
                    rmin,
                    1.0e-6,
                    &format!(
                        "{what}, yaw {yaw}: the eye's distance from the CoM at the pitch limit"
                    ),
                );
                eprintln!(
                    "{what}, yaw {yaw}: least distance {rmin:.3} m, the turn {turn_m:.3} m out; the eye entered the zone at pitch {entry_pitch:+.4} rad, between {before:.3} and {at:.3} m out; \
                     at the pitch limit {:.3} m from the CoM, {:.1}° round from the turn; \
                     outside the zone the eye moved {:.4}..{:.4} m a frame (the drag's own step {:.4} m); inside it at most {:.4} m, {:.2} m per metre of slide (at most {:.2})",
                    (zone.at_limit - com).norm(),
                    bearing.to_degrees(),
                    zone.moved_m[0],
                    zone.moved_m[1],
                    step_m,
                    zone.inside_moved_m[0],
                    zone.inside_moved_m[1],
                    skirt_plateau()
                );
            }
        }
    }

    /// What a sweep of orbit points straight across the zone saw.
    struct Crossing {
        /// How far the eye moved on the frame that entered the zone at
        /// the turn, and the most it moved on any frame (m).
        turn_step_m: f64,
        widest_m: f64,
        /// The nearest the eye came to the CoM (m).
        nearest_m: f64,
        /// The least of the eye's share along the yaw's own side — east of
        /// a craft on +X whose yaw is due north — while it was inside the
        /// zone (m).
        off_side_m: f64,
    }

    /// Sweep orbit points along the floor from 6 m on the yaw's side of
    /// the craft, whose CoM is `com`, to 6 m on the far side, a centimetre
    /// at a time, through the rule: the eyes, and what the sweep saw.
    /// Every centimetre moves the eye, by no more than [`skirt_plateau`]
    /// centimetres; it never comes nearer than `rmin` to the CoM; outside
    /// the zone it is the orbit point itself, inside it on the floor and
    /// exactly `rmin` out.
    fn sweep_across(
        com: DVec3,
        rmin: f64,
        north: DVec3,
        floor: &mut dyn FnMut(DVec3) -> f64,
    ) -> (Vec<DVec3>, Crossing) {
        let centre = DVec3::ZERO;
        let (up, floor_r) = (com / com.norm(), floor(com / com.norm()));
        // The floor point `x` metres north of the point under the craft.
        let on_floor = |x: f64| {
            let (sin, cos) = (x / floor_r).sin_cos();
            (up * cos + north * sin) * floor_r
        };
        let (mut eyes, mut crossing) = (
            Vec::new(),
            Crossing {
                turn_step_m: 0.0,
                widest_m: 0.0,
                nearest_m: f64::MAX,
                off_side_m: f64::MAX,
            },
        );
        let (mut previous, mut was_inside) = (on_floor(6.0), false);
        for step in 0..=1200_usize {
            let x = 6.0 - step as f64 * 0.01;
            let eye = skirt_eye(on_floor(x), com, rmin, north, centre, floor);
            let inside = (on_floor(x) - com).norm() < rmin;
            let (moved, to_com) = ((eye - previous).norm(), (eye - com).norm());
            if step > 0 {
                assert!(moved > 0.0, "x {x}: the sweep did not move the eye");
                assert!(
                    moved <= skirt_plateau() * 0.010_000_01,
                    "x {x}: the eye stepped {moved} m for a centimetre of sweep"
                );
            }
            assert!(
                to_com >= rmin - SKIRT_PRECISION_M,
                "x {x}: the eye came {to_com} m from the CoM, inside the least distance {rmin}"
            );
            if inside {
                close(
                    eye.norm(),
                    floor_r,
                    1.0e-9,
                    &format!("x {x}: the eye left the floor"),
                );
                close(
                    to_com,
                    rmin,
                    1.0e-6,
                    &format!("x {x}: the eye is not on the least distance"),
                );
                crossing.off_side_m = crossing.off_side_m.min(eye.y);
            } else {
                assert_eq!(
                    eye,
                    on_floor(x),
                    "x {x}: outside the zone, the eye is the orbit point"
                );
            }
            if inside != was_inside && x > 0.0 {
                crossing.turn_step_m = moved;
            }
            crossing.widest_m = crossing.widest_m.max(moved);
            crossing.nearest_m = crossing.nearest_m.min(to_com);
            (previous, was_inside) = (eye, inside);
            eyes.push(eye);
        }
        (eyes, crossing)
    }

    /// A travelling anchor's orbit point can sweep straight across the
    /// zone — from 6 m on the yaw's side of the craft to 6 m on the far
    /// side, a centimetre at a time. Outside the zone the eye is the orbit
    /// point itself; inside it rides the circle at exactly the least
    /// distance, round one side — the yaw's own, east of a yaw of due
    /// north — entering at the turn, passing the craft's side where the
    /// orbit point is under the craft, and leaving at the far edge, every
    /// centimetre where `skirt_azimuth` says. The frame that enters at the
    /// turn moves no more than the sweep itself, no frame more than
    /// [`skirt_plateau`] centimetres, and the way back is the same path to
    /// the last digit.
    #[test]
    fn the_eye_goes_round_on_one_side_across_both_edges() {
        let radius_m = STARTER_RADIUS_M;
        let (com, level) = landed(2.892);
        let rmin = min_distance(radius_m);
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let north = level.dir(0.0, 0.0);
        let east = north.cross(level.up);
        let reach = zone_angle(com.x, floor_r, rmin);
        let turn_m = reach * floor_r;
        let on_floor = |x: f64| {
            let (sin, cos) = (x / floor_r).sin_cos();
            (level.up * cos + north * sin) * floor_r
        };
        let (eyes, crossing) = sweep_across(com, rmin, north, &mut flat_floor);
        let Crossing {
            turn_step_m,
            widest_m,
            nearest_m,
            off_side_m,
        } = crossing;
        let at_under = skirt_eye(
            on_floor(0.0),
            com,
            rmin,
            north,
            DVec3::ZERO,
            &mut flat_floor,
        );
        assert!(
            turn_step_m <= 0.010_000_01,
            "entering the zone at the turn, the eye moved {turn_step_m} m for a centimetre of sweep"
        );
        assert!(
            off_side_m >= -1.0e-9,
            "the eye crossed the yaw's line to the other side: {off_side_m} m over"
        );
        // Where the orbit point is under the craft the eye is round the
        // circle as far as `skirt_azimuth` says — most of the way to the
        // craft's side, on the yaw's own side of it.
        let round = skirt_azimuth(1.0);
        close(
            at_under.y,
            turn_m * round.sin(),
            1.0e-3,
            "the eye where the orbit point is under the craft: how far along the yaw's own side",
        );
        close(
            at_under.z,
            turn_m * round.cos(),
            1.0e-3,
            "the eye where the orbit point is under the craft: how far out along the yaw",
        );
        // The edges: 3.80 m out the eye is the orbit point, 3.79 m out it
        // is on the circle; likewise 3.79 and 3.80 m out on the far side.
        let x_at = |step: f64| 6.0 - step * 0.01;
        assert_eq!(
            eyes[220],
            on_floor(x_at(220.0)),
            "3.80 m out: the orbit point"
        );
        assert_eq!(
            eyes[980],
            on_floor(x_at(980.0)),
            "3.80 m out on the far side: the orbit point"
        );
        assert!(
            (eyes[221] - on_floor(x_at(221.0))).norm() > 0.0,
            "3.79 m out: on the circle"
        );
        assert!(
            (eyes[979] - on_floor(x_at(979.0))).norm() > 0.0,
            "3.79 m out on the far side: on the circle"
        );
        // Every centimetre inside the zone: where `skirt_azimuth` says.
        for (step, eye) in eyes.iter().enumerate() {
            let x = 6.0 - step as f64 * 0.01;
            if (on_floor(x) - com).norm() < rmin {
                let bearing = skirt_azimuth(((turn_m - x) / turn_m).clamp(0.0, 2.0));
                let wanted = level.up * (floor_r * reach.cos())
                    + (north * bearing.cos() + east * bearing.sin()) * (floor_r * reach.sin());
                close(
                    (*eye - wanted).norm(),
                    0.0,
                    1.0e-6,
                    &format!("x {x}: where the eye is put"),
                );
            }
        }
        // The way back: the same eyes, to the last digit.
        for (step, eye) in eyes.iter().enumerate().skip(1).rev() {
            let x = 6.0 - step as f64 * 0.01;
            assert_eq!(
                skirt_eye(on_floor(x), com, rmin, north, DVec3::ZERO, &mut flat_floor),
                *eye,
                "x {x}: the way back left the path"
            );
        }
        eprintln!(
            "orbit point swept along the floor from 6 m on the yaw's side of the craft to 6 m on the far side, \
             rmin {rmin:.3} m, turn {turn_m:.4} m: entering at the turn the eye moved {turn_step_m:.5} m for a centimetre; \
             the most it moved for a centimetre {widest_m:.5} m (the circle's pace {:.2}); \
             where the orbit point is under the craft the eye is {:.1}° round from the turn, {:.4} m out and {:.4} m along the yaw's own side; \
             the least of its share on the yaw's own side {off_side_m:.4} m; nearest to the CoM {nearest_m:.9} m",
            skirt_plateau(),
            round.to_degrees(),
            at_under.z,
            at_under.y
        );
    }

    /// The test body's ground rising 20 % to the east, the eye's floor
    /// 0.3 m over it.
    fn slope_floor(dir: DVec3) -> f64 {
        BODY_R + 0.2 * dir.y * BODY_R + EYE_GROUND_CLEARANCE_M
    }

    /// On a slope the zone is not a circle about the point under the
    /// craft: uphill the floor rises toward the CoM, so the least distance
    /// is further out, downhill nearer. Both edges and the eye's way round
    /// are measured on the real floor. Dragged straight down with the yaw
    /// uphill, the eye enters the zone where the sloped floor first is the
    /// least distance from the CoM, rides 0.3 m over its own ground at
    /// exactly that distance, and at the pitch limit sits where
    /// `skirt_azimuth` says — on the first point of that floor along its
    /// bearing that is the least distance from the CoM, found here afresh.
    #[test]
    fn on_a_slope_the_eye_goes_round_the_craft_on_its_own_ground() {
        let radius_m = STARTER_RADIUS_M;
        let eye_m = 4.0 * radius_m;
        let (com, level) = landed(2.892);
        let (rmin, floor_r) = (min_distance(radius_m), BODY_R + EYE_GROUND_CLEARANCE_M);
        // The zone's edges along the slope, `x` metres out with the floor
        // `0.2·x` above the point under the craft: x² + (H ∓ 0.2·x)² = rmin².
        let height_m = 2.892 - EYE_GROUND_CLEARANCE_M;
        let root = (0.16 * height_m * height_m + 4.16 * (rmin * rmin - height_m * height_m)).sqrt();
        let uphill_m = (0.4 * height_m + root) / 2.08;
        let downhill_m = (root - 0.4 * height_m) / 2.08;
        let level_m = (rmin * rmin - height_m * height_m).sqrt();
        assert!(
            uphill_m > level_m + 0.3 && downhill_m < level_m - 0.3,
            "on a 20 % slope the zone runs {downhill_m:.3} m downhill and {uphill_m:.3} m uphill, not lop-sided about the {level_m:.3} m of level ground"
        );
        // Dragged straight down with the yaw uphill — east.
        let yaw = core::f64::consts::FRAC_PI_2;
        let pace = skirt_plateau() * 2.0 * uphill_m / (uphill_m + downhill_m);
        let (zone, _) = sweep_the_zone(com, radius_m, eye_m, yaw, pace, &mut slope_floor);
        let along = level.dir(yaw, 0.0);
        let (entry_pitch, [before, at]) = zone.entry;
        assert!(
            before >= uphill_m - 1.0e-3 && at <= uphill_m + 1.0e-3,
            "the eye entered the zone between {before} and {at} m uphill, not at {uphill_m}"
        );
        // At the pitch limit: where `skirt_azimuth` says — `bearing` round
        // from uphill — on the first point of the sloped floor along that
        // bearing that is `rmin` from the CoM, bisected here afresh.
        let point = solve_orbit(
            &orbit(yaw, -PITCH_LIMIT, eye_m),
            com,
            &level,
            DVec3::ZERO,
            &mut slope_floor,
        )
        .0;
        let bearing = skirt_azimuth(
            2.0 * (uphill_m - along_m(point, com, along, floor_r)) / (uphill_m + downhill_m),
        );
        let side = along.cross(level.up);
        let along_bearing = along * bearing.cos() + side * bearing.sin();
        let on = |angle: f64| {
            let dir = level.up * angle.cos() + along_bearing * angle.sin();
            dir * slope_floor(dir)
        };
        let (mut low, mut high) = (0.0, 0.02);
        for _ in 0..60 {
            let mid = 0.5 * (low + high);
            if (on(mid) - com).norm() < rmin {
                low = mid;
            } else {
                high = mid;
            }
        }
        let wanted = on(high);
        close(
            (zone.at_limit - wanted).norm(),
            0.0,
            1.0e-3,
            "the eye at the pitch limit on the slope",
        );
        eprintln!(
            "20 % slope, the yaw uphill: the zone runs {downhill_m:.3} m downhill and {uphill_m:.3} m uphill of the {level_m:.3} m on level ground; \
             the eye entered it at pitch {entry_pitch:+.4} rad, between {before:.3} and {at:.3} m uphill; \
             at the pitch limit {:.1}° round from uphill, {:.3} m from the CoM (least distance {rmin:.3} m) and {:.3} m over its own ground there; \
             inside the zone the eye moved at most {:.2} m per metre of slide (at most {pace:.2})",
            bearing.to_degrees(),
            (zone.at_limit - com).norm(),
            zone.at_limit.norm() - slope_floor(zone.at_limit / zone.at_limit.norm())
                + EYE_GROUND_CLEARANCE_M,
            zone.inside_moved_m[1]
        );
    }

    /// The intent's orbit point about `anchor`, lifted onto the test body's
    /// flat floor and not kept out.
    fn lifted_about(orbit: &Orbit, anchor: DVec3) -> DVec3 {
        let level = level_frame(anchor, Some(DVec3::ZERO), DVec3::Z);
        solve_orbit(orbit, anchor, &level, DVec3::ZERO, &mut flat_floor).0
    }

    /// What a travel, taken frame by frame, saw.
    struct WentRound {
        frames: usize,
        /// The nearest the eye came to the new craft's CoM (m).
        nearest_m: f64,
        /// The most the eye moved for a metre of the orbit point's own
        /// travel.
        widest: f64,
        /// How much further the eye went over the travel than the orbit
        /// point did: its way round the craft's ball, less the orbit
        /// point's way through it (m).
        extra_m: f64,
        /// The most any frame's move was of the one before, over the
        /// frames moving more than a millimetre.
        widest_ratio: f64,
        /// Whether the orbit point was ever inside the new craft's ball.
        crossed: bool,
        /// The eye at rest.
        eye: DVec3,
        /// The eye and its orbit point, lifted and not kept out, on every
        /// frame.
        eyes: Vec<DVec3>,
        points: Vec<DVec3>,
    }

    /// Step the travel in hand to its end, 60 fps, a frame at a time,
    /// about a craft of the starter's size whose CoM is `com`, holding
    /// every frame to what the keep-out on a travel owes: the eye never
    /// nearer than the least distance to the CoM; no frame moving it
    /// further than `pace` times the orbit point's own travel; where a
    /// `bound` is given, no frame moving beyond that times the one before,
    /// beyond the millimetre the travel itself snaps at the end; and, all
    /// its frames together, the eye going no further than the orbit
    /// point's own travel and the most any nearer way round the ball is
    /// longer than the way through it — half its circumference less its
    /// diameter, a diameter's way round. `orbit` is held throughout;
    /// `start` is the eye and its orbit point on the frame of the change.
    fn travel_round(
        input: &mut FlightInput,
        orbit: &Orbit,
        com: DVec3,
        pace: f64,
        bound: Option<f64>,
        start: (DVec3, DVec3),
    ) -> WentRound {
        let rmin = min_distance(STARTER_RADIUS_M);
        let frame = craft_frame(2, com, STARTER_RADIUS_M, 1000.0 / 60.0);
        let (mut previous_eye, mut previous_point, mut last_move) = (start.0, start.1, None);
        let mut went = WentRound {
            frames: 0,
            nearest_m: f64::MAX,
            widest: 0.0,
            extra_m: 0.0,
            widest_ratio: 0.0,
            crossed: false,
            eye: start.0,
            eyes: Vec::new(),
            points: Vec::new(),
        };
        let mut cam;
        loop {
            cam = step_chase_camera(input, &frame, &mut flat_floor);
            went.frames += 1;
            assert!(went.frames <= 400, "the travel never ended");
            let anchor = input.travel.last.expect("a focus was just solved").anchor;
            let point = lifted_about(orbit, anchor);
            let (moved, step_m) = (
                (cam.eye - previous_eye).norm(),
                (point - previous_point).norm(),
            );
            let to_com = (cam.eye - com).norm();
            assert!(
                to_com >= rmin - SKIRT_PRECISION_M,
                "frame {}: the eye came {to_com} m from the new craft's CoM, inside the least distance {rmin}",
                went.frames
            );
            if let Some(last) = last_move
                && last > TRAVEL_DONE_M
            {
                let ratio = moved / last;
                went.widest_ratio = went.widest_ratio.max(ratio);
                if let Some(bound) = bound {
                    assert!(
                        moved <= bound * last + TRAVEL_DONE_M,
                        "frame {}: the eye moved {moved} m after {last} m, {ratio:.3} times",
                        went.frames
                    );
                }
            }
            assert!(
                step_m <= 1.0e-9 || moved <= pace * step_m * 1.000_001,
                "frame {}: the eye moved {moved} m for the orbit point's {step_m} m",
                went.frames
            );
            went.nearest_m = went.nearest_m.min(to_com);
            went.extra_m += moved - step_m;
            if step_m > 1.0e-6 {
                went.widest = went.widest.max(moved / step_m);
            }
            went.crossed |= (point - com).norm() < rmin;
            went.eyes.push(cam.eye);
            went.points.push(point);
            (previous_eye, previous_point, last_move) = (cam.eye, point, Some(moved));
            went.eye = cam.eye;
            if input.travel.offset == DVec3::ZERO {
                break;
            }
        }
        let roundest_m = (core::f64::consts::PI - 2.0) * rmin;
        assert!(
            went.extra_m <= roundest_m + 1.0e-6,
            "over the travel the eye went {:.3} m further than the orbit point: more than any nearer way round the ball ({roundest_m:.3} m)",
            went.extra_m
        );
        went
    }

    /// A focus change whose orbit point crosses the new craft's zone
    /// obliquely, through the per-frame step: two starter crafts, the first
    /// 8 m south of the second and 4.6 m east of it, the eye dragged down
    /// to where its orbit point rides the floor 5.06 m out along the yaw —
    /// outside both zones, so the eye carries no skirt. The frame of the
    /// change moves neither the eye nor the view. The orbit point then
    /// travels along the floor toward its rest 5.06 m north of the new
    /// craft, cutting across that craft's zone from its north-west side to
    /// its near side, and the eye goes round the circle from where the
    /// orbit point entered to where it left: exactly the least distance
    /// from the CoM on every frame, no frame moving it further than the
    /// circle's own way for the orbit point's travel, all its moves
    /// together no longer than the orbit point's travel and the way round
    /// the circle, and no frame a tenth further than the one before —
    /// beyond the millimetre the travel itself snaps at the end. At rest
    /// the eye is the orbit point again.
    #[test]
    fn a_focus_change_across_the_new_craft_goes_round_it_without_a_jump() {
        let radius_m = STARTER_RADIUS_M;
        let eye_m = 4.0 * radius_m;
        let orbit = orbit(0.0, -1.145, eye_m);
        let (com_b, _) = landed(2.892);
        let com_a = com_b - DVec3::Z * 8.0 + DVec3::Y * 4.6;
        let rmin = min_distance(radius_m);
        let mut input = camera_at(eye_m);
        input.travel.last = Some(Last {
            id: 1,
            anchor: com_a,
            look_at: com_a,
            skirt: DVec3::ZERO,
            eye: DVec3::ZERO,
            carried: DVec3::ZERO,
            moved_m: None,
            travelled: 0.0,
        });
        input.yaw = 0.0;
        input.pitch = -1.145;
        let before = step_chase_camera(
            &mut input,
            &craft_frame(1, com_a, radius_m, 0.0),
            &mut flat_floor,
        );
        let set_out = lifted_about(&orbit, com_a);
        assert_eq!(
            before.eye, set_out,
            "the eye carries the first craft's skirt"
        );
        for com in [com_a, com_b] {
            assert!(
                (set_out - com).norm() >= rmin,
                "the orbit point starts inside a craft's zone: {:.3} m from its CoM",
                (set_out - com).norm()
            );
        }

        // The frame of the change, before any time is put in.
        let still = step_chase_camera(
            &mut input,
            &craft_frame(2, com_b, radius_m, 0.0),
            &mut flat_floor,
        );
        same_view(&still, &before, "the frame of the change");
        assert!(
            !input.travel.began_inside,
            "the orbit point began outside the zone: the keep-out acts at once"
        );

        // The travel: the circle's own way is 1.13 times the orbit point's
        // travel along this line.
        let went = travel_round(
            &mut input,
            &orbit,
            com_b,
            1.25,
            Some(1.1),
            (before.eye, set_out),
        );
        let WentRound {
            frames,
            nearest_m,
            extra_m,
            widest: widest_gain,
            crossed,
            eye,
            ..
        } = went;
        assert!(
            crossed,
            "the orbit point never crossed the new craft's zone"
        );
        assert!(
            input.travel.ride.is_none(),
            "at rest nothing is left of the ride"
        );
        // At rest: the orbit point itself, 5.06 m out — outside the zone.
        let at_rest = lifted_about(&orbit, com_b);
        close(
            (eye - at_rest).norm(),
            0.0,
            1.0e-9,
            "at rest the eye is not the orbit point",
        );
        close(
            at_rest.z - com_b.z,
            5.06,
            1.0e-2,
            "how far out along the yaw the orbit point rests",
        );
        eprintln!(
            "focus change whose orbit point cuts across the new craft's zone obliquely (from 8 m south and 4.6 m east, the orbit point 5.06 m out along the yaw): \
             the frame of the change moved the eye 0 m; the eye went round the circle over {frames} frames, \
             never nearer than {nearest_m:.6} m to the CoM (least distance {rmin:.3} m), its travel exceeding the orbit point's by {extra_m:.3} m at most, moving at most {widest_gain:.2} times the orbit point's own travel; \
             at rest the eye is the orbit point again",
        );
    }

    /// A focus change whose orbit point crosses the new craft's ball IN
    /// THE AIR. The keep-out used to read only an orbit point the floor
    /// had lifted, and the eye was carried straight through the craft.
    /// Two starter crafts 20 m apart along the yaw, the eye at four radii,
    /// with no skirt of the first craft's to carry. On the default camera
    /// (+0.25 rad) the way passes 3.031 m above the new craft's CoM and
    /// the eye goes over the craft on its ball, in the yaw's own vertical
    /// plane; on a level camera the way is a diameter and the eye goes
    /// round the yaw's own side, level with the CoM; a tenth of a radian
    /// under the level the nearer way round is under the floor, and the
    /// eye rides the floor's circle round the yaw's own side instead. On
    /// every frame the eye is the orbit point itself while that is
    /// outside the ball and exactly the least distance from the CoM while
    /// it is inside — never under the floor — moving no further than 1.75
    /// times the orbit point's own travel, and all its moves together no
    /// longer than the orbit point's own travel and the way round the
    /// ball; over the craft, round its side and along the floor's circle,
    /// no frame a tenth further than the one before. The frame of the
    /// change moves nothing, and at rest the eye is the orbit point
    /// again.
    #[test]
    fn a_focus_travel_through_the_air_goes_round_the_craft_on_its_ball() {
        let radius_m = STARTER_RADIUS_M;
        let eye_m = 4.0 * radius_m;
        let rmin = min_distance(radius_m);
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let (com_b, level) = landed(2.892);
        let (north, up) = (level.north, level.up);
        for (what, pitch, how) in [
            ("the default camera", 0.25_f64, "over"),
            ("a level camera", 0.0_f64, "round"),
            ("0.1 rad under the level", -0.1_f64, "under"),
        ] {
            // How far above the anchor's level the orbit point rides.
            let above_m = eye_m * pitch.sin();
            let orbit = orbit(0.0, pitch, eye_m);
            let com_a = com_b - north * 20.0;
            let mut input = camera_at(eye_m);
            input.travel.last = Some(Last {
                id: 1,
                anchor: com_a,
                look_at: com_a,
                skirt: DVec3::ZERO,
                eye: DVec3::ZERO,
                carried: DVec3::ZERO,
                moved_m: None,
                travelled: 0.0,
            });
            input.yaw = 0.0;
            input.pitch = pitch;
            let before = step_chase_camera(
                &mut input,
                &craft_frame(1, com_a, radius_m, 0.0),
                &mut flat_floor,
            );
            let set_out = lifted_about(&orbit, com_a);
            assert_eq!(
                before.eye, set_out,
                "{what}: the eye carries the first craft's skirt"
            );
            let at_rest = lifted_about(&orbit, com_b);
            for (when, point) in [("where it starts", set_out), ("where it rests", at_rest)] {
                assert!(
                    point.norm() > floor_r + 1.0,
                    "{what}: the orbit point is not in the air {when}"
                );
                assert!(
                    (point - com_b).norm() >= rmin,
                    "{what}: the orbit point is inside the new craft's ball {when}"
                );
                // The first craft's local up tilts from the second's by
                // its 20 m along the ground.
                close(
                    (point - com_b).dot(up),
                    above_m,
                    1.0e-3,
                    &format!("{what}: how far above the CoM the way passes, {when}"),
                );
            }

            // The frame of the change, before any time is put in.
            let still = step_chase_camera(
                &mut input,
                &craft_frame(2, com_b, radius_m, 0.0),
                &mut flat_floor,
            );
            same_view(&still, &before, "the frame of the change");
            assert!(
                !input.travel.began_inside,
                "{what}: the eye began outside the ball: the keep-out acts at once"
            );

            // The way round the ball is at most 1.69 times the orbit
            // point's own travel along the way.
            let went = travel_round(
                &mut input,
                &orbit,
                com_b,
                1.75,
                Some(1.1),
                (before.eye, set_out),
            );
            assert!(
                went.crossed,
                "{what}: the orbit point never crossed the new craft's ball"
            );
            assert!(
                input.travel.ride.is_none(),
                "{what}: at rest nothing is left of the ride"
            );
            close(
                (went.eye - at_rest).norm(),
                0.0,
                1.0e-9,
                &format!("{what}: at rest the eye is not the orbit point"),
            );
            let on_the_ball = eyes_on_the_ball(&went, com_b, rmin, floor_r, what);
            let how_it_went = how_it_went(&on_the_ball, &level, com_b, rmin, floor_r, how, what);
            report_through_the_air(what, above_m, rmin, &went, on_the_ball.len(), how_it_went);
        }
    }

    /// Print what a focus change through the air came to: `what` naming
    /// the camera, its way `above_m` over the new craft's CoM, `went` the
    /// travel, the frames the eye was on the ball and how it went there
    /// — at its highest over the CoM's level, and at its farthest along
    /// the yaw's own side (m).
    fn report_through_the_air(
        what: &str,
        above_m: f64,
        rmin: f64,
        went: &WentRound,
        on_the_ball: usize,
        how_it_went: (f64, f64),
    ) {
        let (highest, widest) = how_it_went;
        eprintln!(
            "focus change through the air, {what} (the way {above_m:+.3} m above the new craft's CoM, least distance {rmin:.3} m): the frame of the change moved the eye 0 m; \
             the eye went round the ball over {} frames, on it for {}, \
             never nearer than {:.6} m to the CoM, its travel exceeding the orbit point's by {:.3} m at most, moving at most {:.2} times the orbit point's own travel, \
             the most any frame's move {:.3} times the one before; \
             on the ball it rose at most to {highest:+.3} m over the CoM's level and reached {widest:.3} m along the yaw's own side",
            went.frames, on_the_ball, went.nearest_m, went.extra_m, went.widest, went.widest_ratio,
        );
    }

    /// Hold every frame of `went` — a travel toward a craft whose CoM is
    /// `com` — to the keep-out's letter, and return the eye's offsets from
    /// the CoM over the frames it was on the ball, at least two of them.
    /// Outside the ball the eye is the orbit point itself; on it, exactly
    /// the least distance out, and never under the floor.
    fn eyes_on_the_ball(
        went: &WentRound,
        com: DVec3,
        rmin: f64,
        floor_r: f64,
        what: &str,
    ) -> Vec<DVec3> {
        let mut on_the_ball = Vec::new();
        for (i, (eye, point)) in went.eyes.iter().zip(&went.points).enumerate() {
            if (*point - com).norm() >= rmin {
                assert_eq!(
                    *eye, *point,
                    "{what}, frame {i}: outside the ball, the eye is not the orbit point"
                );
            } else {
                let to_com = (*eye - com).norm();
                assert!(
                    (-SKIRT_PRECISION_M..=1.0e-6).contains(&(to_com - rmin)),
                    "{what}, frame {i}: the eye is {to_com} m from the CoM, not on the least distance {rmin}"
                );
                assert!(
                    eye.norm() >= floor_r - 1.0e-9,
                    "{what}, frame {i}: the eye is under the floor"
                );
                on_the_ball.push(*eye - com);
            }
        }
        assert!(
            on_the_ball.len() >= 2,
            "{what}: the eye was on the ball for {} frames only",
            on_the_ball.len()
        );
        on_the_ball
    }

    /// What the eye did on the ball, as offsets from the craft's CoM over
    /// the frames it was on it, by the way its orbit point crossed it:
    /// `over` the craft, `round` its yaw's own side, or — the nearer way
    /// being under the floor — `under`, along the floor's circle round
    /// that side. Returns the highest it rose above the CoM's level and
    /// the farthest it reached along the yaw's own side (m). `level` is
    /// the craft's own frame, the yaw being north.
    fn how_it_went(
        on_the_ball: &[DVec3],
        level: &LevelFrame,
        com: DVec3,
        rmin: f64,
        floor_r: f64,
        how: &str,
        what: &str,
    ) -> (f64, f64) {
        let (north, east, up) = (level.north, level.east, level.up);
        let (height, flank) = (
            |to_com: DVec3| to_com.dot(up),
            |to_com: DVec3| to_com.dot(east),
        );
        let highest = on_the_ball
            .iter()
            .map(|to_com| height(*to_com))
            .fold(f64::MIN, f64::max);
        let widest = on_the_ball
            .iter()
            .map(|to_com| flank(*to_com))
            .fold(f64::MIN, f64::max);
        for to_com in on_the_ball {
            assert!(
                flank(*to_com) >= -1.0e-9,
                "{what}: the eye crossed to the far side of the yaw's line"
            );
        }
        match how {
            "over" => {
                // Over the craft, in the yaw's own vertical plane.
                for to_com in on_the_ball {
                    close(
                        flank(*to_com),
                        0.0,
                        1.0e-9,
                        &format!("{what}: the eye left the yaw's vertical plane"),
                    );
                    assert!(
                        height(*to_com) > 3.0,
                        "{what}: the eye sank to {} m above the CoM",
                        height(*to_com)
                    );
                }
                assert!(
                    highest > 0.8 * rmin,
                    "{what}: the eye rose {highest} m above the CoM, not over the craft"
                );
            }
            "round" => {
                // A diameter: round the yaw's own side, level with the
                // CoM, from its far side to its near side.
                for to_com in on_the_ball {
                    close(
                        height(*to_com),
                        0.0,
                        5.0e-3,
                        &format!("{what}: the eye off the CoM's level"),
                    );
                }
                // At 60 fps two frames land on that semicircle, each
                // within 50° of its top.
                assert!(
                    widest > 0.7 * rmin,
                    "{what}: the eye came {widest} m along the yaw's own side"
                );
                assert!(
                    on_the_ball[0].dot(north) < 0.0
                        && on_the_ball[on_the_ball.len() - 1].dot(north) > 0.0,
                    "{what}: the eye did not go from the far side round to the near side"
                );
            }
            _ => {
                // The eye rides the floor's circle, round the yaw's own
                // side.
                let on_the_circle = on_the_ball
                    .iter()
                    .filter(|to_com| {
                        ((*(*to_com) + com).norm() - floor_r).abs() <= 1.0e-9
                            && flank(**to_com) > 2.0
                    })
                    .count();
                assert!(
                    on_the_circle >= 1,
                    "{what}: the eye never rode the floor's circle round the yaw's own side"
                );
            }
        }
        (highest, widest)
    }

    /// What a change from inside the new craft's ball came to.
    struct LeftBall {
        /// The camera at the travel's end.
        cam: MapCameraF64,
        /// The travel's frames, the change's own not counted.
        frames: usize,
        /// The most any frame's move was of the one before, over the
        /// frames moving more than a millimetre.
        widest: f64,
        /// The eye's move on the travel's last frame (m).
        last_moved_m: f64,
        /// Whether anything is left of the ride at the travel's end.
        riding: bool,
    }

    /// A focus change on the test body from a craft whose CoM is `com_a`
    /// to one whose CoM is `com_b`, a metre north of it, the eye dragged
    /// to the pitch limit on the first — so that it sits on the first
    /// craft's circle just inside the second's, and the orbit point's own
    /// way runs on through the second's zone. The frame of the change
    /// carries `change_dt_ms`: without frame time it moves nothing; with
    /// it the eye sets off by no more than a tenth beyond the orbit
    /// point's own step, and no nearer to the second craft's CoM. Returns
    /// the camera after that frame, the orbit held, the input, and the
    /// eye's move on it and its distance to the second craft's CoM on the
    /// frame before.
    fn change_from_inside(
        com_a: DVec3,
        com_b: DVec3,
        change_dt_ms: f64,
    ) -> (MapCameraF64, Orbit, FlightInput, f64, f64) {
        let radius_m = STARTER_RADIUS_M;
        let eye_m = 4.0 * radius_m;
        let orbit = orbit(0.0, -PITCH_LIMIT, eye_m);
        let rmin = min_distance(radius_m);
        let dt_ms = 1000.0 / 60.0;
        let mut input = camera_at(eye_m);
        input.travel.last = Some(Last {
            id: 1,
            anchor: com_a,
            look_at: com_a,
            skirt: DVec3::ZERO,
            eye: DVec3::ZERO,
            carried: DVec3::ZERO,
            moved_m: None,
            travelled: 0.0,
        });
        input.yaw = 0.0;
        input.pitch = -PITCH_LIMIT;
        let before = step_chase_camera(
            &mut input,
            &craft_frame(1, com_a, radius_m, dt_ms),
            &mut flat_floor,
        );
        let (com_a_level, mut own_floor) = (landed(2.892).1, flat_floor);
        let orbit_point = solve_orbit(&orbit, com_a, &com_a_level, DVec3::ZERO, &mut own_floor).0;
        assert!(
            (orbit_point - com_b).norm() < rmin,
            "the orbit point does not start inside the second craft's ball: {:.3} m from its CoM",
            (orbit_point - com_b).norm()
        );
        let start_m = (before.eye - com_b).norm();
        assert!(
            start_m < rmin,
            "the eye does not begin inside the second craft's ball: {start_m:.3} m from its CoM (least distance {rmin:.3} m)"
        );

        // The frame of the change.
        let cam = step_chase_camera(
            &mut input,
            &craft_frame(2, com_b, radius_m, change_dt_ms),
            &mut flat_floor,
        );
        assert!(
            input.travel.began_inside,
            "the eye began the travel inside the ball: it leaves by the nearest way out"
        );
        let moved_m = (cam.eye - before.eye).norm();
        if change_dt_ms == 0.0 {
            same_view(&cam, &before, "the frame of the change");
        } else {
            // The orbit point's own step: how far the carried point moved.
            let carried = input.travel.last.expect("a focus was just solved").carried;
            let step_m = (carried - before.eye).norm();
            assert!(
                moved_m > 0.0 && moved_m <= PACE_GAIN * step_m * 1.000_001,
                "the frame of the change moved the eye {moved_m} m for the orbit point's own step {step_m} m"
            );
        }
        assert!(
            (cam.eye - com_b).norm() >= start_m - 1.0e-9,
            "the frame of the change brought the eye nearer to the CoM than {start_m} m"
        );
        (cam, orbit, input, moved_m, start_m)
    }

    /// Step the travel that follows such a frame of change to its end, a
    /// timed frame at a time, and hold every frame to what leaving the
    /// ball owes (`cam` is the camera after the frame of the change, and
    /// `last_move` the eye's move on it where it carried frame time): the
    /// eye's distance to the CoM never falls, and on its way out — still
    /// inside the least distance — it moves away from the craft along the
    /// floor, its distance out from the vertical through it never
    /// falling; from the first frame it is the least distance out it
    /// never is less again, and is exactly that while the orbit point is
    /// inside the ball; it never leaves the floor; and no frame moves it a
    /// tenth beyond the one before, beyond the millimetre the travel
    /// itself snaps at its end.
    fn leave_the_ball(
        cam: MapCameraF64,
        input: &mut FlightInput,
        com_b: DVec3,
        last_move: Option<f64>,
    ) -> LeftBall {
        let rmin = min_distance(STARTER_RADIUS_M);
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let up = level_frame(com_b, Some(DVec3::ZERO), DVec3::Z).up;
        let over = |eye: DVec3| {
            let out = eye - com_b;
            (out - up * out.dot(up)).norm()
        };
        let mut cam = cam;
        let mut previous = cam.eye;
        let (mut was_m, mut was_over_m) = ((cam.eye - com_b).norm(), over(cam.eye));
        let (mut frames, mut widest, mut nearest_out_m, mut last_moved_m) =
            (0, 0.0_f64, f64::MAX, 0.0_f64);
        let (mut last_move, mut riding) = (last_move, true);
        while input.travel.offset != DVec3::ZERO {
            cam = step_chase_camera(
                input,
                &craft_frame(2, com_b, STARTER_RADIUS_M, 1000.0 / 60.0),
                &mut flat_floor,
            );
            frames += 1;
            assert!(frames <= 400, "the travel never ended");
            let (to_com, out_m, moved_m) = (
                (cam.eye - com_b).norm(),
                over(cam.eye),
                (cam.eye - previous).norm(),
            );
            assert!(
                to_com >= was_m - 1.0e-9,
                "frame {frames}: the eye came {to_com} m from the CoM after {was_m} m"
            );
            if to_com < rmin - ON_BALL_M {
                // Still on its way out: along the floor, away from the
                // craft.
                assert!(
                    out_m >= was_over_m - 1.0e-9,
                    "frame {frames}: on its way out the eye moved back toward the vertical through the craft"
                );
            } else {
                nearest_out_m = nearest_out_m.min(to_com);
                // On the ball exactly while the orbit point is inside it.
                let carried = input.travel.last.expect("a focus was just solved").carried;
                if (carried - com_b).norm() < rmin {
                    assert!(
                        (to_com - rmin).abs() <= 1.0e-6,
                        "frame {frames}: the eye is {to_com} m from the CoM, not on the least distance {rmin}"
                    );
                }
            }
            close(
                cam.eye.norm(),
                floor_r,
                1.0e-6,
                &format!("frame {frames}: the eye left the floor"),
            );
            if let Some(last) = last_move
                && last > TRAVEL_DONE_M
            {
                let ratio = moved_m / last;
                widest = widest.max(ratio);
                assert!(
                    moved_m <= 1.1 * last + TRAVEL_DONE_M,
                    "frame {frames}: the eye moved {moved_m} m after {last} m, {ratio:.3} times"
                );
            }
            riding = input.travel.ride.is_some();
            (previous, was_m, was_over_m, last_move, last_moved_m) =
                (cam.eye, to_com, out_m, Some(moved_m), moved_m);
        }
        assert!(
            nearest_out_m >= rmin - SKIRT_PRECISION_M,
            "once out of the ball the eye came back to {nearest_out_m} m from the CoM (least distance {rmin} m)"
        );
        LeftBall {
            cam,
            frames,
            widest,
            last_moved_m,
            riding,
        }
    }

    /// The other way round: the eye itself BEGINS the travel inside the
    /// new craft's ball — a craft a metre away, the eye dragged to the
    /// pitch limit, so that it sits on the first craft's circle just
    /// inside the second's. The orbit point's own way runs on through the
    /// second craft's zone toward its CoM, and the eye used to be handed
    /// over along it: nearer to the CoM before it came out. The frame of
    /// the change moves nothing. Then the eye leaves the ball by the
    /// nearest way out — along the floor, away from the vertical through
    /// the craft — and goes round it to the at-rest eye: its distance to
    /// the CoM never falls on any frame; it is exactly the least distance
    /// out on every frame the orbit point is inside the ball and the eye
    /// is out of it, and never inside the ball again; it never leaves the
    /// floor; no frame moves it a tenth beyond the one before; and the
    /// travel ends on the at-rest eye with no step. So too where the frame
    /// of the change itself carries frame time: the eye sets off on it, by
    /// no more than a tenth beyond the orbit point's own step.
    #[test]
    fn a_focus_change_from_inside_the_new_crafts_ball_leaves_by_the_nearest_way_out() {
        let radius_m = STARTER_RADIUS_M;
        let rmin = min_distance(radius_m);
        let (com_a, _) = landed(2.892);
        let com_b = com_a + DVec3::Z * 1.0;
        for change_dt_ms in [0.0, 1000.0 / 60.0] {
            let (cam, orbit, mut input, first_moved_m, start_m) =
                change_from_inside(com_a, com_b, change_dt_ms);
            let left = leave_the_ball(
                cam,
                &mut input,
                com_b,
                (change_dt_ms > 0.0).then_some(first_moved_m),
            );
            let LeftBall {
                cam,
                frames,
                widest,
                last_moved_m,
                riding,
            } = left;

            // At rest: kept out of the second craft exactly, and nothing
            // left of the ride. The travel's last frame is its own snap
            // of under a millimetre: the ride adds nothing to it.
            let level_b = level_frame(com_b, Some(DVec3::ZERO), DVec3::Z);
            let (lifted_b, on_floor) =
                solve_orbit(&orbit, com_b, &level_b, DVec3::ZERO, &mut flat_floor);
            assert!(on_floor, "the eye is not lifted at the pitch limit");
            let kept = skirt_eye(
                lifted_b,
                com_b,
                rmin,
                level_b.dir(0.0, 0.0),
                DVec3::ZERO,
                &mut flat_floor,
            );
            close(
                (cam.eye - kept).norm(),
                0.0,
                1.0e-9,
                "at rest the eye is not kept out of the second craft",
            );
            assert!(
                last_moved_m <= RIDE_DONE_M,
                "the frame the travel ended on moved the eye {last_moved_m} m: a step"
            );
            assert!(
                !riding && !input.travel.began_inside,
                "at rest nothing is left of the ride"
            );
            eprintln!(
                "focus change to a craft 1 m away with the eye {start_m:.3} m from its CoM (least distance {rmin:.3} m), the frame of the change carrying {change_dt_ms:.1} ms: \
                 that frame moved it {:.4} m; then {frames} frames, \
                 the most any frame's move {widest:.3} times the one before, and never nearer than {start_m:.6} m to the CoM; \
                 the last frame moved it {last_moved_m:.1e} m; at rest {:.6} m from the CoM",
                first_moved_m,
                (cam.eye - com_b).norm(),
            );
        }
    }

    /// How far a free eye `distance_m` from its anchor travels for a drag
    /// of `drag_rad` — the chord of its orbit.
    fn full_step(distance_m: f64, drag_rad: f64) -> f64 {
        distance_m * 2.0 * (0.5 * drag_rad).sin()
    }

    /// A craft BURIED: its CoM 30 m under the ground, orbited from 12 m —
    /// deeper than the eye distance, so with the anchor on the CoM every
    /// orbit point is lifted onto the floor and a vertical drag keeps only
    /// the sideways share `d·cos(pitch)`: 0.35 m for 0.1 rad, and nothing
    /// at the craft's own level, where its slope is zero. The anchor is
    /// lifted instead — 30.3 m of depth plus 12 m, so it is 12 m over the
    /// floor — and the eye is the intent's orbit point about it at every
    /// pitch. The camera looks at the craft 42.3 m under its anchor, not
    /// at the anchor. A 0.01 rad drag at pitch 0, and from ±0.01 rad,
    /// moves the eye the full 0.12 m a free eye travels; a yaw drag swings
    /// it the full arc; the angles are the player's own.
    #[test]
    fn a_buried_craft_is_orbited_from_over_the_ground() {
        let com = DVec3::X * (BODY_R - 30.0);
        let frame = ChaseFrame {
            com,
            centre: Some(DVec3::ZERO),
            ..space_frame(7, 1.0, 1000.0 / 60.0)
        };
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let level = level_frame(com, Some(DVec3::ZERO), DVec3::Z);
        // What the camera owes the intent: the orbit point about the
        // anchor, the craft's depth plus the eye distance up.
        let owed = |input: &FlightInput| {
            com + level.up * (floor_r - com.x + 12.0) + level.dir(input.yaw, input.pitch) * 12.0
        };
        let mut input = camera_at(12.0);
        held(&mut input, 7);
        input.yaw = 0.4;
        input.pitch = 0.0;
        let first = step_chase_camera(&mut input, &frame, &mut flat_floor);
        close(
            (first.eye - owed(&input)).norm(),
            0.0,
            1.0e-9,
            "the eye is not the intent's orbit point, 12 m over the floor",
        );
        let back = |cam: &MapCameraF64| {
            -DVec3::new(
                cam.view_proj[3][0],
                cam.view_proj[3][1],
                cam.view_proj[3][2],
            )
        };
        let to_com = first.eye - com;
        close(
            back(&first).dot(to_com / to_com.norm()),
            1.0,
            1.0e-12,
            "the camera does not look at the craft",
        );
        close(
            first.distance_m,
            to_com.norm(),
            1.0e-9,
            "the solved distance is not the eye's distance to the craft",
        );

        // A 0.01 rad drag at pitch 0 and from either side of it: the full
        // step of a free eye, on the very next frame.
        let mut moved = Vec::new();
        for (from, dpitch) in [(0.0, 0.01), (0.01, -0.01), (0.0, -0.01), (-0.01, 0.01)] {
            input.pitch = from;
            let before = step_chase_camera(&mut input, &frame, &mut flat_floor);
            input.pitch = from + dpitch;
            let after = step_chase_camera(&mut input, &frame, &mut flat_floor);
            let step = (after.eye - before.eye).norm();
            close(
                step,
                full_step(12.0, 0.01),
                1.0e-9,
                &format!("a 0.01 rad drag from pitch {from}"),
            );
            close(
                (after.eye - owed(&input)).norm(),
                0.0,
                1.0e-9,
                "still the intent's orbit point",
            );
            moved.push(step);
        }

        // Yaw +0.2 rad: the full arc of the 12 m circle.
        input.pitch = 0.0;
        let before = step_chase_camera(&mut input, &frame, &mut flat_floor);
        input.yaw += 0.2;
        let after = step_chase_camera(&mut input, &frame, &mut flat_floor);
        let swung = (after.eye - before.eye).norm();
        close(
            swung,
            full_step(12.0, 0.2),
            1.0e-9,
            "the yaw drag does not swing the eye the full arc",
        );
        assert_eq!(
            (input.yaw, input.pitch),
            (0.4 + 0.2, 0.0),
            "the angles are the player's own, whole"
        );
        eprintln!(
            "buried craft, 30 m down from 12 m: the anchor {:.1} m over the floor and {:.1} m over the CoM, the eye {:.3} m from the craft at pitch 0; \
             a 0.01 rad drag at pitch 0 / +0.01 / 0 / −0.01 moves the eye {:.4} / {:.4} / {:.4} / {:.4} m (a free eye {:.4}); \
             yaw +0.2 swings it {:.4} m (free {:.4})",
            12.0,
            floor_r - com.x + 12.0,
            to_com.norm(),
            moved[0],
            moved[1],
            moved[2],
            moved[3],
            full_step(12.0, 0.01),
            swung,
            full_step(12.0, 0.2),
        );
    }

    /// The buried craft zoomed out past its depth — 40 m of eye. The
    /// anchor is lifted to the floor plus the craft's 30.3 m of depth, so
    /// the orbit still dips under the floor below −0.864 rad. A pitch drag
    /// run up through that crossing and back down, 30 frames of 0.03 rad
    /// each way, moves the eye every frame and never by more than the
    /// drag's own step: under the floor the eye is on it exactly, over it
    /// the intent's orbit point exactly, and no step at either crossing.
    #[test]
    fn a_buried_crafts_eye_crosses_the_floor_without_a_step() {
        let com = DVec3::X * (BODY_R - 30.0);
        let frame = ChaseFrame {
            com,
            centre: Some(DVec3::ZERO),
            ..space_frame(7, 1.0, 1000.0 / 60.0)
        };
        let anchor = DVec3::X * (BODY_R + EYE_GROUND_CLEARANCE_M + 30.3);
        let level = level_frame(anchor, Some(DVec3::ZERO), DVec3::Z);
        let mut input = camera_at(40.0);
        held(&mut input, 7);
        input.yaw = 0.4;
        input.pitch = -1.2;
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let asked = |yaw: f64, pitch: f64| anchor + level.dir(yaw, pitch) * 40.0;
        let mut cam = step_chase_camera(&mut input, &frame, &mut flat_floor);
        let drag_step = full_step(40.0, 0.03);
        let (mut crossings, mut widest, mut narrowest) = (0, 0.0_f64, f64::MAX);
        let mut at_crossings = Vec::new();
        for dpitch in [0.03; 30].into_iter().chain([-0.03; 30]) {
            let before = cam.eye;
            let was_under = asked(input.yaw, input.pitch).norm() < floor_r;
            input.pitch += dpitch;
            cam = step_chase_camera(&mut input, &frame, &mut flat_floor);
            let point = asked(input.yaw, input.pitch);
            let moved = (cam.eye - before).norm();
            let ratio = moved / drag_step;
            assert!(
                moved > 0.0,
                "pitch {}: the drag did not move the eye",
                input.pitch
            );
            assert!(
                ratio <= 1.000_1,
                "pitch {}: the eye moved {moved} m for a drag of {drag_step} m",
                input.pitch
            );
            if point.norm() < floor_r {
                close(
                    cam.eye.norm(),
                    floor_r,
                    1.0e-9,
                    &format!("pitch {}: under the floor", input.pitch),
                );
            } else {
                assert_eq!(
                    cam.eye, point,
                    "pitch {}: over the floor, the eye is the asked point",
                    input.pitch
                );
            }
            let under = point.norm() < floor_r;
            if under != was_under {
                crossings += 1;
                at_crossings.push((input.pitch, moved, ratio));
            }
            widest = widest.max(ratio);
            narrowest = narrowest.min(ratio);
        }
        assert_eq!(crossings, 2, "one crossing each way");
        close(
            input.pitch,
            -1.2,
            1.0e-12,
            "the drag came back to where it started",
        );
        for (_, moved, ratio) in &at_crossings {
            close(*moved, 1.04157, 1.0e-4, "the eye's step at the crossing");
            close(
                *ratio,
                0.86801,
                1.0e-4,
                "the eye's share of the drag at the crossing",
            );
        }
        eprintln!(
            "buried craft, 40 m eye, anchor 30.3 m over the floor, drag 0.03 rad a frame ({drag_step:.4} m): eye/drag ratio {narrowest:.4}..={widest:.5}; \
             crossings (pitch, moved m, ratio): {at_crossings:?}"
        );
    }

    /// The ground of a shore for the flooded-craft tests: the waterline
    /// 20 m east of the craft on +X, the land rising east at 25 % and
    /// the sea floor falling west the same way, so the water is 5 m deep
    /// at the craft. `dir` is a unit direction on the test body.
    fn shore_ground(dir: DVec3) -> f64 {
        BODY_R + 0.25 * (dir.y * BODY_R - 20.0)
    }

    /// A craft in the water at a shore: its CoM 3 m under the surface,
    /// where the water is 5 m deep — 1.7 m over the seabed's floor. The
    /// surface is no floor: the anchor is the CoM itself, the eye is the
    /// intent's orbit point about it — under the water, 2 m over the
    /// seabed — and the camera looks at the craft. A 0.01 rad drag at
    /// pitch 0, and from ±0.01 rad, moves the eye the full 0.03 m a free
    /// eye travels.
    #[test]
    fn a_craft_in_the_water_is_orbited_under_the_surface() {
        let com = DVec3::X * (BODY_R - 3.0);
        let frame = ChaseFrame {
            com,
            centre: Some(DVec3::ZERO),
            ..space_frame(7, 1.0, 1000.0 / 60.0)
        };
        let mut shore = |dir: DVec3| eye_floor(shore_ground(dir));
        let level = level_frame(com, Some(DVec3::ZERO), DVec3::Z);
        let owed = |input: &FlightInput| com + level.dir(input.yaw, input.pitch) * 3.0;
        let mut input = camera_at(3.0);
        held(&mut input, 7);
        input.yaw = core::f64::consts::FRAC_PI_2;
        input.pitch = 0.0;
        let first = step_chase_camera(&mut input, &frame, &mut shore);
        close(
            (first.eye - owed(&input)).norm(),
            0.0,
            1.0e-9,
            "the eye is not the intent's orbit point about the CoM",
        );
        close(
            first.eye.norm(),
            ((BODY_R - 3.0).powi(2) + 9.0).sqrt(),
            1.0e-9,
            "the eye is not 3 m out, level with a CoM 3 m under the surface",
        );
        close(
            BODY_R - first.eye.norm(),
            3.0,
            1.0e-5,
            "the eye is not 3 m under the surface",
        );
        assert!(
            first.eye.norm() > shore_ground(first.eye / first.eye.norm()),
            "the eye is not over the seabed's floor: {:.3} m",
            first.eye.norm() - shore_ground(first.eye / first.eye.norm())
        );
        let to_com = first.eye - com;
        let back = -DVec3::new(
            first.view_proj[3][0],
            first.view_proj[3][1],
            first.view_proj[3][2],
        );
        close(
            back.dot(to_com / to_com.norm()),
            1.0,
            1.0e-12,
            "the camera does not look at the craft",
        );

        let mut moved = Vec::new();
        for (from, dpitch) in [(0.0, 0.01), (0.01, -0.01), (0.0, -0.01), (-0.01, 0.01)] {
            input.pitch = from;
            let before = step_chase_camera(&mut input, &frame, &mut shore);
            input.pitch = from + dpitch;
            let after = step_chase_camera(&mut input, &frame, &mut shore);
            let step = (after.eye - before.eye).norm();
            close(
                step,
                full_step(3.0, 0.01),
                1.0e-9,
                &format!("a 0.01 rad drag from pitch {from}"),
            );
            close(
                (after.eye - owed(&input)).norm(),
                0.0,
                1.0e-9,
                "still the intent's orbit point",
            );
            moved.push(step);
        }
        assert_eq!(
            (input.yaw, input.pitch),
            (core::f64::consts::FRAC_PI_2, 0.0),
            "the angles are the player's own, whole"
        );
        eprintln!(
            "craft in the water, CoM 3 m under the surface, from 3 m: the eye {:.3} m from the craft at pitch 0; \
             a 0.01 rad drag at pitch 0 / +0.01 / 0 / −0.01 moves the eye {:.4} / {:.4} / {:.4} / {:.4} m (a free eye {:.4})",
            to_com.norm(),
            moved[0],
            moved[1],
            moved[2],
            moved[3],
            full_step(3.0, 0.01),
        );
    }

    /// The craft in the water, its crossing. Zoomed out to 16 m, the eye
    /// orbits east towards the shore, where the ground rises from the
    /// seabed up the shore's slope, and dips under that floor below
    /// pitch 0.14. A pitch drag run down through the crossing and back
    /// up, 30 frames of 0.02 rad each way, moves the eye every frame and
    /// never by more than the drag's own step: on the floor the eye is on
    /// it exactly, off it the intent's orbit point exactly, and no step
    /// at either crossing.
    #[test]
    fn a_craft_in_the_waters_eye_crosses_the_shore_without_a_step() {
        let com = DVec3::X * (BODY_R - 3.0);
        let frame = ChaseFrame {
            com,
            centre: Some(DVec3::ZERO),
            ..space_frame(7, 1.0, 1000.0 / 60.0)
        };
        let mut shore = |dir: DVec3| eye_floor(shore_ground(dir));
        let anchor = resting_anchor(com, 16.0, Some(DVec3::ZERO), &mut shore);
        close(
            (anchor - com).norm(),
            0.0,
            1.0e-9,
            "the anchor is not the CoM, 1.7 m over the seabed's floor",
        );
        let level = level_frame(anchor, Some(DVec3::ZERO), DVec3::Z);
        let mut input = camera_at(16.0);
        held(&mut input, 7);
        input.yaw = core::f64::consts::FRAC_PI_2;
        input.pitch = 0.35;
        let asked = |yaw: f64, pitch: f64| anchor + level.dir(yaw, pitch) * 16.0;
        // A point's radius from the body centre and the floor along it.
        let radii = |point: DVec3, shore: &mut dyn FnMut(DVec3) -> f64| {
            let r = point.norm();
            (r, shore(point / r))
        };
        let mut cam = step_chase_camera(&mut input, &frame, &mut shore);
        let drag_step = full_step(16.0, 0.02);
        let (mut crossings, mut widest, mut narrowest) = (0, 0.0_f64, f64::MAX);
        let mut at_crossings = Vec::new();
        for dpitch in [-0.02; 30].into_iter().chain([0.02; 30]) {
            let before = cam.eye;
            let (was_r, was_floor_r) = radii(asked(input.yaw, input.pitch), &mut shore);
            input.pitch += dpitch;
            cam = step_chase_camera(&mut input, &frame, &mut shore);
            let (now_r, floor_r) = radii(asked(input.yaw, input.pitch), &mut shore);
            let moved = (cam.eye - before).norm();
            let ratio = moved / drag_step;
            assert!(
                moved > 0.0,
                "pitch {}: the drag did not move the eye",
                input.pitch
            );
            assert!(
                ratio <= 1.000_1,
                "pitch {}: the eye moved {moved} m for a drag of {drag_step} m",
                input.pitch
            );
            if now_r < floor_r {
                close(
                    cam.eye.norm(),
                    floor_r,
                    1.0e-9,
                    &format!("pitch {}: on the floor", input.pitch),
                );
            } else {
                assert_eq!(
                    cam.eye,
                    asked(input.yaw, input.pitch),
                    "pitch {}: over the floor, the eye is the asked point",
                    input.pitch
                );
            }
            if (now_r < floor_r) != (was_r < was_floor_r) {
                crossings += 1;
                at_crossings.push((input.pitch, moved, ratio));
            }
            widest = widest.max(ratio);
            narrowest = narrowest.min(ratio);
        }
        assert_eq!(crossings, 2, "one crossing each way");
        close(
            input.pitch,
            0.35,
            1.0e-12,
            "the drag came back to where it started",
        );
        for (pitch, moved, ratio) in &at_crossings {
            assert!(
                (*pitch - 0.13).abs() < 1.0e-9 || (*pitch - 0.15).abs() < 1.0e-9,
                "a crossing at pitch {pitch}, not beside 0.14"
            );
            close(*moved, 0.13271, 1.0e-4, "the eye's step at the crossing");
            close(
                *ratio,
                0.41471,
                1.0e-4,
                "the eye's share of the drag at the crossing",
            );
        }
        eprintln!(
            "craft in the water, 16 m eye, drag 0.02 rad a frame ({drag_step:.4} m): eye/drag ratio {narrowest:.4}..={widest:.5}; \
             crossings (pitch, moved m, ratio): {at_crossings:?}"
        );
    }

    /// R7: when the focus moves to a vessel 20 m away the eye does not
    /// jump. The frame of the change moves it by exactly one step of the
    /// travel — nothing at all without frame time — and the anchor then
    /// closes on the new craft by the same share each frame (the
    /// travel's own half-life) until it is under a millimetre; from then
    /// on the camera orbits the drawn CoM exactly. A first focus travels
    /// nothing.
    #[test]
    fn a_focus_change_travels_from_the_last_eye() {
        let com = DVec3::new(20.0, 0.0, 0.0);
        let start = || {
            let mut input = camera_at(12.0);
            held(&mut input, 1);
            let before = step_chase_camera(
                &mut input,
                &space_frame(1, 3.0, 1000.0 / 60.0),
                &mut no_floor,
            );
            (input, before)
        };
        // No frame time: the frame of the change moves neither eye nor view.
        let (mut input, before) = start();
        let still = step_chase_camera(
            &mut input,
            &ChaseFrame {
                com,
                ..space_frame(2, 3.0, 0.0)
            },
            &mut no_floor,
        );
        assert_eq!((still.eye, still.view_proj), (before.eye, before.view_proj));

        let (mut input, _before) = start();
        let to = ChaseFrame {
            com,
            ..space_frame(2, 3.0, 1000.0 / 60.0)
        };
        let mut cam = step_chase_camera(&mut input, &to, &mut no_floor);
        let k = travel_share(20.0, 1.0 / 60.0);
        close(
            (cam.eye - before.eye).norm(),
            20.0 * k,
            1.0e-9,
            "one easing step",
        );
        let mut left = 20.0 * (1.0 - k);
        let mut frames = 0;
        while left >= TRAVEL_DONE_M {
            let prev = cam.eye;
            cam = step_chase_camera(&mut input, &to, &mut no_floor);
            left *= 1.0 - k;
            // The frame the travel ends snaps the rest (under 1 mm).
            let step = if left >= TRAVEL_DONE_M { k } else { 1.0 };
            close(
                (cam.eye - prev).norm(),
                left / (1.0 - k) * step,
                1.0e-9,
                "travel step",
            );
            frames += 1;
            assert!(frames < 400, "the travel never ended");
        }
        close(
            (cam.eye - com).norm(),
            input.distance_m(),
            1.0e-9,
            "orbiting the drawn CoM",
        );

        // A first focus starts on the new craft.
        let mut input = camera_at(12.0);
        let first = step_chase_camera(
            &mut input,
            &ChaseFrame {
                com,
                ..space_frame(2, 3.0, 0.0)
            },
            &mut no_floor,
        );
        close(
            (first.eye - com).norm(),
            12.0,
            1.0e-9,
            "a first focus travels nothing",
        );
    }

    /// The travel's own half-life: the zoom's 80 ms for a short change,
    /// and for a long one exactly what brings it under a millimetre in
    /// half a second — the same time for a metre as for 850 km.
    #[test]
    fn a_travel_takes_half_a_second_whatever_its_length() {
        assert_eq!(travel_half_life(1.0e-3 * 1.000_001), EASE_HALF_LIFE_S);
        assert_eq!(travel_half_life(0.05), EASE_HALF_LIFE_S);
        for d0_m in [0.1, 20.0, 850_600.0, 1.0e6] {
            let half = travel_half_life(d0_m);
            assert!(half < EASE_HALF_LIFE_S, "{d0_m} m: {half}");
            close(
                half * (d0_m / TRAVEL_DONE_M).log2(),
                TRAVEL_SETTLE_S,
                1.0e-12,
                &format!("time to done at {d0_m} m"),
            );
        }
        assert!(
            travel_half_life(850_600.0) < travel_half_life(20.0),
            "the further, the faster"
        );
    }

    /// What [`run_travel`] saw on the way.
    struct Travel {
        /// Frames until the travel was done.
        frames: usize,
        /// The anchor's radius after the first timed frame (m).
        first_anchor_r: f64,
        /// How far under the floor the straight line between the two
        /// resting anchors goes (m) — `0` where it never dips.
        deepest_m: f64,
        /// How far the eye moved on the first timed frame, and on the
        /// frame of the change itself, which had no frame time (m).
        moved_m: [f64; 2],
    }

    /// What every frame of a travel keeps on the flat body: the orbit
    /// angles whole, the anchor and the eye on or above the floor.
    fn held_through_travel(
        input: &FlightInput,
        cam: &MapCameraF64,
        angles: (f64, f64),
        floor_r: f64,
        frames: usize,
    ) {
        let last = input.travel.last.expect("a focus was just solved");
        assert_eq!(
            (input.yaw, input.pitch),
            angles,
            "frame {frames}: a focus change touched the orbit angles"
        );
        assert!(
            last.anchor.norm() >= floor_r - 1.0e-9,
            "frame {frames}: the anchor is {} m under the floor",
            floor_r - last.anchor.norm()
        );
        assert!(
            cam.eye.norm() >= floor_r - 1.0e-9,
            "frame {frames}: the eye is under the floor"
        );
    }

    /// The share a travel closed on one frame: what was left shrank by
    /// exactly `share`, or — on the frame it ends — snapped with less
    /// than one step's worth left.
    fn closed_share(left: f64, now: f64, share: f64, frames: usize) {
        if now > 0.0 {
            close(
                now,
                left * (1.0 - share),
                1.0e-9 * left,
                &format!("frame {frames}: the share changed mid-travel"),
            );
        } else {
            assert!(
                left * (1.0 - share) < TRAVEL_DONE_M,
                "frame {frames}: snapped with {left} m left"
            );
        }
    }

    /// At the end of a travel the camera rests on the new craft's anchor
    /// and looks at the craft.
    fn rests_on(input: &FlightInput, cam: &MapCameraF64, to_anchor: DVec3, com: DVec3) {
        let last = input.travel.last.expect("a focus was just solved");
        assert_eq!(
            (last.id, last.anchor),
            (2, to_anchor),
            "the lift outlived the travel"
        );
        close(
            (last.look_at - com).norm(),
            0.0,
            1.0e-9,
            "at rest the camera does not look at the craft",
        );
        close(
            (cam.eye - to_anchor).norm(),
            12.0,
            1.0e-9,
            "orbiting the new craft's resting anchor at rest",
        );
    }

    /// Drive a focus change from a craft at `from` — vessel 1, where the
    /// camera came to rest — to vessel 2 at `com`, `dt_ms` a frame on the
    /// flat body, until the travel is done. The frame of the change, with
    /// no frame time, must move neither the eye, nor the view, nor the
    /// look-at; every later frame must keep the orbit angles exactly, keep
    /// the anchor and the eye on or above the floor, and close the same
    /// share of the travel; at the end the camera must orbit vessel 2's
    /// resting anchor and look at vessel 2.
    fn run_travel(from: DVec3, com: DVec3, dt_ms: f64) -> Travel {
        let resting =
            |craft: DVec3| resting_anchor(craft, 12.0, Some(DVec3::ZERO), &mut flat_floor);
        let (from_anchor, to_anchor) = (resting(from), resting(com));
        let d0_m = (from_anchor - to_anchor).norm();
        let share = travel_share(d0_m, dt_ms / 1000.0);
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let mut input = camera_at(12.0);
        let settled = step_chase_camera(&mut input, &body_frame(1, from, dt_ms), &mut flat_floor);
        let settled_look = input.travel.last.expect("a focus was just solved").look_at;
        let (yaw, pitch) = (input.yaw, input.pitch);

        // The frame of the change, before any time is put in.
        let still = step_chase_camera(&mut input, &body_frame(2, com, 0.0), &mut flat_floor);
        same_view(&still, &settled, "the frame of the change");
        close(
            (input.travel.last.expect("a focus was just solved").look_at - settled_look).norm(),
            0.0,
            1.0e-9,
            "the frame of the change moved the look-at",
        );
        close(
            input.travel.offset.norm(),
            d0_m,
            1.0e-9 * d0_m.max(1.0),
            "the travel does not run from the previous resting anchor to this one",
        );

        let mut cam = step_chase_camera(&mut input, &body_frame(2, com, dt_ms), &mut flat_floor);
        let moved_m = [
            (cam.eye - settled.eye).norm(),
            (still.eye - settled.eye).norm(),
        ];
        close(
            input.travel.offset.norm(),
            d0_m * (1.0 - share),
            1.0e-9 * d0_m.max(1.0),
            "the first timed frame closed one travel share",
        );
        let first_anchor_r = input
            .travel
            .last
            .expect("a focus was just solved")
            .anchor
            .norm();
        let mut deepest_m: f64 = 0.0;
        let mut frames = 1;
        loop {
            held_through_travel(&input, &cam, (yaw, pitch), floor_r, frames);
            deepest_m = deepest_m.max(floor_r - (to_anchor + input.travel.offset).norm());
            if input.travel.offset == DVec3::ZERO {
                break;
            }
            let left = input.travel.offset.norm();
            cam = step_chase_camera(&mut input, &body_frame(2, com, dt_ms), &mut flat_floor);
            closed_share(left, input.travel.offset.norm(), share, frames);
            frames += 1;
            assert!(frames <= 4_000, "the travel never ended");
        }
        rests_on(&input, &cam, to_anchor, com);
        Travel {
            frames,
            first_anchor_r,
            deepest_m,
            moved_m,
        }
    }

    /// R7 for a far change, on the flat body: from 20 m to a thousand
    /// kilometres away, at 30, 60 and 144 fps, the travel is done — the
    /// anchor on the new craft — within half a second plus the frame
    /// that crosses it, the anchor never dips under the floor, and yaw
    /// and pitch come out bit-identical. A thousand kilometres away the
    /// straight line crosses the planet hundreds of kilometres deep: the
    /// anchor rides the floor plus one eye distance instead. Twenty
    /// metres away it never dips and is not lifted.
    #[test]
    fn a_focus_change_is_done_in_half_a_second_and_keeps_the_orbit() {
        let from = DVec3::new(BODY_R + 5.0, 0.0, 0.0);
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        for fps in [30.0, 60.0, 144.0] {
            let dt_ms = 1000.0 / fps;
            for (d0_m, crosses) in [(20.0, false), (1.0e6, true)] {
                let com = neighbour(d0_m);
                close((from - com).norm(), d0_m, 1.0e-7, "the crafts' distance");
                let travel = run_travel(from, com, dt_ms);
                let t_s = travel.frames as f64 * dt_ms / 1000.0;
                assert!(
                    t_s <= TRAVEL_SETTLE_S + dt_ms / 1000.0,
                    "{d0_m} m at {fps} fps: {} frames, {t_s:.3} s",
                    travel.frames
                );
                assert!(
                    t_s >= TRAVEL_SETTLE_S - dt_ms / 1000.0,
                    "{d0_m} m at {fps} fps: done in {t_s:.3} s, too fast for its half-life"
                );
                eprintln!(
                    "focus change {d0_m:.0} m at {fps:.0} fps: half-life {:.1} ms, done in {} frames \
                     ({t_s:.3} s); straight line {:.3} m under the floor at its deepest; \
                     first anchor {:.6} m over the floor",
                    travel_half_life(d0_m) * 1000.0,
                    travel.frames,
                    travel.deepest_m,
                    travel.first_anchor_r - floor_r,
                );
                if crosses {
                    assert!(
                        travel.deepest_m > 12.0,
                        "the straight line never crossed the planet"
                    );
                    close(
                        travel.first_anchor_r,
                        floor_r + 12.0,
                        1.0e-6,
                        "lifted one eye distance over the floor",
                    );
                } else {
                    assert_eq!(
                        travel.deepest_m, 0.0,
                        "a short travel stays above the floor"
                    );
                    close(
                        travel.first_anchor_r,
                        from.norm(),
                        1.0e-4,
                        "a short travel is not lifted",
                    );
                }
            }
        }
    }

    /// A focus change FROM a craft under the floor — 30 m down, its
    /// anchor 12 m over the ground — and one TO it, at 30, 60 and 144
    /// fps, 20 m and a thousand kilometres. The frame of the change moves
    /// the eye by nothing at all: it has no frame time, and the travel
    /// starts where the last frame's eye orbited, which already is the
    /// buried craft's lifted anchor (it used to start 30 m further down,
    /// at the CoM, and be lifted in full on that first frame — a jump of
    /// 12.963 m). The first timed frame moves the eye by one step of the
    /// travel; the travel is done in half a second plus a frame; and at
    /// rest the camera orbits the new craft's resting anchor — over the
    /// ground, for the buried one — and looks at the craft.
    #[test]
    fn a_focus_change_from_or_to_a_buried_craft_moves_nothing_on_its_first_frame() {
        let buried = DVec3::X * (BODY_R - 30.0);
        let above = DVec3::new(BODY_R + 5.0, 0.0, 0.0);
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        for fps in [30.0, 60.0, 144.0] {
            let dt_ms = 1000.0 / fps;
            for (from, to, what) in [
                (buried, neighbour(20.0), "from the buried craft, 20 m"),
                (buried, neighbour(1.0e6), "from the buried craft, 1 000 km"),
                (above, buried, "to the buried craft, 20 m"),
            ] {
                let travel = run_travel(from, to, dt_ms);
                let t_s = travel.frames as f64 * dt_ms / 1000.0;
                assert!(
                    travel.moved_m[1] <= 1.0e-9,
                    "{what} at {fps:.0} fps: the frame of the change moved the eye {} m",
                    travel.moved_m[1]
                );
                assert!(
                    t_s <= TRAVEL_SETTLE_S + dt_ms / 1000.0,
                    "{what} at {fps:.0} fps: {} frames, {t_s:.3} s",
                    travel.frames
                );
                assert!(
                    t_s >= TRAVEL_SETTLE_S - dt_ms / 1000.0,
                    "{what} at {fps:.0} fps: done in {t_s:.3} s, too fast for its half-life"
                );
                eprintln!(
                    "focus change {what} at {fps:.0} fps: the frame of the change moved the eye {:.6} m; \
                     the first timed frame {:.4} m; done in {} frames ({t_s:.3} s); \
                     straight line {:.3} m under the floor at its deepest; first anchor {:.6} m over the floor",
                    travel.moved_m[1],
                    travel.moved_m[0],
                    travel.frames,
                    travel.deepest_m,
                    travel.first_anchor_r - floor_r,
                );
            }
        }
    }

    /// The resting anchor on its own. A craft under the floor keeps its
    /// anchor `depth + min(depth, eye distance)` above its CoM: 30.3 m
    /// down with a 12 m eye, 12 m over the floor; 3 m down with a 12 m
    /// eye, 3 m over it; 3 m down with a 3 m eye, 3 m over it. A craft at
    /// or above the floor rests on its CoM, and without a body there is no
    /// floor to sample.
    #[test]
    fn a_craft_under_the_floor_rests_on_an_anchor_over_the_ground() {
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let rest = |com: DVec3, eye_m: f64| {
            resting_anchor(com, eye_m, Some(DVec3::ZERO), &mut flat_floor).x
        };
        close(
            rest(DVec3::X * (BODY_R - 30.0), 12.0),
            floor_r + 12.0,
            1.0e-9,
            "30.3 m down from 12 m",
        );
        close(
            rest(DVec3::X * (floor_r - 3.0), 12.0),
            floor_r + 3.0,
            1.0e-9,
            "3 m down from 12 m",
        );
        close(
            rest(DVec3::X * (floor_r - 3.0), 3.0),
            floor_r + 3.0,
            1.0e-9,
            "3 m down from 3 m",
        );
        close(
            rest(DVec3::X * (floor_r - 1.0e-4), 12.0),
            floor_r + 1.0e-4,
            1.0e-12,
            "a hair under the floor: its depth over the floor",
        );
        assert_eq!(
            rest(DVec3::X * floor_r, 12.0),
            floor_r,
            "on the floor: the CoM itself"
        );
        assert_eq!(
            rest(DVec3::X * (BODY_R + 5.0), 12.0),
            BODY_R + 5.0,
            "over the floor: the CoM itself"
        );
        let mut never = |_: DVec3| -> f64 { panic!("no body, no floor to sample") };
        let com = DVec3::X * (BODY_R - 30.0);
        assert_eq!(resting_anchor(com, 12.0, None, &mut never), com);
    }

    /// The ground rule for a travelling anchor on its own. One under the
    /// floor is lifted radially to the floor plus its depth up to one
    /// eye distance: 5 m deep, 5 m up; a tenth of an eye distance deep,
    /// a tenth up; anything deeper, a full eye distance up, so the whole
    /// orbit clears the ground. The margin shrinks with the travel that
    /// is left and is nothing once the travel is done — an at-rest
    /// anchor is the craft's own, under the floor or not. An anchor above
    /// the floor, or with no body under it, is handed back untouched.
    #[test]
    fn a_travelling_anchor_is_lifted_just_over_the_ground() {
        let floor_r = BODY_R + EYE_GROUND_CLEARANCE_M;
        let under = DVec3::X * (BODY_R - 300_000.0);
        let lifted = |anchor: DVec3, remaining_m: f64, eye_m: f64| {
            travel_anchor(
                anchor,
                remaining_m,
                eye_m,
                Some(DVec3::ZERO),
                &mut flat_floor,
            )
        };
        let over = |got: DVec3, want_r: f64, what: &str| {
            assert!(got.y == 0.0 && got.z == 0.0, "lifted radially: {got:?}");
            close(got.x, want_r, 1.0e-9, what);
        };
        over(lifted(under, 500_000.0, 12.0), floor_r + 12.0, "deep under");
        over(
            lifted(DVec3::X * (floor_r - 5.0), 500_000.0, 12.0),
            floor_r + 5.0,
            "5 m under",
        );
        over(
            lifted(DVec3::X * (floor_r - 3.0), 6.0, 12.0),
            floor_r + 1.5,
            "3 m under, half an eye distance to go",
        );
        // Done (or never started): the anchor is left where it is.
        assert_eq!(
            lifted(DVec3::X * (floor_r - 3.0), 0.0, 12.0),
            DVec3::X * (floor_r - 3.0)
        );
        assert_eq!(lifted(under, 500_000.0, 0.0), under);
        let above = DVec3::new(BODY_R + 5.0, 0.1, 0.2);
        assert_eq!(lifted(above, 500_000.0, 12.0), above);
        // No body: nothing to lift against — the floor is not sampled.
        let mut never = |_: DVec3| -> f64 { panic!("no body, no floor to sample") };
        assert_eq!(
            travel_anchor(under, 500_000.0, 12.0, None, &mut never),
            under
        );
    }

    /// A change to a craft already under a millimetre from where the eye
    /// orbited is no travel at all — the anchor is the new craft's resting
    /// anchor that same frame — and neither is one from an anchor that is
    /// not a number.
    #[test]
    fn a_focus_change_with_nothing_to_travel_starts_on_the_new_craft() {
        let com = DVec3::new(BODY_R + 5.0, 20.0, 0.0);
        let beside = com + DVec3::X * 5.0e-4;
        assert!((beside - com).norm() < TRAVEL_DONE_M);
        let nan = DVec3::new(f64::NAN, 0.0, 0.0);
        let inf = DVec3::new(f64::INFINITY, 0.0, 0.0);
        for from in [com, beside, nan, inf] {
            let mut input = camera_at(12.0);
            input.travel.last = Some(Last {
                id: 1,
                anchor: from,
                look_at: from,
                skirt: DVec3::ZERO,
                eye: from,
                carried: from,
                moved_m: None,
                travelled: 0.0,
            });
            let cam = step_chase_camera(
                &mut input,
                &body_frame(2, com, 1000.0 / 60.0),
                &mut flat_floor,
            );
            assert_eq!(input.travel.offset, DVec3::ZERO, "from {from:?}");
            assert!(input.travel.half_life_s.is_none(), "from {from:?}");
            assert_eq!(
                input.travel.last.map(|l| (l.id, l.anchor)),
                Some((2, com)),
                "from {from:?}"
            );
            close(
                (cam.eye - com).norm(),
                12.0,
                1.0e-9,
                &format!("from {from:?}: orbiting the new craft"),
            );
        }
    }

    /// One frame of `csp-app`'s loop without the window, 60 fps: the
    /// frame's wall time handed over the way `run_frame` does, the
    /// camera events drained through the Input schedule, the scene clock
    /// standing still.
    fn session_frame(view: &mut crate::View) {
        view.update_session(1000.0 / 60.0, 1, 1, None);
        view.begin_frame();
        view.end_frame();
    }

    /// Pump frames until the flight packet carries the REAL baked
    /// heightfield of the dominant body — the render gate's wait. Until
    /// it lands the floor samples the bare datum.
    fn wait_for_real_heightfield(view: &mut crate::View) {
        let want = {
            let cfg = view.world.resource::<crate::terrain::TerrainConfig>();
            let body_id = view
                .world
                .resource::<FlightSnapshot>()
                .env
                .as_ref()
                .and_then(|e| e.body.as_ref())
                .expect("a dominant body");
            let body = cfg
                .bodies
                .get(body_id.as_str())
                .expect("a terrain entry for the dominant body");
            csp_render::terrain::HeightField::bake_profiled_key(body.seed, body.profile)
        };
        for _ in 0..12_000 {
            session_frame(view);
            if let Some(p) = view.take_flight_packet()
                && let Some(t) = &p.terrain
                && let Some(h) = &t.heightfield
                && h.key == want
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!(
            "the real heightfield never landed in the flight packet — the background bake stalled?"
        );
    }

    /// Where the solved camera and the focused craft sit over the REAL
    /// surface.
    struct OverGround {
        /// The eye's radius from the body centre (m).
        eye_r: f64,
        /// The craft's drawn CoM radius (m).
        com_r: f64,
        /// The ground radius sampled under the eye (m) — the floor's own
        /// sampler, [`crate::terrain::ground_radius`].
        ground_r: f64,
        /// The body's datum radius (m).
        datum_r: f64,
    }

    fn over_ground(view: &mut crate::View, focus: u64) -> OverGround {
        let (eye, com, env) = {
            let scene = view.world.resource::<DrawnScene>();
            let env = scene
                .env()
                .or_else(|| view.world.resource::<FlightSnapshot>().env.as_ref())
                .expect("physics")
                .clone();
            let com = scene.drawn(focus).expect("the focus is drawn").com;
            let eye = view.world.resource::<FlightCameraF64>().0.eye;
            (eye, com, env)
        };
        let rel = eye - env.body_center;
        let ground_r = {
            // A bevy world lends one resource out at a time: the cache
            // leaves it for the sample and goes straight back.
            let mut cache = view
                .world
                .remove_resource::<crate::terrain::TerrainCache>()
                .expect("the terrain cache is installed");
            let cfg = view.world.resource::<crate::terrain::TerrainConfig>();
            let ground_r =
                crate::terrain::ground_radius(cfg, &mut cache, &env, rel / rel.norm(), 0.0);
            view.world.insert_resource(cache);
            ground_r
        };
        OverGround {
            eye_r: rel.norm(),
            com_r: (com - env.body_center).norm(),
            ground_r,
            datum_r: env.radius_m,
        }
    }

    /// The floor `flight_camera` solves against along a unit direction on
    /// the REAL body: [`eye_floor`] over [`crate::terrain::ground_radius`].
    fn floor_along(view: &mut crate::View, dir: DVec3) -> f64 {
        let env = view
            .world
            .resource::<DrawnScene>()
            .env()
            .or_else(|| view.world.resource::<FlightSnapshot>().env.as_ref())
            .expect("physics")
            .clone();
        // A bevy world lends one resource out at a time: the cache
        // leaves it for the sample and goes straight back.
        let mut cache = view
            .world
            .remove_resource::<crate::terrain::TerrainCache>()
            .expect("the terrain cache is installed");
        let cfg = view.world.resource::<crate::terrain::TerrainConfig>();
        let floor_r = eye_floor(crate::terrain::ground_radius(
            cfg, &mut cache, &env, dir, 0.0,
        ));
        view.world.insert_resource(cache);
        floor_r
    }

    /// The eye the camera owes an orbit on the REAL body: the intent's
    /// orbit point about the focus's drawn CoM, lifted radially onto
    /// `flight_camera`'s own floor — [`eye_floor`] over
    /// [`crate::terrain::ground_radius`] — while it is under it.
    fn owed_eye(view: &mut crate::View, focus: u64) -> DVec3 {
        let (com, env, yaw, pitch, distance_m) = {
            let scene = view.world.resource::<DrawnScene>();
            let env = scene
                .env()
                .or_else(|| view.world.resource::<FlightSnapshot>().env.as_ref())
                .expect("physics")
                .clone();
            let com = scene.drawn(focus).expect("the focus is drawn").com;
            let input = view.world.resource::<FlightInput>();
            (com, env, input.yaw, input.pitch, input.distance_m())
        };
        let asked = com
            + level_frame(com, Some(env.body_center), env.spin_axis).dir(yaw, pitch) * distance_m;
        let rel = asked - env.body_center;
        let r = rel.norm();
        let dir = rel / r;
        let floor_r = floor_along(view, dir);
        if r < floor_r {
            env.body_center + dir * floor_r
        } else {
            asked
        }
    }

    /// The solved camera's eye, with the pitch and the distance it was
    /// solved at.
    fn solved(view: &crate::View) -> (DVec3, f64, f64) {
        let (input, cam) = (
            view.world.resource::<FlightInput>(),
            view.world.resource::<FlightCameraF64>(),
        );
        (cam.0.eye, input.pitch, input.distance_m())
    }

    /// The starter craft on the REAL pad, the baked heightfield landed and
    /// the framing settled: the scene both real-pad tests run on. Returns
    /// the view and the craft's id as the flight scene stages it.
    fn real_pad_view() -> (crate::View, u64) {
        let mut view = crate::View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        view.set_flight_viewport(320.0, 240.0);
        // The seed lands in its first stepped frames; then the framing
        // settles and the bake lands on idle ones.
        for _ in 0..3 {
            view.begin_frame();
            view.sim_step(view.sim.clock().ticks() + 1)
                .expect("sim step");
            view.end_frame();
        }
        wait_for_real_heightfield(&mut view);
        for _ in 0..150 {
            session_frame(&mut view);
        }
        let focus = view.flight_focus().expect("the starter craft is focused");
        (view, focus.0)
    }

    /// The floor under one craft on the REAL body, in the least-distance
    /// rule's own terms.
    struct UnderCraft {
        /// The body's centre.
        centre: DVec3,
        /// The craft's local up.
        up: DVec3,
        /// The yaw's horizontal at the craft, squared up to `up`.
        along: DVec3,
        /// The floor's radius under the craft (m).
        n_r: f64,
    }

    impl UnderCraft {
        fn at(view: &mut crate::View, com: DVec3, yaw: f64) -> Self {
            let env = view
                .world
                .resource::<FlightSnapshot>()
                .env
                .as_ref()
                .expect("physics")
                .clone();
            let up = (com - env.body_center) / (com - env.body_center).norm();
            let along = level_frame(com, Some(env.body_center), env.spin_axis).dir(yaw, 0.0);
            let n_r = floor_along(view, up);
            Self {
                centre: env.body_center,
                up,
                along,
                n_r,
            }
        }

        /// How far `point` is along the floor from the point under the
        /// craft.
        fn along_to(&self, point: DVec3) -> f64 {
            let out = point - self.centre;
            let side = out - self.up * out.dot(self.up);
            side.norm().atan2(out.dot(self.up)) * self.n_r
        }

        /// The floor point `x` metres along the floor from the point
        /// under the craft.
        fn floor_at(&self, view: &mut crate::View, x: f64) -> DVec3 {
            let (sin, cos) = (x / self.n_r).sin_cos();
            let dir = self.up * cos + self.along * sin;
            self.centre + dir * floor_along(view, dir)
        }
    }

    /// What a drag back up out of the zone and down into it again saw.
    struct GroundSweep {
        /// The least and the most the eye moved on any frame outside the
        /// zone — the frame that left it and the frame that entered it
        /// again included (m).
        moved_m: [f64; 2],
        /// The most the eye moved on any frame inside the zone, and the
        /// most that was for a metre of its orbit point's slide (m).
        inside_moved_m: [f64; 2],
        /// The nearest the eye came to the CoM (m).
        nearest_m: f64,
        /// The pitch of the frame the eye entered the zone on again (rad),
        /// and how far out along the yaw's line its orbit point was on the
        /// frame before and on that frame (m): the turn lies between the
        /// two.
        entry: (f64, [f64; 2]),
    }

    /// From the eye's place at the pitch limit, drag 60 frames of 0.01 rad
    /// up and the same down through the real input path, a frame at a
    /// time: every frame 0.3 m over the ground and moving, never nearer
    /// than `rmin` to the CoM and exactly that distance from it while it
    /// is inside the zone; by no more than the drag's own step outside the
    /// zone and on the frames that leave and enter it, by no more than
    /// [`skirt_plateau`] times the orbit point's slide inside it; the way
    /// down retracing the way up to a nanometre.
    fn sweep_the_ground(
        view: &mut crate::View,
        focus: u64,
        com: DVec3,
        rmin: f64,
        under: &UnderCraft,
        start: DVec3,
    ) -> GroundSweep {
        let distance_m = view.world.resource::<FlightInput>().distance_m();
        let drag_step = full_step(distance_m, 0.01);
        let mut sweep = GroundSweep {
            moved_m: [f64::MAX, 0.0],
            inside_moved_m: [0.0; 2],
            nearest_m: (start - com).norm(),
            entry: (0.0, [0.0; 2]),
        };
        let (mut up_path, mut down_path) = (Vec::new(), Vec::new());
        // The eye starts at the pitch limit, inside the zone.
        let (mut previous, mut before) = (start, (0.0, true));
        for dpitch in [0.01; 60].into_iter().chain([-0.01; 60]) {
            view.flight_event(FlightEvent::Orbit { dyaw: 0.0, dpitch });
            session_frame(view);
            let (eye, pitch, _) = solved(view);
            let what = format!("pitch {pitch:+.4} rad");
            let over = over_ground(view, focus);
            close(
                over.eye_r - over.ground_r,
                EYE_GROUND_CLEARANCE_M,
                1.0e-6,
                &format!("{what}: the eye over the ground"),
            );
            let point = owed_eye(view, focus);
            let (now, inside) = (under.along_to(point), (point - com).norm() < rmin);
            let (moved, to_com) = ((eye - previous).norm(), (eye - com).norm());
            assert!(
                to_com >= rmin - SKIRT_PRECISION_M,
                "{what}: the eye came {to_com} m from the CoM, inside the least distance {rmin}"
            );
            if inside {
                close(
                    to_com,
                    rmin,
                    1.0e-6,
                    &format!("{what}: the eye is not on the least distance"),
                );
            }
            if inside && before.1 {
                let slid = (now - before.0).abs();
                assert!(moved > 0.0, "{what}: the drag did not move the eye");
                assert!(
                    moved <= skirt_plateau() * slid * 1.001,
                    "{what}: the eye moved {moved} m inside the zone for a slide of {slid} m"
                );
                sweep.inside_moved_m = [
                    sweep.inside_moved_m[0].max(moved),
                    sweep.inside_moved_m[1].max(moved / slid),
                ];
            } else {
                dragged(com, rmin, drag_step, eye, previous, &what);
                sweep.moved_m = [sweep.moved_m[0].min(moved), sweep.moved_m[1].max(moved)];
            }
            if inside && !before.1 {
                sweep.entry = (pitch, [before.0, now]);
            }
            sweep.nearest_m = sweep.nearest_m.min(to_com);
            if dpitch > 0.0 {
                up_path.push(eye);
            } else {
                down_path.push(eye);
            }
            (previous, before) = (eye, (now, inside));
        }
        close(
            solved(view).1,
            -PITCH_LIMIT,
            1.0e-9,
            "the drag came back to where it started",
        );
        // The sixtieth step down lands where the drag started; the other
        // fifty-nine on the eyes of the way up.
        close(
            (previous - start).norm(),
            0.0,
            1.0e-9,
            "the drag did not come back to the eye it started from",
        );
        for (down, up) in down_path.iter().take(59).zip(up_path.iter().rev().skip(1)) {
            close(
                (*down - *up).norm(),
                0.0,
                1.0e-9,
                "a reversed drag left the path",
            );
        }
        sweep
    }

    /// The floor wiring on the REAL body — `flight_camera`'s own sampler
    /// over the real heightfield, no synthetic floor: the seeded starter
    /// craft on Terrin's pad, the background bake waited out the way the
    /// render gate waits for it. Dragged well below the horizon, the eye
    /// rides the floor exactly 0.3 m over the real ground under it, below
    /// the craft's CoM, and the pitch is the dragged one — the floor
    /// rewrites nothing. A reversed drag moves the eye on the very next
    /// frame, along the ground: to the lifted orbit point to the last
    /// digit, and never further than the drag itself.
    #[test]
    fn on_the_real_pad_the_eye_stops_just_over_the_ground() {
        let (mut view, focus) = real_pad_view();
        let framed = over_ground(&mut view, focus);
        assert!(
            framed.eye_r > framed.com_r,
            "by default the craft is framed from above"
        );

        // Dragged well below the horizon: an upward drag, the pitch
        // falling to −0.75 rad.
        view.flight_event(FlightEvent::Orbit {
            dyaw: 0.0,
            dpitch: -1.0,
        });
        session_frame(&mut view);
        let low = over_ground(&mut view, focus);
        let (low_eye, low_pitch, _) = solved(&view);
        close(
            low.eye_r - low.ground_r,
            EYE_GROUND_CLEARANCE_M,
            1.0e-6,
            "the eye over the ground under it",
        );
        assert!(
            low.eye_r < low.com_r,
            "the eye is not below the craft's CoM"
        );
        close(
            low_pitch,
            0.25 - 1.0,
            1.0e-12,
            "the drag reached the pitch whole — the floor rewrote nothing",
        );
        close(
            (low_eye - owed_eye(&mut view, focus)).norm(),
            0.0,
            1.0e-9,
            "the eye is not the asked point lifted onto the real floor",
        );

        // A reversed drag moves the eye on the very next frame: along the
        // ground, away from the point under the craft, to the lifted orbit
        // point — never further than the drag's own step.
        view.flight_event(FlightEvent::Orbit {
            dyaw: 0.0,
            dpitch: 0.02,
        });
        session_frame(&mut view);
        let (risen_eye, now_pitch, distance_m) = solved(&view);
        close(
            (risen_eye - owed_eye(&mut view, focus)).norm(),
            0.0,
            1.0e-9,
            "the eye is not the asked point lifted onto the real floor",
        );
        let risen = over_ground(&mut view, focus);
        let moved = (risen_eye - low_eye).norm();
        let drag_step = distance_m * 2.0 * (0.01_f64).sin();
        eprintln!(
            "real pad: ground {:+.3} m over the datum, craft CoM {:.3} m over the ground; dragged to the floor: \
             eye {:.6} m over the ground, pitch {low_pitch:.4} rad; reversed drag +0.02 rad at {distance_m:.3} m: \
             the eye moved {moved:.4} m along the ground (the drag's own step {drag_step:.4} m), now {:.6} m over the ground",
            low.ground_r - low.datum_r,
            low.com_r - low.ground_r,
            low.eye_r - low.ground_r,
            risen.eye_r - risen.ground_r,
        );
        assert!(
            moved > 0.0 && moved <= drag_step * 1.000_1,
            "the eye moved {moved} m for a drag of {drag_step} m"
        );
        close(
            now_pitch,
            low_pitch + 0.02,
            1.0e-12,
            "the drag reached the pitch whole",
        );
    }

    /// The REAL pad, through the input path: the starter craft dragged
    /// straight down to the pitch limit. Its lifted eye used to end 2.6 m
    /// from the CoM, inside the craft's least distance; it goes round the
    /// craft along the real ground instead — 0.300 m over it, exactly the
    /// least distance from the CoM — and a drag back up out of the zone
    /// and down into it again, 0.01 rad a frame, moves the eye every
    /// frame: by no more than the drag's own step outside the zone and on
    /// the frames that leave and enter it, by no more than the circle's
    /// pace inside it; and back along the same path. The turn it crosses
    /// is the real floor's own: where the floor out along the yaw's line
    /// is first the least distance from the CoM.
    #[test]
    fn on_the_real_pad_the_eye_goes_round_the_starter_craft() {
        let (mut view, focus) = real_pad_view();
        let radius_m = view
            .world
            .resource::<CraftRadii>()
            .0
            .iter()
            .find(|(id, _)| *id == focus)
            .map_or(0.0, |(_, r)| *r);
        let com = view
            .world
            .resource::<DrawnScene>()
            .drawn(focus)
            .expect("the focus is drawn")
            .com;
        let rmin = min_distance(radius_m);
        // The chute's canister on the pod's top node (M4c landing) grew
        // the starter past the 3.062 m the synthetic tests still pin.
        close(
            radius_m,
            3.329_281_447_275,
            1.0e-3,
            "the starter craft's bounding radius",
        );
        let (yaw, distance_m) = {
            let input = view.world.resource::<FlightInput>();
            (input.yaw, input.distance_m())
        };
        let under = UnderCraft::at(&mut view, com, yaw);

        // Straight down: the pitch to its limit in one drag.
        view.flight_event(FlightEvent::Orbit {
            dyaw: 0.0,
            dpitch: -2.0,
        });
        session_frame(&mut view);
        let low = over_ground(&mut view, focus);
        let (low_eye, low_pitch, _) = solved(&view);
        close(
            low_pitch,
            -PITCH_LIMIT,
            1.0e-12,
            "the drag reached the pitch limit",
        );
        close(
            low.eye_r - low.ground_r,
            EYE_GROUND_CLEARANCE_M,
            1.0e-6,
            "the eye over the ground under it",
        );
        close(
            (low_eye - com).norm(),
            rmin,
            1.0e-6,
            "at the pitch limit the eye is on the least distance",
        );

        // The turn on the real floor, bisected here afresh: where the
        // floor out along the yaw's line is first the least distance from
        // the CoM.
        let turn_m = {
            let (mut before, mut after) = (0.0, 12.0);
            for _ in 0..60 {
                let mid = 0.5 * (before + after);
                if (under.floor_at(&mut view, mid) - com).norm() < rmin {
                    before = mid;
                } else {
                    after = mid;
                }
            }
            after
        };
        close(
            (under.floor_at(&mut view, turn_m) - com).norm(),
            rmin,
            1.0e-9,
            "the floor's distance from the CoM at the turn",
        );

        // Back up out of the zone and down into it again.
        let sweep = sweep_the_ground(&mut view, focus, com, rmin, &under, low_eye);
        let (entry_pitch, [before, at]) = sweep.entry;
        assert!(
            before >= turn_m - 1.0e-9 && at <= turn_m + 1.0e-9,
            "the eye entered the zone between {before} and {at} m out, not at the turn {turn_m}"
        );
        let height_m = low.com_r - low.ground_r - EYE_GROUND_CLEARANCE_M;
        let (out, side) = (under.along_to(low_eye), under.along.cross(under.up));
        let over = low_eye - under.centre;
        let bearing = over.dot(side).atan2(over.dot(under.along));
        eprintln!(
            "real pad: radius {radius_m:.3} m, least distance {rmin:.3} m, CoM {:.3} m over the ground, eye {distance_m:.3} m; \
             dragged to the pitch limit: eye {:.6} m over the ground, {:.6} m from the CoM, {:.3} m out along the yaw's line \
             and {:.1}° round from the turn; the turn {turn_m:.3} m along the real ground, where the floor is the least distance from the CoM \
             ({:.3} m on flat ground); back up and down again the eye entered the zone at pitch {entry_pitch:+.4} rad, between {before:.3} and {at:.3} m out; \
             nearest eye over the sweep {:.9} m; outside the zone the eye moved {:.4}..{:.4} m a frame (the drag's own step {:.4} m), \
             inside it at most {:.4} m, {:.2} m per metre of slide (at most {:.2}); the reversed drag retraced every frame to a nanometre",
            low.com_r - low.ground_r,
            low.eye_r - low.ground_r,
            (low_eye - com).norm(),
            out,
            bearing.to_degrees(),
            (rmin * rmin - height_m * height_m).sqrt(),
            sweep.nearest_m,
            sweep.moved_m[0],
            sweep.moved_m[1],
            full_step(distance_m, 0.01),
            sweep.inside_moved_m[0],
            sweep.inside_moved_m[1],
            skirt_plateau()
        );
    }

    /// The bounding radius is the farthest collider corner from the CoM.
    /// A 2 × 2 × 4 m box spanning 1…5 m above a CoM at the origin
    /// reaches sqrt(1 + 1 + 25) = 5.196 m; from a CoM at its own centre
    /// 2.449 m. An empty hull's inverted extents bound nothing.
    #[test]
    fn bounding_radius_reaches_the_farthest_collider_corner() {
        let part = |lo: DVec3, hi: DVec3| {
            let shape = crate::part_mesh::PartShape::Box { lo, hi };
            FlightPart {
                node: 0,
                lo,
                hi,
                transform: csp_phys::PartTransform::IDENTITY,
                shape_key: ShapeKey::of(&shape),
                shape,
                kind: PartKind::Structural,
                model: None,
            }
        };
        let (lo, hi) = (DVec3::new(-1.0, -1.0, 1.0), DVec3::new(1.0, 1.0, 5.0));
        let parts = [
            part(lo, hi),
            part(
                DVec3::new(f64::MAX, f64::MAX, f64::MAX),
                DVec3::new(-f64::MAX, -f64::MAX, -f64::MAX),
            ),
        ];
        close(
            bounding_radius(&parts, DVec3::ZERO),
            27.0_f64.sqrt(),
            1.0e-12,
            "about the origin",
        );
        close(
            bounding_radius(&parts, DVec3::new(0.0, 0.0, 3.0)),
            6.0_f64.sqrt(),
            1.0e-12,
            "about its centre",
        );
        assert_eq!(bounding_radius(&[], DVec3::ZERO), 0.0);
    }
}
