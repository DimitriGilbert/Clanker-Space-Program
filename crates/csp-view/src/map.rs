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
use csp_core::{DVec3, SimTime, TICK_HZ};
use csp_data::{BodyId, KeplerElements};
use csp_render::map::{MapMesh, MapPacket, MapVertex};
use csp_sim::orbit::{OrbitModel, OrbitState, PatchedConics};
use csp_sim::{ManeuverNode, VesselId, World as SimWorld};

// ---------------------------------------------------------------------------
// Tunables (presentation constants — §8.3 owns sim tuning; these are view
// geometry, not balance).
// ---------------------------------------------------------------------------

/// Segments per sampled orbit line.
pub const ORBIT_LINE_SAMPLES: usize = 128;
/// Half-width of the quad-stripped orbit lines.
const LINE_HALF_WIDTH_PX: f64 = 0.75;
/// Screen-space marker half-sizes.
const BODY_MARKER_PX: f64 = 5.0;
/// Vessels draw smaller than bodies.
const VESSEL_MARKER_PX: f64 = 3.5;
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
/// Color for all vessel markers/lines at M1 (bodies get per-index colors).
const VESSEL_COLOR: [f32; 4] = [1.0, 0.55, 0.15, 1.0];
/// M5 maneuver-node color: the node marker and the predicted post-burn
/// conic read as "planned, not flown" — green against the flown
/// orange.
const NODE_COLOR: [f32; 4] = [0.35, 0.95, 0.45, 1.0];
/// Star marker/line color (parent-less body).
const STAR_COLOR: [f32; 4] = [1.0, 0.85, 0.35, 1.0];
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
    /// Left-drag orbit — radians to add to yaw/pitch.
    Orbit { dyaw: f64, dpitch: f64 },
    /// Scroll zoom — multiplicative factor on `distance_m`.
    Zoom { factor: f64 },
    /// Keyboard pan — screen-space pixels (WASD/arrows).
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
    /// Index into [`BodyTable::bodies`].
    pub focus: usize,
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
}

impl MapInput {
    /// Fresh session state for a table — focused on the system root.
    fn new(table: &BodyTable) -> Self {
        Self {
            mode: MapMode::Flight,
            focus: table.default_focus,
            pan_offset: DVec3::ZERO,
            yaw_rad: 0.0,
            pitch_rad: 0.6,
            distance_m: default_distance(table, table.default_focus),
            viewport_px: (1280.0, 720.0),
        }
    }

    /// Apply one event — the `Input` schedule's work.
    fn apply(&mut self, ev: MapKeyEvent, table: &BodyTable) {
        match ev {
            MapKeyEvent::ToggleMode => {
                self.mode = match self.mode {
                    MapMode::Flight => MapMode::Map,
                    MapMode::Map => MapMode::Flight,
                };
                if self.mode == MapMode::Map {
                    self.refocus(table);
                }
            }
            MapKeyEvent::CycleFocus => {
                if self.mode == MapMode::Map && !table.bodies.is_empty() {
                    self.focus = (self.focus + 1) % table.bodies.len();
                    self.refocus(table);
                }
            }
            MapKeyEvent::Orbit { dyaw, dpitch } => {
                self.yaw_rad += dyaw;
                self.pitch_rad = (self.pitch_rad + dpitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
            }
            MapKeyEvent::Zoom { factor } => {
                self.distance_m =
                    (self.distance_m * factor.max(1e-9)).clamp(MIN_DISTANCE_M, MAX_DISTANCE_M);
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

    /// Re-lock the camera on the focus body — resets pan and re-derives
    /// a framing distance.
    fn refocus(&mut self, table: &BodyTable) {
        self.pan_offset = DVec3::ZERO;
        self.distance_m = default_distance(table, self.focus);
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
) {
    for ev in events.pending.drain(..) {
        input.apply(ev, &table);
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
#[derive(Resource, Default)]
pub(crate) struct MapSyncInput {
    /// The sim clock the vessel states were read at.
    pub tick: SimTime,
    /// Every vessel's orbit state + scheduled maneuver node (M5),
    /// ascending-id order.
    pub vessels: Vec<(VesselId, OrbitState, Option<ManeuverNode>)>,
}

/// The Sync-written map mirror — what Presentation and UiBuild read.
#[derive(Resource, Default)]
pub struct MapSnapshot {
    /// The sim tick this snapshot was taken at.
    pub tick: SimTime,
    /// Every vessel's orbit state + scheduled maneuver node (M5),
    /// ascending-id order.
    pub vessels: Vec<(VesselId, OrbitState, Option<ManeuverNode>)>,
}

/// `Sync` schedule system: copy the staged map input into the mirror.
pub(crate) fn sync_map(input: Res<MapSyncInput>, mut snap: ResMut<MapSnapshot>) {
    snap.tick = input.tick;
    snap.vessels.clone_from(&input.vessels);
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
    /// Vessel lines keyed by the exact `OrbitState` they were sampled
    /// from — a `§2.6.6` re-base or a new spawn replaces the elements,
    /// which is the invalidation signal.
    vessel_lines: BTreeMap<VesselId, (OrbitState, Option<Vec<DVec3>>)>,
    /// M5 node previews — see [`NodePreview`].
    node_lines: BTreeMap<VesselId, NodePreview>,
}

/// One vessel's cached node preview: the `(orbit, node)` invalidation
/// key, the predicted post-burn conic (parent-centred, like every
/// sampled line), and the node point's parent-centred position.
struct NodePreview {
    /// The inputs this preview was derived from.
    key: (OrbitState, ManeuverNode),
    /// The predicted post-burn polyline (`None` = unsampleable).
    line: Option<Vec<DVec3>>,
    /// The node point's parent-centred position.
    point: Option<DVec3>,
}

/// The solved map camera for one frame — f64 end to end (§2.3).
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
}

/// The RenderExtract product — the packet `csp-app` hands to
/// `csp_render::map::MapRenderer`. `None` while in [`MapMode::Flight`].
#[derive(Resource, Default)]
pub struct MapFrame(pub Option<MapPacket>);

// ---------------------------------------------------------------------------
// Presentation: orbit evaluation, polyline sampling, camera solve, labels
// ---------------------------------------------------------------------------

/// `Presentation` schedule system — all map derivation in one place
/// (§2.5 #4: "camera solve, LOD selection"). Runs only in map mode;
/// flight frames skip the whole derivation.
#[allow(clippy::too_many_lines)]
pub(crate) fn map_present(
    input: Res<MapInput>,
    model: Res<OrbitModelRes>,
    snap: Res<MapSnapshot>,
    table: Res<BodyTable>,
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

    // System-frame positions: bodies via the parent chain, vessels via
    // state_at + their parent's position.
    scene.body_pos.clear();
    for body in &table.bodies {
        scene
            .body_pos
            .push(body_system_pos(model, &body.id, t).ok());
    }
    scene.vessel_pos.clear();
    for (_id, orbit, _node) in &snap.vessels {
        let pos = (|| {
            let (r, _v) = model.state_at(orbit, t).ok()?;
            let parent = body_system_pos(model, &orbit.parent, t).ok()?;
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
    for (id, orbit, _node) in &snap.vessels {
        let stale = scene
            .vessel_lines
            .get(id)
            .is_none_or(|(cached, _)| cached != orbit);
        if stale {
            let line = sample_orbit(model, orbit);
            scene.vessel_lines.insert(*id, (orbit.clone(), line));
        }
    }
    scene
        .vessel_lines
        .retain(|id, _| snap.vessels.iter().any(|(vid, _, _)| vid == id));

    refresh_node_previews(model, &snap, &mut scene);

    // Camera solve — f64 throughout.
    let focus = scene
        .body_pos
        .get(input.focus)
        .copied()
        .flatten()
        .unwrap_or(DVec3::ZERO)
        + input.pan_offset;
    *cam = solve_camera(&input, focus);
    ui.focus_name = table.focus_name(input.focus).to_string();
    ui.camera_distance_m = input.distance_m;

    // Labels: bodies + vessels that project.
    ui.labels.clear();
    for (body, pos) in table.bodies.iter().zip(scene.body_pos.iter()) {
        if let Some(ndc) = pos.and_then(|p| project(&cam, p)) {
            ui.labels.push(csp_ui::MapLabel {
                ndc,
                text: body.id.to_string(),
                color: body.color,
            });
        }
    }
    for ((id, _orbit, _node), pos) in snap.vessels.iter().zip(scene.vessel_pos.iter()) {
        if let Some(ndc) = pos.and_then(|p| project(&cam, p)) {
            ui.labels.push(csp_ui::MapLabel {
                ndc,
                text: format!("vessel {}", id.0),
                color: VESSEL_COLOR,
            });
        }
    }
    // Node labels — the point rides its parent like the lines do.
    for (id, orbit, node) in &snap.vessels {
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
            let eta = (node.tick.saturating_sub(t.ticks())) as f64 / TICK_HZ as f64;
            ui.labels.push(csp_ui::MapLabel {
                ndc,
                text: format!("node  T−{eta:.0} s  Δv {:.0} m/s", node.dv_magnitude()),
                color: NODE_COLOR,
            });
        }
    }
}

/// M5 node previews — the same invalidation rule the vessel lines use:
/// the predicted post-burn conic and the node point re-derive only
/// when the `(orbit, node)` pair they came from changes.
fn refresh_node_previews(model: &PatchedConics, snap: &MapSnapshot, scene: &mut MapScene) {
    for (id, orbit, node) in &snap.vessels {
        let Some(node) = node else { continue };
        let key = (orbit.clone(), node.clone());
        let stale = scene.node_lines.get(id).is_none_or(|p| p.key != key);
        if stale {
            let predicted = csp_sim::maneuver::predict_maneuver(model, orbit, node).ok();
            let line = predicted.as_ref().and_then(|o| sample_orbit(model, o));
            let point = model
                .state_at(orbit, SimTime(node.tick))
                .ok()
                .map(|(r, _)| r);
            scene
                .node_lines
                .insert(*id, NodePreview { key, line, point });
        }
    }
    scene.node_lines.retain(|id, _| {
        snap.vessels
            .iter()
            .any(|(vid, _, n)| vid == id && n.is_some())
    });
}

/// `RenderExtract` schedule system (§2.5 #6): emit the immutable
/// `MapPacket` — clip-space vertices only. Points are projected
/// per-frame (the camera moves); polylines stay cached.
#[allow(clippy::too_many_lines)]
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
    let mut lines = MapMesh::default();
    let mut markers = MapMesh::default();
    let mut skipped_lines = 0u32;

    // Orbit polylines — parent-centered samples + the parent's current
    // system position. A failed parent solve skips the line entirely
    // (MapScene::body_pos's documented "skipped, not defaulted" policy):
    // offsetting by DVec3::ZERO would draw the child's orbit through the
    // system origin instead of around its parent.
    for (i, line) in scene.body_lines.iter().enumerate() {
        let (Some(points), Some(body)) = (line.as_ref(), table.bodies.get(i)) else {
            continue;
        };
        let Some(offset) = body
            .parent
            .as_ref()
            .and_then(|p| table.index_of(p))
            .and_then(|pi| scene.body_pos.get(pi))
            .copied()
            .flatten()
        else {
            skipped_lines += 1;
            continue;
        };
        let mut dim = body.color;
        dim[3] *= 0.65;
        emit_polyline(&mut lines.vertices, points, offset, &cam, vp, dim);
    }
    for (state, line) in scene.vessel_lines.values() {
        let Some(points) = line else { continue };
        let Some(offset) = table
            .index_of(&state.parent)
            .and_then(|pi| scene.body_pos.get(pi))
            .copied()
            .flatten()
        else {
            skipped_lines += 1;
            continue;
        };
        let mut color = VESSEL_COLOR;
        color[3] *= 0.8;
        emit_polyline(&mut lines.vertices, points, offset, &cam, vp, color);
    }
    // M5 predicted post-burn conics — the same parent-centred offset
    // convention; a failed prediction is "no line", never defaulted.
    for preview in scene.node_lines.values() {
        let state = &preview.key.0;
        let Some(points) = preview.line.as_ref() else {
            continue;
        };
        let Some(offset) = table
            .index_of(&state.parent)
            .and_then(|pi| scene.body_pos.get(pi))
            .copied()
            .flatten()
        else {
            skipped_lines += 1;
            continue;
        };
        let mut color = NODE_COLOR;
        color[3] *= 0.75;
        emit_polyline(&mut lines.vertices, points, offset, &cam, vp, color);
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

    // Markers — screen-space quads at projected positions.
    for (body, pos) in table.bodies.iter().zip(scene.body_pos.iter()) {
        if let Some(ndc) = pos.and_then(|p| project(&cam, p)) {
            emit_quad(&mut markers.vertices, ndc, BODY_MARKER_PX, vp, body.color);
        }
    }
    for pos in &scene.vessel_pos {
        if let Some(ndc) = pos.and_then(|p| project(&cam, p)) {
            emit_quad(
                &mut markers.vertices,
                ndc,
                VESSEL_MARKER_PX,
                vp,
                VESSEL_COLOR,
            );
        }
    }
    // Node markers — the node point offset by its parent's position,
    // drawn slightly larger than a vessel marker.
    for preview in scene.node_lines.values() {
        let state = &preview.key.0;
        let (Some(p), Some(off)) = (
            preview.point,
            table
                .index_of(&state.parent)
                .and_then(|pi| scene.body_pos.get(pi))
                .copied()
                .flatten(),
        ) else {
            continue;
        };
        if let Some(ndc) = project(&cam, p + off) {
            emit_quad(
                &mut markers.vertices,
                ndc,
                BODY_MARKER_PX - 0.5,
                vp,
                NODE_COLOR,
            );
        }
    }

    frame.0 = Some(MapPacket { lines, markers });
}

// ---------------------------------------------------------------------------
// Sim access — the ONE place map code reads csp-sim. Called by
// View::end_frame's staging step; systems below never touch `SimWorld`.
// ---------------------------------------------------------------------------

/// Stage the map's sim reads for the `Sync` schedule (vessel orbit
/// states + clock — everything else derives from `OrbitModelRes`).
pub(crate) fn capture_sync_input(sim: &SimWorld) -> MapSyncInput {
    MapSyncInput {
        tick: sim.clock(),
        // Only on-rails vessels have a conic to draw — bubble-resident
        // and surface-latched vessels are skipped (the flight-scene
        // presentation path for them is a later milestone's work).
        vessels: sim
            .vessels()
            .iter()
            .filter_map(|(id, v)| v.orbit().cloned().map(|o| (*id, o, v.maneuver.clone())))
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

/// Sample an orbit into a parent-centered polyline (§2.8 `state_at`).
///
/// Closed orbits (`e < 1`): `ORBIT_LINE_SAMPLES` segments uniform in
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
    if e < 1.0 {
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
            let Ok((r, _v)) = model.state_at(state, SimTime(t_ticks.max(0) as u64)) else {
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
        let nu_max = if e > 1.0 + 1e-9 {
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
            let Ok((r, _v)) = model.state_at(state, SimTime(t_ticks as u64)) else {
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
    if e < 1.0 {
        let ea = 2.0 * f64::atan(f64::sqrt((1.0 - e) / (1.0 + e)) * f64::tan(nu * 0.5));
        ea - e * f64::sin(ea)
    } else if e <= 1.0 + 1e-9 {
        // Barker's equation (parabola).
        let t = f64::tan(nu * 0.5);
        t + t * t * t / 3.0
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

/// Emit a screen-space quad (two triangles) centered on `ndc`.
fn emit_quad(
    verts: &mut Vec<MapVertex>,
    ndc: [f32; 3],
    half_px: f64,
    vp: (f64, f64),
    color: [f32; 4],
) {
    let hx = (half_px * 2.0 / vp.0.max(1.0)) as f32;
    let hy = (half_px * 2.0 / vp.1.max(1.0)) as f32;
    let [x, y, z] = ndc;
    let corners = [
        [x - hx, y - hy, z],
        [x + hx, y - hy, z],
        [x + hx, y + hy, z],
        [x - hx, y + hy, z],
    ];
    for idx in [0usize, 1, 2, 0, 2, 3] {
        verts.push(MapVertex {
            pos: corners[idx],
            color,
        });
    }
}

/// Emit a polyline as per-segment quads in screen space — the honest M1
/// substitute for line primitives (which have no width control). Points
/// are world-frame, `offset` is added in f64 before projection; segments
/// whose either endpoint fails [`project`] are skipped.
fn emit_polyline(
    verts: &mut Vec<MapVertex>,
    points: &[DVec3],
    offset: DVec3,
    cam: &MapCameraF64,
    vp: (f64, f64),
    color: [f32; 4],
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
        // (emit_quad, MapInput::apply, solve_camera) — they exist only to
        // avoid div-by-zero on a zero-size viewport, so use the same bound.
        let fx = vp.0.max(1.0) / 2.0;
        let fy = vp.1.max(1.0) / 2.0;
        let dx = f64::from(b[0] - a[0]) * fx;
        let dy = f64::from(b[1] - a[1]) * fy;
        let len = f64::sqrt(dx * dx + dy * dy);
        if len < 1e-9 {
            continue;
        }
        let ox = (-dy / len * LINE_HALF_WIDTH_PX / fx) as f32;
        let oy = (dx / len * LINE_HALF_WIDTH_PX / fy) as f32;
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
            });
        }
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
        );
        assert!(input.pan_offset.norm() > 0.0);
        let start = input.focus; // the star — last in table order
        input.apply(MapKeyEvent::CycleFocus, &table);
        assert_eq!(input.focus, (start + 1) % table.bodies.len());
        assert_eq!(input.pan_offset, DVec3::ZERO); // refocus resets pan
        for _ in 0..table.bodies.len() {
            input.apply(MapKeyEvent::CycleFocus, &table);
        }
        assert_eq!(input.focus, (start + 1) % table.bodies.len()); // wrap + 1
        // CycleFocus is a no-op in Flight mode.
        input.mode = MapMode::Flight;
        input.apply(MapKeyEvent::CycleFocus, &table);
        assert_eq!(input.focus, (start + 1) % table.bodies.len());
    }

    /// The mode toggle flips both ways.
    #[test]
    fn mode_toggles() {
        let table = BodyTable::from_model(&test_system());
        let mut input = MapInput::new(&table);
        assert_eq!(input.mode, MapMode::Flight);
        input.apply(MapKeyEvent::ToggleMode, &table);
        assert_eq!(input.mode, MapMode::Map);
        input.apply(MapKeyEvent::ToggleMode, &table);
        assert_eq!(input.mode, MapMode::Flight);
    }

    /// Zoom clamps at both ends; orbit clamps pitch.
    #[test]
    fn zoom_and_orbit_clamps() {
        let table = BodyTable::from_model(&test_system());
        let mut input = MapInput::new(&table);
        for _ in 0..100 {
            input.apply(MapKeyEvent::Zoom { factor: 0.5 }, &table);
        }
        assert_eq!(input.distance_m, MIN_DISTANCE_M);
        for _ in 0..200 {
            input.apply(MapKeyEvent::Zoom { factor: 2.0 }, &table);
        }
        assert_eq!(input.distance_m, MAX_DISTANCE_M);
        for _ in 0..100 {
            input.apply(
                MapKeyEvent::Orbit {
                    dyaw: 0.0,
                    dpitch: 1.0,
                },
                &table,
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
        for v in packet
            .markers
            .vertices
            .iter()
            .chain(packet.lines.vertices.iter())
        {
            assert!(v.pos.iter().all(|c| c.is_finite()));
        }
        // 3 bodies with markers + 1 vessel = 4 quads × 6 verts.
        assert_eq!(packet.markers.vertices.len(), 4 * 6);
    }
}
