//! The M1 map view (PLAN §5.1 R0, M1 exit row): the system map's
//! presentation side — mode state, camera, orbit polylines, markers, and
//! label data.
//!
//! # Data flow (§2.5)
//!
//! ```text
//! csp-app ──MapKeyEvent──▶ MapInputEvents ──(Input: apply_map_input)──▶ MapInput
//! csp-sim ──vessels/tick──▶ MapSyncInput ──(Sync: sync_map)──▶ MapSnapshot
//! OrbitModelRes ──(Presentation: map_present)──▶ MapGeometry + MapCache
//!                                              + MapCameraF64 + MapLabels
//! (RenderExtract: extract_map) ──▶ MapFrame(csp_render::map::MapPacket)
//! ```
//!
//! `csp-render` never sees `csp-sim` (§2.1): the packet is plain f32
//! vertex data. All f64 work — the camera-relative subtraction, orbit
//! sampling, projection — happens HERE before the f32 cast (§2.3: the
//! GPU receives only camera-relative f32).
//!
//! # Precision (§2.3)
//!
//! The home system spans ~4×10¹⁰ m; f32 has ~1 m resolution at 10⁷ m.
//! Every world position is therefore subtracted from the f64 eye
//! *before* the f32 cast inside [`project`] — vertices reaching
//! `csp-render` are already clip-space NDC.
//!
//! # Polyline cadence
//!
//! Body elements are epoch-anchored constants (§2.2's partition-
//! independence note) — their polylines are sampled once, lazily, and
//! cached in [`MapCache`]. Vessel polylines are re-sampled only when the
//! `OrbitState` changes (spawn, §2.6.6 SOI re-base) — sampled lines are
//! kept parent-centered and offset by the parent's *current* position at
//! extract, so the line rides its parent like KSP's map does.

use std::collections::BTreeMap;

use bevy_ecs::prelude::*;
use csp_core::{DVec3, SimError, SimTime, TICK_HZ};
use csp_data::{BodyId, KeplerElements};
use csp_render::map::{MapMesh, MapPacket, MapVertex};
use csp_sim::orbit::{OrbitModel, OrbitState, PatchedConics, SoiDirection};
use csp_sim::{ManeuverNode, ManeuverStatus, SimCommand, VesselId, World as SimWorld};

// ---------------------------------------------------------------------------
// Tunables (presentation constants — §8.3 owns sim tuning; these are view
// geometry, not balance).
// ---------------------------------------------------------------------------

/// Segments per sampled orbit line.
pub const ORBIT_LINE_SAMPLES: usize = 128;
/// Half-width of the quad-stripped orbit lines.
const LINE_HALF_WIDTH_PX: f64 = 0.85;
/// Width multiplier for the focused body's/vessel's orbit line.
const FOCUS_LINE_WIDTH: f64 = 1.6;
/// Screen-space marker half-sizes (the disc — the glow extends ~3×).
const BODY_MARKER_PX: f64 = 4.5;
/// Vessels draw smaller than bodies.
const VESSEL_MARKER_PX: f64 = 3.0;
/// The star's marker reads bigger — it is the system's anchor point.
const STAR_MARKER_SCALE: f64 = 1.7;
/// Focused-target ring half-size (a ring reads "this is the camera's
/// reference" better than a color swap on a 5 px disc).
const FOCUS_RING_PX: f64 = 9.0;
/// Apsis markers on the focused vessel's orbit (Ap/Pe ring half-size).
const APSIS_MARKER_PX: f64 = 4.5;
/// Pick radius for click-to-focus — screen px around a marker's anchor.
/// `pub(crate)`: the `UiBuild` hit-test in `crate` uses the same bound.
pub(crate) const PICK_RADIUS_PX: f64 = 14.0;
/// Starfield: how many world-anchored backdrop stars the extract emits.
const STAR_COUNT: usize = 700;
/// Distance (m) the backdrop stars sit at — effectively infinite:
/// far enough that translation never parallaxes them, finite enough
/// that the f64 clip solve stays conditioned.
const STAR_DISTANCE_M: f64 = 1.0e28;
/// Camera vertical field of view (rad).
const MAP_FOV_Y: f64 = std::f64::consts::FRAC_PI_3;
/// Clip-space w reject bound — points this close to or behind the eye
/// plane are dropped (they cannot be projected sanely).
const CLIP_EPS: f64 = 1e-9;
/// Camera distance limits: 100 km up (inside Terrin's 84 Mm SOI) to
/// 2×10¹¹ m out (5× the system's 4×10¹⁰ m extent, §2.3 scale check).
const MIN_DISTANCE_M: f64 = 1.0e5;
const MAX_DISTANCE_M: f64 = 2.0e11;
/// Pitch clamp, ~89°.
const PITCH_LIMIT: f64 = 1.55;
/// Fraction of the hyperbolic asymptote anomaly drawn for open orbits —
/// keeps the polyline finite while covering the SOI-crossing leg.
const OPEN_ORBIT_NU_FILL: f64 = 0.92;
/// `|e − 1|` at/below which an orbit is drawn as parabolic — mirrors
/// `csp_sim::orbit::E_PARABOLIC_TOL` (`pub(crate)` there, so mirrored
/// literally). The sim encodes parabolas as `a := −p < 0`; the sampler
/// must classify the same band identically or a parabolic-encoded
/// orbit with `e` just under 1.0 would take the closed branch, fail
/// its `a > 0` check, and draw no line at all. `pub(crate)` so the
/// flight HUD's osculating readouts gate the same sliver.
pub(crate) const E_PARABOLIC_TOL: f64 = 1e-9;
/// Color for all vessel markers/lines at M1 (bodies get per-index colors).
const VESSEL_COLOR: [f32; 4] = [1.0, 0.55, 0.15, 1.0];
/// M5 maneuver-node color: the node marker and the predicted post-burn
/// conic read as "planned, not flown" — green against the flown
/// orange.
const NODE_COLOR: [f32; 4] = [0.35, 0.95, 0.45, 1.0];
/// Star marker/line color (parent-less body).
const STAR_COLOR: [f32; 4] = [1.0, 0.85, 0.35, 1.0];
/// Surface-site marker/label color — a cool cyan so a pad or recovery
/// field reads distinct from bodies, vessels, and maneuver nodes.
const SITE_COLOR: [f32; 4] = [0.45, 0.85, 0.90, 1.0];
/// W2.4: the staged transfer plan's conic + its encounter marker —
/// a violet that reads "planned route" against the flown orange and
/// the node green.
const PLAN_COLOR: [f32; 4] = [0.68, 0.50, 1.0, 1.0];
/// W2.4: an `Enter` encounter marker (SOI entry is the leg the player
/// aimed for — it reads like a node).
const ENCOUNTER_ENTER_COLOR: [f32; 4] = [0.45, 0.95, 0.65, 1.0];
/// W2.4: an `Exit` encounter marker — the neutral vessel tint.
const ENCOUNTER_EXIT_COLOR: [f32; 4] = [1.0, 0.70, 0.35, 1.0];
/// W2.4: an approach the model reports as impacting (`pe < radius`).
const ENCOUNTER_IMPACT_COLOR: [f32; 4] = [1.0, 0.35, 0.30, 1.0];
/// Grace ticks past an armed alarm's tick before it is reported as
/// lapsed rather than fired — long enough to cover the enqueue →
/// drain → `clock+1` clamp chain (a `SetAlarm` can legally fire one
/// tick *after* the requested tick), short enough that a rejected
/// command's row stops pretending it is armed within seconds.
const ALARM_LAPSE_TICKS: u64 = 2 * TICK_HZ;
/// The alarm book's cap — oldest dismissed-eligible rows drop first
/// so a script/alarm storm cannot grow the panel unboundedly.
const ALARM_BOOK_CAP: usize = 32;
/// Segments per drawn SOI ring — matches the orbit-line density.
const SOI_RING_SAMPLES: usize = 64;
/// Assigned round-robin to non-star bodies.
const BODY_PALETTE: [[f32; 4]; 5] = [
    [0.35, 0.65, 1.0, 1.0],
    [0.75, 0.75, 0.75, 1.0],
    [0.95, 0.55, 0.35, 1.0],
    [0.55, 0.90, 0.55, 1.0],
    [0.80, 0.60, 1.0, 1.0],
];

// ---------------------------------------------------------------------------
// Input / mode state
// ---------------------------------------------------------------------------

/// Flight view vs system map — presentation session state (§2.2 C),
/// never sim state, never logged, never hashed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MapMode {
    /// The existing flight view (nothing drawn at M1 besides UI).
    #[default]
    Flight,
    /// The system map.
    Map,
}

/// One camera/mode action the app layer translates winit input into.
/// Queued through [`crate::View::map_event`] and applied by the `Input`
/// schedule's [`apply_map_input`] system (§2.5 #1: camera input lives in
/// the Input stage, not in winit handlers).
#[derive(Clone, Copy, Debug)]
pub enum MapKeyEvent {
    /// `M` — flip between [`MapMode::Flight`] and [`MapMode::Map`].
    ToggleMode,
    /// `Tab` — cycle the camera's focus body (resets pan + distance).
    CycleFocus,
    /// Click-to-focus a body by [`BodyTable`] index (resets pan +
    /// re-frames; clicking the already-focused body recenters it).
    FocusBody(usize),
    /// Click-to-focus a vessel by id — the same anchor
    /// [`MapInput::focus_vessel`] uses, so a re-keyed snapshot cannot
    /// retarget the click.
    FocusVessel(VesselId),
    /// Left-drag orbit — radians to add to yaw/pitch.
    Orbit { dyaw: f64, dpitch: f64 },
    /// Scroll zoom — multiplicative factor on `distance_m`. With a
    /// live [`MapInput::cursor_px`] the zoom aims at the point under
    /// the cursor (KSP-style), so pointing and scrolling places the
    /// camera anywhere in the system.
    Zoom { factor: f64 },
    /// Free pan — screen-space pixels (WASD/arrows, or middle/right
    /// drag converted to px upstream). The look-at point detaches from
    /// the focus body freely; there is no clamp.
    Pan { dx_px: f64, dy_px: f64 },
}

/// The per-body constants the map needs — built once from the orbit
/// model at `View` construction (bodies never mutate at M1). Vector
/// order IS the focus-cycle order and matches `orbits.bodies()`'s
/// `BTreeMap` (§2.2 deterministic body order).
#[derive(Resource)]
pub struct BodyTable {
    /// All bodies, `BTreeMap` order.
    pub bodies: Vec<MapBody>,
    /// `BodyId` → index into [`Self::bodies`].
    index: BTreeMap<BodyId, usize>,
    /// Focus index the map opens on — the root body (the star).
    default_focus: usize,
}

/// One body's static map entry.
pub struct MapBody {
    /// The data-id; doubles as the map label at M1 (loc keys resolve in
    /// a later milestone).
    pub id: BodyId,
    /// Parent body — `None` for the star.
    pub parent: Option<BodyId>,
    /// SOI radius (m) — sets the default focus distance.
    pub soi_m: Option<f64>,
    /// Marker/line RGBA.
    pub color: [f32; 4],
}

impl BodyTable {
    /// Snapshot the constant parts of the orbit model.
    pub(crate) fn from_model(model: &PatchedConics) -> Self {
        let mut bodies = Vec::new();
        let mut index = BTreeMap::new();
        let mut palette = 0usize;
        let mut default_focus = 0usize;
        for (id, body) in model.bodies() {
            let color = if body.parent.is_none() {
                default_focus = bodies.len();
                STAR_COLOR
            } else {
                let c = BODY_PALETTE[palette % BODY_PALETTE.len()];
                palette += 1;
                c
            };
            index.insert(id.clone(), bodies.len());
            bodies.push(MapBody {
                id: id.clone(),
                parent: body.parent.clone(),
                soi_m: body.soi_m,
                color,
            });
        }
        Self {
            bodies,
            index,
            default_focus,
        }
    }

    /// Index of a body, if known.
    pub fn index_of(&self, id: &BodyId) -> Option<usize> {
        self.index.get(id).copied()
    }

    /// The focus body's name — the data id at M1.
    pub fn focus_name(&self, focus: usize) -> &str {
        self.bodies.get(focus).map(|b| b.id.as_str()).unwrap_or("—")
    }
}

/// Map camera + mode — session state in the presentation world.
///
/// `pan_offset` is added to the focus body's position: panning detaches
/// the camera from exact body lock; cycling focus resets it. All angles
/// are radians, all distances meters — f64 throughout (§2.3).
#[derive(Resource, Debug)]
pub struct MapInput {
    /// Flight vs map.
    pub mode: MapMode,
    /// Index into [`BodyTable::bodies`] — or `bodies.len() + i` into
    /// [`MapSnapshot::vessels`] when the focus cycle lands on a vessel.
    pub focus: usize,
    /// The vessel `focus` addresses, by id. `MapSnapshot::vessels` is
    /// re-keyed every frame — a removal shifts every later index — so
    /// an index alone cannot hold a vessel across frames: `sync_map`
    /// re-anchors `focus` at the id's current slot, and drops the
    /// vessel focus back to the default body when the id leaves the
    /// snapshot (the stale-index → camera-at-world-origin defect).
    /// `None` whenever `focus` addresses the body half.
    pub(crate) focus_vessel: Option<VesselId>,
    /// Free offset added to the focus position (pan).
    pub pan_offset: DVec3,
    /// Camera yaw about the world's +z (ecliptic normal).
    pub yaw_rad: f64,
    /// Camera pitch — 0 = looking straight down on the ecliptic.
    pub pitch_rad: f64,
    /// Eye distance from the focus point (m).
    pub distance_m: f64,
    /// Current viewport in physical pixels — written by csp-app each
    /// frame via [`crate::View::set_map_viewport`].
    pub viewport_px: (f64, f64),
    /// Cursor position in physical pixels — staged each map frame by
    /// `UiBuild` from egui's pointer state. Feeds scroll-zoom's
    /// zoom-toward-cursor offset; `None` when the pointer is off-window
    /// (zoom then homes on the focus point as before).
    pub cursor_px: Option<(f64, f64)>,
}

impl MapInput {
    /// Fresh session state for a table — focused on the system root.
    fn new(table: &BodyTable) -> Self {
        Self {
            mode: MapMode::Flight,
            focus: table.default_focus,
            focus_vessel: None,
            pan_offset: DVec3::ZERO,
            yaw_rad: 0.0,
            pitch_rad: 0.6,
            distance_m: default_distance(table, table.default_focus),
            viewport_px: (1280.0, 720.0),
            cursor_px: None,
        }
    }

    /// Apply one event — the `Input` schedule's work.
    fn apply(&mut self, ev: MapKeyEvent, table: &BodyTable, snap: &MapSnapshot) {
        match ev {
            MapKeyEvent::ToggleMode => {
                self.mode = match self.mode {
                    MapMode::Flight => MapMode::Map,
                    MapMode::Map => MapMode::Flight,
                };
                if self.mode == MapMode::Map {
                    self.refocus(table, snap);
                }
            }
            MapKeyEvent::CycleFocus => {
                // Focus cycles bodies THEN vessels — a vessel index is
                // `bodies.len() + i` into `snap.vessels` order. The
                // vessel half is recorded by id so `sync_map` can hold
                // the focus across the snapshot's re-keying.
                if self.mode == MapMode::Map {
                    let n = table.bodies.len() + snap.vessels.len();
                    if n > 0 {
                        self.focus = (self.focus + 1) % n;
                        self.focus_vessel = self
                            .vessel_focus(table)
                            .and_then(|i| snap.vessels.get(i))
                            .map(|(id, ..)| *id);
                        self.refocus(table, snap);
                    }
                }
            }
            MapKeyEvent::FocusBody(i) => {
                // Click-to-focus: valid in map mode only, and `refocus`
                // on the *current* body is the recenter gesture (pan
                // resets, distance re-frames) rather than a no-op.
                if self.mode == MapMode::Map && i < table.bodies.len() {
                    self.focus = i;
                    self.focus_vessel = None;
                    self.refocus(table, snap);
                }
            }
            MapKeyEvent::FocusVessel(id) => {
                if self.mode == MapMode::Map
                    && let Some(i) = snap.vessels.iter().position(|(vid, ..)| *vid == id)
                {
                    self.focus = table.bodies.len() + i;
                    self.focus_vessel = Some(id);
                    self.refocus(table, snap);
                }
            }
            MapKeyEvent::Orbit { dyaw, dpitch } => {
                self.yaw_rad += dyaw;
                self.pitch_rad = (self.pitch_rad + dpitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
            }
            MapKeyEvent::Zoom { factor } => {
                let old = self.distance_m;
                let new = (old * factor.max(1e-9)).clamp(MIN_DISTANCE_M, MAX_DISTANCE_M);
                // Zoom-toward-cursor: the world point under the cursor
                // stays under the cursor, so the look-at point shifts
                // toward it by the fraction the distance actually
                // changed. Point + scroll therefore places the camera
                // anywhere — the cursor's focus-plane offset is right/
                // up vector lengths at the OLD distance, and the
                // residual `off·(1 − new/old)` is what the new, tighter
                // frame leaves un-covered.
                if let Some((cx, cy)) = self.cursor_px {
                    let (w, h) = self.viewport_px;
                    if w > 1.0 && h > 1.0 {
                        let nx = 2.0 * cx / w - 1.0;
                        let ny = 1.0 - 2.0 * cy / h;
                        let half_h = old * f64::tan(MAP_FOV_Y * 0.5);
                        let half_w = half_h * (w / h);
                        let r = rot3(self.yaw_rad, self.pitch_rad);
                        let off = mat3_vec(&r, DVec3::new(nx * half_w, ny * half_h, 0.0));
                        self.pan_offset = self.pan_offset + off * (1.0 - new / old);
                    }
                }
                self.distance_m = new;
            }
            MapKeyEvent::Pan { dx_px, dy_px } => {
                // world units per screen px at the focus distance;
                // screen y is down, view y is up — flip dy.
                let units_per_px =
                    2.0 * self.distance_m * f64::tan(MAP_FOV_Y * 0.5) / self.viewport_px.1.max(1.0);
                let r = rot3(self.yaw_rad, self.pitch_rad);
                self.pan_offset = self.pan_offset
                    + mat3_vec(
                        &r,
                        DVec3::new(dx_px * units_per_px, -dy_px * units_per_px, 0.0),
                    );
            }
        }
    }

    /// Re-lock the camera on the focus target — resets pan and
    /// re-derives a framing distance: `3·|a|` of the focused vessel's
    /// conic for the vessel half of the cycle, `5·SOI` for a body
    /// (the whole-system fallback for the star). `pub(crate)` so
    /// [`crate::View::set_mode`] reuses this ONE framing — its own
    /// body-only copy used to snap a vessel focus to the 6e10 m
    /// system distance on every mode flip (P09-F1).
    pub(crate) fn refocus(&mut self, table: &BodyTable, snap: &MapSnapshot) {
        self.pan_offset = DVec3::ZERO;
        self.distance_m = match self.vessel_focus(table).and_then(|i| snap.vessels.get(i)) {
            // Vessel focus: frame ~3 semi-major axes out (a low orbit
            // clamps to the minimum — still far tighter than a body).
            Some((_, o, ..)) => {
                (3.0 * o.elements.semi_major_axis_m.abs()).clamp(MIN_DISTANCE_M, MAX_DISTANCE_M)
            }
            None => default_distance(table, self.focus),
        };
    }

    /// `Some(vessel index)` when `focus` addresses the vessel half of
    /// the focus cycle (indices `>= bodies.len()`).
    pub(crate) fn vessel_focus(&self, table: &BodyTable) -> Option<usize> {
        self.focus.checked_sub(table.bodies.len())
    }
}

/// Framing distance for a focus body: ~5 SOI radii out, or a whole-
/// system view when the body has no SOI (the star).
fn default_distance(table: &BodyTable, focus: usize) -> f64 {
    table
        .bodies
        .get(focus)
        .and_then(|b| b.soi_m)
        .map(|soi| (5.0 * soi).clamp(MIN_DISTANCE_M, MAX_DISTANCE_M))
        .unwrap_or(6.0e10)
}

/// Staged key events — the plumbing entry point for [`View::map_event`]
/// (crate::View::map_event). Drained each frame by [`apply_map_input`].
#[derive(Resource, Default)]
pub(crate) struct MapInputEvents {
    /// FIFO of events since the last `Input` run.
    pub pending: Vec<MapKeyEvent>,
}

/// `Input` schedule system (§2.5 #1 — "camera input"): drain staged map
/// events into [`MapInput`].
pub(crate) fn apply_map_input(
    mut events: ResMut<MapInputEvents>,
    mut input: ResMut<MapInput>,
    table: Res<BodyTable>,
    snap: Res<MapSnapshot>,
) {
    for ev in events.pending.drain(..) {
        input.apply(ev, &table, &snap);
    }
}

// ---------------------------------------------------------------------------
// Sim → presentation mirror
// ---------------------------------------------------------------------------

/// Post-step staging for the map's sim reads — written by
/// [`crate::View::end_frame`] every frame (vessel `OrbitState` clones
/// are cheap and epoch-anchored). Mirrors `SyncInput`'s role: the mirror
/// *write* into [`MapSnapshot`] is the `sync_map` system (Appendix A
/// item 1's "writes are systems").
///
/// Positions are NOT staged: they derive from `OrbitModelRes` at the
/// snapshot tick inside `map_present` — one place does all orbit
/// evaluation.
/// One map-side vessel row — `(id, orbit state, armed node,
/// on-rails flag, burn estimate)`. W2.4's burn estimate pushed the
/// tuple to five fields; the alias keeps the snapshot legible.
pub type VesselMapRow = (
    VesselId,
    OrbitState,
    Option<ManeuverNode>,
    bool,
    Option<f64>,
);

#[derive(Resource, Default)]
pub(crate) struct MapSyncInput {
    /// The sim clock the vessel states were read at.
    pub tick: SimTime,
    /// Every vessel's `(id, orbit, scheduled node, on_rails, burn_s)` —
    /// ascending-id order. `on_rails` marks the residence the entry's
    /// conic came from: a stored conic for `OnRails`, a freshly fitted
    /// osculating conic for `InBubble` — the distinction the maneuver
    /// panel needs (a `SetManeuver` applies only to an on-rails
    /// vessel; P09-F2). `burn_s` is the node's estimated burn duration
    /// (Δv ÷ full-thrust accel over the stored craft) — `None` for a
    /// craft-less or thrust-less vessel or no armed node.
    pub vessels: Vec<VesselMapRow>,
}

/// The Sync-written map mirror — what Presentation and UiBuild read.
#[derive(Resource, Default)]
pub struct MapSnapshot {
    /// The sim tick this snapshot was taken at.
    pub tick: SimTime,
    /// See [`MapSyncInput::vessels`].
    pub vessels: Vec<VesselMapRow>,
}

/// `Sync` schedule system: copy the staged map input into the mirror,
/// then repair the session's vessel focus against the re-keyed list.
/// `focus_vessel` is the focus index's anchor: an id still present
/// re-anchors `focus` at its (possibly shifted) slot; an id that left
/// the snapshot — the vessel landed, unstaged, or was removed — drops
/// the vessel focus back to the default body. Without this the stale
/// index addressed past `vessels.len()`: `vessel_focus` still returned
/// `Some(i)` while `vessel_pos.get(i)` yielded `None`, snapping the
/// camera to the world origin and the HUD's focus name to "—".
pub(crate) fn sync_map(
    input: Res<MapSyncInput>,
    mut snap: ResMut<MapSnapshot>,
    mut map_input: ResMut<MapInput>,
    table: Res<BodyTable>,
) {
    snap.tick = input.tick;
    snap.vessels.clone_from(&input.vessels);
    if let Some(id) = map_input.focus_vessel {
        match snap.vessels.iter().position(|(vid, ..)| *vid == id) {
            Some(i) => map_input.focus = table.bodies.len() + i,
            None => {
                map_input.focus = table.default_focus;
                map_input.focus_vessel = None;
                map_input.refocus(&table, &snap);
            }
        }
    }
}

/// The orbit model as a presentation resource — a clone taken at `View`
/// construction. Legitimate in the ECS world: the model is data-derived
/// construction config (§7.3's re-derived-on-load class), never mutates,
/// never hashed — it is NOT authoritative sim state.
#[derive(Resource, Clone)]
pub(crate) struct OrbitModelRes(pub PatchedConics);

/// The derived map scene: system-frame positions evaluated from the
/// snapshot plus the polyline cache (see module docs for the cadence
/// contract). Everything here is recomputed/cached from sim mirrors —
/// presentation data, never authoritative.
#[derive(Resource, Default)]
pub struct MapScene {
    /// System-frame position per [`BodyTable`] index (`None` = solve
    /// failed this frame — the marker/line are skipped, not defaulted).
    pub body_pos: Vec<Option<DVec3>>,
    /// System-frame positions in [`MapSnapshot::vessels`] order.
    pub vessel_pos: Vec<Option<DVec3>>,
    /// Per-body orbit line, sampled once (body elements never change).
    /// `None` = the body has no orbit (the star) or sampling failed.
    body_lines: Vec<Option<Vec<DVec3>>>,
    /// Vessel lines keyed by an epsilon-quantized digest of the
    /// `OrbitState` they were sampled from — a `§2.6.6` re-base or a
    /// new spawn replaces the elements past the quantization steps,
    /// which is the invalidation signal. The polyline is a whole-
    /// conic silhouette: an InBubble vessel's per-frame osculating
    /// refit moves it far less than a texel, so keying on the exact
    /// state (whose `epoch` advances every frame) made a hit
    /// structurally impossible — ~65 refits × 129 `state_at` solves
    /// per map frame at the debris cap (C12-F2).
    vessel_lines: BTreeMap<VesselId, VesselLine>,
    /// M5 node previews — see [`NodePreview`].
    node_lines: BTreeMap<VesselId, NodePreview>,
    /// System-frame Ap/Pe points on the *focused* orbit (vessel or
    /// body) — recomputed each map frame in `map_present`, drawn as
    /// ring markers by `extract_map`. Empty when the focus has no
    /// conic (the star) or no vessel is focused and the focused body
    /// is the root.
    pub(crate) focus_apsis: Vec<DVec3>,
    /// System-frame surface-site anchors (the launch complex and the
    /// recovery sites on the dominant body) — solved in `map_present`
    /// only while the camera is focused on the site body inside a few
    /// radii, drawn as ring markers by `extract_map`.
    pub(crate) site_markers: Vec<DVec3>,
    /// W2.4: predicted SOI encounter anchors — `(system-frame pos,
    /// color)` pairs drawn as ring markers by `extract_map`. See
    /// [`refresh_planning`].
    pub(crate) encounter_marks: Vec<(DVec3, [f32; 4])>,
    /// W2.4: SOI boundary rings — `(center, radius_m, color)` circles
    /// in the ecliptic plane, regenerated per frame from the staged
    /// ring set (the focused body, planned-transfer endpoints, and
    /// `Enter`-encounter targets).
    pub(crate) soi_rings: Vec<(DVec3, f64, [f32; 4])>,
    /// W2.4: the staged transfer plan's conic — `(parent, polyline)`,
    /// parent-centred like every sampled line. `None` while no plan
    /// is staged (or its conic is unsampleable).
    plan_line: Option<(BodyId, Vec<DVec3>)>,
    /// The staged plan's invalidation key — its departure tick. A
    /// re-planned transfer re-samples; an unchanged one rides.
    plan_line_key: Option<u64>,
    /// W2.4: Ap/Pe points on the focused vessel's predicted post-burn
    /// conic — drawn in [`NODE_COLOR`] rings, distinct from the flown
    /// conic's apsides.
    pub(crate) pred_apsis: Vec<DVec3>,
    /// §2.5's `presentation_tick`: the non-authoritative f64 tick the
    /// map evaluates positions at — advanced by wall-dt × the stored
    /// warp level and clamped to `[prev_snap_tick, snap.tick]` so it
    /// may lie between the pre- and post-advance clock but never
    /// extrapolates past what the sim produced.
    present_tick: f64,
    /// The last frame's snapshot tick — `present_tick`'s lower clamp.
    prev_snap_tick: u64,
    /// `false` until the first `map_present` anchors `present_tick`.
    present_init: bool,
}

/// One vessel's cached orbit line: the quantized staleness key, the
/// `OrbitState` it was sampled from (extract needs the parent id for
/// the ride-along offset), and the sampled parent-centred polyline.
struct VesselLine {
    /// See [`orbit_line_key`].
    key: OrbitLineKey,
    /// The state the line was sampled from — `state.parent` rides the
    /// extract's per-parent offset.
    state: OrbitState,
    /// The sampled polyline (`None` = unsampleable).
    line: Option<Vec<DVec3>>,
}

/// Epsilon-quantized digest of an `OrbitState` for the polyline
/// caches. The sampled line is a whole-conic silhouette — for
/// identical *shape* the only epoch dependence is the `t ≥ 0` clip
/// on open orbits — so per-frame refit noise must not invalidate:
/// `a` quantizes relatively (orbits span 10⁵–10¹¹ m), `e` and the
/// angles absolutely, and `epoch` in tick buckets coarse enough that
/// an in-bubble fleet resamples on the order of seconds, not frames.
/// Quantization (not a tolerance compare) is what bounds staleness —
/// drift crossing a step edge resamples once and re-anchors.
#[derive(Clone, PartialEq)]
struct OrbitLineKey {
    /// Central body the elements orbit.
    parent: BodyId,
    /// `semi_major_axis_m` / (`A_REL`·|a| + 1 m), rounded.
    a: i64,
    /// `eccentricity` / 1e-5 — the e≈1 classification sliver stays
    /// narrow; a crossing resamples when the conic family flips.
    e: i64,
    /// `inclination_rad` / 1e-3.
    inc: i64,
    /// `ascending_node_rad` / 1e-3.
    node: i64,
    /// `arg_periapsis_rad` / 1e-3.
    argp: i64,
    /// `mean_anomaly_epoch_rad` / 1e-3 — only the `t < 0` clip reads
    /// it, so it quantizes as coarsely as the other angles.
    m0: i64,
    /// `epoch.ticks()` / [`EPOCH_STEP_TICKS`].
    epoch: i64,
}

/// Epoch bucket width for [`OrbitLineKey::epoch`] (ticks — 4 s at
/// 64 Hz). Bounds how long the `t < 0` clip can lag the true epoch
/// while still letting a free-falling fleet's lines persist.
const EPOCH_STEP_TICKS: u64 = 256;

/// The quantized invalidation key for a vessel `OrbitState` — see
/// [`OrbitLineKey`]. NaN/∞ fields saturate to a fixed bucket (the
/// state draws nothing either way — the key only orders refresh).
fn orbit_line_key(state: &OrbitState) -> OrbitLineKey {
    let el = &state.elements;
    let q = |v: f64, step: f64| (v / step).round() as i64;
    OrbitLineKey {
        parent: state.parent.clone(),
        // `a` spans 10⁵–10¹¹ m — an absolute step big enough to
        // swallow refit noise would never invalidate an outer-planet
        // conic, and `a/(|a|·rel)` is a constant, so the bucket is
        // the SIGNED `ln(a)` quantized at 1e-4 (a relative change
        // resamples, a fixed few-meter drift does not; hyperbolic
        // a < 0 lands in negative buckets, still relative).
        a: (el.semi_major_axis_m.signum() * el.semi_major_axis_m.abs().ln() / 1e-4).round() as i64,
        e: q(el.eccentricity, 1e-5),
        inc: q(el.inclination_rad, 1e-3),
        node: q(el.ascending_node_rad, 1e-3),
        argp: q(el.arg_periapsis_rad, 1e-3),
        m0: q(el.mean_anomaly_epoch_rad, 1e-3),
        epoch: (state.epoch.ticks() / EPOCH_STEP_TICKS) as i64,
    }
}

/// One vessel's cached node preview: the `(orbit key, node)`
/// invalidation pair (the same quantized rule the vessel lines use —
/// a live vessel's scheduled node used to re-solve the predicted
/// conic every frame too), the predicted post-burn conic
/// (parent-centred, like every sampled line), and the node point's
/// parent-centred position.
struct NodePreview {
    /// The inputs this preview was derived from.
    key: (OrbitLineKey, ManeuverNode),
    /// The orbit the preview was derived from — `parent` rides the
    /// extract's per-parent offset.
    state: OrbitState,
    /// The predicted post-burn polyline (`None` = unsampleable).
    line: Option<Vec<DVec3>>,
    /// The node point's parent-centred position.
    point: Option<DVec3>,
}

/// The solved map camera for one frame — f64 end to end (§2.3).
/// Flight's chase camera is `flight::FlightCameraF64`, a *separate*
/// resource: `map_present` and `flight_present` sit in one unordered
/// Presentation tuple, so a shared `ResMut` would race.
#[derive(Resource, Default)]
pub struct MapCameraF64 {
    /// camera-relative world → clip (`P · Rᵀ`), f64.
    pub view_proj: [[f64; 4]; 4],
    /// Eye position in the system frame (f64 world).
    pub eye: DVec3,
    /// Eye–focus distance (m) — HUD display.
    pub distance_m: f64,
    /// `false` until `map_present` solves once.
    pub valid: bool,
}

/// Everything `UiBuild` needs for the map overlay — written by
/// `map_present`, drawn by `csp_ui::map_overlay`. The label type lives
/// in `csp-ui` (presentation-agnostic plain data; `csp-ui` may not
/// depend on this crate, §2.1).
#[derive(Resource, Default)]
pub struct MapUiData {
    /// Mode this pass's data belongs to — kept current even in Flight
    /// so `UiBuild` can gate on it.
    pub mode: MapMode,
    /// Focus body name for the HUD.
    pub focus_name: String,
    /// Eye–focus distance (m), HUD display.
    pub camera_distance_m: f64,
    /// Projected label anchors for this frame.
    pub labels: Vec<csp_ui::MapLabel>,
    /// Click-to-focus targets — projected marker anchors paired with
    /// the focus they switch to, hit-tested by `UiBuild` against the
    /// egui pointer (markers are too small to name in winit space).
    pub picks: Vec<MapPick>,
    /// The focused orbit's readout for the info panel — `None` when
    /// the focus is the star or its conic cannot be read.
    pub orbit_info: Option<csp_ui::MapOrbitInfo>,
}

/// A click-to-focus target — the screen anchor plus the destination.
/// Identity rides this table rather than the label text so a renamed
/// label can never desync the pick.
#[derive(Clone, Copy, Debug)]
pub struct MapPick {
    /// Clip-space anchor (same projection the marker used).
    pub ndc: [f32; 3],
    /// What a click on this anchor focuses.
    pub target: MapFocusTarget,
}

/// Where a click takes the camera.
#[derive(Clone, Copy, Debug)]
pub enum MapFocusTarget {
    /// A [`BodyTable`] index.
    Body(usize),
    /// A vessel id — matched against the *snapshot* at apply time, so
    /// a vessel that left between frames simply does nothing.
    Vessel(VesselId),
}

/// The RenderExtract product — the packet `csp-app` hands to
/// `csp_render::map::MapRenderer`. `None` while in [`MapMode::Flight`].
#[derive(Resource, Default)]
pub struct MapFrame(pub Option<MapPacket>);

// ---------------------------------------------------------------------------
// W2.4 — planning surface: the alarm book + the staged transfer plan
// (PLAN §4 v1/M5, the KAC-equivalent row). Session state (§2.2-C):
// the sim's event heap is authoritative; these mirror what the player
// asked for and what `fired_events` confirmed.
// ---------------------------------------------------------------------------

/// What kind of deadline one alarm-book entry rides — the field the
/// fired-event matcher keys on (`PlayerAlarm` by label,
/// `TransferWindow` by body pair).
#[derive(Clone, Debug)]
pub enum AlarmKind {
    /// A `SetAlarm` product — matches `EventKind::PlayerAlarm` by
    /// label (the sim carries no id; the label is the handle).
    Manual,
    /// A `WatchTransferWindow` product — matches
    /// `EventKind::TransferWindow` on `(origin, destination)`.
    Window {
        /// The departure body.
        origin: BodyId,
        /// The arrival body.
        destination: BodyId,
    },
}

/// One alarm the player armed through the planning panel — the
/// panel's mirror of a scheduled `PlayerAlarm`/`TransferWindow`
/// event. It counts down, flips to `Fired` when
/// `AdvanceReport::fired_events` shows the event, or `Lapsed` when
/// its tick passed unobserved (a drain-time rejection, or a past-tick
/// request the apply re-clamped — honest either way).
#[derive(Clone, Debug)]
pub struct AlarmEntry {
    /// The requested fire tick (`SetAlarm`/`WatchTransferWindow` may
    /// still clamp it to `clock+1` at drain — display is approximate).
    pub tick: u64,
    /// The player's label (or the panel's generated one).
    pub label: String,
    /// What the entry names.
    pub kind: AlarmKind,
    /// The tick the matching event fired at, once observed.
    pub fired: Option<u64>,
    /// `tick + ALARM_LAPSE_TICKS` passed with no matching fire — the
    /// command never landed. Set by [`MapPlanning::alarm_rows`].
    pub lapsed: bool,
}

/// The W2.4 planning session state — the alarm book plus the staged
/// `plan_transfer` result. Presentation-session data only: written
/// inside `UiBuild`/`sim_step` plumbing, read by `map_present` and
/// the panel. Never sim state, never logged, never hashed.
#[derive(Resource, Default)]
pub struct MapPlanning {
    /// Alarms/watches armed through the panel (plus unmatched fires
    /// recorded as fired rows — see [`Self::note_fired`]).
    pub alarms: Vec<AlarmEntry>,
    /// The last `plan_transfer` result + its resolutions.
    pub plan: Option<PlannedTransfer>,
    /// The last planner failure's reason ("no feasible cell in
    /// window", "bodies share no parent") — the panel's error line.
    pub plan_error: Option<String>,
}

/// The last `plan_transfer` result staged for the panel + the map:
/// the plan itself (conic drawn as the planned route), the resolved
/// `departure_node` for the picked vessel, the first predicted
/// encounter, and the honest failure strings for each leg that could
/// not resolve.
pub struct PlannedTransfer {
    /// The refined plan (bodies, ticks, leg, transfer conic).
    pub plan: csp_sim::transfer::TransferPlan,
    /// `departure_node` for the picked vessel — `Some` only when the
    /// ejection geometry solved AND the node still sits ahead of the
    /// clock it was resolved against.
    pub node: Option<csp_sim::transfer::DepartureNode>,
    /// The vessel the node was resolved for (`None` = no eligible
    /// pick — the button stays unoffered, not dead).
    pub node_vessel: Option<VesselId>,
    /// Why no node resolved — the ejection's own refusal text.
    pub node_error: Option<String>,
    /// `predict_encounter_excluding` on `plan.conic` — the first real
    /// SOI crossing with the origin's egress filtered out.
    pub encounter: Option<csp_sim::transfer::EncounterPrediction>,
}

impl MapPlanning {
    /// Fold `AdvanceReport::fired_events` into the book: mark each
    /// armed entry fired on its first matching `PlayerAlarm` /
    /// `TransferWindow`, and record an unmatched fire as a fired row
    /// of its own — an alarm the panel never armed (a script's, an
    /// MCP-set one) still surfaces rather than vanishing.
    pub(crate) fn note_fired(&mut self, events: &[csp_sim::events::ScheduledEvent]) {
        use csp_sim::events::EventKind as K;
        for e in events {
            let (label, kind): (String, AlarmKind) = match &e.kind {
                K::PlayerAlarm { label } => (label.clone(), AlarmKind::Manual),
                K::TransferWindow {
                    origin,
                    destination,
                } => (
                    format!("{origin} → {destination} window"),
                    AlarmKind::Window {
                        origin: origin.clone(),
                        destination: destination.clone(),
                    },
                ),
                _ => continue,
            };
            let matches = |a: &&mut AlarmEntry| -> bool {
                a.fired.is_none()
                    && match (&a.kind, &kind) {
                        (AlarmKind::Manual, AlarmKind::Manual) => a.label == label,
                        (
                            AlarmKind::Window {
                                origin: o1,
                                destination: d1,
                            },
                            AlarmKind::Window {
                                origin: o2,
                                destination: d2,
                            },
                        ) => o1 == o2 && d1 == d2,
                        _ => false,
                    }
            };
            match self.alarms.iter_mut().find(matches) {
                Some(a) => a.fired = Some(e.tick),
                None => self.alarms.push(AlarmEntry {
                    tick: e.tick,
                    label,
                    kind,
                    fired: Some(e.tick),
                    lapsed: false,
                }),
            }
        }
        // Bound the book: drop fired/lapsed rows oldest-first past the
        // cap (armed rows are never evicted — an armed alarm can only
        // leave by firing or lapsing).
        while self.alarms.len() > ALARM_BOOK_CAP
            && let Some(i) = self
                .alarms
                .iter()
                .position(|a| a.fired.is_some() || a.lapsed)
        {
            self.alarms.remove(i);
        }
    }

    /// The panel's alarm rows — resolved against `now`, marking
    /// lapsed entries in place (an armed entry `ALARM_LAPSE_TICKS`
    /// past its tick with no observed fire never landed).
    pub(crate) fn alarm_rows(&mut self, now: u64) -> Vec<csp_ui::AlarmRow> {
        let hz = TICK_HZ as f64;
        let mut rows = Vec::with_capacity(self.alarms.len());
        for (i, a) in self.alarms.iter_mut().enumerate() {
            if a.fired.is_none() && !a.lapsed && now > a.tick.saturating_add(ALARM_LAPSE_TICKS) {
                a.lapsed = true;
            }
            rows.push(csp_ui::AlarmRow {
                label: a.label.clone(),
                eta_s: (a.tick as i64 - now as i64) as f64 / hz,
                state: if a.fired.is_some() {
                    csp_ui::AlarmState::Fired
                } else if a.lapsed {
                    csp_ui::AlarmState::Lapsed
                } else {
                    csp_ui::AlarmState::Armed
                },
                index: i,
            });
        }
        rows
    }

    /// The panel's plan row — `tick` resolves the countdowns.
    pub(crate) fn plan_row(&self, tick: u64) -> Option<csp_ui::PlanRow> {
        let p = self.plan.as_ref()?;
        let hz = TICK_HZ as f64;
        let until = |t: u64| (t as i64 - tick as i64) as f64 / hz;
        Some(csp_ui::PlanRow {
            origin: p.plan.origin.to_string(),
            destination: p.plan.destination.to_string(),
            depart_in_s: until(p.plan.departure_tick),
            duration_s: p.plan.duration_ticks as f64 / hz,
            dv_depart_mps: p.plan.leg.dv_depart_mps,
            dv_arrive_mps: p.plan.leg.dv_arrive_mps,
            node: p
                .node
                .as_ref()
                .zip(p.node_vessel)
                .map(|(n, v)| csp_ui::PlanNodeRow {
                    vessel: v.0,
                    in_s: until(n.tick),
                    dv_mps: (n.dv_prograde_mps * n.dv_prograde_mps
                        + n.dv_normal_mps * n.dv_normal_mps
                        + n.dv_radial_mps * n.dv_radial_mps)
                        .sqrt(),
                }),
            node_error: p.node_error.clone(),
            encounter: p.encounter.as_ref().map(|e| csp_ui::PlanEncounterRow {
                body: e.event.body.to_string(),
                enter: e.event.direction == SoiDirection::Enter,
                in_s: until(e.event.tick),
                periapsis_m: e.approach.as_ref().map(|a| a.periapsis_m),
                v_inf_mps: e.approach.as_ref().and_then(|a| a.v_inf_mps),
                impacts: e.approach.as_ref().and_then(|a| a.impacts),
            }),
        })
    }
}

/// The `PorkchopQuery` the panel's "Find window" implies — the same
/// shape the sim's own transfer tests use, generalized to any
/// sibling pair: departure window `[now + 1 day, now + 0.9·synodic]`,
/// duration window `[0.4, 1.6]·T_hohmann`, 24×24 cells, zero-rev
/// legs, Oberth-priced departure against `park_r` and v∞ arrival.
fn porkchop_query(
    model: &PatchedConics,
    origin: &BodyId,
    destination: &BodyId,
    now: SimTime,
    park_r: f64,
) -> Result<csp_sim::transfer::PorkchopQuery, SimError> {
    let o = model.body_orbit(origin)?;
    let d = model.body_orbit(destination)?;
    let (Some(oe), Some(de)) = (&o.elements, &d.elements) else {
        return Err(SimError::Recoverable("plan: endpoint has no orbit"));
    };
    let (Some(op), Some(dp)) = (&o.parent, &d.parent) else {
        return Err(SimError::Recoverable("plan: bodies share no parent"));
    };
    if op != dp {
        return Err(SimError::Recoverable("plan: bodies share no parent"));
    }
    let mu_p = model.body_orbit(op)?.mu_m3_per_s2;
    let hz = TICK_HZ as f64;
    let n = |a: f64| (mu_p / (a * a * a)).sqrt();
    let synodic_s = core::f64::consts::TAU
        / (n(oe.semi_major_axis_m) - n(de.semi_major_axis_m))
            .abs()
            .max(1e-12);
    let a_t = (oe.semi_major_axis_m + de.semi_major_axis_m) * 0.5;
    let t_h_s = core::f64::consts::PI * (a_t * a_t * a_t / mu_p).sqrt();
    Ok(csp_sim::transfer::PorkchopQuery {
        origin: origin.clone(),
        destination: destination.clone(),
        depart_lo: now.ticks().saturating_add(21_600 * TICK_HZ),
        depart_hi: now.ticks().saturating_add((synodic_s * 0.9 * hz) as u64),
        duration_lo: (t_h_s * 0.4 * hz) as u64,
        duration_hi: (t_h_s * 1.6 * hz).max(t_h_s * 0.4 * hz + 64.0) as u64,
        depart_cells: 24,
        duration_cells: 24,
        depart_cost: csp_sim::transfer::EndCost::CircularPark(park_r),
        arrive_cost: csp_sim::transfer::EndCost::AtInfinity,
        max_revs: 0,
    })
}

/// Resolve one [`csp_ui::PlanningCommand`] — panel output translated
/// into its sim effect. `SetAlarm`/`WatchTransferWindow`/`SetManeuver`
/// become `SimCommand`s on `out` (the outbox's next-tick hand-off,
/// §2.5); the planner calls (`plan_transfer`/`departure_node`/
/// `predict_encounter`) run on the spot — they are pure queries over
/// the data-derived orbit model, not sim state — and stage their
/// result in `planning` for the next frame's display + draw.
/// `picked_vessel` is the panel's departure-vessel pick.
///
/// Every resolution happens HERE, against the same mirrors the panel
/// was drawn from — a stale panel can never smuggle a command the
/// mirrors stopped justifying (the `program_command` drop rule's
/// sibling).
#[allow(clippy::too_many_lines)] // one verb = one arm, like program_command
pub(crate) fn resolve_planning_command(
    cmd: csp_ui::PlanningCommand,
    model: &PatchedConics,
    snap: &MapSnapshot,
    focus_vessel: Option<VesselId>,
    picked_vessel: Option<VesselId>,
    planning: &mut MapPlanning,
    out: &mut Vec<SimCommand>,
) {
    use csp_ui::PlanningCommand as C;
    let now = snap.tick;
    let hz = TICK_HZ as f64;
    match cmd {
        C::FindWindow {
            origin,
            destination,
        } => {
            let (origin, destination) = (BodyId::from(origin), BodyId::from(destination));
            // The departure pricing's parking radius: the picked
            // vessel's semi-major axis when it orbits the origin (the
            // burn it would actually fly), else the KSP-standard
            // low orbit stand-in of radius + 200 km.
            let park_r = picked_vessel
                .and_then(|vid| snap.vessels.iter().find(|(id, ..)| *id == vid))
                .filter(|(_, o, ..)| o.parent == origin)
                .map(|(_, o, ..)| o.elements.semi_major_axis_m.abs())
                .or_else(|| model.body_radius(&origin).map(|r| r + 2.0e5))
                .unwrap_or(1.0e6);
            planning.plan_error = None;
            planning.plan = None;
            match porkchop_query(model, &origin, &destination, now, park_r)
                .and_then(|q| csp_sim::transfer::plan_transfer(model, &q))
            {
                Ok(plan) => {
                    // The departure burn for the picked vessel — an
                    // eligible pick rides a closed orbit about the
                    // plan's origin (the ejection's own precondition).
                    let (node, node_vessel, node_error) = match picked_vessel.and_then(|vid| {
                        snap.vessels
                            .iter()
                            .find(|(id, ..)| *id == vid)
                            .map(|(_, o, ..)| (vid, o))
                    }) {
                        Some((vid, orbit)) => {
                            match csp_sim::transfer::departure_node(model, orbit, &plan, now) {
                                Ok(n) => (Some(n), Some(vid), None),
                                Err(e) => (None, None, Some(e.reason().to_string())),
                            }
                        }
                        None => (None, None, None),
                    };
                    // First real encounter on the transfer conic —
                    // the origin's egress is excluded (the conic is
                    // stamped at the origin's centre, so the exit is
                    // never a residence in its frame).
                    let horizon = SimTime::from_ticks(
                        plan.arrival_tick
                            .saturating_add(plan.duration_ticks / 4)
                            .max(now.ticks() + 1),
                    );
                    let encounter = csp_sim::transfer::predict_encounter_excluding(
                        model,
                        &plan.conic,
                        SimTime::from_ticks(plan.departure_tick.max(now.ticks() + 1)),
                        horizon,
                        None,
                        Some(&plan.origin),
                    )
                    .ok()
                    .flatten();
                    planning.plan = Some(PlannedTransfer {
                        plan,
                        node,
                        node_vessel,
                        node_error,
                        encounter,
                    });
                }
                Err(e) => {
                    planning.plan_error = Some(e.reason().to_string());
                }
            }
        }
        C::ArmAlarm { label, in_s } => {
            let tick = now.ticks().saturating_add((in_s.max(0.0) * hz) as u64 + 1);
            planning.alarms.push(AlarmEntry {
                tick,
                label: label.clone(),
                kind: AlarmKind::Manual,
                fired: None,
                lapsed: false,
            });
            out.push(SimCommand::SetAlarm { tick, label });
        }
        C::ArmApsis { apoapsis } => {
            let Some(orbit) = focus_vessel
                .and_then(|id| snap.vessels.iter().find(|(vid, ..)| *vid == id))
                .map(|(_, o, ..)| o)
            else {
                return;
            };
            let Ok(mu) = model.body_orbit(&orbit.parent).map(|b| b.mu_m3_per_s2) else {
                return;
            };
            let Ok(Some(tick)) = csp_sim::scripts::resolve_apsis_ticks(
                &orbit.elements,
                mu,
                orbit.epoch.ticks(),
                now.ticks(),
                apoapsis,
            ) else {
                return;
            };
            let label = format!(
                "vessel {} {}",
                focus_vessel.map_or(0, |v| v.0),
                if apoapsis { "apoapsis" } else { "periapsis" }
            );
            planning.alarms.push(AlarmEntry {
                tick,
                label: label.clone(),
                kind: AlarmKind::Manual,
                fired: None,
                lapsed: false,
            });
            out.push(SimCommand::SetAlarm { tick, label });
        }
        C::ArmNodeAlarm => {
            let Some(node) = focus_vessel
                .and_then(|id| snap.vessels.iter().find(|(vid, ..)| *vid == id))
                .and_then(|(_, _, n, ..)| n.as_ref())
            else {
                return;
            };
            let label = format!("vessel {} maneuver node", focus_vessel.map_or(0, |v| v.0));
            planning.alarms.push(AlarmEntry {
                tick: node.tick,
                label: label.clone(),
                kind: AlarmKind::Manual,
                fired: None,
                lapsed: false,
            });
            out.push(SimCommand::SetAlarm {
                tick: node.tick,
                label,
            });
        }
        C::WatchWindow => {
            let Some(plan) = planning.plan.as_ref().map(|p| &p.plan) else {
                return;
            };
            planning.alarms.push(AlarmEntry {
                tick: plan.departure_tick,
                label: format!("{} → {} window", plan.origin, plan.destination),
                kind: AlarmKind::Window {
                    origin: plan.origin.clone(),
                    destination: plan.destination.clone(),
                },
                fired: None,
                lapsed: false,
            });
            out.push(SimCommand::WatchTransferWindow {
                origin: plan.origin.clone(),
                destination: plan.destination.clone(),
                tick: plan.departure_tick,
            });
        }
        C::ArmWindowAlarm => {
            let Some(plan) = planning.plan.as_ref().map(|p| &p.plan) else {
                return;
            };
            let label = format!("{} → {} window", plan.origin, plan.destination);
            planning.alarms.push(AlarmEntry {
                tick: plan.departure_tick,
                label: label.clone(),
                kind: AlarmKind::Manual,
                fired: None,
                lapsed: false,
            });
            out.push(SimCommand::SetAlarm {
                tick: plan.departure_tick,
                label,
            });
        }
        C::CreateNode { vessel } => {
            let Some(plan) = planning.plan.as_ref().map(|p| &p.plan) else {
                return;
            };
            let Some(orbit) = snap
                .vessels
                .iter()
                .find(|(id, ..)| *id == VesselId(vessel))
                .map(|(_, o, ..)| o)
            else {
                planning.plan_error = Some(format!("vessel {vessel} left the snapshot"));
                return;
            };
            // Re-resolve against the CURRENT orbit + clock — the
            // staged node could sit in the past by press time, and
            // `departure_node` refuses `tick <= now` rather than
            // producing a mis-phased burn.
            match csp_sim::transfer::departure_node(model, orbit, plan, now) {
                Ok(n) => {
                    out.push(SimCommand::SetManeuver {
                        vessel: VesselId(vessel),
                        tick: n.tick,
                        dv_prograde_mps: n.dv_prograde_mps,
                        dv_normal_mps: n.dv_normal_mps,
                        dv_radial_mps: n.dv_radial_mps,
                    });
                }
                Err(e) => {
                    if let Some(p) = planning.plan.as_mut() {
                        p.node_error = Some(e.reason().to_string());
                    }
                }
            }
        }
        C::DismissAlarm { index } => {
            // Only a resolved row may leave — an armed alarm cannot
            // be cancelled (the sim has no unschedule verb), so the
            // button is never offered for one; re-check honestly.
            if planning
                .alarms
                .get(index)
                .is_some_and(|a| a.fired.is_some() || a.lapsed)
            {
                planning.alarms.remove(index);
            }
        }
    }
}

/// `Presentation` schedule system — all map derivation in one place
/// (§2.5 #4: "camera solve, LOD selection"). Runs only in map mode;
/// flight frames skip the whole derivation.
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)] // the site marker adds the staged flight env + site/loc tables
pub(crate) fn map_present(
    input: Res<MapInput>,
    model: Res<OrbitModelRes>,
    snap: Res<MapSnapshot>,
    table: Res<BodyTable>,
    sites: Res<crate::structures::SiteTable>,
    flight_snap: Res<crate::flight::FlightSnapshot>,
    loc: Res<crate::LocStrings>,
    session: Res<crate::SessionMirror>,
    warp: Res<crate::WarpMirror>,
    planning: Res<MapPlanning>,
    mut scene: ResMut<MapScene>,
    mut cam: ResMut<MapCameraF64>,
    mut ui: ResMut<MapUiData>,
) {
    ui.mode = input.mode;
    if input.mode != MapMode::Map {
        return;
    }
    let model = &model.0;
    let t = snap.tick;

    // §2.5's `presentation_tick`: a separate, non-authoritative f64
    // clock that may lie between the pre- and post-advance clock.
    // Advanced by wall-dt × the stored warp level — the same rate the
    // sim advances — then clamped to the snapshot so it never
    // extrapolates past what the sim produced. Under rails warp this
    // is what keeps ~18-sim-minute-per-frame position jumps from
    // strobing: the marker lands at the wall-time-proportional point
    // each frame instead of the integer-tick grid.
    {
        let now_tick = t.ticks() as f64;
        let rate_tps = f64::from(warp.0) * TICK_HZ as f64;
        let dt_s = (session.dt_wall_ms / 1000.0).clamp(0.0, 0.1);
        if !scene.present_init || scene.present_tick > now_tick {
            scene.present_tick = now_tick; // first frame, or a rewound clock
        }
        scene.present_init = true;
        scene.present_tick = (scene.present_tick + dt_s * rate_tps).min(now_tick);
        scene.prev_snap_tick = scene.prev_snap_tick.min(t.ticks());
    }
    let pt = scene.present_tick.max(scene.prev_snap_tick as f64);
    scene.prev_snap_tick = t.ticks();

    // System-frame positions: bodies via the parent chain, vessels via
    // state_at + their parent's position — all evaluated at `pt`, the
    // fractional presentation tick (integer-floor/ceil lerp; the
    // conic solve only speaks whole ticks).
    scene.body_pos.clear();
    for body in &table.bodies {
        scene
            .body_pos
            .push(body_system_pos_frac(model, &body.id, pt).ok());
    }
    scene.vessel_pos.clear();
    for (_id, orbit, ..) in &snap.vessels {
        let pos = (|| {
            let (r, _v) = state_at_frac(model, orbit, pt).ok()?;
            let parent = body_system_pos_frac(model, &orbit.parent, pt).ok()?;
            Some(parent + r)
        })();
        scene.vessel_pos.push(pos);
    }

    // Polyline refresh — the documented cadence: bodies once ever,
    // vessels when their OrbitState changed.
    if scene.body_lines.len() != table.bodies.len() {
        scene.body_lines = vec![None; table.bodies.len()];
    }
    for (i, body) in table.bodies.iter().enumerate() {
        if scene.body_lines[i].is_none()
            && let Some(elements) = model
                .body_orbit(&body.id)
                .ok()
                .and_then(|b| b.elements.clone())
            && let Some(parent) = &body.parent
        {
            let state = OrbitState {
                parent: parent.clone(),
                elements,
                epoch: SimTime::ZERO,
            };
            scene.body_lines[i] = sample_orbit(model, &state);
        }
    }
    refresh_vessel_lines(model, &snap, &mut scene);

    refresh_node_previews(model, &snap, &mut scene);

    // Camera solve — f64 throughout. The focus cycle's vessel half
    // (indices `>= bodies.len()`) addresses `scene.vessel_pos`.
    let focus = match input.vessel_focus(&table) {
        Some(i) => scene.vessel_pos.get(i).copied().flatten(),
        None => scene.body_pos.get(input.focus).copied().flatten(),
    }
    .unwrap_or(DVec3::ZERO)
        + input.pan_offset;
    *cam = solve_camera(&input, focus);
    ui.focus_name = match input.vessel_focus(&table).and_then(|i| snap.vessels.get(i)) {
        Some((id, ..)) => format!("vessel {}", id.0),
        None => table.focus_name(input.focus).to_string(),
    };
    ui.camera_distance_m = input.distance_m;

    // Labels: bodies + vessels that project — each projected marker is
    // also a click-to-focus target (`UiBuild` hit-tests the pointer).
    ui.labels.clear();
    ui.picks.clear();
    for (i, (body, pos)) in table.bodies.iter().zip(scene.body_pos.iter()).enumerate() {
        if let Some(ndc) = pos.and_then(|p| project(&cam, p)) {
            ui.labels.push(csp_ui::MapLabel {
                ndc,
                text: body.id.to_string(),
                color: body.color,
                kind: csp_ui::MapLabelKind::Body,
            });
            ui.picks.push(MapPick {
                ndc,
                target: MapFocusTarget::Body(i),
            });
        }
    }
    for ((id, _orbit, ..), pos) in snap.vessels.iter().zip(scene.vessel_pos.iter()) {
        if let Some(ndc) = pos.and_then(|p| project(&cam, p)) {
            ui.labels.push(csp_ui::MapLabel {
                ndc,
                text: format!("vessel {}", id.0),
                color: VESSEL_COLOR,
                kind: csp_ui::MapLabelKind::Vessel,
            });
            ui.picks.push(MapPick {
                ndc,
                target: MapFocusTarget::Vessel(*id),
            });
        }
    }
    // Node labels — the point rides its parent like the lines do.
    for (id, orbit, node, _rails, burn_s) in &snap.vessels {
        let Some(node) = node else { continue };
        let (Some(preview), Some(off)) = (
            scene.node_lines.get(id),
            table
                .index_of(&orbit.parent)
                .and_then(|pi| scene.body_pos.get(pi))
                .copied()
                .flatten(),
        ) else {
            continue;
        };
        let Some(point) = preview.point else {
            continue;
        };
        if let Some(ndc) = project(&cam, point + off) {
            // W2.4: the node's own status + the staged burn-time
            // estimate — "T−.. · Δv .. · ~Ns burn" while scheduled,
            // the remaining Δv while the executor owns the throttle.
            let text = match &node.status {
                ManeuverStatus::Burning(b) => {
                    format!("node BURNING  Δv left {:.0} m/s", b.dv_remaining_mps)
                }
                ManeuverStatus::Scheduled => {
                    let eta = (node.tick.saturating_sub(t.ticks())) as f64 / TICK_HZ as f64;
                    let est = burn_s.map_or(String::new(), |s| format!("  ~{s:.0} s burn"));
                    format!("node  T−{eta:.0} s  Δv {:.0} m/s{est}", node.dv_magnitude())
                }
            };
            ui.labels.push(csp_ui::MapLabel {
                ndc,
                text,
                color: NODE_COLOR,
                kind: csp_ui::MapLabelKind::Node,
            });
        }
    }

    // The focused conic — Ap/Pe marks on it plus the info panel's
    // readout. Vessels and bodies share the element shape; the star
    // (or any unsolvable parent position) leaves both empty.
    scene.focus_apsis.clear();
    ui.orbit_info = None;
    if let Some(focus) = focused_conic(&input, &table, &snap, model) {
        ui.orbit_info = orbit_info(&focus, model);
        if let Some(parent_pos) = table
            .index_of(&focus.parent)
            .and_then(|pi| scene.body_pos.get(pi))
            .copied()
            .flatten()
        {
            // Pe always exists on a conic; Ap only when it closes.
            let parent_radius = model.body_radius(&focus.parent);
            for (nu, tag) in [(0.0, "Pe"), (std::f64::consts::PI, "Ap")] {
                if nu != 0.0 && focus.elements.eccentricity >= 1.0 - E_PARABOLIC_TOL {
                    continue; // open conic — no apoapsis
                }
                let Some(rel) = apsis_point(focus.elements, nu) else {
                    continue;
                };
                let world = parent_pos + rel;
                scene.focus_apsis.push(world);
                let Some(ndc) = project(&cam, world) else {
                    continue;
                };
                // Surface altitude when the parent's radius is known,
                // else the raw radius (the star has no surface).
                let shown = parent_radius.map_or(rel.norm(), |rad| rel.norm() - rad);
                ui.labels.push(csp_ui::MapLabel {
                    ndc,
                    text: format!("{tag} {}", csp_ui::fmt_distance(shown)),
                    color: focus.line_color,
                    kind: csp_ui::MapLabelKind::Apsis,
                });
            }
        }
    }

    // Surface sites on the dominant body — the launch complex and the
    // recovery fields — marked while the camera is focused on their
    // body inside a few radii (at system zoom the ring would sit
    // under the body marker anyway). The staged flight env carries
    // the dominant body's spin; a site on any other body has no
    // staged rotation to place it by, so it is skipped honestly.
    scene.site_markers.clear();
    if input.vessel_focus(&table).is_none()
        && let Some(env) = flight_snap.env.as_ref()
        && let Some(dom) = env.body.as_ref()
        && table.bodies.get(input.focus).is_some_and(|b| b.id == *dom)
        && model
            .body_radius(dom)
            .is_some_and(|r| input.distance_m < 6.0 * r)
        && let Some(body_pos) = table
            .index_of(dom)
            .and_then(|i| scene.body_pos.get(i))
            .copied()
            .flatten()
    {
        for site in &sites.sites {
            if site.body != *dom {
                continue;
            }
            let pos = crate::structures::site_world_pos(env, site, body_pos);
            scene.site_markers.push(pos);
            if let Some(ndc) = project(&cam, pos) {
                ui.labels.push(csp_ui::MapLabel {
                    ndc,
                    text: loc
                        .0
                        .get(site.display_name.as_str())
                        .cloned()
                        .unwrap_or_else(|| site.id.as_str().to_string()),
                    color: SITE_COLOR,
                    kind: csp_ui::MapLabelKind::Site,
                });
            }
        }
    }

    // W2.4: encounter prediction + SOI rings + the staged transfer
    // plan's conic — the KAC-equivalent geometry the panel resolves.
    refresh_planning(
        model, &input, &snap, &table, &planning, &mut scene, &mut ui, &cam, t,
    );
}

/// One [`predict_encounter`]-produced crossing, drawn: the marker at
/// the crossing point (the conic's own position at the event tick,
/// riding its parent's current position like the lines do), tinted
/// by direction — red when the model reports the approach impacts —
/// plus the labeled readout.
#[allow(clippy::too_many_arguments)] // one mark = one data bag
fn push_encounter(
    model: &PatchedConics,
    table: &BodyTable,
    conic: &OrbitState,
    pred: &csp_sim::transfer::EncounterPrediction,
    t: SimTime,
    scene: &mut MapScene,
    ui: &mut MapUiData,
    cam: &MapCameraF64,
) {
    let ev = &pred.event;
    let enter = ev.direction == SoiDirection::Enter;
    let mut label = format!(
        "{} {} T−{}",
        if enter { "enters" } else { "leaves" },
        ev.body,
        csp_ui::fmt_period((ev.tick.saturating_sub(t.ticks())) as f64 / TICK_HZ as f64),
    );
    let mut color = if enter {
        ENCOUNTER_ENTER_COLOR
    } else {
        ENCOUNTER_EXIT_COLOR
    };
    if let Some(a) = &pred.approach {
        let alt = model
            .body_radius(&ev.body)
            .map(|r| a.periapsis_m - r)
            .unwrap_or(a.periapsis_m);
        label.push_str(&format!("  Pe {}", csp_ui::fmt_distance(alt)));
        if let Some(v) = a.v_inf_mps {
            label.push_str(&format!("  v∞ {:.1} km/s", v / 1e3));
        }
        if a.impacts == Some(true) {
            color = ENCOUNTER_IMPACT_COLOR;
            label.push_str("  IMPACTS");
        }
    }
    let Some(off) = table
        .index_of(&conic.parent)
        .and_then(|pi| scene.body_pos.get(pi))
        .copied()
        .flatten()
    else {
        return;
    };
    let Ok((rel, _)) = model.state_at(conic, SimTime::from_ticks(ev.tick)) else {
        return;
    };
    let world = off + rel;
    scene.encounter_marks.push((world, color));
    if let Some(ndc) = project(cam, world) {
        ui.labels.push(csp_ui::MapLabel {
            ndc,
            text: label,
            color,
            kind: csp_ui::MapLabelKind::Encounter,
        });
    }
}

/// The encounter scan's forward window — the same scale the sim's
/// §2.6.6 publish uses (a few periods of the tightest closed orbit
/// in play): 4× the smallest closed period among the conic itself
/// and its parent's SOI-carrying children; a bounded fallback when
/// nothing in play is closed.
fn predict_horizon(model: &PatchedConics, conic: &OrbitState, now: SimTime) -> SimTime {
    let mut tightest_s = f64::INFINITY;
    let period = |mu: f64, el: &KeplerElements| {
        (el.eccentricity < 1.0 - E_PARABOLIC_TOL && el.semi_major_axis_m > 0.0 && mu > 0.0)
            .then(|| 2.0 * std::f64::consts::PI * (el.semi_major_axis_m.powi(3) / mu).sqrt())
    };
    if let Ok(mu) = model.body_orbit(&conic.parent).map(|b| b.mu_m3_per_s2)
        && let Some(p) = period(mu, &conic.elements)
    {
        tightest_s = tightest_s.min(p);
    }
    for (id, body) in model.bodies() {
        if body.parent.as_ref() == Some(&conic.parent)
            && body.soi_m.is_some()
            && *id != conic.parent
            && let Ok(mu) = model.body_orbit(&conic.parent).map(|b| b.mu_m3_per_s2)
            && let Some(el) = &body.elements
            && let Some(p) = period(mu, el)
        {
            tightest_s = tightest_s.min(p);
        }
    }
    // Fallback: nothing closed in play — a bounded guess, not an
    // unbounded scan (the budget still caps the work).
    let span_s = if tightest_s.is_finite() {
        4.0 * tightest_s
    } else {
        86_400.0
    };
    SimTime::from_ticks(now.ticks().saturating_add((span_s * TICK_HZ as f64) as u64))
}

/// The W2.4 geometry pass — the focused vessel's own next SOI
/// crossing, its predicted post-burn conic's apsides and first
/// encounter, the staged transfer plan's route line + encounter +
/// endpoint rings, and the focused body's SOI boundary. Pure queries
/// over the data-derived model, recomputed each map frame; the
/// plan's polyline re-samples only when the staged plan changes.
#[allow(clippy::too_many_arguments)] // one pass = one data bag
fn refresh_planning(
    model: &PatchedConics,
    input: &MapInput,
    snap: &MapSnapshot,
    table: &BodyTable,
    planning: &MapPlanning,
    scene: &mut MapScene,
    ui: &mut MapUiData,
    cam: &MapCameraF64,
    t: SimTime,
) {
    scene.encounter_marks.clear();
    scene.soi_rings.clear();
    scene.pred_apsis.clear();
    if planning.plan.is_none() {
        scene.plan_line = None;
        scene.plan_line_key = None;
    }
    // Bodies whose SOI ring draws — the focused body plus every body
    // an Enter mark or the staged plan names.
    let mut ring_bodies: Vec<BodyId> = Vec::new();
    let mut ring = |id: &BodyId| {
        if !ring_bodies.contains(id)
            && let Some(i) = table.index_of(id)
            && table.bodies[i].soi_m.is_some()
        {
            ring_bodies.push(id.clone());
        }
    };

    // The focused vessel's live-conic crossing + the predicted
    // post-burn conic's apsides and first encounter.
    if let Some(i) = input.vessel_focus(table)
        && let Some((_id, orbit, node, ..)) = snap.vessels.get(i)
    {
        if let Ok(Some(pred)) = csp_sim::transfer::predict_encounter(
            model,
            orbit,
            t,
            predict_horizon(model, orbit, t),
            None,
        ) {
            if pred.event.direction == SoiDirection::Enter {
                ring(&pred.event.body.clone());
            }
            push_encounter(model, table, orbit, &pred, t, scene, ui, cam);
        }
        if let Some(node) = node
            && matches!(node.status, ManeuverStatus::Scheduled)
            && let Ok(pred_conic) = csp_sim::maneuver::predict_maneuver(model, orbit, node)
        {
            // Apsides on the predicted conic — the burn's own Ap/Pe,
            // drawn in node color so they read "planned, not flown".
            if let Some(parent_pos) = table
                .index_of(&pred_conic.parent)
                .and_then(|pi| scene.body_pos.get(pi))
                .copied()
                .flatten()
            {
                for nu in [0.0, std::f64::consts::PI] {
                    if nu != 0.0 && pred_conic.elements.eccentricity >= 1.0 - E_PARABOLIC_TOL {
                        continue;
                    }
                    if let Some(rel) = apsis_point(&pred_conic.elements, nu) {
                        scene.pred_apsis.push(parent_pos + rel);
                    }
                }
            }
            if let Ok(Some(pred)) = csp_sim::transfer::predict_encounter(
                model,
                &pred_conic,
                t,
                predict_horizon(model, &pred_conic, t),
                None,
            ) {
                if pred.event.direction == SoiDirection::Enter {
                    ring(&pred.event.body.clone());
                }
                push_encounter(model, table, &pred_conic, &pred, t, scene, ui, cam);
            }
        }
    }

    // The staged transfer plan: the route polyline (re-sampled when
    // the plan changes), its predicted encounter, and both endpoint
    // rings — the plan's geometry IS the SOI hand-off.
    if let Some(planned) = &planning.plan {
        let plan = &planned.plan;
        if scene.plan_line_key != Some(plan.departure_tick) {
            scene.plan_line =
                sample_orbit(model, &plan.conic).map(|line| (plan.conic.parent.clone(), line));
            scene.plan_line_key = Some(plan.departure_tick);
        }
        if let Some(pred) = &planned.encounter {
            if pred.event.direction == SoiDirection::Enter {
                ring(&pred.event.body.clone());
            }
            push_encounter(model, table, &plan.conic, pred, t, scene, ui, cam);
        }
        ring(&plan.origin.clone());
        ring(&plan.destination.clone());
    }
    // The focused body's own boundary — even with nothing staged, an
    // SOI circle answers "what am I inside" at a glance.
    if input.vessel_focus(table).is_none()
        && let Some(body) = table.bodies.get(input.focus)
    {
        ring(&body.id.clone());
    }
    // Emit the rings — circles in the ecliptic plane at each body's
    // current (presentation-tick) position, tinted in the body's own
    // palette color.
    for id in &ring_bodies {
        let Some(i) = table.index_of(id) else {
            continue;
        };
        let (Some(soi), Some(pos)) = (
            table.bodies[i].soi_m,
            scene.body_pos.get(i).copied().flatten(),
        ) else {
            continue;
        };
        let mut color = table.bodies[i].color;
        color[3] *= 0.6;
        scene.soi_rings.push((pos, soi, color));
    }
}

/// `body_system_pos` at a fractional tick — the §2.5 presentation
/// tick's evaluation rule: `state_at` speaks whole ticks, so the
/// position is the floor/ceil evaluations lerped. Positions move at
/// most ~v·(1/64 s) between ticks — linear interpolation is exact to
/// far under a texel.
fn body_system_pos_frac(
    model: &PatchedConics,
    id: &BodyId,
    t: f64,
) -> Result<DVec3, csp_core::SimError> {
    let lo = t.max(0.0).floor() as u64;
    let f = (t.max(0.0) - lo as f64).min(1.0);
    let p0 = body_system_pos(model, id, SimTime::from_ticks(lo))?;
    if f <= 0.0 {
        return Ok(p0);
    }
    let p1 = body_system_pos(model, id, SimTime::from_ticks(lo.saturating_add(1)))?;
    Ok(p0 + (p1 - p0) * f)
}

/// `state_at` at a fractional tick — the lerp rule
/// [`body_system_pos_frac`] uses, applied to a vessel's conic.
fn state_at_frac(
    model: &PatchedConics,
    s: &OrbitState,
    t: f64,
) -> Result<(DVec3, DVec3), csp_core::SimError> {
    let lo = t.max(0.0).floor() as u64;
    let f = (t.max(0.0) - lo as f64).min(1.0);
    let (r0, v0) = model.state_at(s, SimTime::from_ticks(lo))?;
    if f <= 0.0 {
        return Ok((r0, v0));
    }
    let (r1, v1) = model.state_at(s, SimTime::from_ticks(lo.saturating_add(1)))?;
    Ok((r0 + (r1 - r0) * f, v0 + (v1 - v0) * f))
}

/// The conic the camera is focused on — the focused vessel's orbit or
/// the focused body's own orbit about its parent (`None` for the
/// star). The display name and the line color (vessel orange vs the
/// body's palette entry) ride along so the apsis marks match what
/// the player sees drawn.
struct FocusedConic<'a> {
    /// Display name for the info panel.
    name: String,
    /// The central body the elements orbit.
    parent: BodyId,
    /// The conic elements (parent-centred, own epoch).
    elements: &'a KeplerElements,
    /// The orbit line's color — apsis marks take the same tint.
    line_color: [f32; 4],
}

/// Resolve [`MapInput::focus`] into the conic it tracks.
fn focused_conic<'a>(
    input: &MapInput,
    table: &'a BodyTable,
    snap: &'a MapSnapshot,
    model: &'a PatchedConics,
) -> Option<FocusedConic<'a>> {
    match input.vessel_focus(table) {
        Some(i) => {
            let (id, orbit, ..) = snap.vessels.get(i)?;
            Some(FocusedConic {
                name: format!("vessel {}", id.0),
                parent: orbit.parent.clone(),
                elements: &orbit.elements,
                line_color: VESSEL_COLOR,
            })
        }
        None => {
            let body = table.bodies.get(input.focus)?;
            let parent = body.parent.as_ref()?;
            let elements = model.body_orbit(&body.id).ok()?.elements.as_ref()?;
            Some(FocusedConic {
                name: body.id.to_string(),
                parent: parent.clone(),
                elements,
                line_color: body.color,
            })
        }
    }
}

/// The info panel's numbers — apsis radii + period straight off the
/// elements. `None` when the parent's μ record is missing.
fn orbit_info(focus: &FocusedConic<'_>, model: &PatchedConics) -> Option<csp_ui::MapOrbitInfo> {
    let mu = model.body_orbit(&focus.parent).ok()?.mu_m3_per_s2;
    let el = focus.elements;
    let e = el.eccentricity;
    let a = el.semi_major_axis_m;
    let p = p_latus(el)?;
    let closed = e < 1.0 - E_PARABOLIC_TOL && a > 0.0 && mu > 0.0;
    Some(csp_ui::MapOrbitInfo {
        name: focus.name.clone(),
        parent_name: focus.parent.to_string(),
        apo_r_m: closed.then(|| p / (1.0 - e)),
        pe_r_m: p / (1.0 + e),
        parent_radius_m: model.body_radius(&focus.parent),
        period_s: closed.then(|| 2.0 * std::f64::consts::PI * f64::sqrt(a * a * a / mu)),
    })
}

/// Semi-latus rectum `p` for any conic in the sim's encoding — the
/// apsis geometry's shared constant. Elliptic/hyperbolic:
/// `p = |a(1 − e²)|`; the `|e − 1| ≤ E_PARABOLIC_TOL` band carries the
/// sim's parabola convention `a := −p`, so `p = |a|` literally.
/// `None` when the elements cannot form a conic (p ≤ 0 / non-finite).
fn p_latus(el: &KeplerElements) -> Option<f64> {
    let e = el.eccentricity;
    let a = el.semi_major_axis_m;
    let p = if (e - 1.0).abs() <= E_PARABOLIC_TOL {
        a.abs()
    } else {
        (a * (1.0 - e * e)).abs()
    };
    (p.is_finite() && p > 0.0).then_some(p)
}

/// The parent-centred apsis point at true anomaly `nu` — pure conic
/// geometry (`r = p/(1 + e·cos ν)` in the perifocal frame, rotated to
/// inertial), no time solve needed. `None` when `nu` lands past the
/// hyperbolic asymptote (`1 + e·cos ν ≤ 0`).
fn apsis_point(el: &KeplerElements, nu: f64) -> Option<DVec3> {
    let e = el.eccentricity;
    let p = p_latus(el)?;
    let denom = 1.0 + e * f64::cos(nu);
    if denom <= 0.0 {
        return None;
    }
    let r = p / denom;
    let (s, c) = f64::sin_cos(nu);
    Some(perifocal_to_inertial(DVec3::new(r * c, r * s, 0.0), el))
}

/// Perifocal → inertial `R3(Ω)·R1(i)·R3(ω)` — mirrored literally from
/// `csp_sim::orbit::perifocal_to_inertial` (which is `pub(crate)`
/// there): the apsis points are conic geometry, and the view cannot
/// reach the sim's private helper.
fn perifocal_to_inertial(v: DVec3, el: &KeplerElements) -> DVec3 {
    let (sw, cw) = f64::sin_cos(el.arg_periapsis_rad);
    let (si, ci) = f64::sin_cos(el.inclination_rad);
    let (so, co) = f64::sin_cos(el.ascending_node_rad);
    DVec3::new(
        (co * cw - so * ci * sw) * v.x + (-co * sw - so * ci * cw) * v.y + so * si * v.z,
        (so * cw + co * ci * sw) * v.x + (-so * sw + co * ci * cw) * v.y - co * si * v.z,
        (si * sw) * v.x + (si * cw) * v.y + ci * v.z,
    )
}

/// Vessel orbit lines — refreshed only when the orbit's quantized key
/// moves (see [`orbit_line_key`]). Extracted from `map_present` so the
/// breakup-scenario budget test can drive it directly.
fn refresh_vessel_lines(model: &PatchedConics, snap: &MapSnapshot, scene: &mut MapScene) {
    for (id, orbit, ..) in &snap.vessels {
        let key = orbit_line_key(orbit);
        let stale = scene.vessel_lines.get(id).is_none_or(|l| l.key != key);
        if stale {
            let line = sample_orbit(model, orbit);
            scene.vessel_lines.insert(
                *id,
                VesselLine {
                    key,
                    state: orbit.clone(),
                    line,
                },
            );
        }
    }
    scene
        .vessel_lines
        .retain(|id, _| snap.vessels.iter().any(|(vid, ..)| vid == id));
}

/// M5 node previews — the same invalidation rule the vessel lines use:
/// the predicted post-burn conic and the node point re-derive only
/// when the `(orbit, node)` pair they came from changes.
fn refresh_node_previews(model: &PatchedConics, snap: &MapSnapshot, scene: &mut MapScene) {
    for (id, orbit, node, ..) in &snap.vessels {
        let Some(node) = node else { continue };
        let key = (orbit_line_key(orbit), node.clone());
        let stale = scene.node_lines.get(id).is_none_or(|p| p.key != key);
        if stale {
            let predicted = csp_sim::maneuver::predict_maneuver(model, orbit, node).ok();
            let line = predicted.as_ref().and_then(|o| sample_orbit(model, o));
            let point = model
                .state_at(orbit, SimTime::from_ticks(node.tick))
                .ok()
                .map(|(r, _)| r);
            scene.node_lines.insert(
                *id,
                NodePreview {
                    key,
                    state: orbit.clone(),
                    line,
                    point,
                },
            );
        }
    }
    scene.node_lines.retain(|id, _| {
        snap.vessels
            .iter()
            .any(|(vid, _, n, ..)| vid == id && n.is_some())
    });
}

/// `RenderExtract` schedule system (§2.5 #6): emit the immutable
/// `MapPacket` — clip-space vertices only. Points are projected
/// per-frame (the camera moves); polylines stay cached.
#[allow(clippy::too_many_lines)]
#[allow(clippy::cognitive_complexity)] // the extract walk is one pass over one snapshot
pub(crate) fn extract_map(
    input: Res<MapInput>,
    table: Res<BodyTable>,
    scene: Res<MapScene>,
    cam: Res<MapCameraF64>,
    mut frame: ResMut<MapFrame>,
    mut skipped_prev: Local<u32>,
) {
    if input.mode != MapMode::Map || !cam.valid {
        frame.0 = None;
        return;
    }
    let vp = input.viewport_px;
    let mut bg = MapMesh::default();
    let mut lines = MapMesh::default();
    let mut markers = MapMesh::default();
    let mut skipped_lines = 0u32;

    // The backdrop — a fixed world-anchored starfield. Stars sit
    // effectively at infinity: they parallax with camera rotation but
    // not translation, which is exactly how a real sky reads.
    emit_starfield(&mut bg.vertices, &cam, vp);

    // A parent's current position, by table index — the offset every
    // parent-centred polyline rides. `None` on a failed solve (the
    // "skipped, not defaulted" policy — offsetting by ZERO would draw
    // the child's orbit through the system origin).
    let parent_pos = |id: &BodyId| {
        table
            .index_of(id)
            .and_then(|pi| scene.body_pos.get(pi))
            .copied()
            .flatten()
    };

    // Orbit polylines — the focused conic draws brighter and wider so
    // "which of these lines is my reference" reads at a glance.
    let focus_body = input.vessel_focus(&table).is_none().then_some(input.focus);
    for (i, line) in scene.body_lines.iter().enumerate() {
        let (Some(points), Some(body)) = (line.as_ref(), table.bodies.get(i)) else {
            continue;
        };
        let Some(offset) = body.parent.as_ref().and_then(&parent_pos) else {
            skipped_lines += 1;
            continue;
        };
        let focused = focus_body == Some(i);
        let mut color = body.color;
        color[3] *= if focused { 0.9 } else { 0.55 };
        let half_px = if focused {
            LINE_HALF_WIDTH_PX * FOCUS_LINE_WIDTH
        } else {
            LINE_HALF_WIDTH_PX
        };
        emit_polyline(
            &mut lines.vertices,
            points,
            offset,
            &cam,
            vp,
            color,
            half_px,
        );
    }
    for (id, entry) in &scene.vessel_lines {
        let Some(points) = &entry.line else {
            continue;
        };
        let Some(offset) = parent_pos(&entry.state.parent) else {
            skipped_lines += 1;
            continue;
        };
        let focused = input.focus_vessel == Some(*id);
        let mut color = VESSEL_COLOR;
        color[3] *= if focused { 0.95 } else { 0.55 };
        let half_px = if focused {
            LINE_HALF_WIDTH_PX * FOCUS_LINE_WIDTH
        } else {
            LINE_HALF_WIDTH_PX
        };
        emit_polyline(
            &mut lines.vertices,
            points,
            offset,
            &cam,
            vp,
            color,
            half_px,
        );
    }
    // M5 predicted post-burn conics — the same parent-centred offset
    // convention; a failed prediction is "no line", never defaulted.
    for preview in scene.node_lines.values() {
        let Some(points) = preview.line.as_ref() else {
            continue;
        };
        let Some(offset) = parent_pos(&preview.state.parent) else {
            skipped_lines += 1;
            continue;
        };
        let mut color = NODE_COLOR;
        color[3] *= 0.8;
        emit_polyline(
            &mut lines.vertices,
            points,
            offset,
            &cam,
            vp,
            color,
            LINE_HALF_WIDTH_PX,
        );
    }
    // Observability without per-frame spam: warn only when the skipped
    // count grows, so a persistently failing parent logs once while a
    // flapping one re-logs after recovering to zero.
    if skipped_lines > *skipped_prev {
        log::warn!(
            "extract_map: skipped {skipped_lines} orbit polyline(s) — parent position solve failed"
        );
    }
    *skipped_prev = skipped_lines;

    // Markers — a soft glow under an AA disc, with a ring on whichever
    // target the camera is focused on.
    for (i, (body, pos)) in table.bodies.iter().zip(scene.body_pos.iter()).enumerate() {
        let Some(ndc) = pos.and_then(|p| project(&cam, p)) else {
            continue;
        };
        let scale = if body.parent.is_none() {
            STAR_MARKER_SCALE
        } else {
            1.0
        };
        let mut glow = body.color;
        glow[3] *= 0.35;
        emit_shape(
            &mut markers.vertices,
            ndc,
            BODY_MARKER_PX * scale * 3.2,
            vp,
            glow,
            MapVertex::KIND_GLOW,
        );
        emit_shape(
            &mut markers.vertices,
            ndc,
            BODY_MARKER_PX * scale,
            vp,
            body.color,
            MapVertex::KIND_DISC,
        );
        if focus_body == Some(i) {
            emit_shape(
                &mut markers.vertices,
                ndc,
                FOCUS_RING_PX,
                vp,
                [1.0, 1.0, 1.0, 0.9],
                MapVertex::KIND_RING,
            );
        }
    }
    for (i, pos) in scene.vessel_pos.iter().enumerate() {
        let Some(ndc) = pos.and_then(|p| project(&cam, p)) else {
            continue;
        };
        let focused = input.vessel_focus(&table) == Some(i);
        let mut glow = VESSEL_COLOR;
        glow[3] *= if focused { 0.45 } else { 0.3 };
        emit_shape(
            &mut markers.vertices,
            ndc,
            VESSEL_MARKER_PX * 3.0,
            vp,
            glow,
            MapVertex::KIND_GLOW,
        );
        emit_shape(
            &mut markers.vertices,
            ndc,
            VESSEL_MARKER_PX,
            vp,
            VESSEL_COLOR,
            MapVertex::KIND_DISC,
        );
        if focused {
            emit_shape(
                &mut markers.vertices,
                ndc,
                FOCUS_RING_PX,
                vp,
                [1.0, 1.0, 1.0, 0.9],
                MapVertex::KIND_RING,
            );
        }
    }
    // Node markers — a hollow ring reads "planned point", not a body.
    for preview in scene.node_lines.values() {
        let (Some(p), Some(off)) = (preview.point, parent_pos(&preview.state.parent)) else {
            continue;
        };
        if let Some(ndc) = project(&cam, p + off) {
            emit_shape(
                &mut markers.vertices,
                ndc,
                BODY_MARKER_PX,
                vp,
                NODE_COLOR,
                MapVertex::KIND_RING,
            );
        }
    }
    // Ap/Pe rings on the focused conic — same parent-centred points
    // `map_present` labeled, now as geometry.
    let apsis_color = if input.vessel_focus(&table).is_some() {
        VESSEL_COLOR
    } else {
        table
            .bodies
            .get(input.focus)
            .map(|b| b.color)
            .unwrap_or(STAR_COLOR)
    };
    for p in &scene.focus_apsis {
        if let Some(ndc) = project(&cam, *p) {
            emit_shape(
                &mut markers.vertices,
                ndc,
                APSIS_MARKER_PX,
                vp,
                apsis_color,
                MapVertex::KIND_RING,
            );
        }
    }
    // Surface-site rings — the launch complex + recovery fields on the
    // focused body (populated by `map_present` only when the camera is
    // close enough for a surface feature to resolve).
    for p in &scene.site_markers {
        if let Some(ndc) = project(&cam, *p) {
            let mut glow = SITE_COLOR;
            glow[3] *= 0.3;
            emit_shape(
                &mut markers.vertices,
                ndc,
                VESSEL_MARKER_PX * 2.4,
                vp,
                glow,
                MapVertex::KIND_GLOW,
            );
            emit_shape(
                &mut markers.vertices,
                ndc,
                BODY_MARKER_PX,
                vp,
                SITE_COLOR,
                MapVertex::KIND_RING,
            );
        }
    }
    // W2.4: SOI boundary rings — ecliptic-plane circles at each
    // staged body's current position (regenerated per frame: the
    // centers move with the presentation tick like the markers do).
    for (center, radius, color) in &scene.soi_rings {
        let mut pts = Vec::with_capacity(SOI_RING_SAMPLES + 1);
        for i in 0..=SOI_RING_SAMPLES {
            let a = 2.0 * std::f64::consts::PI * i as f64 / SOI_RING_SAMPLES as f64;
            let (s, c) = a.sin_cos();
            pts.push(*center + DVec3::new(radius * c, radius * s, 0.0));
        }
        let mut c = *color;
        c[3] *= 0.35;
        emit_polyline(
            &mut lines.vertices,
            &pts,
            DVec3::ZERO,
            &cam,
            vp,
            c,
            LINE_HALF_WIDTH_PX * 0.7,
        );
    }
    // W2.4: the staged transfer plan's conic — parent-centred like
    // every sampled line, riding the common parent's position.
    if let Some((parent, points)) = &scene.plan_line
        && let Some(off) = parent_pos(parent)
    {
        let mut c = PLAN_COLOR;
        c[3] *= 0.8;
        emit_polyline(
            &mut lines.vertices,
            points,
            off,
            &cam,
            vp,
            c,
            LINE_HALF_WIDTH_PX * FOCUS_LINE_WIDTH,
        );
    }
    // W2.4: predicted encounter marks — hollow rings tinted by the
    // prediction's direction/impact read.
    for (pos, color) in &scene.encounter_marks {
        if let Some(ndc) = project(&cam, *pos) {
            emit_shape(
                &mut markers.vertices,
                ndc,
                APSIS_MARKER_PX * 1.3,
                vp,
                *color,
                MapVertex::KIND_RING,
            );
        }
    }
    // W2.4: apsides on the predicted post-burn conic — node color,
    // "planned" like the conic they mark.
    for p in &scene.pred_apsis {
        if let Some(ndc) = project(&cam, *p) {
            emit_shape(
                &mut markers.vertices,
                ndc,
                APSIS_MARKER_PX,
                vp,
                NODE_COLOR,
                MapVertex::KIND_RING,
            );
        }
    }

    frame.0 = Some(MapPacket { bg, lines, markers });
}

// ---------------------------------------------------------------------------
// Sim access — the ONE place map code reads csp-sim. Called by
// View::end_frame's staging step; systems below never touch `SimWorld`.
// ---------------------------------------------------------------------------

/// Stage the map's sim reads for the `Sync` schedule (vessel orbit
/// states + clock — everything else derives from `OrbitModelRes`).
pub(crate) fn capture_sync_input(sim: &SimWorld) -> MapSyncInput {
    // On-rails vessels carry their conic; an InBubble vessel's
    // osculating conic is fitted from its CoM state so the flying
    // vessel draws on the map too (its line re-samples every frame —
    // the `OrbitState` changes with the integration). Surface-latched
    // vessels stay skipped (a parked craft has no conic to draw).
    let (env, dominant) = (sim.environment(), sim.dominant_body());
    MapSyncInput {
        tick: sim.clock(),
        vessels: sim
            .vessels()
            .iter()
            .filter_map(|(id, v)| {
                // W2.4: the node label's burn-time estimate — Δv ÷
                // full-thrust accel over the stored craft (`None` for
                // craft-less/thrust-less vessels or no armed node).
                let burn_s = v.maneuver.as_ref().and_then(|n| {
                    let craft = v.craft.as_ref()?;
                    let mass: f64 = craft.parts.iter().map(|p| p.mass_kg).sum();
                    let thrust: f64 = craft
                        .parts
                        .iter()
                        .flat_map(|p| p.force_sites.iter())
                        .filter(|s| s.kind == csp_phys::ForceSiteKind::Thrust && s.rated_n > 0.0)
                        .map(|s| s.rated_n)
                        .sum();
                    (mass > 0.0 && thrust > 0.0)
                        .then(|| n.dv_magnitude() / (thrust / mass))
                        .filter(|s| s.is_finite())
                });
                if let Some(o) = v.orbit() {
                    return Some((*id, o.clone(), v.maneuver.clone(), true, burn_s));
                }
                let (env, dom, body) = (env?, dominant?, v.body()?);
                let rel = body.com_world() - env.body_center;
                let elements =
                    csp_sim::orbit::cartesian_to_elements(rel, body.vel, env.mu_m3_per_s2).ok()?;
                Some((
                    *id,
                    OrbitState {
                        parent: dom.clone(),
                        elements,
                        epoch: sim.clock(),
                    },
                    v.maneuver.clone(),
                    false,
                    burn_s,
                ))
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Math
// ---------------------------------------------------------------------------

/// A body's system-frame position: its parent-centered conic offset plus
/// the parent's position, recursively to the root (§2.8: `state_at`
/// positions are parent-centered; composing the chain is the caller's
/// job). `pub(crate)` for the flight view's sun-direction solve (R2).
pub(crate) fn body_system_pos(
    model: &PatchedConics,
    id: &BodyId,
    t: SimTime,
) -> Result<DVec3, csp_core::SimError> {
    let body = model.body_orbit(id)?;
    let (Some(parent), Some(elements)) = (&body.parent, &body.elements) else {
        return Ok(DVec3::ZERO); // the star is the frame origin
    };
    let state = OrbitState {
        parent: parent.clone(),
        elements: elements.clone(),
        epoch: SimTime::ZERO,
    };
    let (r, _v) = model.state_at(&state, t)?;
    Ok(body_system_pos(model, parent, t)? + r)
}

/// The velocity counterpart of [`body_system_pos`]: each link's
/// parent-centered `state_at` velocity composes the same way the
/// position does — the chain frames are inertial (translations
/// differ, never rotations). `pub(crate)` for the flight view's
/// rails-staged focus vessel (C19-F2): its `FlightBody::vel` is
/// dominant-frame inertial, so a non-dominant parent's conic velocity
/// needs the parent's system-frame velocity folded in.
pub(crate) fn body_system_vel(
    model: &PatchedConics,
    id: &BodyId,
    t: SimTime,
) -> Result<DVec3, csp_core::SimError> {
    let body = model.body_orbit(id)?;
    let (Some(parent), Some(elements)) = (&body.parent, &body.elements) else {
        return Ok(DVec3::ZERO); // the star is the frame's rest origin
    };
    let state = OrbitState {
        parent: parent.clone(),
        elements: elements.clone(),
        epoch: SimTime::ZERO,
    };
    let (_r, v) = model.state_at(&state, t)?;
    Ok(body_system_vel(model, parent, t)? + v)
}

/// Sample an orbit into a parent-centered polyline (§2.8 `state_at`).
///
/// Closed orbits (`e < 1 − E_PARABOLIC_TOL`, the sim's elliptic band):
/// `ORBIT_LINE_SAMPLES` segments uniform in
/// mean anomaly over one full period starting at `epoch` — first and
/// last sample coincide. Open orbits: samples uniform in *true* anomaly
/// over [`OPEN_ORBIT_NU_FILL`] of the asymptotic range — mean-anomaly
/// sampling would pile every point at periapsis. Samples landing before
/// sim-time 0 are skipped (`state_at` propagates backward fine, but the
/// integer clock cannot go negative).
///
/// `None` when the orbit cannot be sampled at all (bad μ, solver
/// failure at every sample) — callers treat `None` as "no line".
fn sample_orbit(model: &PatchedConics, state: &OrbitState) -> Option<Vec<DVec3>> {
    let mu = model.body_orbit(&state.parent).ok()?.mu_m3_per_s2;
    let e = state.elements.eccentricity;
    let a = state.elements.semi_major_axis_m;
    let epoch_ticks = state.epoch.ticks() as i64;

    let mut out = Vec::with_capacity(ORBIT_LINE_SAMPLES + 1);
    if e < 1.0 - E_PARABOLIC_TOL {
        if !(a > 0.0 && a.is_finite() && mu > 0.0) {
            return None;
        }
        let period = 2.0 * std::f64::consts::PI * f64::sqrt(a * a * a / mu);
        if !period.is_finite() {
            return None;
        }
        for i in 0..=ORBIT_LINE_SAMPLES {
            let t_ticks = epoch_ticks
                + (period * TICK_HZ as f64 * i as f64 / ORBIT_LINE_SAMPLES as f64).round() as i64;
            let Ok((r, _v)) = model.state_at(state, SimTime::from_ticks(t_ticks.max(0) as u64))
            else {
                continue;
            };
            if r.is_finite() {
                out.push(r);
            }
        }
    } else {
        if !(mu > 0.0 && a.is_finite() && a != 0.0) {
            return None;
        }
        let nu_max = if e > 1.0 + E_PARABOLIC_TOL {
            f64::acos(-1.0 / e) * OPEN_ORBIT_NU_FILL
        } else {
            // Parabolic encoding (e ≈ 1): the anomaly range is ±π.
            std::f64::consts::PI * OPEN_ORBIT_NU_FILL
        };
        let n = f64::sqrt(mu / f64::abs(a * a * a));
        for i in 0..=ORBIT_LINE_SAMPLES {
            let nu = -nu_max + 2.0 * nu_max * i as f64 / ORBIT_LINE_SAMPLES as f64;
            let m = mean_anomaly_of_true(&state.elements, nu);
            if !m.is_finite() {
                continue;
            }
            let dt_s = (m - state.elements.mean_anomaly_epoch_rad) / n;
            let t_ticks = epoch_ticks + (dt_s * TICK_HZ as f64).round() as i64;
            if t_ticks < 0 {
                continue; // the integer clock cannot go below zero
            }
            let Ok((r, _v)) = model.state_at(state, SimTime::from_ticks(t_ticks as u64)) else {
                continue;
            };
            if r.is_finite() {
                out.push(r);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Mean anomaly `M` corresponding to true anomaly `ν` — the inverse the
/// open-orbit sampler needs to place an `state_at` call on the conic.
/// (Presentation math: plain f64 — the determinism-domain libm rules do
/// not apply in csp-view.)
fn mean_anomaly_of_true(elements: &KeplerElements, nu: f64) -> f64 {
    let e = elements.eccentricity;
    if e < 1.0 - E_PARABOLIC_TOL {
        let ea = 2.0 * f64::atan(f64::sqrt((1.0 - e) / (1.0 + e)) * f64::tan(nu * 0.5));
        ea - e * f64::sin(ea)
    } else if e <= 1.0 + E_PARABOLIC_TOL {
        // Barker's equation (parabola): `M = (t + t³/3)/2` — the same
        // convention `csp_sim::orbit` uses (`state_at` reads this `M`
        // back; the un-halved form would double the sampled time).
        let t = f64::tan(nu * 0.5);
        0.5 * (t + t * t * t / 3.0)
    } else {
        // tanh(F/2) = sqrt((e−1)/(e+1)) · tan(ν/2);  M = e·sinh F − F.
        let f = 2.0 * f64::atanh(f64::sqrt((e - 1.0) / (e + 1.0)) * f64::tan(nu * 0.5));
        e * f64::sinh(f) - f
    }
}

/// Rotation `R = Rz(yaw) · Rx(pitch)` — camera basis (columns are the
/// view axes in world space). At yaw=pitch=0 the eye sits on +z looking
/// down onto the ecliptic.
pub(crate) fn rot3(yaw: f64, pitch: f64) -> [[f64; 3]; 3] {
    let (sy, cy) = f64::sin_cos(yaw);
    let (sp, cp) = f64::sin_cos(pitch);
    [
        [cy, -sy * cp, sy * sp],
        [sy, cy * cp, -cy * sp],
        [0.0, sp, cp],
    ]
}

pub(crate) fn mat3_vec(m: &[[f64; 3]; 3], v: DVec3) -> DVec3 {
    DVec3::new(
        m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z,
        m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z,
        m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z,
    )
}

/// Perspective projection: reverse-Z with an infinite far plane
/// (§2.3's depth contract: `[0,1]`, 1 = near). `clip.w = −z_view`, so
/// `clip.z = near` gives `ndc.z = near/(−z_view)` → 1 at the near plane,
/// →0 at infinity.
pub(crate) fn perspective_revz(fov_y: f64, aspect: f64, near: f64) -> [[f64; 4]; 4] {
    let f = 1.0 / f64::tan(fov_y * 0.5);
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, 0.0, near],
        [0.0, 0.0, -1.0, 0.0],
    ]
}

pub(crate) fn mat4_mul(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + a[i][3] * b[3][j];
        }
    }
    out
}

/// Solve the frame's map camera from [`MapInput`] + a focus point.
fn solve_camera(input: &MapInput, focus: DVec3) -> MapCameraF64 {
    solve_orbit_camera(
        input.yaw_rad,
        input.pitch_rad,
        input.distance_m,
        input.viewport_px,
        focus,
        MAP_FOV_Y,
        1e-3,
    )
}

/// The shared orbit-camera solve — `yaw`/`pitch` place the eye on the
/// R = Rz(yaw)·Rx(pitch) basis's third column at `distance_m` from
/// `focus`; `fov_y`/`near` are the reverse-Z projection's parameters
/// (§2.3). Used by the map (system frame) and the VAB (craft frame).
pub(crate) fn solve_orbit_camera(
    yaw_rad: f64,
    pitch_rad: f64,
    distance_m: f64,
    viewport_px: (f64, f64),
    focus: DVec3,
    fov_y: f64,
    near: f64,
) -> MapCameraF64 {
    let r = rot3(yaw_rad, pitch_rad);
    let eye = focus + DVec3::new(r[0][2], r[1][2], r[2][2]) * distance_m;
    let view = [
        [r[0][0], r[1][0], r[2][0], 0.0],
        [r[0][1], r[1][1], r[2][1], 0.0],
        [r[0][2], r[1][2], r[2][2], 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let aspect = (viewport_px.0 / viewport_px.1.max(1.0)).max(1e-6);
    let proj = perspective_revz(fov_y, aspect, near);
    MapCameraF64 {
        view_proj: mat4_mul(&proj, &view),
        eye,
        distance_m,
        valid: true,
    }
}

/// Project a system-frame world point to clip-space NDC — the §2.3
/// transform: the f64 camera-relative subtraction happens HERE, before
/// any f32 cast. `None` when the point is behind/on the eye plane or
/// non-finite (the caller skips it — lines drop the segment, markers
/// drop the quad).
fn project(cam: &MapCameraF64, world: DVec3) -> Option<[f32; 3]> {
    project_into(&cam.view_proj, cam.eye, world)
}

/// [`project`] over explicit camera parts — the VAB's extract shares
/// the f64-relative-subtract-then-f32 rule without borrowing the map's
/// camera resource type.
pub(crate) fn project_into(
    view_proj: &[[f64; 4]; 4],
    eye: DVec3,
    world: DVec3,
) -> Option<[f32; 3]> {
    let rel = world - eye; // f64 — the only world-frame math
    let m = view_proj;
    let clip = [
        m[0][0] * rel.x + m[0][1] * rel.y + m[0][2] * rel.z + m[0][3],
        m[1][0] * rel.x + m[1][1] * rel.y + m[1][2] * rel.z + m[1][3],
        m[2][0] * rel.x + m[2][1] * rel.y + m[2][2] * rel.z + m[2][3],
        m[3][0] * rel.x + m[3][1] * rel.y + m[3][2] * rel.z + m[3][3],
    ];
    // `clip.w = −z_view` — non-finite or ≤ eps means behind/on the eye
    // plane: unprojectable, not a real NDC point.
    if !clip[3].is_finite()
        || clip[3] <= CLIP_EPS
        || !clip[0].is_finite()
        || !clip[1].is_finite()
        || !clip[2].is_finite()
    {
        return None;
    }
    Some([
        (clip[0] / clip[3]) as f32,
        (clip[1] / clip[3]) as f32,
        (clip[2] / clip[3]) as f32,
    ])
}

/// Emit a screen-space quad (two triangles) centered on `ndc`, shaped
/// by `kind` (see `MapVertex::KIND_*` — solid/disc/ring/glow). Shaped
/// kinds get `uv` spanning `[-1,1]` across the quad so the fragment
/// shader can measure radius.
fn emit_shape(
    verts: &mut Vec<MapVertex>,
    ndc: [f32; 3],
    half_px: f64,
    vp: (f64, f64),
    color: [f32; 4],
    kind: f32,
) {
    let hx = (half_px * 2.0 / vp.0.max(1.0)) as f32;
    let hy = (half_px * 2.0 / vp.1.max(1.0)) as f32;
    let [x, y, z] = ndc;
    let corners = [
        ([x - hx, y - hy, z], [-1.0f32, -1.0]),
        ([x + hx, y - hy, z], [1.0, -1.0]),
        ([x + hx, y + hy, z], [1.0, 1.0]),
        ([x - hx, y + hy, z], [-1.0, 1.0]),
    ];
    for idx in [0usize, 1, 2, 0, 2, 3] {
        verts.push(MapVertex {
            pos: corners[idx].0,
            color,
            uv: corners[idx].1,
            kind,
        });
    }
}

/// The backdrop starfield — `STAR_COUNT` points on a fixed hash-seeded
/// sphere around the eye. Deterministic per index, so the sky is
/// identical every frame with no stored table and no RNG state.
///
/// Stars draw as crisp POINTS (the flight sky's procedural field is
/// sharp 1–2 px stars, and this matches it): a hard `KIND_DISC` core
/// of ~2–4 px whose brightness bends with the magnitude hash — most
/// faint, a few burning. The old field gave every star a 3–8 px
/// `KIND_GLOW` quadratic puff at low alpha; at map scale those read as
/// large grey blobs, not stars. Only the brightest few get a tight
/// halo on top of the core.
fn emit_starfield(verts: &mut Vec<MapVertex>, cam: &MapCameraF64, vp: (f64, f64)) {
    for i in 0..STAR_COUNT as u64 {
        // Uniform direction on the sphere (z = 2u−1, φ = 2πv) plus two
        // spare uniforms for size/brightness.
        let u_dir = splitmix01(i.wrapping_mul(4) + 1);
        let u_phi = splitmix01(i.wrapping_mul(4) + 2);
        let u_size = splitmix01(i.wrapping_mul(4) + 3);
        let u_tint = splitmix01(i.wrapping_mul(4) + 4);
        let z = 2.0 * u_dir - 1.0;
        let phi = 2.0 * std::f64::consts::PI * u_phi;
        let s = f64::sqrt((1.0 - z * z).max(0.0));
        let dir = DVec3::new(s * f64::cos(phi), s * f64::sin(phi), z);
        let Some(ndc) = project(cam, cam.eye + dir * STAR_DISTANCE_M) else {
            continue;
        };
        // Mostly blue-white; a warm tail keeps the field from reading
        // as uniform noise.
        let (r, g, b): (f32, f32, f32) = if u_tint < 0.12 {
            (1.0, 0.82, 0.62)
        } else if u_tint < 0.28 {
            (0.68, 0.8, 1.0)
        } else {
            (0.92, 0.94, 1.0)
        };
        // Bent magnitude — most stars stay small and dim, a few carry
        // the field. Squaring `u_size` keeps the tail sparse.
        let mag = u_size * u_size;
        // A ~1 px half-size disc is a sharp point, not a puff; bright
        // stars run a little larger and a lot hotter.
        let half_px = 0.9 + 1.1 * mag;
        let alpha = (0.4 + 0.6 * mag) as f32;
        emit_shape(
            verts,
            ndc,
            half_px,
            vp,
            [r, g, b, alpha],
            MapVertex::KIND_DISC,
        );
        // The rare bright star gets a tight halo (~4× the core) so it
        // reads as a star and not a pixel — ~8% of the field.
        if u_size > 0.92 {
            emit_shape(
                verts,
                ndc,
                half_px * 4.0,
                vp,
                [r, g, b, alpha * 0.3],
                MapVertex::KIND_GLOW,
            );
        }
    }
}

/// splitmix64's finalizer over a seed, rescaled to `[0,1)` — a
/// stateless per-index hash so the starfield needs no table.
fn splitmix01(seed: u64) -> f64 {
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 11) as f64 * (1.0 / 9.007_199_254_740_992e15)
}

/// Emit a polyline as per-segment quads in screen space — the honest
/// substitute for line primitives (which have no width control) —
/// with a small disc at every interior vertex so segment joints don't
/// crack on tight curves. Points are world-frame, `offset` is added in
/// f64 before projection; segments whose either endpoint fails
/// [`project`] are skipped.
fn emit_polyline(
    verts: &mut Vec<MapVertex>,
    points: &[DVec3],
    offset: DVec3,
    cam: &MapCameraF64,
    vp: (f64, f64),
    color: [f32; 4],
    half_px: f64,
) {
    let mut prev: Option<[f32; 3]> = None;
    for p in points {
        let ndc = project(cam, *p + offset);
        let (Some(a), Some(b)) = (prev, ndc) else {
            prev = ndc;
            continue;
        };
        prev = ndc;
        // Perpendicular in pixel space so the width is constant on screen.
        // Viewport-dimension guards clamp at 1.0 everywhere in this file
        // (emit_shape, MapInput::apply, solve_camera) — they exist only to
        // avoid div-by-zero on a zero-size viewport, so use the same bound.
        let fx = vp.0.max(1.0) / 2.0;
        let fy = vp.1.max(1.0) / 2.0;
        let dx = f64::from(b[0] - a[0]) * fx;
        let dy = f64::from(b[1] - a[1]) * fy;
        let len = f64::sqrt(dx * dx + dy * dy);
        if len < 1e-9 {
            continue;
        }
        let ox = (-dy / len * half_px / fx) as f32;
        let oy = (dx / len * half_px / fy) as f32;
        let quad = [
            [a[0] + ox, a[1] + oy, a[2]],
            [a[0] - ox, a[1] - oy, a[2]],
            [b[0] - ox, b[1] - oy, b[2]],
            [b[0] + ox, b[1] + oy, b[2]],
        ];
        for idx in [0usize, 1, 2, 0, 2, 3] {
            verts.push(MapVertex {
                pos: quad[idx],
                color,
                uv: [0.0, 0.0],
                kind: MapVertex::KIND_SOLID,
            });
        }
        // Round join — a disc of the line's half-width at the shared
        // vertex covers the seam between per-segment quads.
        emit_shape(verts, a, half_px, vp, color, MapVertex::KIND_DISC);
    }
    if let Some(last) = prev {
        emit_shape(verts, last, half_px, vp, color, MapVertex::KIND_DISC);
    }
}

// ---------------------------------------------------------------------------
// View-side plumbing (resource construction)
// ---------------------------------------------------------------------------

/// Build the map resources for [`crate::View::with_sim`] — the body
/// table from the sim's orbit model plus the resources the schedules
/// drive.
pub(crate) fn build_resources(sim: &SimWorld) -> (BodyTable, MapInput, OrbitModelRes) {
    let table = BodyTable::from_model(sim.orbits());
    let input = MapInput::new(&table);
    let model = OrbitModelRes(sim.orbits().clone());
    (table, input, model)
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use csp_data::BodyId;
    use csp_sim::SimCommand;
    use csp_sim::orbit::BodyOrbit;
    use csp_sim::{SourceClass, World as SimWorld};

    // §3.0 home-system scale constants (same values as csp-sim fixtures).
    const MU_SOLEN: f64 = 1.1723e18;
    const MU_TERRIN: f64 = 3.5316e12;
    const MU_RASK: f64 = 6.5138e10;
    const TERRIN_A: f64 = 1.36e10;
    const RASK_A: f64 = 1.2e7;

    fn circular_elements(a: f64, m0: f64) -> KeplerElements {
        KeplerElements {
            semi_major_axis_m: a,
            eccentricity: 0.0,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: m0,
            period_s: f64::INFINITY, // the model never reads this
        }
    }

    /// sol → p (Terrin-like) → m (Rask-like moon).
    fn test_system() -> PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("p"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(8.4e7),
                parent: Some(BodyId::from("sol")),
                elements: Some(circular_elements(TERRIN_A, 0.0)),
            },
        );
        bodies.insert(
            BodyId::from("m"),
            BodyOrbit {
                mu_m3_per_s2: MU_RASK,
                soi_m: Some(2.43e6),
                parent: Some(BodyId::from("p")),
                elements: Some(circular_elements(RASK_A, 0.0)),
            },
        );
        PatchedConics::new(bodies)
    }

    fn map_view() -> crate::View {
        crate::View::with_sim(SimWorld::with_system(test_system()))
    }

    /// Closed-orbit sampling: `ORBIT_LINE_SAMPLES + 1` points over one
    /// full period, first ≈ last, all at the orbit radius for a circle.
    #[test]
    fn closed_orbit_sampling_spans_the_period() {
        let model = test_system();
        let state = OrbitState {
            parent: BodyId::from("sol"),
            elements: circular_elements(TERRIN_A, 0.7),
            epoch: SimTime::ZERO,
        };
        let line = sample_orbit(&model, &state).expect("closed orbit samples");
        assert_eq!(line.len(), ORBIT_LINE_SAMPLES + 1);
        let first = line[0];
        let last = *line.last().unwrap();
        // One full period closes the loop — within solver precision.
        assert!(
            first.distance(last) < TERRIN_A * 1e-6,
            "closed polyline must return to its start: {:?} vs {:?}",
            first,
            last
        );
        for p in &line {
            assert!((p.norm() - TERRIN_A).abs() < TERRIN_A * 1e-6);
        }
    }

    /// An eccentric closed orbit still closes and samples count is sane.
    #[test]
    fn eccentric_orbit_closes() {
        let model = test_system();
        let mut el = circular_elements(8.4e7 * 0.4, 0.0);
        el.eccentricity = 0.5;
        let state = OrbitState {
            parent: BodyId::from("p"),
            elements: el,
            epoch: SimTime::ZERO,
        };
        let line = sample_orbit(&model, &state).expect("ellipse samples");
        assert_eq!(line.len(), ORBIT_LINE_SAMPLES + 1);
        // r must stay inside [r_p, r_a] = a(1∓e).
        let a = 8.4e7 * 0.4;
        for p in &line {
            assert!(p.norm() > a * 0.49 && p.norm() < a * 1.51);
        }
    }

    /// Open (hyperbolic) orbits sample a bounded polyline within the
    /// fill fraction's radius envelope — never a period (none exists).
    #[test]
    fn open_orbit_sampling_is_bounded() {
        let model = test_system();
        let e_v = 1.5;
        let r_p = 7.0e5;
        let el = KeplerElements {
            semi_major_axis_m: -r_p / (e_v - 1.0),
            eccentricity: e_v,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: 0.0,
            period_s: f64::INFINITY,
        };
        let state = OrbitState {
            parent: BodyId::from("p"),
            elements: el.clone(),
            epoch: SimTime::ZERO,
        };
        let line = sample_orbit(&model, &state).expect("hyperbola samples");
        assert!(line.len() > ORBIT_LINE_SAMPLES / 2);
        // All points lie on the conic: r ≥ r_p, r ≤ r(ν_max).
        let p_latus = el.semi_major_axis_m.abs() * (e_v * e_v - 1.0);
        let nu_max = f64::acos(-1.0 / e_v) * OPEN_ORBIT_NU_FILL;
        let r_max = p_latus / (1.0 + e_v * f64::cos(nu_max));
        for pt in &line {
            assert!(pt.norm() >= r_p * 0.999);
            assert!(pt.norm() <= r_max * 1.001);
        }
        // Epoch 0 → the inbound leg (ν < 0 ⇒ t < 0) cannot be sampled
        // (integer clock), so the line runs from periapsis (x = r_p > 0)
        // out along the escape asymptote (ν_max > 90° ⇒ x < 0).
        assert!(line.first().unwrap().x > 0.0);
        assert!(line.last().unwrap().x < 0.0);
    }

    /// A parabolic-encoded orbit (`e ≈ 1`, `a := −p`) samples a finite
    /// line too — the Barker branch of `mean_anomaly_of_true`.
    #[test]
    fn parabolic_orbit_sampling_is_bounded() {
        let model = test_system();
        let r_p = 7.0e5;
        let el = KeplerElements {
            semi_major_axis_m: -2.0 * r_p, // a := −p
            eccentricity: 1.0,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: 0.0,
            period_s: f64::INFINITY,
        };
        let state = OrbitState {
            parent: BodyId::from("p"),
            elements: el,
            epoch: SimTime::ZERO,
        };
        let line = sample_orbit(&model, &state).expect("parabola samples");
        assert!(line.len() > ORBIT_LINE_SAMPLES / 2);
        for pt in &line {
            assert!(pt.norm() >= r_p * 0.99);
        }
    }

    /// Regression: a parabolic-encoded orbit (`a := −p < 0`) whose `e`
    /// lands *just under* 1.0 — inside the sim's `|e − 1| ≤ 1e-9`
    /// parabolic band (`csp_sim::orbit::E_PARABOLIC_TOL`) — must take
    /// the open branch. Classifying on raw `e < 1.0` sent it to the
    /// closed branch, where the `a > 0` check rejected the `−p`
    /// encoding and `sample_orbit` returned `None` — no orbit line.
    #[test]
    fn parabolic_e_sliver_still_samples() {
        let model = test_system();
        let r_p = 7.0e5;
        let el = KeplerElements {
            semi_major_axis_m: -2.0 * r_p, // a := −p
            // 5e-16 below 1.0 — inside the parabolic band, but `e < 1.0`
            // is still literally true.
            eccentricity: 1.0 - 5e-16,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: 0.0,
            period_s: f64::INFINITY,
        };
        let state = OrbitState {
            parent: BodyId::from("p"),
            elements: el,
            epoch: SimTime::ZERO,
        };
        let line = sample_orbit(&model, &state).expect("parabolic sliver samples");
        assert!(line.len() > ORBIT_LINE_SAMPLES / 2);
        for pt in &line {
            assert!(pt.norm() >= r_p * 0.99);
        }
    }

    /// Focus cycling wraps over the body table and resets pan; the star
    /// (no SOI) gets a whole-system distance.
    #[test]
    fn focus_cycle_wraps_and_refocuses() {
        let table = BodyTable::from_model(&test_system());
        // table order = BTreeMap order: m, p, sol.
        assert_eq!(
            table
                .bodies
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>(),
            vec!["m", "p", "sol"]
        );
        assert_eq!(table.bodies[table.default_focus].id.as_str(), "sol");
        let mut input = MapInput::new(&table);
        input.mode = MapMode::Map;
        input.apply(
            MapKeyEvent::Pan {
                dx_px: 100.0,
                dy_px: 0.0,
            },
            &table,
            &MapSnapshot::default(),
        );
        assert!(input.pan_offset.norm() > 0.0);
        let start = input.focus; // the star — last in table order
        input.apply(MapKeyEvent::CycleFocus, &table, &MapSnapshot::default());
        assert_eq!(input.focus, (start + 1) % table.bodies.len());
        assert_eq!(input.pan_offset, DVec3::ZERO); // refocus resets pan
        for _ in 0..table.bodies.len() {
            input.apply(MapKeyEvent::CycleFocus, &table, &MapSnapshot::default());
        }
        assert_eq!(input.focus, (start + 1) % table.bodies.len()); // wrap + 1
        // CycleFocus is a no-op in Flight mode.
        input.mode = MapMode::Flight;
        input.apply(MapKeyEvent::CycleFocus, &table, &MapSnapshot::default());
        assert_eq!(input.focus, (start + 1) % table.bodies.len());
    }

    /// The mode toggle flips both ways.
    #[test]
    fn mode_toggles() {
        let table = BodyTable::from_model(&test_system());
        let mut input = MapInput::new(&table);
        assert_eq!(input.mode, MapMode::Flight);
        input.apply(MapKeyEvent::ToggleMode, &table, &MapSnapshot::default());
        assert_eq!(input.mode, MapMode::Map);
        input.apply(MapKeyEvent::ToggleMode, &table, &MapSnapshot::default());
        assert_eq!(input.mode, MapMode::Flight);
    }

    /// Click-to-focus lands on the clicked body/vessel and recenters
    /// pan — the "can't change ref" path that doesn't need the
    /// focus-cycle key at all.
    #[test]
    fn focus_events_retarget_the_camera() {
        let table = BodyTable::from_model(&test_system());
        let mut input = MapInput::new(&table);
        input.mode = MapMode::Map;
        // A click on 'p' takes the focus there and clears pan.
        input.pan_offset = DVec3::new(1.0, 2.0, 3.0);
        let p = table.index_of(&BodyId::from("p")).unwrap();
        input.apply(MapKeyEvent::FocusBody(p), &table, &MapSnapshot::default());
        assert_eq!(input.focus, p);
        assert_eq!(input.pan_offset, DVec3::ZERO);
        assert_eq!(input.distance_m, default_distance(&table, p));
        // A click on a vessel focuses the vessel-half of the cycle and
        // anchors the id — a snapshot re-key can't retarget it.
        let vid = VesselId(7);
        let snap = MapSnapshot {
            tick: SimTime::ZERO,
            vessels: vec![(
                vid,
                OrbitState {
                    parent: BodyId::from("p"),
                    elements: circular_elements(6.75e5, 0.0),
                    epoch: SimTime::ZERO,
                },
                None,
                true,
                None,
            )],
        };
        input.apply(MapKeyEvent::FocusVessel(vid), &table, &snap);
        assert_eq!(input.vessel_focus(&table), Some(0));
        assert_eq!(input.focus_vessel, Some(vid));
        // An id not in the snapshot does nothing — the click raced a
        // despawn and must not retarget onto a different vessel.
        input.apply(MapKeyEvent::FocusVessel(VesselId(99)), &table, &snap);
        assert_eq!(input.focus_vessel, Some(vid));
        // Out-of-range body clicks are ignored; clicks do nothing in
        // flight mode.
        input.apply(MapKeyEvent::FocusBody(usize::MAX), &table, &snap);
        assert_eq!(input.focus_vessel, Some(vid));
        input.mode = MapMode::Flight;
        input.apply(MapKeyEvent::FocusBody(p), &table, &snap);
        assert_eq!(input.focus_vessel, Some(vid));
    }

    /// Zoom-toward-cursor: the world point under the cursor stays
    /// under the cursor — the look-at offset shifts toward it by
    /// `1 − new/old` of its focus-plane offset.
    #[test]
    fn zoom_tracks_the_cursor_point() {
        let table = BodyTable::from_model(&test_system());
        let mut input = MapInput::new(&table);
        input.mode = MapMode::Map;
        input.viewport_px = (1000.0, 1000.0);
        // yaw = pitch = 0 ⇒ camera basis is the identity: screen x is
        // world +x, screen up is world +y.
        input.yaw_rad = 0.0;
        input.pitch_rad = 0.0;
        input.distance_m = 1.0e10;
        // Cursor at the right edge, vertically centered → the point
        // under it sits at world offset (+half_w, 0, 0) on the
        // focus plane.
        input.cursor_px = Some((1000.0, 500.0));
        input.apply(
            MapKeyEvent::Zoom { factor: 0.5 },
            &table,
            &MapSnapshot::default(),
        );
        assert_eq!(input.distance_m, 5.0e9);
        let half_w = 1.0e10 * f64::tan(MAP_FOV_Y * 0.5);
        let expected = DVec3::new(half_w * 0.5, 0.0, 0.0);
        let d = (input.pan_offset - expected).norm() / half_w;
        assert!(d < 1e-9, "pan moved toward the cursor point: {d}");
        // No cursor → the distance still zooms, pan stays put.
        input.cursor_px = None;
        input.apply(
            MapKeyEvent::Zoom { factor: 0.5 },
            &table,
            &MapSnapshot::default(),
        );
        assert_eq!(input.distance_m, 2.5e9);
        assert_eq!(input.pan_offset, expected);
    }

    /// The apsis geometry: a circular orbit's Pe/Ap are both `a` out
    /// and antiparallel; an eccentric conic spans [a(1−e), a(1+e)];
    /// a hyperbola has no Ap at ν = π.
    #[test]
    fn apsis_points_match_the_conic() {
        let el = circular_elements(1.0e7, 0.0);
        let pe = apsis_point(&el, 0.0).unwrap();
        let ap = apsis_point(&el, std::f64::consts::PI).unwrap();
        assert!((pe.norm() - 1.0e7).abs() < 1.0);
        assert!((ap.norm() - 1.0e7).abs() < 1.0);
        assert!(pe.dot(ap) < 0.0);
        let mut ecc = circular_elements(1.0e7, 0.0);
        ecc.eccentricity = 0.5;
        assert!((apsis_point(&ecc, 0.0).unwrap().norm() - 5.0e6).abs() < 1.0);
        assert!((apsis_point(&ecc, std::f64::consts::PI).unwrap().norm() - 1.5e7).abs() < 1.0);
        let mut hyp = circular_elements(-2.0e6, 0.0); // a < 0
        hyp.eccentricity = 1.5;
        // p = |a(1−e²)| = 2.5e6; r_p = p/(1+e) = 1.0e6.
        assert!((apsis_point(&hyp, 0.0).unwrap().norm() - 1.0e6).abs() < 1.0);
        assert!(apsis_point(&hyp, std::f64::consts::PI).is_none());
    }

    /// Zoom clamps at both ends; orbit clamps pitch.
    #[test]
    fn zoom_and_orbit_clamps() {
        let table = BodyTable::from_model(&test_system());
        let mut input = MapInput::new(&table);
        for _ in 0..100 {
            input.apply(
                MapKeyEvent::Zoom { factor: 0.5 },
                &table,
                &MapSnapshot::default(),
            );
        }
        assert_eq!(input.distance_m, MIN_DISTANCE_M);
        for _ in 0..200 {
            input.apply(
                MapKeyEvent::Zoom { factor: 2.0 },
                &table,
                &MapSnapshot::default(),
            );
        }
        assert_eq!(input.distance_m, MAX_DISTANCE_M);
        for _ in 0..100 {
            input.apply(
                MapKeyEvent::Orbit {
                    dyaw: 0.0,
                    dpitch: 1.0,
                },
                &table,
                &MapSnapshot::default(),
            );
        }
        assert_eq!(input.pitch_rad, PITCH_LIMIT);
    }

    /// §2.3's core test: at system scale (~1.4e10 m) the camera-relative
    /// subtraction must happen in f64 — a point a few km from the focus
    /// must project to well-conditioned NDC magnitudes, and a point near
    /// the eye must be REJECTED (behind-camera / near-plane), not cast.
    #[test]
    fn camera_relative_projection_at_system_scale() {
        let table = BodyTable::from_model(&test_system());
        let mut input = MapInput::new(&table);
        input.mode = MapMode::Map;
        // Focus on terrin-analog 'p' at 1.36e10 m.
        input.focus = table.index_of(&BodyId::from("p")).unwrap();
        input.distance_m = 1.0e9;
        let focus_pos = DVec3::new(TERRIN_A, 0.0, 0.0);
        let cam = solve_camera(&input, focus_pos);
        // A point 1 km off the focus — 10 orders below the system scale.
        let near_focus = focus_pos + DVec3::new(1.0e3, 0.0, 0.0);
        let ndc = project(&cam, near_focus).expect("near-focus point projects");
        for v in ndc {
            assert!(v.is_finite() && v.abs() < 10.0);
        }
        // The focus itself is at screen center.
        let c = project(&cam, focus_pos).unwrap();
        assert!(c[0].abs() < 1e-6 && c[1].abs() < 1e-6);
        // A point between eye and near bound must not project.
        let behind = focus_pos + DVec3::new(0.0, 0.0, input.distance_m * 1.5);
        assert!(project(&cam, behind).is_none());
    }

    /// End-to-end: a `View` with a spawned vessel runs the frame
    /// schedules headless and produces a non-empty map packet.
    #[test]
    fn map_packet_extracts_headless() {
        let mut view = map_view();
        // Spawn an on-rails vessel (LEO-ish around 'p').
        view.enqueue_command(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("p"),
                elements: circular_elements(6.75e5, 0.0),
            },
        );
        view.map_event(MapKeyEvent::ToggleMode);
        // The §2.5 frame shape, headless: Input applies the queued map
        // event, SimStep drains the spawn, the map stages flow through
        // Sync → Presentation → RenderExtract.
        view.begin_frame();
        view.sim_step(1).unwrap();
        view.end_frame();
        assert_eq!(view.map_mode(), MapMode::Map);
        let packet = view.take_map_packet().expect("map mode emits a packet");
        assert!(!packet.markers.vertices.is_empty());
        assert!(!packet.lines.vertices.is_empty());
        assert!(!packet.bg.vertices.is_empty(), "the starfield draws");
        for v in packet
            .markers
            .vertices
            .iter()
            .chain(packet.lines.vertices.iter())
            .chain(packet.bg.vertices.iter())
        {
            assert!(v.pos.iter().all(|c| c.is_finite()));
        }
        // 3 bodies × (glow + disc) + the focused star's ring + 1 vessel
        // × (glow + disc) = 9 quads × 6 verts.
        assert_eq!(packet.markers.vertices.len(), 9 * 6);
    }

    /// C12-F2: the polyline cache must actually hit. A breakup-scale
    /// fleet of InBubble-style orbits — freshly fitted each frame, so
    /// `epoch` advances every call — resamples each line ONCE, then
    /// holds it while only `epoch` and sub-quantization noise move.
    /// Before the quantized key, every frame re-solved ~65 × 129
    /// `state_at`s (~8.4k Kepler solves per map frame).
    #[test]
    fn in_bubble_fleet_does_not_resample_every_frame() {
        let model = test_system();
        let fleet: Vec<OrbitState> = (0..64)
            .map(|i| OrbitState {
                parent: BodyId::from("p"),
                elements: circular_elements(7.0e5 + i as f64 * 1.0e3, i as f64 * 0.01),
                epoch: SimTime::from_ticks(10_000),
            })
            .collect();
        let mut scene = MapScene::default();
        let snap = |tick: u64, fleet: &[OrbitState]| MapSnapshot {
            tick: SimTime::from_ticks(tick),
            vessels: fleet
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    let mut o = o.clone();
                    // The staged InBubble conic refits each frame:
                    // epoch = now, elements wobble below the quanta.
                    o.epoch = SimTime::from_ticks(tick);
                    o.elements.mean_anomaly_epoch_rad += 1e-5;
                    (VesselId(i as u64), o, None, false, None)
                })
                .collect(),
        };
        for frame in 0..3u64 {
            let s = snap(10_000 + frame, &fleet);
            refresh_vessel_lines(&model, &s, &mut scene);
        }
        // 64 vessels, one resample each (the first frame) — the next
        // two frames moved only `epoch` (within its bucket) and `m0`
        // (below 1e-3 rad), so every cached line still carries the
        // FIRST frame's state.
        assert_eq!(scene.vessel_lines.len(), 64);
        for (i, entry) in &scene.vessel_lines {
            assert_eq!(
                entry.state.epoch,
                SimTime::from_ticks(10_000),
                "vessel {} resampled on an epoch/drift-only frame",
                i.0
            );
            assert!(entry.line.is_some());
        }
        // A real conic change still invalidates.
        let mut moved = fleet[0].clone();
        moved.epoch = SimTime::from_ticks(10_002);
        moved.elements.semi_major_axis_m *= 1.01;
        let s = MapSnapshot {
            tick: SimTime::from_ticks(10_002),
            vessels: vec![(VesselId(0), moved, None, false, None)],
        };
        refresh_vessel_lines(&model, &s, &mut scene);
        assert_eq!(
            scene.vessel_lines[&VesselId(0)].state.epoch,
            SimTime::from_ticks(10_002),
            "a genuine element change must resample"
        );
    }

    /// Pass-1: a physics-resident (InBubble) vessel is staged into
    /// `MapSyncInput` via its osculating conic — the map draws the
    /// flying vessel, not just on-rails ones.
    #[test]
    fn in_bubble_vessel_appears_in_map_sync() {
        let env = csp_phys::Environment {
            body_center: DVec3::ZERO,
            radius_m: 6.0e5,
            mu_m3_per_s2: MU_TERRIN,
            body_spin: DVec3::ZERO,
            atmosphere: None,
            settle_v: 0.05,
            settle_w: 0.05,
            settle_ticks: 128,
            friction_mu: 0.4,
            drag_cd: 1.0,
            terrain: None,
        };
        let mut sim = SimWorld::with_physics(
            test_system(),
            csp_sim::PhysicsConfig {
                environment: env,
                dominant: BodyId::from("p"),
                bubble_radius_m: csp_sim::DEFAULT_BUBBLE_RADIUS_M,
            },
            1024,
        );
        let part = csp_phys::WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            parachute: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: 100.0,
            com: DVec3::ZERO,
            inertia: csp_phys::SymMat3::solid_box(100.0, DVec3::new(0.5, 0.5, 0.5)),
            collider: csp_phys::ColliderShape::Box {
                half_extents: DVec3::new(0.5, 0.5, 0.5),
            },
            force_sites: Vec::new(),
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: csp_data::ThermalDef {
                heat_capacity_j_per_k: 400.0,
                max_temp_k: 1_200.0,
                emissivity: 0.8,
                conduction_w_per_k: 50.0,
            },
        };
        let mut graph = csp_phys::PartGraph::new(csp_phys::PartNodeId(0));
        graph.nodes.insert(
            csp_phys::PartNodeId(0),
            csp_phys::PartNode::new(None, csp_phys::PartTransform::IDENTITY, 0),
        );
        let r = 6.0e5 + 100_000.0;
        sim.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnPhysicalVessel {
                craft: csp_sim::CraftDef {
                    hash_memo: Default::default(),
                    part_ids: vec![],
                    graph,
                    part_costs: vec![0.0],
                    parts: vec![part],
                    reliability: vec![],
                },
                pos: DVec3::new(r, 0.0, 0.0),
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::new(0.0, (MU_TERRIN / r).sqrt(), 0.0),
                ang_vel: DVec3::ZERO,
            },
        );
        sim.step_frame(1).unwrap();
        let staged = capture_sync_input(&sim);
        assert_eq!(
            staged.vessels.len(),
            1,
            "the InBubble vessel must be staged for the map"
        );
        let (id, orbit, ..) = &staged.vessels[0];
        assert_eq!(*id, csp_sim::VesselId(0));
        assert_eq!(orbit.parent.as_str(), "p");
        assert_eq!(orbit.epoch, staged.tick);
        assert!(orbit.elements.semi_major_axis_m > 0.0);
        // The osculating conic places it at the spawned altitude.
        let (pos, _) = sim
            .orbits()
            .state_at(orbit, staged.tick)
            .expect("conic solves at its epoch");
        assert!((pos.norm() - r).abs() < 1.0);
    }

    // ---- W2.4: planning surface --------------------------------------------

    /// solen → terrin + duva — the home system's two planets as a
    /// sibling pair under the star (the transfer-planner fixture).
    fn transfer_system() -> PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("solen"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("terrin"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(8.4e7),
                parent: Some(BodyId::from("solen")),
                elements: Some(circular_elements(1.36e10, 0.0)),
            },
        );
        bodies.insert(
            BodyId::from("duva"),
            BodyOrbit {
                mu_m3_per_s2: 3.0137e11,
                soi_m: Some(4.79e7),
                parent: Some(BodyId::from("solen")),
                elements: Some(circular_elements(2.07e10, 1.0)),
            },
        );
        let mut model = PatchedConics::new(bodies);
        model.set_body_radius(&BodyId::from("terrin"), 6.0e5);
        model.set_body_radius(&BodyId::from("duva"), 3.2e5);
        model
    }

    /// W2.4 gate: `plan_transfer` is reachable from the map — a
    /// `FindWindow` panel command resolves against the orbit model
    /// and stages a real plan (departure tick, Δv, conic) for the
    /// player to act on.
    #[test]
    fn plan_transfer_reachable_through_the_panel() {
        let model = transfer_system();
        let snap = MapSnapshot {
            tick: SimTime::ZERO,
            vessels: Vec::new(),
        };
        let mut planning = MapPlanning::default();
        let mut out = Vec::new();
        resolve_planning_command(
            csp_ui::PlanningCommand::FindWindow {
                origin: "terrin".to_string(),
                destination: "duva".to_string(),
            },
            &model,
            &snap,
            None,
            None,
            &mut planning,
            &mut out,
        );
        let plan = planning
            .plan
            .as_ref()
            .unwrap_or_else(|| panic!("no plan staged — err: {:?}", planning.plan_error));
        assert!(plan.plan.departure_tick > 0);
        assert!(plan.plan.arrival_tick > plan.plan.departure_tick);
        assert!(plan.plan.leg.total_dv_mps.is_finite() && plan.plan.leg.total_dv_mps > 0.0);
        // The planned route samples into a polyline for the map.
        assert!(
            sample_orbit(&model, &plan.plan.conic).is_some(),
            "the plan's transfer conic must draw"
        );
    }

    /// W2.4 gate: a `SetAlarm` the player issued fires its
    /// `PlayerAlarm` event and the planning mirror surfaces it — the
    /// armed book entry flips to fired, and a fire the book never
    /// armed still records a row.
    #[test]
    fn player_alarm_surfaces_in_the_planning_mirror() {
        let mut view = map_view();
        let tick = 128u64;
        // Arm through the panel path: book entry + SimCommand, the
        // same pair resolve_planning_command produces.
        {
            let model = view.world.resource::<OrbitModelRes>().0.clone();
            let mut planning = view.world.resource_mut::<MapPlanning>();
            let mut out = Vec::new();
            resolve_planning_command(
                csp_ui::PlanningCommand::ArmAlarm {
                    label: "mid-course".to_string(),
                    in_s: tick as f64 / TICK_HZ as f64,
                },
                &model,
                &MapSnapshot {
                    tick: SimTime::ZERO,
                    vessels: Vec::new(),
                },
                None,
                None,
                &mut planning,
                &mut out,
            );
            assert_eq!(out.len(), 1);
            for cmd in out {
                view.enqueue_command(SourceClass::Ui, 1, cmd);
            }
        }
        // Also enqueue an alarm the book never armed — it must still
        // surface as a fired row.
        view.enqueue_command(
            SourceClass::Ui,
            1,
            SimCommand::SetAlarm {
                tick,
                label: "unarmed".to_string(),
            },
        );
        view.sim_step(tick + 2).expect("advance past the alarm");
        let planning = view.world.resource::<MapPlanning>();
        let armed = planning
            .alarms
            .iter()
            .find(|a| a.label == "mid-course")
            .expect("the armed entry exists");
        // ArmAlarm schedules at `in_s + 1 tick` — the same +1 the
        // sim's clamp applies to at-now requests.
        assert_eq!(armed.fired, Some(tick + 1), "the armed alarm fired");
        let unarmed = planning
            .alarms
            .iter()
            .find(|a| a.label == "unarmed")
            .expect("an unarmed fire still records a row");
        assert!(unarmed.fired.is_some());
        // And the rows read as fired, not armed or lapsed.
        let rows = view
            .world
            .resource_mut::<MapPlanning>()
            .alarm_rows(tick + 2);
        assert!(rows.iter().all(|r| r.state == csp_ui::AlarmState::Fired));
    }

    /// The book's lapse rule: an armed entry whose tick passes with
    /// no observed fire reports "lapsed" — never "fired".
    #[test]
    fn alarm_rows_lapse_when_nothing_fired() {
        let mut planning = MapPlanning::default();
        planning.alarms.push(AlarmEntry {
            tick: 64,
            label: "never landed".to_string(),
            kind: AlarmKind::Manual,
            fired: None,
            lapsed: false,
        });
        let rows = planning.alarm_rows(64 + ALARM_LAPSE_TICKS + 1);
        assert_eq!(rows[0].state, csp_ui::AlarmState::Lapsed);
    }
}
