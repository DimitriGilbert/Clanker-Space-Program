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
use csp_core::{DQuat, DVec3};
use csp_phys::PartTransform;
use csp_render::vab::{VabPacket, VabVertex};

use crate::map::{self, MapCameraF64, project_into, solve_orbit_camera};

/// Camera field of view and near plane for the craft view — the craft
/// is meters-scale, so `near` shrinks accordingly (reverse-Z).
const VAB_FOV_Y: f64 = std::f64::consts::FRAC_PI_3;
const VAB_NEAR_M: f64 = 0.05;
/// Orbit/zoom limits (m).
const MIN_DISTANCE_M: f64 = 0.5;
const MAX_DISTANCE_M: f64 = 2_000.0;
/// Pitch clamp — the FULL pole-to-pole orbit. The eye's offset is
/// `Rz(yaw)·Rx(pitch)`'s third column, so its height over the focus is
/// `cos(pitch)·distance`: a sub-π/2 clamp keeps the eye above the
/// craft's mid-plane forever, which leaves the bottom (−z) cap —
/// and its `bottom` attach node — invisible and unhittable. `±π`
/// reaches the straight-up view from directly beneath the stack.
const PITCH_LIMIT: f64 = std::f64::consts::PI;

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
        // A press carrying a part — or begun on one, about to lift it
        // — is a placement/move gesture, not a camera drag: the
        // egui-side pointer pass sets `orbit_block` for the press's
        // whole life so dragging a part moves the part.
        let orbit_blocked = matches!(ev, VabEvent::Orbit { .. }) && vab.0.orbit_block();
        if !orbit_blocked {
            input.apply(&ev);
        }
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
    /// `true` while this part is the selection — `extract_vab` emits
    /// an inflated accent shell behind it (the 3D selection outline).
    pub selected: bool,
    /// The real baked model (`data/art/parts.csar`) when the catalog
    /// covers the part — `None` draws the kitbash silhouette.
    pub model: Option<std::sync::Arc<crate::art::PartModel>>,
}

/// A placement ghost — the armed catalog part rendered translucent at
/// its snap pose. Carries no pick id (the GPU pick can't select it).
#[derive(Clone, Debug)]
pub struct GhostMesh {
    /// Local→craft-body transform (the parent's body transform
    /// composed with the plan's parent-local snap).
    pub transform: csp_phys::PartTransform,
    /// The ghost part's kitbash silhouette.
    pub shape: crate::part_mesh::PartShape,
    /// Tint incl. alpha — green when the plan is placeable, red when
    /// the planner reported a refusal.
    pub color: [f32; 4],
    /// The real baked model for the armed part, when resolvable.
    pub model: Option<std::sync::Arc<crate::art::PartModel>>,
}

/// The Presentation product: the craft's part meshes in body frame +
/// bounds. Rebuilt each VAB frame from the session's cached
/// `body_transforms` + `analysis` (dirty-gated upstream).
#[derive(Resource, Default)]
pub struct VabScene {
    /// One mesh entry per placed part, ascending id.
    pub parts: Vec<PartMesh>,
    /// The placement ghosts — the armed catalog part rendered at each
    /// planned copy's snap pose (radial symmetry expands the set).
    pub ghosts: Vec<GhostMesh>,
    /// Attach-node markers on the hovered part while a placement is
    /// armed — `(body-space point, color)` rendered as small octahedra
    /// (the "where it snaps" affordance).
    pub markers: Vec<(DVec3, [f32; 4])>,
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
/// and solve the scene + camera + hover/ghost/markers. Runs only in
/// VAB mode.
#[allow(clippy::too_many_lines)] // the presentation pipeline is one pass — splitting it fragments the data flow
pub(crate) fn vab_present(
    mut input: ResMut<VabInput>,
    mut vab: ResMut<VabSessionRes>,
    mut scene: ResMut<VabScene>,
    mut cam: ResMut<VabCameraF64>,
    art: Option<Res<crate::art::ArtAssets>>,
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
    // A lifted part (and its subtree) draws as the cursor's ghost,
    // not as a placed solid — it's "in hand", so it can't be hovered
    // or picked either.
    let move_set: std::collections::BTreeSet<csp_phys::PartNodeId> = vab
        .0
        .move_node()
        .map(|n| vab.0.session().subtree_ids(n).into_iter().collect())
        .unwrap_or_default();
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
        if move_set.contains(&id) {
            continue;
        }
        let (Some(&t), Some(&eff)) = (vab.0.body_transforms().get(&id), effective.get(&id)) else {
            continue;
        };
        let color = category_color(vab.0.def_of(id).map(|d| d.category), sel == Some(id));
        let shape = crate::part_mesh::classify_effective(eff);
        let model = art.as_ref().and_then(|a| {
            // The shared module-family classifier — the same
            // `part_kind` flight runs over the same resolved
            // `WeldPart` (`EffectivePart::weld_input`), so the kind
            // arm `model_key` falls back on agrees across the
            // editor↔flight seam (C20-F1).
            a.0.models
                .get(crate::art::model_key(
                    Some(eff.part_id.as_str()),
                    crate::flight::part_kind(&eff.weld_input),
                    crate::part_mesh::is_pointed(&shape),
                ))
                .cloned()
        });
        scene.parts.push(PartMesh {
            node: id,
            transform: t,
            shape,
            color,
            selected: sel == Some(id),
            model,
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

    // -----------------------------------------------------------------
    // CPU hover pick + held-part ghost. The GPU pick pass only fires
    // on clicks; the hover path needs a hit EVERY frame (ghost,
    // attach-node markers, right-click remove), so the ray is cast
    // here against the kitbash AABBs — cheap at VAB part counts and
    // exact for the collider shapes the meshes bake.
    scene.ghosts.clear();
    scene.markers.clear();
    let hover = vab
        .0
        .cursor_px()
        .and_then(|(x, y)| hover_pick(&input, &cam.0, &scene.parts, x, y));
    vab.0.set_hover(hover);

    // The carried part — KSP's held ghost, in one of three poses:
    // snapped green where the plan succeeds, red where it refuses
    // (with the reason pushed to `ghost_status`), or floating at the
    // cursor at focus depth when nothing is under it.
    let mut status: Option<String> = None;
    // The floating anchor: the cursor ray pushed to the focus plane's
    // depth — the part rides the cursor in the air.
    let float_anchor = vab.0.cursor_px().and_then(|(x, y)| {
        cursor_ray(&input, x, y).map(|dir| {
            let depth = (focus - cam.0.eye).norm().max(1.0);
            cam.0.eye + dir * depth
        })
    });
    // The whole-craft carry needs a world-space drop point that only
    // the view can compute — push it every frame so `Drop`/`Select`
    // on empty space can land the lifted root there.
    vab.0.set_drop_anchor(float_anchor);
    match vab.0.carried() {
        Some(csp_ui::vab::Carried::Placing(part)) => {
            if let Some(eff) = vab.0.resolve_stock(part) {
                let shape = crate::part_mesh::classify_effective(&eff);
                if let Some(hit) = hover {
                    let plan = vab.0.plan_attach(part, hit.node);
                    if plan.edges.is_empty() {
                        status = plan.error.clone();
                        // Red ghost anchored at the hit point — the
                        // "can't here" read.
                        if let Some(&pt) = vab.0.body_transforms().get(&hit.node) {
                            scene.ghosts.push(GhostMesh {
                                transform: PartTransform::new(pt.apply(hit.local), pt.rotation),
                                shape: shape.clone(),
                                color: [0.95, 0.30, 0.25, 0.55],
                                model: ghost_model(
                                    &art,
                                    &eff,
                                    crate::part_mesh::is_pointed(&shape),
                                ),
                            });
                        }
                    } else {
                        if let Some(&parent_t) = vab.0.body_transforms().get(&hit.node) {
                            for t in &plan.transforms {
                                scene.ghosts.push(GhostMesh {
                                    transform: parent_t.compose(*t),
                                    shape: shape.clone(),
                                    color: [0.35, 0.95, 0.45, 0.45],
                                    model: ghost_model(
                                        &art,
                                        &eff,
                                        crate::part_mesh::is_pointed(&shape),
                                    ),
                                });
                            }
                        }
                    }
                    emit_markers(
                        &vab.0,
                        &mut scene,
                        hit.node,
                        &plan,
                        freed_move_node(&vab.0, hit.node),
                    );
                } else if let Some(anchor) = float_anchor {
                    scene.ghosts.push(GhostMesh {
                        transform: PartTransform::new(anchor, DQuat::IDENTITY),
                        shape: shape.clone(),
                        color: [0.65, 0.78, 1.0, 0.40],
                        model: ghost_model(&art, &eff, crate::part_mesh::is_pointed(&shape)),
                    });
                }
            }
        }
        Some(csp_ui::vab::Carried::Moving(node)) => {
            // The carried subtree rides rigidly: each member's pose
            // relative to the lifted node folds under wherever the
            // lift lands.
            let rels = vab.0.subtree_rel_transforms(node);
            let mut members: Vec<(
                csp_phys::PartTransform,
                crate::part_mesh::PartShape,
                Option<std::sync::Arc<crate::art::PartModel>>,
            )> = Vec::with_capacity(rels.len());
            for (id, rel) in &rels {
                let Some(rec) = vab.0.session().node(*id) else {
                    continue;
                };
                if let Some(eff) = vab.0.resolve_built(rec.built()) {
                    let shape = crate::part_mesh::classify_effective(&eff);
                    members.push((
                        *rel,
                        shape.clone(),
                        ghost_model(&art, &eff, crate::part_mesh::is_pointed(&shape)),
                    ));
                }
            }
            if members.is_empty() {
                status = Some("the carried part failed to resolve".to_string());
            }
            let (anchor, color): (Option<PartTransform>, [f32; 4]) = if let Some(hit) = hover {
                let plan = vab.0.plan_move(node, hit.node);
                emit_markers(
                    &vab.0,
                    &mut scene,
                    hit.node,
                    &plan,
                    freed_move_node(&vab.0, hit.node),
                );
                if plan.edges.is_empty() {
                    status = plan.error.clone();
                    (
                        vab.0
                            .body_transforms()
                            .get(&hit.node)
                            .map(|pt| PartTransform::new(pt.apply(hit.local), pt.rotation))
                            .or_else(|| {
                                float_anchor.map(|a| PartTransform::new(a, DQuat::IDENTITY))
                            }),
                        [0.95, 0.30, 0.25, 0.55],
                    )
                } else {
                    (
                        vab.0
                            .body_transforms()
                            .get(&hit.node)
                            .map(|pt| pt.compose(plan.transforms[0])),
                        [0.35, 0.95, 0.45, 0.45],
                    )
                }
            } else {
                // Floating — keep the lifted node's current body
                // orientation while it rides the cursor.
                let rot = vab
                    .0
                    .body_transforms()
                    .get(&node)
                    .map(|t| t.rotation)
                    .unwrap_or(DQuat::IDENTITY);
                (
                    float_anchor.map(|a| PartTransform::new(a, rot)),
                    [0.65, 0.78, 1.0, 0.40],
                )
            };
            if let Some(base) = anchor {
                for (rel, shape, model) in &members {
                    scene.ghosts.push(GhostMesh {
                        transform: base.compose(*rel),
                        shape: shape.clone(),
                        color,
                        model: model.clone(),
                    });
                }
            }
        }
        None => {}
    }
    vab.0.set_ghost_status(status);
}

/// The node id a lifted move frees on `hit_node`, if it is the moved
/// part's current parent — its old mount reads "free" to the markers
/// while the lift is in progress.
fn freed_move_node(
    vab: &csp_ui::vab::VabSession,
    hit_node: csp_phys::PartNodeId,
) -> Option<String> {
    let moving = vab.move_node()?;
    vab.session()
        .node(moving)
        .and_then(|n| n.edge())
        .filter(|e| e.parent == hit_node)
        .map(|e| e.parent_node.clone())
}

/// Attach-node markers on the hovered part while something is
/// carried: free authored nodes green, bound ones dim, the plan's
/// chosen parent node brightest — the "where it snaps" read a text
/// panel can't give.
fn emit_markers(
    vab: &csp_ui::vab::VabSession,
    scene: &mut VabScene,
    hit_node: csp_phys::PartNodeId,
    plan: &csp_ui::vab::AttachPlan,
    freed: Option<String>,
) {
    let (Some(&t), Some(def)) = (vab.body_transforms().get(&hit_node), vab.def_of(hit_node)) else {
        return;
    };
    let mut bound = vab.session().bound_attach_nodes(hit_node);
    if let Some(f) = freed {
        bound.remove(&f);
    }
    let chosen = plan.edges.first().map(|e| e.parent_node.as_str());
    let (bs, bt) = vab
        .session()
        .node(hit_node)
        .map(|n| (n.built().scale, n.built().stretch))
        .unwrap_or((1.0, 1.0));
    for n in &def.attach_nodes {
        let Some(sn) = csp_phys::session::scaled_attach_node(def, &n.id, bs, bt) else {
            continue;
        };
        let pos = t.apply(sn.position_m);
        let c = if chosen == Some(n.id.as_str()) {
            [0.2, 1.0, 0.4, 1.0]
        } else if bound.contains(&n.id) {
            [0.45, 0.45, 0.45, 0.5]
        } else {
            [0.25, 0.9, 0.35, 0.8]
        };
        scene.markers.push((pos, c));
    }
    // The plan's surface mount point gets a marker too.
    if let Some(e) = plan.edges.first()
        && e.parent_node
            .starts_with(csp_phys::session::SURFACE_NODE_PREFIX)
        && let Some(sn) = csp_phys::session::decode_surface_node(&e.parent_node)
    {
        // `surf:` ids store def-local values — scale to the node's
        // live build before applying its transform.
        let p = DVec3::new(
            sn.position_m.x * bs,
            sn.position_m.y * bs,
            sn.position_m.z * bs * bt,
        );
        scene.markers.push((t.apply(p), [0.2, 1.0, 0.4, 1.0]));
    }
}

/// The viewport cursor's world-space ray direction (origin is
/// `cam.eye`) — shared by the hover pick and the carried ghost's
/// floating anchor.
fn cursor_ray(input: &VabInput, px_x: f64, px_y: f64) -> Option<DVec3> {
    let (w, h) = input.viewport_px;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    // NDC → view → world. The projection is `perspective_revz`: on the
    // −z forward plane, x_ndc = fx·vx, y_ndc = fy·vy.
    let nx = 2.0 * px_x / w - 1.0;
    let ny = 1.0 - 2.0 * px_y / h;
    let f = 1.0 / f64::tan(VAB_FOV_Y * 0.5);
    let aspect = (w / h).max(1e-6);
    let dir_view = DVec3::new(nx * aspect / f, ny / f, -1.0);
    let dir_len = dir_view.norm();
    if dir_len <= 0.0 {
        return None;
    }
    let dir_view = dir_view / dir_len;
    let r = map::rot3(input.yaw_rad, input.pitch_rad);
    Some(map::mat3_vec(&r, dir_view))
}

/// Cast the viewport cursor's ray against each part's kitbash AABB in
/// its local frame; the nearest hit wins. Returns the node, the hit
/// point in the node's scaled local frame (the surface-mount plan
/// records it verbatim), and which face region it lands on.
fn hover_pick(
    input: &VabInput,
    cam: &MapCameraF64,
    parts: &[PartMesh],
    px_x: f64,
    px_y: f64,
) -> Option<csp_ui::vab::HoverHit> {
    let dir = cursor_ray(input, px_x, px_y)?;
    let mut best: Option<(f64, csp_phys::PartNodeId, DVec3, csp_ui::vab::AttachRegion)> = None;
    for p in parts {
        let inv = p.transform.inverse();
        let o = inv.apply(cam.eye);
        let d = inv.apply_dir(dir);
        let (lo, hi) = p.shape.local_aabb();
        let Some((t, face)) = slab_hit(o, d, lo, hi) else {
            continue;
        };
        let hit_pt = o + d * t;
        let region = match face {
            2 => {
                if hit_pt.z >= 0.0 {
                    csp_ui::vab::AttachRegion::TopCap
                } else {
                    csp_ui::vab::AttachRegion::BottomCap
                }
            }
            _ => csp_ui::vab::AttachRegion::Side,
        };
        if best.as_ref().is_none_or(|(bt, ..)| t < *bt) {
            best = Some((t, p.node, hit_pt, region));
        }
    }
    best.map(|(_, node, local, region)| csp_ui::vab::HoverHit {
        node,
        local,
        region,
    })
}

/// Ray–AABB slab test in a part's local frame: `(t_hit, face_axis)`
/// where the face axis 0/1/2 = x/y/z of the entry face. `None` on a
/// miss or a degenerate ray.
fn slab_hit(o: DVec3, d: DVec3, lo: DVec3, hi: DVec3) -> Option<(f64, usize)> {
    let mut t_min = f64::NEG_INFINITY;
    let mut t_max = f64::INFINITY;
    let mut face = 0usize;
    let ov = [o.x, o.y, o.z];
    let dv = [d.x, d.y, d.z];
    let lv = [lo.x, lo.y, lo.z];
    let hv = [hi.x, hi.y, hi.z];
    for axis in 0..3 {
        let (oc, dc, lc, hc) = (ov[axis], dv[axis], lv[axis], hv[axis]);
        if dc.abs() < 1e-12 {
            if oc < lc || oc > hc {
                return None;
            }
            continue;
        }
        let (mut t1, mut t2) = ((lc - oc) / dc, (hc - oc) / dc);
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        if t1 > t_min {
            t_min = t1;
            face = axis;
        }
        t_max = t_max.min(t2);
        if t_min > t_max {
            return None;
        }
    }
    let t = if t_min >= 0.0 { t_min } else { t_max };
    (t >= 0.0 && t.is_finite()).then_some((t, face))
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
/// tagged with its 1-based pick index. `pick_generation` stamps the
/// packet's `pick_ids` table so a late GPU readback decoded against
/// a REPLACED session is dropped, not mis-applied (P10-F3) — it is
/// the session-replacement counter, not `craft_epoch` (which only
/// New/load bump, for the camera refit — C17-F3).
pub(crate) fn extract_vab(
    input: Res<VabInput>,
    vab: Res<VabSessionRes>,
    scene: Res<VabScene>,
    cam: Res<VabCameraF64>,
    mut frame: ResMut<VabFrame>,
) {
    if !input.open || !cam.0.valid {
        frame.0 = None;
        return;
    }
    let mut packet = VabPacket {
        pick_generation: vab.0.pick_generation(),
        ..VabPacket::default()
    };
    // The editor environment — a backdrop gradient (97.8% of the
    // frame was a dead void), a lit hangar floor under the craft's
    // lowest extent, a soft contact blob grounding the craft, and a
    // reference grid (vision-report VAB-3.1). It goes to its OWN
    // stream: the color pass draws it without depth writes (it can
    // never hide a part) and the pick pass never sees it (a click
    // through the floor resolves to the part behind it — the
    // "can't place shit below" fix).
    emit_environment(&mut packet.env_vertices, &cam.0, &scene);
    let mut tris = Vec::new();
    for p in &scene.parts {
        // The selection outline: an inflated copy of the silhouette
        // emitted FIRST in the selection accent — the part's own tris
        // overwrite its face, leaving the shell's rim as a visible
        // outline. Pick id 0 keeps the shell unpickable.
        if p.selected {
            let shell = crate::part_mesh::inflated(&p.shape, scene.extent_m * 0.008 + 0.01);
            emit_triangles(
                &mut packet.vertices,
                &mut tris,
                &cam.0,
                &shell,
                None,
                p.transform,
                [0.25, 1.0, 0.5, 1.0],
                0,
            );
        }
        let pick = packet.pick_ids.len() as u32 + 1;
        packet.pick_ids.push(p.node.0);
        emit_part(&mut packet.vertices, &mut tris, &cam.0, p, pick);
    }
    // Placement ghosts — translucent, unpickable, emitted after the
    // solid craft so their alpha reads over it.
    for g in &scene.ghosts {
        emit_triangles(
            &mut packet.vertices,
            &mut tris,
            &cam.0,
            &g.shape,
            g.model.as_deref(),
            g.transform,
            g.color,
            0,
        );
    }
    // Attach-node markers — a tiny octahedron per point.
    let marker_m = (scene.extent_m * 0.012 + 0.02).max(0.03);
    for (pos, color) in &scene.markers {
        emit_octa(&mut packet.vertices, &cam.0, *pos, marker_m, *color);
    }
    frame.0 = Some(packet);
}

/// The VAB studio environment (vision-report 3.1 — the craft floated
/// in a 97.8%-uniform void): a far-plane backdrop gradient, a lit
/// floor disc fading at its rim, a soft contact blob under the craft,
/// and a Cartesian reference grid. Everything is emitted into the
/// packet's `env_vertices` stream — drawn first WITHOUT depth writes
/// (decoration never occludes a part) and never rasterized by the
/// pick pass (a click through the floor resolves to the part behind
/// it). Before the split the floor shared the pick stream's depth
/// buffer, so from beneath the craft it won the depth test in both
/// passes: the bottom cap was unviewable AND unpickable.
#[allow(clippy::too_many_lines)] // one environment assembly pass; splitting fragments the scene
fn emit_environment(verts: &mut Vec<VabVertex>, cam: &MapCameraF64, scene: &VabScene) {
    // Backdrop: a two-stop vertical gradient quad pinned just inside
    // the far plane (NDC z = 1e-5 — the VAB depth test is `Greater`
    // against a 0.0 clear, so z = 0 itself can never pass). Unlit:
    // these are authored colors, not lit material.
    let zenith = [0.028, 0.034, 0.058, 1.0];
    let nadir = [0.010, 0.012, 0.024, 1.0];
    for (pos, color) in [
        ([-1.0f32, -1.0, 1e-5], nadir),
        ([1.0, -1.0, 1e-5], nadir),
        ([1.0, 1.0, 1e-5], zenith),
        ([-1.0, -1.0, 1e-5], nadir),
        ([1.0, 1.0, 1e-5], zenith),
        ([-1.0, 1.0, 1e-5], zenith),
    ] {
        verts.push(unlit_v(pos, color, 0));
    }

    // The floor sits just under the craft's lowest hull point so the
    // stack reads as standing on it; an empty craft gets a default
    // apron under the origin.
    let mut min_z = f64::INFINITY;
    for p in &scene.parts {
        let (lo, hi) = p.shape.local_aabb();
        for x in [lo.x, hi.x] {
            for y in [lo.y, hi.y] {
                for z in [lo.z, hi.z] {
                    min_z = min_z.min(p.transform.apply(DVec3::new(x, y, z)).z);
                }
            }
        }
    }
    let extent = scene.extent_m.max(1.0);
    // A real gap below the lowest hull point (was 2% — the apron
    // hugged the bottom cap, which is half of why the stack read as
    // glued to the ground and why the cap was hard to reach). The
    // floor no longer occludes or picks, so this margin is purely
    // compositional — a wide enough gap that a low camera reads the
    // cap as free space, not floor.
    let floor_z = if min_z.is_finite() {
        min_z - (0.30 * extent).max(0.15)
    } else {
        scene.center.z - extent * 1.2
    };
    let c = scene.center;
    // One floor triangle — all three corners must project (the same
    // all-or-nothing rule `emit_part` uses); emitted LIT so the apron
    // shades under the packet's studio light like the parts do.
    let emit_floor_tri = |verts: &mut Vec<VabVertex>, world: [DVec3; 3], alpha: [f32; 3]| {
        let [Some(c0), Some(c1), Some(c2)] =
            world.map(|q| project_into(&cam.view_proj, cam.eye, q))
        else {
            return;
        };
        for (k, q) in world.iter().enumerate() {
            let rel = *q - cam.eye;
            verts.push(VabVertex {
                pos: [c0, c1, c2][k],
                world: f3(rel),
                nrm: [0.0, 0.0, 1.0],
                color: [0.115, 0.125, 0.160, alpha[k]],
                mat: [0.04, 8.0, 0.9],
                pick: 0,
            });
        }
    };

    // Lit floor disc — a center fan plus an outer ring whose alpha
    // fades to nothing so the apron melts into the backdrop instead
    // of hard-clipping at its rim.
    const SEGS: usize = 48;
    let r_in = (extent * 3.0).clamp(6.0, 300.0);
    let r_out = r_in * 1.8;
    let ring = |r: f64, i: usize| {
        let t = (i % SEGS) as f64 * std::f64::consts::TAU / SEGS as f64;
        DVec3::new(c.x + r * t.cos(), c.y + r * t.sin(), floor_z)
    };
    for i in 0..SEGS {
        emit_floor_tri(
            verts,
            [
                DVec3::new(c.x, c.y, floor_z),
                ring(r_in, i),
                ring(r_in, i + 1),
            ],
            [1.0, 1.0, 1.0],
        );
    }
    // Fade ring — same lit shading, alpha 1 → 0 across the rim.
    for i in 0..SEGS {
        emit_floor_tri(
            verts,
            [ring(r_in, i), ring(r_out, i), ring(r_out, i + 1)],
            [1.0, 0.0, 0.0],
        );
        emit_floor_tri(
            verts,
            [ring(r_in, i), ring(r_out, i + 1), ring(r_in, i + 1)],
            [1.0, 0.0, 1.0],
        );
    }
    // Contact blob — an unlit alpha-gradient fan a few millimetres
    // over the floor, grounding the craft the way the terrain pass's
    // blob shadow does on the pad.
    let blob_r = extent * 1.05;
    let z_blob = floor_z + 0.004;
    for i in 0..SEGS {
        let t0 = (i % SEGS) as f64 * std::f64::consts::TAU / SEGS as f64;
        let t1 = ((i + 1) % SEGS) as f64 * std::f64::consts::TAU / SEGS as f64;
        let world = [
            DVec3::new(c.x, c.y, z_blob),
            DVec3::new(c.x + blob_r * t0.cos(), c.y + blob_r * t0.sin(), z_blob),
            DVec3::new(c.x + blob_r * t1.cos(), c.y + blob_r * t1.sin(), z_blob),
        ];
        let [Some(c0), Some(c1), Some(c2)] =
            world.map(|q| project_into(&cam.view_proj, cam.eye, q))
        else {
            continue;
        };
        for (k, a) in [0.42f32, 0.0, 0.0].iter().enumerate() {
            verts.push(unlit_v([c0, c1, c2][k], [0.0, 0.0, 0.02, *a], 0));
        }
    }

    // Reference grid — thin unlit quads on the plane; line spacing is
    // a round fraction of the craft extent so it reads at any scale.
    let z_grid = floor_z + 0.007;
    let spacing = {
        let s = extent / 6.0;
        let p = s.log10().floor();
        let m = s / 10f64.powf(p);
        let m = if m < 1.5 {
            1.0
        } else if m < 3.5 {
            2.0
        } else if m < 7.5 {
            5.0
        } else {
            10.0
        };
        m * 10f64.powf(p)
    };
    let half = r_in * 0.55;
    let w = (extent * 0.004).clamp(0.004, 0.05);
    let n_lines = (half / spacing).floor() as i32;
    for i in -n_lines..=n_lines {
        let off = i as f64 * spacing;
        let major = i == 0 || i % 5 == 0;
        let col = if major {
            [0.36, 0.44, 0.62, 0.55]
        } else {
            [0.26, 0.32, 0.46, 0.28]
        };
        for (a, b) in [
            (
                DVec3::new(c.x + off - w, c.y - half, z_grid),
                DVec3::new(c.x + off + w, c.y + half, z_grid),
            ),
            (
                DVec3::new(c.x - half, c.y + off - w, z_grid),
                DVec3::new(c.x + half, c.y + off + w, z_grid),
            ),
        ] {
            let q = [
                a,
                DVec3::new(b.x, a.y, a.z),
                b,
                a,
                b,
                DVec3::new(a.x, b.y, a.z),
            ];
            let mut clip = [None; 6];
            let mut all = true;
            for (k, p) in q.iter().enumerate() {
                clip[k] = project_into(&cam.view_proj, cam.eye, *p);
                all &= clip[k].is_some();
            }
            if !all {
                continue;
            }
            for c in clip {
                verts.push(unlit_v(c.unwrap_or([0.0; 3]), col, 0));
            }
        }
    }
}

/// `DVec3` → f32 triple — the camera-relative world position and
/// world normal channels of the lit vertex format.
fn f3(v: DVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

/// An unlit-stream vertex — selection shells, attach-node markers,
/// ghost accents: `nrm` ≈ 0 marks the fragment-shader passthrough so
/// `color` lands unchanged.
fn unlit_v(pos: [f32; 3], color: [f32; 4], pick: u32) -> VabVertex {
    VabVertex {
        pos,
        world: [0.0; 3],
        nrm: [0.0; 3],
        color,
        mat: [0.0; 3],
        pick,
    }
}

/// Material terms for the kitbash fallback in the editor — a generic
/// painted-metal pair; the real models carry their own baked terms.
const VAB_KITBASH_MAT: [f32; 3] = [0.45, 44.0, 1.0];

/// The baked-model triangle walk `emit_part` and `emit_triangles`
/// share (C17-F9 — was two hand-kept copies): `emit_model` wedges +
/// authored albedo + baked material terms, transformed and projected
/// corner-whole like the kitbash path. The two tint decisions are
/// caller policy — `tint` multiplies the albedo rgb (white keeps it
/// raw, `emit_part`'s look) and `alpha` overrides the albedo alpha
/// (`Some`, the ghost's translucent tint) or leaves the baked
/// vertex alpha (`None`, `emit_part`'s raw alpha).
#[allow(clippy::too_many_arguments)]
fn emit_model_tris(
    verts: &mut Vec<VabVertex>,
    cam: &MapCameraF64,
    m: &crate::art::PartModel,
    shape: &crate::part_mesh::PartShape,
    transform: csp_phys::PartTransform,
    tint: [f32; 3],
    alpha: Option<f32>,
    pick: u32,
) {
    let (lo, hi) = shape.local_aabb();
    let mut mtris = Vec::new();
    crate::part_mesh::emit_model(m, lo, hi, &mut mtris);
    for t in &mtris {
        let world = [t.a, t.b, t.c].map(|q| transform.apply(q));
        let [Some(c0), Some(c1), Some(c2)] =
            world.map(|q| project_into(&cam.view_proj, cam.eye, q))
        else {
            continue;
        };
        for (k, &(n, alb, mt)) in [(t.na, t.ca, t.ma), (t.nb, t.cb, t.mb), (t.nc, t.cc, t.mc)]
            .iter()
            .enumerate()
        {
            verts.push(VabVertex {
                pos: [c0, c1, c2][k],
                world: f3(world[k] - cam.eye),
                nrm: f3(transform.apply_dir(n)),
                color: [
                    alb[0] * tint[0],
                    alb[1] * tint[1],
                    alb[2] * tint[2],
                    alpha.unwrap_or(alb[3]),
                ],
                mat: mt,
                pick,
            });
        }
    }
}

/// Emit one kitbash shape's triangles under `transform` —
/// `emit_part`'s core with the shape/color/pick as parameters so the
/// selection shell, ghosts, and solid parts share the code path.
/// Models and kitbash silhouettes emit LIT vertices (the fragment
/// shader lights them per pixel under `packet.light`).
#[allow(clippy::too_many_arguments)]
fn emit_triangles(
    verts: &mut Vec<VabVertex>,
    tris: &mut Vec<crate::part_mesh::SmoothTri>,
    cam: &MapCameraF64,
    shape: &crate::part_mesh::PartShape,
    model: Option<&crate::art::PartModel>,
    transform: csp_phys::PartTransform,
    color: [f32; 4],
    pick: u32,
) {
    if let Some(m) = model {
        // Ghost model: the real mesh under the ghost's translucent
        // tint (albedo × tint-hue, alpha from the ghost color).
        emit_model_tris(
            verts,
            cam,
            m,
            shape,
            transform,
            [color[0], color[1], color[2]],
            Some(color[3]),
            pick,
        );
        return;
    }
    tris.clear();
    crate::part_mesh::emit_smooth(shape, tris);
    for t in tris.iter() {
        let world = [t.a, t.b, t.c].map(|q| transform.apply(q));
        let [Some(c0), Some(c1), Some(c2)] =
            world.map(|q| project_into(&cam.view_proj, cam.eye, q))
        else {
            continue;
        };
        for (k, &n) in [t.na, t.nb, t.nc].iter().enumerate() {
            verts.push(VabVertex {
                pos: [c0, c1, c2][k],
                world: f3(world[k] - cam.eye),
                nrm: f3(transform.apply_dir(n)),
                color,
                mat: VAB_KITBASH_MAT,
                pick,
            });
        }
    }
}

/// Emit a small unlit octahedron at `pos` — the attach-node marker.
/// Pick id 0; the marker is an affordance, not a selectable part.
fn emit_octa(verts: &mut Vec<VabVertex>, cam: &MapCameraF64, pos: DVec3, m: f64, color: [f32; 4]) {
    let pts = [
        DVec3::new(m, 0.0, 0.0),
        DVec3::new(-m, 0.0, 0.0),
        DVec3::new(0.0, m, 0.0),
        DVec3::new(0.0, -m, 0.0),
        DVec3::new(0.0, 0.0, m),
        DVec3::new(0.0, 0.0, -m),
    ];
    // The octahedron's 8 faces — apex pairs around the equator.
    let faces: [[usize; 3]; 8] = [
        [4, 0, 2],
        [4, 2, 1],
        [4, 1, 3],
        [4, 3, 0],
        [5, 2, 0],
        [5, 1, 2],
        [5, 3, 1],
        [5, 0, 3],
    ];
    for f in &faces {
        let world = f.map(|i| pos + pts[i]);
        let [Some(c0), Some(c1), Some(c2)] =
            world.map(|q| project_into(&cam.view_proj, cam.eye, q))
        else {
            continue;
        };
        for c in [c0, c1, c2] {
            verts.push(unlit_v(c, color, 0));
        }
    }
}

/// Resolve a ghost/carried part's model through the same id-first
/// keying `vab_present` uses for placed parts — `pointed` comes from
/// the caller's already-classified `PartShape` (C20-F1: the ghost
/// used to hard-code Structural/flat and could resolve a different
/// key than the placed part it previews).
fn ghost_model(
    art: &Option<Res<crate::art::ArtAssets>>,
    eff: &csp_phys::EffectivePart,
    pointed: bool,
) -> Option<std::sync::Arc<crate::art::PartModel>> {
    art.as_ref()?
        .0
        .models
        .get(crate::art::model_key(
            Some(eff.part_id.as_str()),
            crate::flight::part_kind(&eff.weld_input),
            pointed,
        ))
        .cloned()
}

/// Push one part's triangles into `verts`. A triangle with ANY corner
/// failing projection (behind the eye plane) is dropped whole rather
/// than clipped or clamped — the box pass's per-face rule carried
/// over per-triangle; at these scales the near plane never sits inside
/// the craft in practice. All part streams emit LIT vertices — the
/// fragment shader shades per pixel under `packet.light`.
fn emit_part(
    verts: &mut Vec<VabVertex>,
    tris: &mut Vec<crate::part_mesh::SmoothTri>,
    cam: &MapCameraF64,
    p: &PartMesh,
    pick: u32,
) {
    if let Some(m) = &p.model {
        // Real baked model (procedural, parts.csar): wedge normals +
        // authored albedo + baked material terms — raw albedo and the
        // baked per-vertex alpha (the white-tint/alpha-defer decision
        // `emit_model_tris` documents).
        emit_model_tris(
            verts,
            cam,
            m,
            &p.shape,
            p.transform,
            [1.0, 1.0, 1.0],
            None,
            pick,
        );
        return;
    }
    tris.clear();
    crate::part_mesh::emit_smooth(&p.shape, tris);
    for t in tris.iter() {
        let world = [t.a, t.b, t.c].map(|q| p.transform.apply(q));
        let [Some(c0), Some(c1), Some(c2)] =
            world.map(|q| project_into(&cam.view_proj, cam.eye, q))
        else {
            continue; // tri crosses the eye plane — drop it whole
        };
        for (k, &n) in [t.na, t.nb, t.nc].iter().enumerate() {
            verts.push(VabVertex {
                pos: [c0, c1, c2][k],
                world: f3(world[k] - cam.eye),
                nrm: f3(p.transform.apply_dir(n)),
                color: p.color,
                mat: VAB_KITBASH_MAT,
                pick,
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The orbit clamp must let the eye below the craft's mid-plane.
    /// `cos(pitch)·distance` is the eye's height over the focus, so a
    /// sub-π/2 clamp keeps the bottom cap — and its `bottom` attach
    /// node — forever unviewable and unhittable ("can't place shit
    /// below"). Pole-to-pole (`±π`) reaches the view from directly
    /// beneath the stack.
    #[test]
    fn orbit_reaches_below_the_craft() {
        let mut input = VabInput::default();
        for _ in 0..200 {
            input.apply(&VabEvent::Orbit {
                dyaw: 0.0,
                dpitch: 1.0,
            });
        }
        assert_eq!(input.pitch_rad, PITCH_LIMIT);
        // At the pole the eye sits directly under the focus: the
        // camera looks straight up at the bottom cap.
        let cam = solve_orbit_camera(
            0.0,
            input.pitch_rad,
            10.0,
            input.viewport_px,
            DVec3::ZERO,
            VAB_FOV_Y,
            VAB_NEAR_M,
        );
        assert!(cam.valid);
        assert!(
            cam.eye.z < -9.0,
            "eye never left the upper half: eye.z = {}",
            cam.eye.z
        );
    }

    /// From beneath, the center-screen hover ray enters the part's
    /// −z AABB face and reports `BottomCap` — the region
    /// `VabSession::plan_axial` reads to pick the parent's
    /// down-facing authored node.
    #[test]
    fn hover_from_below_hits_the_bottom_cap() {
        let mut input = VabInput {
            yaw_rad: 0.0,
            pitch_rad: std::f64::consts::PI, // directly below, looking up
            distance_m: 10.0,
            ..VabInput::default()
        };
        let cam = solve_orbit_camera(
            input.yaw_rad,
            input.pitch_rad,
            input.distance_m,
            input.viewport_px,
            DVec3::ZERO,
            VAB_FOV_Y,
            VAB_NEAR_M,
        );
        let parts = vec![PartMesh {
            node: csp_phys::PartNodeId(1),
            transform: PartTransform::IDENTITY,
            shape: crate::part_mesh::PartShape::Box {
                lo: DVec3::new(-0.5, -0.5, -0.5),
                hi: DVec3::new(0.5, 0.5, 0.5),
            },
            color: [1.0; 4],
            selected: false,
            model: None,
        }];
        let (w, h) = input.viewport_px;
        let hit = hover_pick(&input, &cam, &parts, w * 0.5, h * 0.5)
            .expect("center pixel must hit the box from below");
        assert_eq!(hit.node, csp_phys::PartNodeId(1));
        assert_eq!(hit.region, csp_ui::vab::AttachRegion::BottomCap);
        assert!(hit.local.z < 0.0);
        // And the top cap still classifies from above.
        input.pitch_rad = 0.0;
        let cam = solve_orbit_camera(
            input.yaw_rad,
            input.pitch_rad,
            input.distance_m,
            input.viewport_px,
            DVec3::ZERO,
            VAB_FOV_Y,
            VAB_NEAR_M,
        );
        let hit = hover_pick(&input, &cam, &parts, w * 0.5, h * 0.5)
            .expect("center pixel must hit the box from above");
        assert_eq!(hit.region, csp_ui::vab::AttachRegion::TopCap);
    }

    /// PRD-11 H1 end-to-end through the real `View`: the camera
    /// orbited straight under the craft, the cursor over the bottom
    /// cap — the pick→select channel attaches the armed part BELOW
    /// the stack, and the packet's environment stream never enters
    /// the pickable stream (a floor pixel can't shadow a part id).
    #[test]
    fn bottom_cap_pick_attaches_below_and_env_never_picks() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let parts = csp_data::load_parts(&data.join("parts")).unwrap();
        let res = csp_data::load_resources(&data.join("resources.ron")).unwrap();
        let mut view = crate::View::with_sim(csp_sim::World::new());
        view.install_vab_catalog(parts, res, csp_data::data_set_hash(&data).ok());
        view.set_mode(crate::ViewMode::Vab);
        // The default-on tutorial card anchors center-top and masks
        // dead-center of the viewport — dismiss it so the probe pixel
        // reaches the scene.
        view.set_tutorial_active(false);
        view.set_vab_viewport(800.0, 600.0);
        let tank = csp_data::PartId::from("csp.tank.s320");
        view.with_vab_session(|v| {
            v.apply(csp_ui::vab::VabAction::SetRoot(tank.clone()))
                .unwrap()
        });
        let root_id = view.with_vab_session(|v| v.session().root().unwrap());
        // Orbit straight under the craft (pitch clamps at PI) — the
        // drag event lands through the real event drain.
        view.vab_event(VabEvent::Orbit {
            dyaw: 0.0,
            dpitch: 10.0,
        });
        view.begin_frame();
        // Pointer at viewport center; egui keeps it across frames.
        view.set_egui_input(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            focused: true,
            events: vec![egui::Event::PointerMoved(egui::pos2(400.0, 300.0))],
            ..Default::default()
        });
        // Two frames: UiBuild publishes cursor_px into the session,
        // Presentation turns it into `hover` on the NEXT frame.
        view.end_frame();
        view.begin_frame();
        view.end_frame();
        let hit = view
            .with_vab_session(|v| v.hover())
            .expect("cursor over the bottom cap from below");
        assert_eq!(hit.node, root_id);
        assert_eq!(hit.region, csp_ui::vab::AttachRegion::BottomCap);
        // The pick stream carries ONLY pickable geometry: the floor /
        // backdrop / grid ride `env_vertices` and are never
        // rasterized by the object-id pass.
        let packet = view.take_vab_packet().expect("a VAB packet this frame");
        assert!(!packet.env_vertices.is_empty(), "the hangar is drawn");
        assert!(packet.env_vertices.iter().all(|v| v.pick == 0));
        let n_ids = packet.pick_ids.len() as u32;
        assert!(
            packet.vertices.iter().all(|v| v.pick <= n_ids),
            "every pickable vertex decodes to a real part index (or 0)"
        );
        // Arm the tank and click the cap — the pick-landed Select
        // places it BELOW the stack (canonical axial attach).
        view.with_vab_session(|v| v.apply(csp_ui::vab::VabAction::CatalogPick(tank)).unwrap());
        view.vab_event(VabEvent::Pick(Some(root_id.0)));
        view.begin_frame();
        view.end_frame();
        assert_eq!(view.with_vab_session(|v| v.session().len()), 2);
        let off = view.with_vab_session(|v| {
            v.session()
                .iter()
                .find(|(id, _)| *id != root_id)
                .map(|(_, n)| n.transform().offset)
                .unwrap()
        });
        assert!(off.z < 0.0, "the new part landed below the stack");
    }

    /// Signed 2D edge function — twice the signed area of (a, b, c).
    fn edge2(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> f32 {
        (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0])
    }

    /// Rasterize `verts` through the pick pipeline's math — the C17
    /// verifier's probe: NDC positions as a w=1 passthrough
    /// (`vs_main` writes `vec4(v.pos, 1)`), reverse-Z depth with
    /// `Greater` winning against a 0.0 clear, pixel-center sampling,
    /// triangle list, no cull. `ranges` selects WHICH vertices draw:
    /// production draws [`csp_render::vab::pickable_ranges`]; the
    /// pre-fix pass drew `0..len`. Returns the pick index per pixel
    /// of the `w`×`h` target (0 = background).
    fn raster_pick(
        verts: &[csp_render::vab::VabVertex],
        ranges: &[std::ops::Range<u32>],
        w: usize,
        h: usize,
    ) -> Vec<u32> {
        let mut depth = vec![0.0f32; w * h];
        let mut ids = vec![0u32; w * h];
        for r in ranges {
            for tri in verts[r.start as usize..r.end as usize].chunks_exact(3) {
                let p = [tri[0].pos, tri[1].pos, tri[2].pos];
                let area = edge2(p[0], p[1], p[2]);
                if area == 0.0 {
                    continue;
                }
                let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                for q in p {
                    x0 = x0.min(q[0]);
                    x1 = x1.max(q[0]);
                    y0 = y0.min(q[1]);
                    y1 = y1.max(q[1]);
                }
                // NDC → pixel: px = (x+1)/2·w, py = (1−y)/2·h.
                let px0 = (((x0 + 1.0) * 0.5 * w as f32).floor() as i32).max(0);
                let px1 = (((x1 + 1.0) * 0.5 * w as f32).ceil() as i32).min(w as i32);
                let py0 = (((1.0 - y1) * 0.5 * h as f32).floor() as i32).max(0);
                let py1 = (((1.0 - y0) * 0.5 * h as f32).ceil() as i32).min(h as i32);
                for py in py0..py1 {
                    for px in px0..px1 {
                        // Pixel center → NDC.
                        let nx = (px as f32 + 0.5) / w as f32 * 2.0 - 1.0;
                        let ny = 1.0 - (py as f32 + 0.5) / h as f32 * 2.0;
                        let c = [nx, ny, 0.0];
                        let w0 = edge2(p[1], p[2], c);
                        let w1 = edge2(p[2], p[0], c);
                        let w2 = edge2(p[0], p[1], c);
                        // No cull: either winding counts as inside.
                        if w0 * area < 0.0 || w1 * area < 0.0 || w2 * area < 0.0 {
                            continue;
                        }
                        let z = (w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2]) / area;
                        let i = py as usize * w + px as usize;
                        // `Greater` wins, strict — a 0.0 fragment
                        // can never land on the 0.0-cleared target.
                        if z > depth[i] {
                            depth[i] = z;
                            ids[i] = tri[0].pick;
                        }
                    }
                }
            }
        }
        ids
    }

    /// C17-F1 probe — software-rasterize the REAL packet two ways and
    /// show the fix end-to-end: the stream with the root SELECTED
    /// carries the inflated pick-0 shell, which strictly encloses the
    /// part and beats it at every covered pixel under `Greater`-wins
    /// reverse-Z (the verifier measured the click pixel decoding 0 at
    /// 1440/1440 samples). Drawing the whole stream reproduces that
    /// shadow; drawing [`csp_render::vab::pickable_ranges`] — what
    /// `encode_pick` now issues — decodes the root's pick index at
    /// the click pixel. Same rasterizer, same packet: only the vertex
    /// selection differs, which IS the fix.
    #[test]
    fn pick_probe_decodes_the_part_not_the_decoration() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let parts = csp_data::load_parts(&data.join("parts")).unwrap();
        let res = csp_data::load_resources(&data.join("resources.ron")).unwrap();
        let mut view = crate::View::with_sim(csp_sim::World::new());
        view.install_vab_catalog(parts, res, csp_data::data_set_hash(&data).ok());
        view.set_mode(crate::ViewMode::Vab);
        view.set_tutorial_active(false);
        view.set_vab_viewport(800.0, 600.0);
        let tank = csp_data::PartId::from("csp.tank.s320");
        view.with_vab_session(|v| {
            v.apply(csp_ui::vab::VabAction::SetRoot(tank.clone()))
                .unwrap()
        });
        // `SetRoot` selects the root — the packet's `vertices` then
        // carries the pick-0 inflated shell around it.
        let root_id = view.with_vab_session(|v| v.session().root().unwrap());
        view.begin_frame();
        view.set_egui_input(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            focused: true,
            events: vec![egui::Event::PointerMoved(egui::pos2(400.0, 300.0))],
            ..Default::default()
        });
        view.end_frame();
        view.begin_frame();
        view.end_frame();
        let packet = view.take_vab_packet().expect("a VAB packet this frame");
        // Non-vacuous: the stream DOES carry decoration — and the
        // cursor is on the root.
        assert!(
            packet.vertices.iter().any(|v| v.pick == 0),
            "the selection shell never made the stream"
        );
        let hit = view
            .with_vab_session(|v| v.hover())
            .expect("cursor over the root");
        assert_eq!(hit.node, root_id);

        let click = 300 * 800 + 400;
        let all: Vec<std::ops::Range<u32>> =
            std::iter::once(0..packet.vertices.len() as u32).collect();
        let pre_fix = raster_pick(&packet.vertices, &all, 800, 600);
        assert_eq!(
            pre_fix[click], 0,
            "the old pass should shadow the click pixel under the shell"
        );
        let post_fix = raster_pick(
            &packet.vertices,
            &csp_render::vab::pickable_ranges(&packet.vertices),
            800,
            600,
        );
        let px = post_fix[click];
        assert!(
            px >= 1 && packet.pick_ids[(px - 1) as usize] == root_id.0,
            "the click pixel must decode the root, got pick {px}"
        );
        // And the shell's shadow was broad, not a fringe case: most
        // of the root's projected pixels sat under pick-0 geometry.
        let shadowed = pre_fix
            .iter()
            .zip(&post_fix)
            .filter(|(old, new)| **old == 0 && **new != 0)
            .count();
        assert!(
            shadowed > 100,
            "only {shadowed} pixels changed — the probe barely exercises the shell"
        );
    }

    /// C17-F3 end-to-end: an undo keeps the camera exactly where the
    /// player framed it — distance, pan, and the consumed one-shot
    /// fit all survive — while the packet's `pick_generation` still
    /// advances (the stale-pick guard keeps its teeth on a session
    /// restore). The pre-split `craft_epoch` bump failed every one
    /// of these.
    #[test]
    fn undo_keeps_the_camera_and_advances_pick_generation() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let parts = csp_data::load_parts(&data.join("parts")).unwrap();
        let res = csp_data::load_resources(&data.join("resources.ron")).unwrap();
        let mut view = crate::View::with_sim(csp_sim::World::new());
        view.install_vab_catalog(parts, res, csp_data::data_set_hash(&data).ok());
        view.set_mode(crate::ViewMode::Vab);
        view.set_tutorial_active(false);
        view.set_vab_viewport(800.0, 600.0);
        let tank = csp_data::PartId::from("csp.tank.s320");
        view.with_vab_session(|v| {
            v.apply(csp_ui::vab::VabAction::SetRoot(tank.clone()))
                .unwrap()
        });
        let root = view.with_vab_session(|v| v.session().root().unwrap());
        // A frame pair: Presentation runs `vab_present`, the one-shot
        // fit frames the craft.
        view.begin_frame();
        view.end_frame();
        // The player reframes — orbit + zoom + pan land through the
        // real event drain.
        view.vab_event(VabEvent::Orbit {
            dyaw: 0.4,
            dpitch: -0.3,
        });
        view.vab_event(VabEvent::Zoom { factor: 0.5 });
        view.vab_event(VabEvent::Pan {
            dx_px: 60.0,
            dy_px: -40.0,
        });
        view.begin_frame();
        view.end_frame();
        let (dist, pan) = {
            let i = view.world.resource::<VabInput>();
            (i.distance_m, i.pan_offset)
        };
        // Make history, then walk it: attach + remove + undo.
        let child = view.with_vab_session(|v| {
            v.apply(csp_ui::vab::VabAction::AttachAuto {
                part: tank.clone(),
                parent: root,
            })
            .unwrap();
            v.selected().unwrap()
        });
        view.with_vab_session(|v| v.apply(csp_ui::vab::VabAction::Remove(child)).unwrap());
        let gen_before = view.with_vab_session(|v| v.pick_generation());
        let epoch_before = view.with_vab_session(|v| v.craft_epoch());
        view.with_vab_session(|v| v.apply(csp_ui::vab::VabAction::Undo).unwrap());
        assert_eq!(
            view.with_vab_session(|v| v.craft_epoch()),
            epoch_before,
            "undo must not bump the camera-refit epoch"
        );
        view.begin_frame();
        view.end_frame();
        let i = view.world.resource::<VabInput>();
        assert_eq!(i.distance_m, dist, "undo must not re-derive zoom");
        assert_eq!(i.pan_offset, pan, "undo must not drop the pan");
        assert!(i.focused_once, "the consumed fit stays consumed");
        // ... while the stale-pick guard still invalidated the old
        // `pick_ids` table.
        let packet = view.take_vab_packet().expect("a VAB packet this frame");
        assert_eq!(packet.pick_generation, gen_before + 1);
    }
}
