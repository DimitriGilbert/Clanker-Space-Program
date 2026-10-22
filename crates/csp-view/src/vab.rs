//! The M3 VAB craft view (PLAN M3 exit row: "GPU picking", §9 "Editor
//! viewport is `csp-render` + GPU picking").
//!
//! # Data flow (§2.5)
//!
//! ```text
//! csp-app ──VabEvent──▶ VabInputEvents ──(Input: apply_vab_input)──▶ VabInput
//!                                                        └─ pick/load ─▶ VabSessionRes
//! VabSessionRes ──(Presentation: vab_present)──▶ VabScene + VabCameraF64
//! (RenderExtract: extract_vab) ──▶ VabFrame(csp_render::vab::VabPacket)
//! ```
//!
//! The [`VabSession`] instance — the §2.2-C working craft — is hosted
//! here as the [`VabSessionRes`] resource (the same hosting pattern as
//! `SessionMirror`/`MapInput`); the type and its op channel
//! ([`csp_ui::vab::VabAction`]) live in `csp-ui` per §2.1.
//!
//! The view itself is placeholder-tier per §3.7: each part renders as
//! its resolved bounding box (`EffectivePart.size_m`) under the node's
//! body-frame transform, depth-tested; the pick pass re-rasterizes the
//! same boxes into an object-id texture (`csp_render::vab`).

use std::collections::BTreeMap;

use bevy_ecs::prelude::*;
use csp_core::DVec3;
use csp_render::vab::{VabPacket, VabVertex};

use crate::map::{self, MapCameraF64, project_into, solve_orbit_camera};

/// Camera field of view and near plane for the craft view — the craft
/// is meters-scale, so `near` shrinks accordingly (reverse-Z).
const VAB_FOV_Y: f64 = std::f64::consts::FRAC_PI_3;
const VAB_NEAR_M: f64 = 0.05;
/// Orbit/zoom limits (m).
const MIN_DISTANCE_M: f64 = 0.5;
const MAX_DISTANCE_M: f64 = 2_000.0;
/// Pitch clamp, ~89°.
const PITCH_LIMIT: f64 = 1.55;

/// One winit-side event for the VAB — staged by `csp-app` through
/// [`crate::View::vab_event`], drained by the `Input` schedule's
/// [`apply_vab_input`].
#[derive(Clone, Debug)]
pub enum VabEvent {
    /// Left-drag orbit — radians to add to yaw/pitch.
    Orbit { dyaw: f64, dpitch: f64 },
    /// Scroll zoom — multiplicative factor on `distance_m`.
    Zoom { factor: f64 },
    /// Keyboard pan — screen-space pixels.
    Pan { dx_px: f64, dy_px: f64 },
    /// A GPU pick readback resolved: `Some(node_id)` selects the part,
    /// `None` (background pixel) clears the selection.
    Pick(Option<u64>),
    /// Craft-file bytes arrived from the app's store → the session's
    /// `LoadBytes` action.
    LoadCraft(Vec<u8>),
    /// An app-side result string for the panel's notice row.
    Notice(String),
}

/// The working-craft instance — `csp_ui::vab::VabSession` hosted as a
/// presentation-world resource (module doc).
#[derive(Resource, Default)]
pub struct VabSessionRes(pub csp_ui::vab::VabSession);

/// Staged winit events for the VAB — the [`crate::View::vab_event`]
/// queue, drained each frame by [`apply_vab_input`].
#[derive(Resource, Default)]
pub(crate) struct VabInputEvents {
    /// FIFO of events since the last `Input` run.
    pub pending: Vec<VabEvent>,
}

/// VAB camera + open state — session state (§2.2-C), never sim state.
#[derive(Resource, Debug)]
pub struct VabInput {
    /// The VAB editor is the active mode.
    pub open: bool,
    /// Camera yaw/pitch about the craft (radians).
    pub yaw_rad: f64,
    /// Camera pitch.
    pub pitch_rad: f64,
    /// Eye distance from the craft center (m).
    pub distance_m: f64,
    /// Pan offset added to the craft center (craft frame, m).
    pub pan_offset: DVec3,
    /// Current viewport in physical pixels (plumbed by `csp-app`).
    pub viewport_px: (f64, f64),
    /// `false` until the first `vab_present` frames a nonempty craft —
    /// distance derives from the craft extent once.
    pub focused_once: bool,
}

impl Default for VabInput {
    fn default() -> Self {
        Self {
            open: false,
            yaw_rad: 0.7,
            pitch_rad: 0.4,
            distance_m: 20.0,
            pan_offset: DVec3::ZERO,
            viewport_px: (1280.0, 720.0),
            focused_once: false,
        }
    }
}

impl VabInput {
    /// Apply one event — the `Input` schedule's camera half (session
    /// ops route to [`VabSessionRes`] inside `apply_vab_input`).
    fn apply(&mut self, ev: &VabEvent) {
        match *ev {
            VabEvent::Orbit { dyaw, dpitch } => {
                self.yaw_rad += dyaw;
                self.pitch_rad = (self.pitch_rad + dpitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
            }
            VabEvent::Zoom { factor } => {
                self.distance_m =
                    (self.distance_m * factor.max(1e-9)).clamp(MIN_DISTANCE_M, MAX_DISTANCE_M);
            }
            VabEvent::Pan { dx_px, dy_px } => {
                let units_per_px =
                    2.0 * self.distance_m * f64::tan(VAB_FOV_Y * 0.5) / self.viewport_px.1.max(1.0);
                let r = map::rot3(self.yaw_rad, self.pitch_rad);
                self.pan_offset = self.pan_offset
                    + map::mat3_vec(
                        &r,
                        DVec3::new(dx_px * units_per_px, -dy_px * units_per_px, 0.0),
                    );
            }
            _ => {}
        }
    }
}

/// `Input` schedule system: drain staged events — camera ops into
/// [`VabInput`], session ops into the working craft through
/// `VabAction` (the §2.2-C channel).
pub(crate) fn apply_vab_input(
    mut events: ResMut<VabInputEvents>,
    mut input: ResMut<VabInput>,
    mut vab: ResMut<VabSessionRes>,
) {
    let pending: Vec<VabEvent> = events.pending.drain(..).collect();
    for ev in pending {
        input.apply(&ev);
        match ev {
            VabEvent::Pick(id) => {
                let _ = vab
                    .0
                    .apply(csp_ui::vab::VabAction::Select(id.map(csp_phys::PartNodeId)));
            }
            VabEvent::LoadCraft(bytes) => {
                let _ = vab.0.apply(csp_ui::vab::VabAction::LoadBytes(bytes));
            }
            VabEvent::Notice(n) => {
                vab.0.set_notice(Some(n));
            }
            VabEvent::Orbit { .. } | VabEvent::Zoom { .. } | VabEvent::Pan { .. } => {}
        }
    }
}

/// One part's render box: body-frame center + half-extents + the axis
/// basis (columns of the node's rotation, applied per corner).
#[derive(Clone, Copy, Debug)]
pub struct PartBox {
    /// The node it belongs to.
    pub node: csp_phys::PartNodeId,
    /// Body-frame center (m).
    pub center: DVec3,
    /// Body-frame rotation — `PartTransform::rotation` columns applied
    /// to the half extents.
    pub rotation: csp_phys::PartTransform,
    /// Half-extents of the resolved bounding box (m).
    pub half: DVec3,
    /// Linear RGBA — category-tinted; brightened when selected.
    pub color: [f32; 4],
}

/// The Presentation product: the craft's boxes in body frame +
/// bounds. Rebuilt each VAB frame from the session's cached
/// `body_transforms` + `analysis` (dirty-gated upstream).
#[derive(Resource, Default)]
pub struct VabScene {
    /// One box per placed part, ascending id.
    pub boxes: Vec<PartBox>,
    /// Body-frame bounding-sphere center + radius for camera framing.
    pub center: DVec3,
    /// Craft extent (m) — max corner distance from `center`.
    pub extent_m: f64,
}

/// The solved VAB camera — `MapCameraF64` reused (same orbit
/// convention, craft frame instead of system frame).
#[derive(Resource, Default)]
pub struct VabCameraF64(pub MapCameraF64);

/// The RenderExtract product — `None` while the VAB is closed.
#[derive(Resource, Default)]
pub struct VabFrame(pub Option<VabPacket>);

/// Craft file names the app's store knows — plumbed per frame; the
/// save/load UI lists them.
#[derive(Resource, Default)]
pub struct CraftFileList(pub Vec<String>);

/// `Presentation` schedule system: recompute the session (dirty-gated)
/// and solve the scene + camera. Runs only in VAB mode.
pub(crate) fn vab_present(
    mut input: ResMut<VabInput>,
    mut vab: ResMut<VabSessionRes>,
    mut scene: ResMut<VabScene>,
    mut cam: ResMut<VabCameraF64>,
) {
    if !input.open {
        scene.boxes.clear();
        cam.0.valid = false;
        return;
    }
    vab.0.recompute();
    scene.boxes.clear();
    let sel = vab.0.selected();
    // Resolved sizes come from the cached analysis (`size_m` is already
    // scaled); `iter()` and `effective` share ascending-id order — the
    // effective_index contract.
    let sizes: BTreeMap<csp_phys::PartNodeId, DVec3> = vab
        .0
        .session()
        .iter()
        .zip(
            vab.0
                .analysis()
                .map(|a| a.effective.as_slice())
                .unwrap_or(&[])
                .iter(),
        )
        .map(|((id, _), eff)| (id, eff.size_m))
        .collect();
    for (id, _n) in vab.0.session().iter() {
        let (Some(&t), Some(&sz)) = (vab.0.body_transforms().get(&id), sizes.get(&id)) else {
            continue;
        };
        let color = category_color(vab.0.def_of(id).map(|d| d.category), sel == Some(id));
        scene.boxes.push(PartBox {
            node: id,
            center: t.offset,
            rotation: t,
            half: sz * 0.5,
            color,
        });
    }
    // Camera framing: bounding sphere over the boxes.
    if scene.boxes.is_empty() {
        scene.center = DVec3::ZERO;
        scene.extent_m = 1.0;
    } else {
        let mut c = DVec3::ZERO;
        for b in &scene.boxes {
            c = c + b.center;
        }
        c = c * (1.0 / scene.boxes.len() as f64);
        scene.center = c;
        scene.extent_m = scene
            .boxes
            .iter()
            .map(|b| (b.center - c).norm() + b.half.norm())
            .fold(1.0_f64, f64::max);
    }
    if !input.focused_once && !scene.boxes.is_empty() {
        input.distance_m = (scene.extent_m * 3.0).clamp(MIN_DISTANCE_M, MAX_DISTANCE_M);
        input.focused_once = true;
    }
    let focus = scene.center + input.pan_offset;
    cam.0 = solve_orbit_camera(
        input.yaw_rad,
        input.pitch_rad,
        input.distance_m.max(MIN_DISTANCE_M),
        input.viewport_px,
        focus,
        VAB_FOV_Y,
        VAB_NEAR_M,
    );
}

/// Flat color per part category — placeholder tier: legibility, not art.
fn category_color(cat: Option<csp_data::PartCategory>, selected: bool) -> [f32; 4] {
    let base = match cat {
        Some(csp_data::PartCategory::Engine) => [0.85, 0.45, 0.20, 1.0],
        Some(csp_data::PartCategory::FuelTank) => [0.45, 0.55, 0.70, 1.0],
        Some(csp_data::PartCategory::Control) => [0.45, 0.75, 0.50, 1.0],
        Some(csp_data::PartCategory::Structure) => [0.60, 0.60, 0.60, 1.0],
        Some(csp_data::PartCategory::Payload) => [0.70, 0.60, 0.30, 1.0],
        Some(csp_data::PartCategory::Science) => [0.55, 0.45, 0.80, 1.0],
        Some(csp_data::PartCategory::Utility) | None => [0.65, 0.65, 0.55, 1.0],
    };
    if selected {
        [
            (base[0] * 1.35_f32).min(1.0),
            (base[1] * 1.35_f32).min(1.0),
            (base[2] * 1.35_f32).min(1.0),
            1.0,
        ]
    } else {
        base
    }
}

/// `RenderExtract` schedule system: emit the `VabPacket` — box faces
/// projected to clip space, each part's verts tagged with its 1-based
/// pick index.
pub(crate) fn extract_vab(
    input: Res<VabInput>,
    scene: Res<VabScene>,
    cam: Res<VabCameraF64>,
    mut frame: ResMut<VabFrame>,
) {
    if !input.open || !cam.0.valid {
        frame.0 = None;
        return;
    }
    let mut packet = VabPacket::default();
    for b in &scene.boxes {
        let pick = packet.pick_ids.len() as u32 + 1;
        packet.pick_ids.push(b.node.0);
        emit_box(&mut packet.vertices, &cam.0, b, pick);
    }
    frame.0 = Some(packet);
}

/// Push a box's 12 triangles (36 verts) into `verts`. A face whose
/// quad has ANY corner failing projection (behind the eye plane) is
/// dropped whole rather than clipped or clamped — at these scales the
/// near plane never sits inside the craft in practice.
fn emit_box(verts: &mut Vec<VabVertex>, cam: &MapCameraF64, b: &PartBox, pick: u32) {
    let (hx, hy, hz) = (b.half.x, b.half.y, b.half.z);
    // Local-frame corners; rotated into body frame via `apply`.
    let locals = [
        DVec3::new(-hx, -hy, -hz),
        DVec3::new(hx, -hy, -hz),
        DVec3::new(hx, hy, -hz),
        DVec3::new(-hx, hy, -hz),
        DVec3::new(-hx, -hy, hz),
        DVec3::new(hx, -hy, hz),
        DVec3::new(hx, hy, hz),
        DVec3::new(-hx, hy, hz),
    ];
    let world: Vec<DVec3> = locals.iter().map(|p| b.rotation.apply(*p)).collect();
    let clip: Vec<Option<[f32; 3]>> = world
        .iter()
        .map(|p| project_into(&cam.view_proj, cam.eye, *p))
        .collect();
    // 6 faces × 2 tris — quad corner indices.
    const FACES: [[usize; 4]; 6] = [
        [0, 1, 2, 3], // −z
        [5, 4, 7, 6], // +z
        [4, 0, 3, 7], // −x
        [1, 5, 6, 2], // +x
        [4, 5, 1, 0], // −y
        [3, 2, 6, 7], // +y
    ];
    for f in FACES {
        let idx = [f[0], f[1], f[2], f[0], f[2], f[3]];
        if idx.iter().any(|i| clip[*i].is_none()) {
            continue; // face crosses the eye plane — drop it whole
        }
        for i in idx {
            if let Some(pos) = clip[i] {
                verts.push(VabVertex {
                    pos,
                    color: b.color,
                    pick,
                });
            }
        }
    }
}
