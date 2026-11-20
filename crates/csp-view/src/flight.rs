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
/// Default camera distance (m) — restored by `flight_present` on
/// every focus-id change (C19-F4: a kept zoom framed the new focus at
/// the old vessel's range).
const FLIGHT_DEFAULT_DIST: f64 = 60.0;
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
}

/// [`FlightSyncInput::rails`] mirrored — a standalone resource for the
/// same reason [`LiveVesselCount`] is one (the audio test literal-
/// constructs `FlightSnapshot`, so that struct cannot grow).
#[derive(Resource, Default)]
pub struct RailsStaged(pub Vec<u64>);

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
/// that sees a post-launch vessel carrying the captured craft
/// fingerprint keeps it.
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
        stages: Vec<StageActions>,
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
    /// fingerprint and re-derives. The parts vec is `Arc`-shared
    /// into each staged [`FlightBody`] — the cache entry is
    /// immutable per fingerprint, so the share is honest (C19-F5).
    bubble_parts: BTreeMap<u64, (u64, Arc<Vec<FlightPart>>)>,
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
    /// `vessel_parts`' output over the welded hulls — `Arc`-shared
    /// into the staged bodies (immutable per fingerprint, C19-F5).
    parts: Arc<Vec<FlightPart>>,
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
                        Arc::clone(&entry.1)
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
            // C19-F2: the focus handoff — a vessel that converted to
            // rails while it held (or pins) the focus keeps staging
            // off its conic, so `resolve` still returns it. Entries
            // keep arena order (ascending `VesselId`), so a hinted
            // newest vessel stays `vessels.last()` — the same
            // resolver picks it again. Unhinted rails vessels stage
            // nothing, exactly as before.
            VesselState::OnRails(orbit) => {
                if rails_focus.contains(id)
                    && let Some((body, prop)) =
                        stage_rails(sim, env_ref, *id, vessel, orbit, cache, art)
                {
                    rails.push(id.0);
                    vessels.push(body);
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
) -> Option<(FlightBody, VesselPropulsion)> {
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
    let (parts, com_local, plumes, nodes) = match vessel.craft.as_ref() {
        Some(craft) => {
            let key = craft_fingerprint(craft);
            if cache.entries.get(&id.0).is_none_or(|e| e.key != key) {
                let welded = csp_phys::weld(&craft.graph, &craft.parts).ok()?;
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
            (
                Arc::clone(&entry.parts),
                entry.com_local,
                entry.plumes.clone(),
                craft.graph.nodes.len(),
            )
        }
        // Craft-less (elements-only) vessel: the hull-AABB fallback has
        // no hulls to box — an empty part list still leaves the staged
        // pose/HUD honest (the camera and telemetry keep working).
        None => (Arc::new(Vec::new()), DVec3::ZERO, Vec::new(), 0),
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
        parts: Arc::clone(&entry.parts),
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
) {
    snap.env.clone_from(&staged.env);
    snap.vessels = std::mem::take(&mut staged.vessels);
    live.0 = staged.live_vessels;
    prop.0 = std::mem::take(&mut staged.propulsion);
    rails.0 = std::mem::take(&mut staged.rails);
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
    let proj = perspective_revz(FLIGHT_FOV_Y, aspect, FLIGHT_NEAR_M);
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

/// The R1 cascade arm threshold (terrain-overhaul 3b): the flight
/// extract solves cascades only while the sun clears the chase
/// focus's local horizon by more than this (sun·up). 0.06 ≈ 3.4°
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
/// math over the SOLVED chase camera + the staged `FlightEnv` sun
/// (§2.5: RenderExtract owns presentation derivation; nothing here
/// reads the sim). The camera basis is recovered from `view_proj`
/// exactly like `sky_view_rot`: the reverse-Z projection's last row
/// is `[0,0,−1,0]`, so `view_proj`'s row 3 is `−z_out` — the forward
/// — and the focus recovered as `eye − z_out·distance` (the same
/// reconstruction `vessel_shadow` uses) supplies the local-level up.
/// Returns the all-zero DISARMED row when the sun sits at or below
/// the arm threshold; the solve itself is `csp_render::shadow::
/// solve_cascades` (f64, texel-snapped — see its docs).
fn cascade_uniforms(
    cam: &MapCameraF64,
    env: &FlightEnv,
    viewport_px: (f64, f64),
) -> csp_render::shadow::ShadowUniforms {
    let z_out = DVec3::new(
        -cam.view_proj[3][0],
        -cam.view_proj[3][1],
        -cam.view_proj[3][2],
    );
    let focus_rel = (cam.eye - env.body_center) - z_out * cam.distance_m;
    let fl = focus_rel.norm();
    let up = if fl > env.radius_m * 1.0e-6 {
        focus_rel / fl
    } else {
        DVec3::Z
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
    for p in focus.parts.iter() {
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
    mut input: ResMut<FlightInput>,
    snap: Res<FlightSnapshot>,
    live: Res<LiveVesselCount>,
    rails: Res<RailsStaged>,
    plans: Res<StagePlans>,
    prop: Res<VesselPropulsionStaged>,
    terrain_cfg: Res<TerrainConfig>,
    mut terrain_cache: ResMut<TerrainCache>,
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
    // C19-F4: reframe on a focus-id change — a kept zoom would frame
    // the NEW vessel at the OLD one's distance (zoom to 3 m on a
    // canister, then a fragment or second launch inherits a 3 m
    // chase). Orientation keeps (the player's orbit drag is session
    // intent); only the range resets to the default frame.
    if sfx.prev.map(|p| p.focus) != focus.map(|f| f.id) {
        input.distance_m = FLIGHT_DEFAULT_DIST;
    }
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
    for p in b.parts.iter() {
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
    p: &FlightPart,
    tris: &[crate::part_mesh::ModelTri],
    vp: &[[f64; 4]; 4],
    eye: DVec3,
) {
    for t in tris.iter() {
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
    /// Kitbash silhouettes: `(shape bits, kind, debris)` → tris.
    smooth: BTreeMap<(ShapeKey, u8, bool), Arc<Vec<BakedTri>>>,
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
        kind: PartKind,
        debris: bool,
    ) -> Arc<Vec<BakedTri>> {
        let key = (ShapeKey::of(shape), kind_tag(kind), debris);
        if let Some(t) = self.smooth.get(&key) {
            return Arc::clone(t);
        }
        if self.smooth.len() >= 256 {
            self.smooth.clear();
        }
        let baked = Arc::new(bake_smooth(shape, kind, debris));
        self.smooth.insert(key, Arc::clone(&baked));
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
        && let Some(env) = &snap.env
    {
        let tp = crate::terrain::build_packet(
            &terrain_cfg,
            &mut terrain_cache,
            env,
            &cam.0,
            input.viewport_px,
            clock.0.ticks() as f64 / csp_core::TICK_HZ as f64,
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
            cascade_uniforms(&cam.0, env, input.viewport_px)
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
            packet.far_vertices.push(unlit_v(pos, color));
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
    for b in &snap.vessels {
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
                emit_model_part(v, b, p, &tris, vp, eye);
                continue;
            }
            let baked = geom.smooth_tris(&p.shape, p.kind, b.debris);
            for t in baked.iter() {
                // A tri with any corner behind the eye plane drops
                // whole — the same all-or-nothing rule the VAB packet
                // uses — rather than emitting a corrupted fan.
                let world = [t.a, t.b, t.c].map(|q| b.pos + b.rot.rotate(p.transform.apply(q)));
                let [Some(c0), Some(c1), Some(c2)] = world.map(|q| project_into(vp, eye, q)) else {
                    continue;
                };
                for (k, nrm) in [t.na, t.nb, t.nc].iter().enumerate() {
                    let n_w = b.rot.rotate(p.transform.apply_dir(*nrm));
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
        // red rim (`emit_sheath` carries the details). Stagnation side
        // = the surface-relative velocity direction (the sim's own
        // `v_rel` convention) — its magnitude is also the speed the
        // C19-F1 gate reads, so the sheath can't light on a slow
        // ascent pass however dense the air.
        let v_rel = snap
            .env
            .as_ref()
            .map_or(b.vel, |e| b.vel - e.body_spin.cross(b.com - e.body_center));
        let plasma_i = plasma_intensity(b.plasma_w_m2, v_rel.norm());
        if plasma_i > 0.0 {
            let flow = if v_rel.norm_squared() > 1.0 {
                v_rel / v_rel.norm()
            } else {
                DVec3::ZERO
            };
            emit_sheath(v, vp, eye, b, flow, plasma_i, &mut geom.sheath_tris);
        }

        // §3.7 ground-contact dust: a translucent tan ring hugging
        // the surface under the vessel's lowest hull vertex.
        if b.contact
            && let Some(env) = snap.env.as_ref()
        {
            let mut low = b.com;
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
        world.insert_resource(TerrainConfig::default());
        world.insert_resource(TerrainCache::default());
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
        });
        world.insert_resource(FlightSnapshot::default());
        world.insert_resource(LiveVesselCount::default());
        world.insert_resource(VesselPropulsionStaged::default());
        world.insert_resource(RailsStaged::default());
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
        assert!(
            world.resource::<FlightSyncInput>().vessels.is_empty()
                && world.resource::<FlightSyncInput>().propulsion.is_empty()
                && world.resource::<FlightSyncInput>().rails.is_empty(),
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
        let t = g.smooth_tris(&box_a, PartKind::Tank, false);
        assert_eq!(g.smooth_bakes, 1);
        assert!(!t.is_empty());
        g.smooth_tris(&box_a, PartKind::Tank, false);
        assert_eq!(g.smooth_bakes, 1, "an identical key must hit");
        g.smooth_tris(&box_a, PartKind::Tank, true);
        assert_eq!(g.smooth_bakes, 2, "debris folds into the key");
        g.smooth_tris(&box_a, PartKind::Engine, false);
        assert_eq!(g.smooth_bakes, 3, "kind folds into the key (livery)");
        g.smooth_tris(&box_b, PartKind::Tank, false);
        assert_eq!(g.smooth_bakes, 4, "shape bits fold into the key");
        // The debris variant's colors differ from the clean one.
        let clean = g.smooth_tris(&box_a, PartKind::Tank, false);
        let debris = g.smooth_tris(&box_a, PartKind::Tank, true);
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
}
