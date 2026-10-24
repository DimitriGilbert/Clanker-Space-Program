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
//! The view itself renders §3.7's kitbash tier: each part draws its
//! classified [`crate::part_mesh::PartShape`] — a lathe of the
//! resolved collider's silhouette, or the engine/pod override —
//! under the node's body-frame transform, flat category-tinted with a
//! fixed key light, depth-tested; the pick pass re-rasterizes the
//! same vertices into an object-id texture (`csp_render::vab`).

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
    /// The session's [`csp_ui::vab::VabSession::craft_epoch`] the
    /// camera last framed — `New`/load bumps it and the one-shot
    /// refit must run again (a stale `focused_once` would frame the
    /// new craft at the old craft's distance).
    pub craft_epoch: u64,
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
            craft_epoch: u64::MAX, // nothing framed yet
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

/// One part's render entry: the node's composed body-frame transform,
/// the classified kitbash silhouette (part-local), and the category
/// tint. `transform` plays the old `center`+`rotation` pair's role —
/// the mesh carries part-local geometry the transform places.
#[derive(Clone, Debug)]
pub struct PartMesh {
    /// The node it belongs to.
    pub node: csp_phys::PartNodeId,
    /// Part-local → body frame (the node's composed weld transform).
    pub transform: csp_phys::PartTransform,
    /// The classified silhouette in part-local space.
    pub shape: crate::part_mesh::PartShape,
    /// Linear RGBA — category-tinted; brightened when selected.
    pub color: [f32; 4],
}

/// The Presentation product: the craft's part meshes in body frame +
/// bounds. Rebuilt each VAB frame from the session's cached
/// `body_transforms` + `analysis` (dirty-gated upstream).
#[derive(Resource, Default)]
pub struct VabScene {
    /// One mesh entry per placed part, ascending id.
    pub parts: Vec<PartMesh>,
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
        scene.parts.clear();
        cam.0.valid = false;
        return;
    }
    vab.0.recompute();
    // A `New` or a loaded craft is a different object than the one
    // the camera framed — re-arm the one-shot fit and drop the stale
    // pan (it was measured around the old craft's center).
    if vab.0.craft_epoch() != input.craft_epoch {
        input.craft_epoch = vab.0.craft_epoch();
        input.focused_once = false;
        input.pan_offset = DVec3::ZERO;
    }
    scene.parts.clear();
    let sel = vab.0.selected();
    // The cached analysis carries each node's `EffectivePart` — the
    // resolved collider + module list + attach nodes the silhouette
    // classifier reads (`iter()` and `effective` share ascending-id
    // order — the effective_index contract).
    let effective: BTreeMap<csp_phys::PartNodeId, &csp_phys::EffectivePart> = vab
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
        .map(|((id, _), eff)| (id, eff))
        .collect();
    for (id, _n) in vab.0.session().iter() {
        let (Some(&t), Some(&eff)) = (vab.0.body_transforms().get(&id), effective.get(&id)) else {
            continue;
        };
        let color = category_color(vab.0.def_of(id).map(|d| d.category), sel == Some(id));
        scene.parts.push(PartMesh {
            node: id,
            transform: t,
            shape: crate::part_mesh::classify_effective(eff),
            color,
        });
    }
    // Camera framing: bounding sphere over the meshes' AABBs.
    if scene.parts.is_empty() {
        scene.center = DVec3::ZERO;
        scene.extent_m = 1.0;
    } else {
        let mut c = DVec3::ZERO;
        for p in &scene.parts {
            c = c + p.transform.offset;
        }
        c = c * (1.0 / scene.parts.len() as f64);
        scene.center = c;
        scene.extent_m = scene
            .parts
            .iter()
            .map(|p| {
                let (lo, hi) = p.shape.local_aabb();
                let mut m = 0.0_f64;
                for x in [lo.x, hi.x] {
                    for y in [lo.y, hi.y] {
                        for z in [lo.z, hi.z] {
                            let w = p.transform.apply(DVec3::new(x, y, z));
                            m = m.max((w - c).norm());
                        }
                    }
                }
                m
            })
            .fold(1.0_f64, f64::max);
    }
    if !input.focused_once && !scene.parts.is_empty() {
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

/// `RenderExtract` schedule system: emit the `VabPacket` — the
/// kitbash triangles projected to clip space, each part's verts
/// tagged with its 1-based pick index.
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
    let mut tris = Vec::new();
    for p in &scene.parts {
        let pick = packet.pick_ids.len() as u32 + 1;
        packet.pick_ids.push(p.node.0);
        emit_part(&mut packet.vertices, &mut tris, &cam.0, p, pick);
    }
    frame.0 = Some(packet);
}

/// The VAB's fixed key light — a body-frame direction like the flight
/// view's fallback sun. Per-face lambert is what makes a lathed
/// silhouette legible under flat category colors; the pick pass never
/// reads color, so shading cannot affect picking.
const VAB_LIGHT: DVec3 = DVec3::new(0.4, 0.3, 0.86);

/// Push one part's kitbash triangles into `verts`. A triangle with
/// ANY corner failing projection (behind the eye plane) is dropped
/// whole rather than clipped or clamped — the box pass's per-face
/// rule carried over per-triangle; at these scales the near plane
/// never sits inside the craft in practice.
fn emit_part(
    verts: &mut Vec<VabVertex>,
    tris: &mut Vec<crate::part_mesh::MeshTri>,
    cam: &MapCameraF64,
    p: &PartMesh,
    pick: u32,
) {
    tris.clear();
    crate::part_mesh::emit(&p.shape, tris);
    let light = {
        let l = VAB_LIGHT;
        l / l.norm()
    };
    for t in tris.iter() {
        let world = [t.a, t.b, t.c].map(|q| p.transform.apply(q));
        let [Some(c0), Some(c1), Some(c2)] =
            world.map(|q| project_into(&cam.view_proj, cam.eye, q))
        else {
            continue; // tri crosses the eye plane — drop it whole
        };
        let shade = (0.55 + 0.45 * p.transform.apply_dir(t.n).dot(light).max(0.0)) as f32;
        let color = [
            p.color[0] * shade,
            p.color[1] * shade,
            p.color[2] * shade,
            p.color[3],
        ];
        for pos in [c0, c1, c2] {
            verts.push(VabVertex { pos, color, pick });
        }
    }
}
