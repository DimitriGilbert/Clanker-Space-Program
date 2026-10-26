//! The M3 VAB — the editor's session-state owner and its egui panels
//! (PLAN M3 exit row, §2.2 category C).
//!
//! # Placement
//!
//! [`VabSession`] is the working-craft *instance* §2.1 assigns to
//! `csp-ui`: it owns the `csp_phys::session::CraftSession` (the type
//! lives in `csp-phys` — see its module doc for why), the loaded part
//! catalog + resource registry, the [`MissionProfile`] stub, the
//! selection, and the cached [`CraftAnalysis`]/validation. The instance
//! itself is hosted as a resource in `csp-view`'s presentation world —
//! the same hosting pattern `SessionMirror`/`MapInput` use — so the
//! `UiBuild` schedule's panel pass and the `Input` schedule's event
//! drain both reach it without `csp-app` ever reading the ECS world.
//!
//! # The session-command channel (§2.2)
//!
//! Every mutation goes through [`VabSession::apply`]`(VabAction)` —
//! the editor's `SessionCommand` analogue: egui panels call it
//! directly, winit-side input (GPU pick results, camera keys) arrives
//! through `csp-view`'s `VabEvent` queue into the same method, and
//! session-class MCP tools (`craft.load`, `editor.build`) will call it
//! at M9c. Nothing in the session is hashed, logged, or replayed —
//! §2.2-C state.
//!
//! # Readout
//!
//! `recompute` runs `csp_phys::readout::analyze` + `session.validate`
//! + `session.body_transforms` only when the session is dirty — every
//! mutating op (a stage assignment included: it changes the resolved
//! stage table) marks dirty, while a pick does not re-resolve. The
//! panel shows the stage-stats rows plus the [`MissionProfile`]
//! comparison.
//!
//! # The held-part model (KSP's VAB)
//!
//! A palette click never places anything — it lifts the part onto the
//! cursor (`pending_part`/`drag_part`), where it floats as a ghost
//! until the player clicks a craft part (the drop — `Select(Some)`
//! consumed through [`VabSession::plan_attach`], so a side-face click
//! surface-mounts and a cap click stacks), clicks the empty build
//! area of an empty craft (the root), or cancels (Esc/right-click —
//! [`VabAction::CancelPlacement`]). A click on empty space keeps the
//! part held. Placed parts move the same way: drag one — or click the
//! already-selected one — and it lifts as a ghost
//! ([`VabAction::Grab`], `move_node`), reattaching through
//! `CraftSession::reattach` on the next drop so the node keeps its
//! id, build, and subtree. [`VabSession::carried`] is the one
//! accessor every affordance (ghost, banner, cancel, drop) reads.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use csp_core::{DQuat, DVec3};
use csp_data::{
    LoadedParts, LocFile, ModuleDef, PartDef, PartId, ResourceFile, ScaleRule, VariantId,
};
use csp_phys::graph::PartTransform;
use csp_phys::readout::CraftAnalysis;
use csp_phys::resolve::EffectivePart;
use csp_phys::session::{AttachEdge, CraftSession, Validation};
use csp_phys::{BuiltPart, MissionProfile, PartNodeId};

/// How a placement decides *where* the new part mounts — the attach
/// UX toggle in the action bar. `Auto` reads the hovered face: top or
/// bottom caps stack axially, the hull side surface-mounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachMode {
    /// Pick by the hovered region — caps stack, sides surface-mount.
    Auto,
    /// Stack on an authored axial node (above or below).
    Axial,
    /// Surface-mount upright — the part keeps its own +z axis and
    /// lies flush against the hull (KSP's radial attach).
    Side,
    /// Mount an authored node (typically the part's `top`) against the
    /// hull — the part sticks out perpendicular to the surface.
    End,
}

/// Which face of the hovered part the pointer is over — the view
/// reports it with the hit; [`AttachMode::Auto`] and the axial plan's
/// side pick read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachRegion {
    /// The +z (up-stack) cap.
    TopCap,
    /// The −z (down-stack) cap.
    BottomCap,
    /// The hull side — a surface mount.
    Side,
}

/// Where the pointer currently sits on the craft — the CPU hover pick
/// the view computes each frame and pushes in. `local` is the hit
/// point in the node's *scaled* part-local frame (the same frame
/// [`csp_phys::session::scaled_attach_node`] reports), so a surface
/// mount can record it verbatim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HoverHit {
    /// The part under the cursor.
    pub node: PartNodeId,
    /// Hit point in the node's scaled part-local frame.
    pub local: DVec3,
    /// Which face the hit is on.
    pub region: AttachRegion,
}

/// What the cursor is carrying — KSP's held part. `csp-view` draws it
/// as a ghost that follows the cursor (snapped green over a placeable
/// target, red over a refused one, floating over empty space); a
/// click or drop commits it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Carried<'a> {
    /// A catalog part picked up for its first placement.
    Placing(&'a PartId),
    /// A placed part lifted to move (with its subtree) — the node id.
    Moving(PartNodeId),
}

/// What a placement would produce — computed by
/// [`VabSession::plan_attach`] without touching the session so the
/// ghost preview shows exactly what a drop commits.
#[derive(Clone, Debug, Default)]
pub struct AttachPlan {
    /// One edge per placed copy (radial symmetry expands the set).
    /// Empty on failure — `error` says why.
    pub edges: Vec<AttachEdge>,
    /// The parent-local transform each edge snaps to, parallel to
    /// `edges` — the ghost's pose (body-frame it via the parent's
    /// composed transform).
    pub transforms: Vec<PartTransform>,
    /// The refusal reason when nothing could be planned.
    pub error: Option<String>,
}

/// A mode-switch request the top bar emits — `csp-view` maps these to
/// its `ViewMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeSelect {
    /// The flight view.
    Flight,
    /// The system map.
    Map,
    /// The VAB.
    Vab,
}

/// One VAB operation — the session-command vocabulary (module doc).
/// Panels, input events, and future session-class tools share this.
#[derive(Clone, Debug)]
pub enum VabAction {
    /// Discard the craft and start empty (name kept).
    New,
    /// Place `part` as the root (empty session only).
    SetRoot(PartId),
    /// Snap a fresh `part` under `parent`, auto-choosing the first
    /// size-class-compatible (parent, child) attach-node pair.
    AttachAuto {
        /// The catalog part to place.
        part: PartId,
        /// The node to hang it under.
        parent: PartNodeId,
    },
    /// The palette's "pick up a part" — the KSP held-part gesture:
    /// the click never places anything; it arms the ghost that
    /// follows the cursor. The ghost commits when the player clicks a
    /// craft part (consumed by [`Self::Select`]`(Some)` through
    /// [`VabSession::plan_attach`], so a side-face click
    /// surface-mounts like a drag-drop) or clicks the empty build
    /// area of an empty craft (the part becomes the root). Clicking
    /// empty space on a non-empty craft keeps it held — only
    /// [`Self::CancelPlacement`] (Esc/right-click/the banner button)
    /// or re-clicking the armed row puts it back. Picking a different
    /// row swaps the carried part.
    CatalogPick(PartId),
    /// Put back whatever the cursor carries — an armed catalog part
    /// or a lifted placed part (which never left the session, so the
    /// "put back" is free). The palette banner's cancel button,
    /// right-click while carrying, and Esc all land here.
    CancelPlacement,
    /// Commit whatever the cursor carries at the current
    /// [`VabSession::hover`] target — the drag-release landing for
    /// both a palette drag and a lifted part. With no hover target
    /// the carry stays armed (a drop on empty space lets go of
    /// nothing); on an empty craft a carried catalog part becomes
    /// the root.
    Drop,
    /// The drag-and-drop landing for a *catalog* part: arm `part` and
    /// resolve [`Self::Drop`] in the same op — the palette-drag
    /// release and headless tests both use it.
    PlaceAtHover {
        /// The catalog part being dropped.
        part: PartId,
    },
    /// Lift a placed part onto the cursor to move it (with its
    /// subtree) — the click-on-selected and drag-lift gestures and the
    /// inspector's "pick up" button all land here. The root refuses:
    /// it defines the build frame (reroot moves the origin).
    Grab(PartNodeId),
    /// Remove `node` and its subtree.
    Remove(PartNodeId),
    /// Re-root the tree at `node` (the VAB's reroot tool).
    Reroot(PartNodeId),
    /// The 3D-view click + panel clicks, unified: while the cursor
    /// carries a part this is the *drop* — `Some` is the new mount
    /// point, `None` an empty-space click (keeps the carry, or places
    /// the root on an empty craft). Carrying nothing, `Some` selects
    /// (or lifts the already-selected part for a move) and `None`
    /// deselects.
    Select(Option<PartNodeId>),
    /// Assign `node` to `stage`.
    SetStage {
        /// The node to move.
        node: PartNodeId,
        /// Stage index — higher fires earlier.
        stage: u32,
    },
    /// Rescale `node` (gated by the def's `ScaleRule`).
    SetScale {
        /// The node to rescale.
        node: PartNodeId,
        /// New linear scale factor.
        scale: f64,
    },
    /// Restretch `node` — [`csp_phys::BuiltPart::stretch`], the
    /// stack-axis length factor, gated by the same `ScaleRule` bounds.
    SetStretch {
        /// The node to restretch.
        node: PartNodeId,
        /// New stack-axis multiplier.
        stretch: f64,
    },
    /// Radial symmetry count for surface mounts — 1, 2, or 4 copies.
    SetSymmetry(u8),
    /// The placement attach-mode override.
    SetAttachMode(AttachMode),
    /// Rebuild `node` as `variant`.
    SetVariant {
        /// The node to re-variant.
        node: PartNodeId,
        /// The variant to apply.
        variant: VariantId,
    },
    /// Rename the craft.
    Rename(String),
    /// Replace the session from craft-file bytes (`CSPCRAFT`).
    LoadBytes(Vec<u8>),
    /// Set the mission profile's target Δv (m/s).
    SetTargetDeltaV(f64),
    /// Set the mission profile's minimum launch TWR.
    SetMinLaunchTwr(f64),
    /// Insert an empty stage at index `at` (shift populated up).
    InsertStage(u32),
    /// Delete stage `at` (its parts fold to `at − 1`).
    RemoveStage(u32),
    /// Reindex populated stages to `0..k`.
    CompactStages,
    /// Undo the last craft mutation — restores the pre-op
    /// `CraftSession` snapshot from the bounded undo stack.
    Undo,
    /// Redo the last undone mutation.
    Redo,
    /// Translate the whole craft in the build frame — `delta` adds to
    /// the root's stored offset; the stack follows rigidly (the
    /// root's rotation is preserved). The free-reposition verb.
    Translate(DVec3),
    /// Remove `node` WITHOUT its subtree — each direct child promotes
    /// to `node`'s parent, re-snapped through a free
    /// size-class-matched attach node
    /// ([`CraftSession::remove_and_reparent`]). Refuses on the root
    /// and when a child finds no mate on the grandparent.
    RemoveOnly(PartNodeId),
}

/// Why a [`VabAction`] was refused — session op failures plus the
/// VAB-specific ones (no catalog, no selection, bad file).
#[derive(Clone, Debug, PartialEq)]
pub enum VabError {
    /// A `CraftSession` op failed.
    Session(csp_phys::session::SessionError),
    /// A craft file failed to decode.
    CraftFile(csp_phys::session::CraftFileError),
    /// The part catalog isn't loaded (a catalog-less headless session;
    /// an empty catalog refuses placement ops).
    NoCatalog,
    /// The op needs a selected part and none is selected.
    NoSelection,
    /// A placement op named a part id the loaded catalog doesn't
    /// contain — distinct from [`Self::NoCompatibleAttach`], which is
    /// about attach-node pairing, not catalog membership.
    PartNotInCatalog {
        /// The part id that isn't in the catalog.
        part: PartId,
    },
    /// `AttachAuto` found no size-class-compatible attach-node pair.
    NoCompatibleAttach {
        /// The part that failed to attach.
        part: PartId,
        /// The parent it was tried under.
        parent: PartNodeId,
    },
    /// A placement (or a loaded craft) would exceed the session's part
    /// limit — the §6.3 web-demo cap is the only producer at v1.
    PartLimit {
        /// The configured cap.
        limit: usize,
        /// What the op would have produced.
        count: usize,
    },
    /// Craft-file import is disabled for this session — the §6.3
    /// demo's no-save rule (`VabAction::LoadBytes` is refused).
    FileIoDisabled,
    /// §3.3: the op named a catalog part whose `tech_requirement` is
    /// not researched — placement refuses, never silently downgrades.
    PartLocked {
        /// The tech-locked part.
        part: PartId,
        /// The node that would unlock it.
        node: csp_data::TechNodeId,
    },
}

impl fmt::Display for VabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(e) => write!(f, "{e}"),
            Self::CraftFile(e) => write!(f, "{e}"),
            Self::NoCatalog => write!(f, "part catalog is not loaded"),
            Self::NoSelection => write!(f, "no part selected"),
            Self::PartNotInCatalog { part } => {
                write!(f, "part '{part}' is not in the loaded catalog")
            }
            Self::NoCompatibleAttach { part, parent } => write!(
                f,
                "no compatible attach node pair for {part} under {parent:?}"
            ),
            Self::PartLimit { limit, count } => {
                write!(f, "part limit {limit} exceeded ({count} parts)")
            }
            Self::FileIoDisabled => {
                write!(f, "craft file import/export is disabled in the web demo")
            }
            Self::PartLocked { part, node } => {
                write!(f, "part '{part}' is tech-locked — requires {node}")
            }
        }
    }
}

impl std::error::Error for VabError {}

/// The §3.3 progression state the editor gates placement on — staged
/// by the host (csp-view) from the sim's `TechState` + installed
/// tree. `None` on the session means "no progression data" — an
/// ungated editor (headless fixtures, tree-less worlds).
#[derive(Clone, Debug)]
pub struct VabTech {
    /// The merged tech tree — track lookups for the placement-time
    /// upgrade freeze (`BuiltPart::upgrade_levels_at_build`).
    pub tree: csp_data::TechTree,
    /// The researched node set — `PartDef::tech_requirement`
    /// membership here is the whole availability gate.
    pub researched: std::collections::BTreeSet<csp_data::TechNodeId>,
    /// Purchased upgrade-track levels — composed into
    /// `upgrade_levels_at_build` when a catalog part is placed.
    pub track_levels: std::collections::BTreeMap<csp_data::UpgradeTrackId, u8>,
}

/// The VAB working session — see the module doc. `Default` is an empty
/// session with no catalog (the headless state; the panel reports
/// "catalog not loaded"), no part cap, and file IO allowed.
pub struct VabSession {
    session: CraftSession,
    catalog: Option<LoadedParts>,
    resources: Option<ResourceFile>,
    /// The English loc table — the catalog's display-name half
    /// (`PartDef::display_name` is a `LocKey`). Data-derived, same
    /// class as the catalog; absent in headless fixtures, where names
    /// fall back to the part id.
    loc: Option<LocFile>,
    profile: MissionProfile,
    /// Currently selected node (inspector + attach target).
    selected: Option<PartNodeId>,
    /// The armed pending placement — the KSP held part: a palette
    /// click lifts the catalog part onto the cursor as a ghost; it
    /// commits when the player clicks a craft part (or the empty
    /// build area for the root) and stays held otherwise. See
    /// [`VabAction::CatalogPick`]. Session state — never serialized
    /// into the craft file.
    pending_part: Option<PartId>,
    /// The palette-row drag currently in flight. A drag arms
    /// `pending_part` like a click; the release resolves through
    /// [`VabAction::Drop`] over the viewport or cancels over a panel.
    drag_part: Option<PartId>,
    /// The placed node lifted for a move — the cursor carries it and
    /// its whole subtree as ghosts; a click or drop reattaches it.
    /// The lift is presentation-only: the node never leaves the
    /// session, so a cancel just clears this field and the part is
    /// exactly where it was.
    move_node: Option<PartNodeId>,
    /// The placed part an LMB press over the viewport began on — the
    /// drag-lift candidate: if the press turns into a drag this node
    /// is grabbed. `None` for presses on empty space or a panel.
    press_node: Option<PartNodeId>,
    /// Set each frame by `vab_pointer_input` while an LMB press over
    /// the viewport is a placement/move gesture (carrying a part, or
    /// pressing on one). `csp-view` skips camera-orbit deltas while
    /// this is set so dragging a part moves the part, not the camera.
    orbit_block: bool,
    /// The carried ghost's live status — the plan's refusal reason
    /// while hovering an invalid target, pushed by `csp-view` each
    /// frame so the hint bar can show *why* a spot won't take.
    ghost_status: Option<String>,
    /// The CPU hover pick — the part face the pointer is over, in the
    /// node's scaled local frame. `csp-view` pushes it every frame the
    /// cursor is over the 3D viewport (and clears it when it leaves);
    /// the attach planner and the right-click remove read it.
    hover: Option<HoverHit>,
    /// The pointer position in physical surface px while it is over
    /// the 3D viewport — pushed by `vab_pointer_input` from egui each
    /// frame. `csp-view`'s hover ray is cast through it; `None` over a
    /// masked window rect or off-window.
    cursor_px: Option<(f64, f64)>,
    /// Radial symmetry copies for surface mounts — 1, 2, or 4.
    symmetry: u8,
    /// The attach-mode override — `Auto` reads the hovered face.
    attach_mode: AttachMode,
    /// The last op's status/error line — surfaced by the panel.
    notice: Option<String>,
    /// Set by every mutating op; cleared by `recompute`.
    dirty: bool,
    /// The dirty-gated readout + validation + body-frame transforms.
    analysis: Option<CraftAnalysis>,
    /// `analyze`'s failure, when the last recompute failed to resolve.
    analysis_error: Option<String>,
    validation: Option<Validation>,
    body_xforms: BTreeMap<PartNodeId, PartTransform>,
    /// The reliability probe — the inspector's editable regime
    /// metrics, fed to `csp_phys::stress_breakdown` so the part's
    /// authored stress rules show which would fire under a
    /// hypothetical regime (§3.4: "hidden stress multipliers are bad
    /// design"). Session state only (§2.2-C), never hashed.
    stress_probe: csp_phys::RegimeMetrics,
    /// The save-slot name the file field edits (persists across saves).
    pub save_name: String,
    /// §6.3's demo cap — `Some(n)` refuses a placement that would take
    /// the craft past `n` parts and a `LoadBytes` whose file carries
    /// more. `None` is uncapped (the desktop default).
    part_limit: Option<usize>,
    /// §6.3's no-save rule — `false` refuses `VabAction::LoadBytes` and
    /// hides the panel's save/load row. `true` is the desktop default.
    file_io: bool,
    /// The §3.3 progression state — staged by the host each frame
    /// (`View::stage_vab_tech`). `None` = no progression data: the
    /// palette and placement gates are off, upgrades compose empty.
    tech: Option<VabTech>,
    /// Craft-generation counter — bumped by `VabAction::New` and a
    /// successful `LoadBytes`. The 3D view watches it so the camera's
    /// one-shot fit re-arms on a craft that is not the one it framed
    /// (edits don't bump it — only replacing the craft does).
    craft_epoch: u64,
    /// The undo history — whole-session snapshots taken before every
    /// mutating op (a `CraftSession` clone is the cheap, exact inverse
    /// of any op — every mutation funnels through [`Self::apply`]).
    /// Bounded at [`UNDO_DEPTH`]; the oldest entries fall off.
    undo_stack: VecDeque<CraftSession>,
    /// Sessions undone — cleared by the next mutating op.
    redo_stack: VecDeque<CraftSession>,
    /// The world-space point a carried ROOT drops at — the cursor ray
    /// pushed to the craft-focus depth, computed by `csp-view` each
    /// frame (it owns the camera). `None` while the cursor is off the
    /// viewport.
    drop_anchor: Option<DVec3>,
    /// Whether the in-flight LMB press began over the viewport — the
    /// `orbit_block` press anchor. A viewport press suppresses camera
    /// orbit for the gesture's whole life even when the pointer slides
    /// over a panel mid-drag (the old current-position check dropped
    /// the block mid-drag over the palette — PRD-11 L6).
    press_viewport: bool,
}

impl Default for VabSession {
    /// The desktop defaults: uncapped, file IO allowed.
    fn default() -> Self {
        Self {
            session: CraftSession::default(),
            catalog: None,
            resources: None,
            loc: None,
            profile: MissionProfile::default(),
            selected: None,
            pending_part: None,
            drag_part: None,
            move_node: None,
            press_node: None,
            orbit_block: false,
            ghost_status: None,
            hover: None,
            cursor_px: None,
            symmetry: 1,
            attach_mode: AttachMode::Auto,
            notice: None,
            dirty: false,
            analysis: None,
            analysis_error: None,
            validation: None,
            body_xforms: BTreeMap::new(),
            stress_probe: csp_phys::RegimeMetrics::default(),
            save_name: String::new(),
            part_limit: None,
            file_io: true,
            tech: None,
            craft_epoch: 0,
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            drop_anchor: None,
            press_viewport: false,
        }
    }
}

/// Undo history depth — a `CraftSession` clone per frame; far past
/// any realistic edit chain.
const UNDO_DEPTH: usize = 64;

impl VabSession {
    /// Empty session with a catalog — the native startup state.
    pub fn new(catalog: LoadedParts, resources: ResourceFile) -> Self {
        Self {
            catalog: Some(catalog),
            resources: Some(resources),
            profile: MissionProfile::default(),
            dirty: true,
            ..Self::default()
        }
    }

    /// Install/replace the part catalog + resource registry (called by
    /// `View::install_vab_catalog` once `data/` is loaded).
    pub fn set_catalog(&mut self, catalog: LoadedParts, resources: ResourceFile) {
        self.catalog = Some(catalog);
        self.resources = Some(resources);
        self.dirty = true;
    }

    /// Install the localization table the palette resolves
    /// `PartDef::display_name` keys against (`View::from_source` loads
    /// `data/loc/en.ron` beside the catalog). Absent →
    /// [`Self::display_name`] falls back to the part id.
    pub fn set_loc(&mut self, loc: LocFile) {
        self.loc = Some(loc);
    }

    /// The player-facing name of a catalog part — the loc-resolved
    /// `display_name`, or the part id itself when no loc table is
    /// installed or the key is missing (honest, never blank).
    pub fn display_name(&self, part_id: &PartId) -> String {
        self.def_by_id(part_id)
            .and_then(|d| {
                self.loc
                    .as_ref()
                    .and_then(|l| l.strings.get(d.display_name.as_str()))
            })
            .cloned()
            .unwrap_or_else(|| part_id.as_str().to_string())
    }

    /// The display name of a *placed* node — `#id` when the node is
    /// gone (a stale reference in a message).
    fn node_label(&self, node: PartNodeId) -> String {
        self.session
            .node(node)
            .map(|n| self.display_name(&n.built().part_id))
            .unwrap_or_else(|| format!("#{}", node.0))
    }

    /// The craft-generation counter — bumped on `New` and a
    /// successful `LoadBytes`. The craft view resets its one-shot
    /// camera fit when it changes (a `focused_once` left armed would
    /// frame the NEW craft at the OLD craft's distance).
    pub fn craft_epoch(&self) -> u64 {
        self.craft_epoch
    }

    /// Whether an undoable craft mutation is on the stack.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether a redo is available.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Push/clear the whole-craft drop anchor — `csp-view` owns this
    /// (it computes the point from the camera + cursor each frame).
    pub fn set_drop_anchor(&mut self, p: Option<DVec3>) {
        self.drop_anchor = p;
    }

    /// The press anchor for `orbit_block` — see the field doc.
    pub fn press_on_viewport(&self) -> bool {
        self.press_viewport
    }

    /// Set/clear the press anchor — `vab_pointer_input` owns this.
    pub fn set_press_on_viewport(&mut self, on: bool) {
        self.press_viewport = on;
    }

    /// The armed pending placement, if any (see
    /// [`VabAction::CatalogPick`]) — the palette banner and hint bar
    /// surface it.
    pub fn pending_part(&self) -> Option<&PartId> {
        self.pending_part.as_ref()
    }

    /// The part a palette drag is carrying, if any — the drag-armed
    /// placement (`pending_part` is its click-armed sibling).
    pub fn drag_part(&self) -> Option<&PartId> {
        self.drag_part.as_ref()
    }

    /// Set/clear the in-flight palette drag — the palette panel owns
    /// this. A starting drag also arms `pending_part` so the banner,
    /// the ghost, and a stray mid-drag click agree on what is being
    /// placed; it also puts back any lifted part (one carry at a
    /// time). Clearing the drag leaves the arm alone.
    pub fn set_drag_part(&mut self, part: Option<PartId>) {
        self.drag_part = part.clone();
        if let Some(p) = part {
            self.pending_part = Some(p);
            self.move_node = None;
        }
    }

    /// The part any placement gesture would drop — drag first, then
    /// the click-armed pending part.
    pub fn armed_part(&self) -> Option<&PartId> {
        self.drag_part.as_ref().or(self.pending_part.as_ref())
    }

    /// The node lifted for a move, if still placed (see
    /// [`VabAction::Grab`]).
    pub fn move_node(&self) -> Option<PartNodeId> {
        self.move_node.filter(|id| self.session.contains(*id))
    }

    /// What the cursor is carrying, if anything — the held-part state
    /// the ghost view and every cancel/drop path read.
    pub fn carried(&self) -> Option<Carried<'_>> {
        if let Some(n) = self.move_node() {
            Some(Carried::Moving(n))
        } else {
            self.armed_part().map(Carried::Placing)
        }
    }

    /// The placed part an LMB press began on — the drag-lift
    /// candidate (set/cleared by `vab_pointer_input`).
    pub fn press_node(&self) -> Option<PartNodeId> {
        self.press_node
    }

    /// Set/clear the drag-lift candidate.
    pub fn set_press_node(&mut self, node: Option<PartNodeId>) {
        self.press_node = node;
    }

    /// Whether a placement/move gesture owns the LMB press —
    /// `csp-view` skips camera-orbit deltas while this is set.
    pub fn orbit_block(&self) -> bool {
        self.orbit_block
    }

    /// Push the orbit-block flag — `vab_pointer_input` owns this.
    pub fn set_orbit_block(&mut self, on: bool) {
        self.orbit_block = on;
    }

    /// The carried ghost's live status (the current plan refusal), if
    /// any — `csp-view` pushes it each frame.
    pub fn ghost_status(&self) -> Option<&str> {
        self.ghost_status.as_deref()
    }

    /// Push/clear the carried ghost's status — `csp-view` owns this.
    pub fn set_ghost_status(&mut self, status: Option<String>) {
        self.ghost_status = status;
    }

    /// The live CPU hover pick, if the cursor is over a craft part.
    pub fn hover(&self) -> Option<HoverHit> {
        self.hover
    }

    /// Push/clear the frame's hover pick — `csp-view` owns this.
    pub fn set_hover(&mut self, hover: Option<HoverHit>) {
        self.hover = hover;
    }

    /// The viewport cursor position (physical px), if the pointer is
    /// over the 3D view — `csp-view` casts its hover ray through it.
    pub fn cursor_px(&self) -> Option<(f64, f64)> {
        self.cursor_px
    }

    /// Push/clear the viewport cursor — `vab_pointer_input` owns this.
    pub fn set_cursor_px(&mut self, px: Option<(f64, f64)>) {
        self.cursor_px = px;
    }

    /// The radial symmetry count for surface mounts (1, 2, or 4).
    pub fn symmetry(&self) -> u8 {
        self.symmetry
    }

    /// The attach-mode override — `Auto` reads the hovered face.
    pub fn attach_mode(&self) -> AttachMode {
        self.attach_mode
    }

    /// The underlying session — read-only for panels/scene building.
    /// Mutation goes through [`Self::apply`].
    pub fn session(&self) -> &CraftSession {
        &self.session
    }

    /// The selected node, if still placed.
    pub fn selected(&self) -> Option<PartNodeId> {
        self.selected.filter(|id| self.session.contains(*id))
    }

    /// The mission profile the readout compares against.
    pub fn profile(&self) -> &MissionProfile {
        &self.profile
    }

    /// The last op's status line, if any — the panel's notice row.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Set or clear the notice — for app-side results (craft IO).
    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    /// The cached readout — `None` until `recompute` or while the
    /// resolve fails ([`Self::analysis_error`]).
    pub fn analysis(&self) -> Option<&CraftAnalysis> {
        self.analysis.as_ref()
    }

    /// The resolve failure of the last `recompute`, if any.
    pub fn analysis_error(&self) -> Option<&str> {
        self.analysis_error.as_deref()
    }

    /// The cached `validate` result.
    pub fn validation(&self) -> Option<&Validation> {
        self.validation.as_ref()
    }

    /// Body-frame transforms per node — the craft view's geometry.
    pub fn body_transforms(&self) -> &BTreeMap<PartNodeId, PartTransform> {
        &self.body_xforms
    }

    /// The def for a placed node (`built.part_id` lookup).
    pub fn def_of(&self, node: PartNodeId) -> Option<&PartDef> {
        let part_id = &self.session.node(node)?.built().part_id;
        self.def_by_id(part_id)
    }

    /// The def for a catalog part id.
    pub fn def_by_id(&self, part_id: &PartId) -> Option<&PartDef> {
        self.catalog.as_ref().and_then(|c| {
            c.parts
                .iter()
                .find(|p| p.def.id == *part_id)
                .map(|p| &p.def)
        })
    }

    /// The catalog, if loaded.
    pub fn catalog(&self) -> Option<&LoadedParts> {
        self.catalog.as_ref()
    }

    /// Set the session's part cap — `Some(n)` is the §6.3 demo's ≤60
    /// rule; `None` restores the uncapped desktop behavior.
    pub fn set_part_limit(&mut self, limit: Option<usize>) {
        self.part_limit = limit;
    }

    /// The configured part cap, if any.
    pub fn part_limit(&self) -> Option<usize> {
        self.part_limit
    }

    /// Stage the §3.3 progression state — the host pushes the sim's
    /// `TechState` + tree each frame so the palette dims tech-locked
    /// parts and placements freeze the purchased upgrade levels.
    /// `None` disables the gate (no tree installed).
    pub fn set_tech(&mut self, tech: Option<VabTech>) {
        self.tech = tech;
    }

    /// The §3.3 availability gate: `true` when `def`'s
    /// `tech_requirement` is researched — always `true` when the
    /// session carries no progression data (the empty-gate
    /// convention, like a missing catalog's honest-empty palette).
    pub fn part_unlocked(&self, def: &PartDef) -> bool {
        self.tech
            .as_ref()
            .is_none_or(|t| t.researched.contains(&def.tech_requirement))
    }

    /// Enable/disable craft-file import/export — §6.3's "no save" rule.
    /// `false` refuses [`VabAction::LoadBytes`] and hides the panel's
    /// file row (the app additionally drops save/load outbox requests).
    pub fn set_file_io(&mut self, allowed: bool) {
        self.file_io = allowed;
    }

    /// Whether craft-file import/export is enabled.
    pub fn file_io(&self) -> bool {
        self.file_io
    }

    /// The §6.3 cap check — refuses when `count` parts exceeds the
    /// session's configured limit.
    fn check_part_count(&self, count: usize) -> Result<(), VabError> {
        if let Some(limit) = self.part_limit
            && count > limit
        {
            return Err(VabError::PartLimit { limit, count });
        }
        Ok(())
    }

    /// Recompute the readout/validation/transforms when the session
    /// changed — the §2.5-friendly "not per frame" cadence the M3 row
    /// asks for. Cheap when clean; honest cost when dirty.
    pub fn recompute(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.validation = self.catalog.as_ref().map(|c| self.session.validate(c));
        self.analysis = None;
        self.analysis_error = None;
        if let (Some(defs), Some(res)) = (&self.catalog, &self.resources) {
            match csp_phys::readout::analyze(&self.session, defs, res) {
                Ok(a) => self.analysis = Some(a),
                Err(e) => self.analysis_error = Some(e.to_string()),
            }
        }
        self.body_xforms = self.session.body_transforms();
    }

    /// Apply one [`VabAction`] — the shared entry for panels, input
    /// events, and (later) session-class tools. Mutations mark the
    /// session dirty for the next `recompute`; every call lands a
    /// line in [`Self::notice`] — a confirmation on success, the
    /// refusal on failure — so no op is ever silent.
    ///
    /// Mutating ops push the pre-op session onto the bounded undo
    /// stack FIRST, so every destructive gesture (delete, palette
    /// drop, drag-to-remove, stage edits) is Ctrl+Z-recoverable —
    /// the "editor ate my rocket" fix. A mutation that resolves to a
    /// no-op (drop keeping the carry, a refused plan) pushes nothing.
    pub fn apply(&mut self, action: VabAction) -> Result<(), VabError> {
        let before = Self::is_history_op(&action).then(|| self.session.clone());
        let r = self.apply_inner(action);
        match &r {
            Ok(msg) => {
                self.notice = msg.clone();
                if let Some(prev) = before
                    && self.session != prev
                {
                    self.redo_stack.clear();
                    self.undo_stack.push_back(prev);
                    while self.undo_stack.len() > UNDO_DEPTH {
                        self.undo_stack.pop_front();
                    }
                }
            }
            Err(e) => self.notice = Some(e.to_string()),
        }
        r.map(|_| ())
    }

    /// Whether `action` can mutate the `CraftSession` — the undo
    /// snapshot gate. Session-idle gestures (selection, palette
    /// arming, camera-adjacent state) and the history ops themselves
    /// are excluded; `Rename` is too (per-keystroke snapshots would
    /// flood the stack).
    fn is_history_op(action: &VabAction) -> bool {
        !matches!(
            action,
            VabAction::Undo
                | VabAction::Redo
                | VabAction::CatalogPick(_)
                | VabAction::CancelPlacement
                | VabAction::Grab(_)
                | VabAction::Select(_)
                | VabAction::SetSymmetry(_)
                | VabAction::SetAttachMode(_)
                | VabAction::SetTargetDeltaV(_)
                | VabAction::SetMinLaunchTwr(_)
                | VabAction::Rename(_)
        )
    }

    /// The op dispatch — see [`Self::apply`]. `Ok(Some(_))` is a
    /// player-facing confirmation; `Ok(None)` clears the notice (a
    /// quiet op — selection, rename keystroke, profile drag — whose
    /// result is already visible where it happened).
    #[allow(clippy::too_many_lines)] // one arm per op — the dispatch table IS the list
    fn apply_inner(&mut self, action: VabAction) -> Result<Option<String>, VabError> {
        Ok(match action {
            VabAction::New => {
                self.session = CraftSession::with_name(self.session.name());
                self.selected = None;
                self.pending_part = None;
                self.drag_part = None;
                self.move_node = None;
                self.press_node = None;
                self.craft_epoch += 1;
                self.dirty = true;
                Some("new craft — pick up a catalog part and click the build area".to_string())
            }
            VabAction::SetRoot(part_id) => Some(self.place_root(&part_id)?),
            VabAction::AttachAuto { part, parent } => Some(self.place_attach(&part, parent)?),
            VabAction::CatalogPick(part) => self.catalog_pick(&part)?,
            VabAction::CancelPlacement => {
                // All three takes must run — `||` short-circuits.
                let a = self.pending_part.take().is_some();
                let b = self.drag_part.take().is_some();
                let c = self.move_node.take().is_some();
                if a || b || c {
                    Some("placement cancelled — part put back".to_string())
                } else {
                    None
                }
            }
            VabAction::PlaceAtHover { part } => self.place_at_hover(&part)?,
            VabAction::Drop => self.drop_carried()?,
            VabAction::Grab(node) => self.grab(node)?,
            VabAction::Remove(node) => Some(self.remove_node(node)?),
            VabAction::Reroot(node) => {
                // A carry can't survive a root change — the moved
                // node's own edge may be rewritten by the reversal.
                self.move_node = None;
                self.session.reroot(node).map_err(VabError::Session)?;
                self.dirty = true;
                Some(format!("re-rooted the craft at {}", self.node_label(node)))
            }
            VabAction::Select(sel) => self.select(sel)?,
            VabAction::SetStage { node, stage } => {
                let label = self.node_label(node);
                self.session
                    .set_stage(node, stage)
                    .map_err(VabError::Session)?;
                self.dirty = true;
                Some(format!("{label} → stage {stage}"))
            }
            VabAction::SetScale { node, scale } => {
                let label = self.node_label(node);
                let defs = self.catalog.as_ref().ok_or(VabError::NoCatalog)?;
                self.session
                    .set_scale(defs, node, scale)
                    .map_err(VabError::Session)?;
                self.dirty = true;
                Some(format!("{label} scale → {scale:.2}"))
            }
            VabAction::SetStretch { node, stretch } => {
                let label = self.node_label(node);
                let defs = self.catalog.as_ref().ok_or(VabError::NoCatalog)?;
                self.session
                    .set_stretch(defs, node, stretch)
                    .map_err(VabError::Session)?;
                self.dirty = true;
                Some(format!("{label} length ×{stretch:.2}"))
            }
            VabAction::SetSymmetry(n) => {
                self.symmetry = match n {
                    2 => 2,
                    4 => 4,
                    _ => 1,
                };
                Some(format!("symmetry ×{}", self.symmetry))
            }
            VabAction::SetAttachMode(mode) => {
                self.attach_mode = mode;
                Some(format!(
                    "attach mode: {}",
                    match mode {
                        AttachMode::Auto => "auto (hover picks the face)",
                        AttachMode::Axial => "stack above/below",
                        AttachMode::Side => "surface mount — upright",
                        AttachMode::End => "surface mount — end-on",
                    }
                ))
            }
            VabAction::SetVariant { node, variant } => {
                let label = self.node_label(node);
                let defs = self.catalog.as_ref().ok_or(VabError::NoCatalog)?;
                self.session
                    .set_variant(defs, node, variant.clone())
                    .map_err(VabError::Session)?;
                self.dirty = true;
                Some(format!("{label} variant → {}", variant.as_str()))
            }
            VabAction::Rename(name) => {
                self.session.set_name(name);
                None
            }
            VabAction::LoadBytes(bytes) => Some(self.load_bytes(&bytes)?),
            VabAction::SetTargetDeltaV(dv) => {
                if dv.is_finite() && dv >= 0.0 {
                    self.profile.target_delta_v_m_per_s = dv;
                }
                None
            }
            VabAction::SetMinLaunchTwr(t) => {
                if t.is_finite() && t >= 0.0 {
                    self.profile.min_launch_twr = t;
                }
                None
            }
            VabAction::InsertStage(at) => {
                self.session.insert_stage(at).map_err(VabError::Session)?;
                self.dirty = true;
                Some(format!("inserted stage {at}"))
            }
            VabAction::RemoveStage(at) => {
                self.session.remove_stage(at);
                self.dirty = true;
                Some(format!(
                    "removed stage {at} — its parts folded down a stage"
                ))
            }
            VabAction::CompactStages => {
                self.session.compact_stages();
                self.dirty = true;
                Some("compacted stage numbering".to_string())
            }
            VabAction::Undo => self.undo(),
            VabAction::Redo => self.redo(),
            VabAction::Translate(delta) => {
                let Some(root) = self.session.root() else {
                    return Err(VabError::Session(
                        csp_phys::session::SessionError::EmptySession,
                    ));
                };
                let t = self
                    .session
                    .node(root)
                    .map(|n| n.transform())
                    .unwrap_or(PartTransform::IDENTITY);
                self.session
                    .set_transform(root, PartTransform::new(t.offset + delta, t.rotation))
                    .map_err(VabError::Session)?;
                self.dirty = true;
                Some(format!(
                    "moved the craft ({:+.2}, {:+.2}, {:+.2}) m",
                    delta.x, delta.y, delta.z
                ))
            }
            VabAction::RemoveOnly(node) => Some(self.remove_only(node)?),
        })
    }

    /// [`VabAction::Undo`]'s dispatch — restore the newest snapshot.
    fn undo(&mut self) -> Option<String> {
        let Some(prev) = self.undo_stack.pop_back() else {
            return Some("nothing to undo".to_string());
        };
        self.redo_stack
            .push_back(std::mem::replace(&mut self.session, prev));
        self.after_history_restore("undone — Ctrl+Shift+Z redoes")
    }

    /// [`VabAction::Redo`]'s dispatch — replay the newest undo.
    fn redo(&mut self) -> Option<String> {
        let Some(next) = self.redo_stack.pop_back() else {
            return Some("nothing to redo".to_string());
        };
        self.undo_stack
            .push_back(std::mem::replace(&mut self.session, next));
        self.after_history_restore("redone")
    }

    /// The shared tail of undo/redo: node references that name gone
    /// ids are scrubbed, the craft generation bumps (an in-flight GPU
    /// pick must not decode against the restored craft), and the
    /// recompute re-arms.
    fn after_history_restore(&mut self, msg: &str) -> Option<String> {
        let gone = |s: &CraftSession, id: Option<PartNodeId>| id.is_some_and(|i| !s.contains(i));
        if gone(&self.session, self.selected) {
            self.selected = None;
        }
        if gone(&self.session, self.move_node) {
            self.move_node = None;
        }
        if gone(&self.session, self.press_node) {
            self.press_node = None;
        }
        self.craft_epoch += 1;
        self.dirty = true;
        Some(msg.to_string())
    }

    /// The palette's "pick up" — [`VabAction::CatalogPick`]'s
    /// dispatch. A catalog click never places anything: it lifts the
    /// part onto the cursor as a ghost (KSP's held part). The drop is
    /// a separate gesture — [`Self::select`]/[`Self::drop_carried`].
    /// The catalog check runs before ANY branch so a catalog-less
    /// session refuses honestly instead of arming a ghost that can
    /// never resolve.
    fn catalog_pick(&mut self, part: &PartId) -> Result<Option<String>, VabError> {
        if self.catalog.is_none() {
            return Err(VabError::NoCatalog);
        }
        // Refuse to arm a part the catalog doesn't carry — arming a
        // bad id would park the editor in a state whose only exits
        // are cancel and a guaranteed-failing attach.
        let def = self
            .def_by_id(part)
            .ok_or(VabError::PartNotInCatalog { part: part.clone() })?;
        // §3.3: a tech-locked part refuses to arm — the palette row
        // is dimmed, but the op path gates identically.
        if !self.part_unlocked(def) {
            return Err(VabError::PartLocked {
                part: part.clone(),
                node: def.tech_requirement.clone(),
            });
        }
        // Re-clicking the armed row puts the part back — a click that
        // resolves to nothing must still do something honest.
        if self.pending_part.as_ref() == Some(part) && self.drag_part.is_none() {
            self.pending_part = None;
            return Ok(Some("placement cancelled — part put back".to_string()));
        }
        // One carry at a time: picking up a catalog part puts any
        // lifted placed part back (it never left the session) and
        // swaps the held ghost.
        self.move_node = None;
        self.pending_part = Some(part.clone());
        Ok(Some(format!(
            "picked up {} — click a craft part to attach it, or empty \
             space to keep carrying",
            self.display_name(part)
        )))
    }

    /// [`VabAction::Remove`]'s dispatch — the node plus its whole
    /// subtree; clears the selection when it was inside the removed
    /// set. Returns the confirmation line.
    fn remove_node(&mut self, node: PartNodeId) -> Result<String, VabError> {
        let label = self.node_label(node);
        let removed = self.session.remove_part(node).map_err(VabError::Session)?;
        if self.selected.is_some_and(|s| removed.contains(&s)) {
            self.selected = None;
        }
        // A carry can't outlive the carried part.
        if self.move_node.is_some_and(|m| removed.contains(&m)) {
            self.move_node = None;
        }
        if self.press_node.is_some_and(|p| removed.contains(&p)) {
            self.press_node = None;
        }
        self.dirty = true;
        Ok(if removed.len() > 1 {
            format!(
                "removed {label} + subtree ({} parts) — Ctrl+Z restores",
                removed.len()
            )
        } else {
            format!("removed {label} — Ctrl+Z restores")
        })
    }

    /// [`VabAction::RemoveOnly`]'s dispatch — delete just this part;
    /// its children promote to the grandparent, re-snapped through
    /// free size-class-matched attach nodes. The refusal (root, or a
    /// child with nowhere to land) removes nothing.
    fn remove_only(&mut self, node: PartNodeId) -> Result<String, VabError> {
        let defs = self.catalog.as_ref().ok_or(VabError::NoCatalog)?;
        let label = self.node_label(node);
        let n_children = self.session.children_ids(node).len();
        self.session
            .remove_and_reparent(defs, node)
            .map_err(VabError::Session)?;
        if self.selected == Some(node) {
            self.selected = None;
        }
        self.dirty = true;
        Ok(if n_children > 0 {
            format!("removed {label} — its {n_children} children moved up (Ctrl+Z restores)")
        } else {
            format!("removed {label} — Ctrl+Z restores")
        })
    }

    /// [`VabAction::LoadBytes`]'s dispatch. §6.3's no-save rule —
    /// craft-file import is off in the demo build (`file_io`), and a
    /// loaded craft counts against the same part cap as a built one.
    fn load_bytes(&mut self, bytes: &[u8]) -> Result<String, VabError> {
        if !self.file_io {
            return Err(VabError::FileIoDisabled);
        }
        let session = CraftSession::from_bytes(bytes).map_err(VabError::CraftFile)?;
        self.check_part_count(session.len())?;
        self.session = session;
        self.selected = None;
        self.pending_part = None;
        self.drag_part = None;
        self.move_node = None;
        self.press_node = None;
        self.craft_epoch += 1;
        self.dirty = true;
        Ok(format!("loaded craft '{}'", self.session.name()))
    }

    /// [`VabAction::Select`]'s dispatch — the 3D-view click,
    /// unified. While the cursor carries a part the click is the
    /// *drop*: `Some(node)` is the new mount point (consumed through
    /// [`Self::attach_planned`]/[`Self::move_planned`], so a side-face
    /// click surface-mounts exactly like a drag-drop; a failed attach
    /// keeps the carry), `None` is an empty-space click — it keeps
    /// the part held (KSP: nothing on empty space) except on an empty
    /// craft, where it places the root. Carrying nothing, `Some`
    /// selects — or lifts the already-selected part for a move — and
    /// `None` deselects.
    fn select(&mut self, sel: Option<PartNodeId>) -> Result<Option<String>, VabError> {
        let target = sel.filter(|id| self.session.contains(*id));
        if let Some(node) = self.move_node {
            return Ok(Some(match target {
                Some(parent) => self.move_planned(node, parent)?,
                None if self.session.node(node).is_some_and(|n| n.edge().is_none()) => {
                    // Carrying the root IS carrying the craft — an
                    // empty-space click lands it (a free reposition),
                    // it can never mount a part.
                    self.drop_whole_craft(node)?
                }
                None => format!(
                    "carrying {} — click a craft part to reattach it",
                    self.node_label(node)
                ),
            }));
        }
        if let Some(part) = self.pending_part.clone() {
            return Ok(Some(match target {
                Some(parent) => self.attach_planned(&part, parent)?,
                None if self.session.is_empty() => self.place_root(&part)?,
                None => format!(
                    "carrying {} — click a craft part to place it",
                    self.display_name(&part)
                ),
            }));
        }
        // A click on the already-selected part lifts it — the
        // click-to-move affordance, same as the drag-lift.
        if let Some(id) = target
            && self.selected() == Some(id)
        {
            return self.grab(id);
        }
        self.selected = target;
        Ok(None)
    }

    /// [`VabAction::Grab`]'s dispatch — lift `node` onto the cursor
    /// for a move. The lift is presentation-only (the node keeps its
    /// edge until the drop rewrites it), so a cancel costs nothing.
    /// The root lifts the WHOLE craft — it anchors the build frame,
    /// so the carry is the entire stack and can only drop onto empty
    /// space (a free reposition via `set_transform`), never mount a
    /// part.
    fn grab(&mut self, node: PartNodeId) -> Result<Option<String>, VabError> {
        let Some(rec) = self.session.node(node) else {
            return Err(VabError::Session(
                csp_phys::session::SessionError::UnknownNode { id: node },
            ));
        };
        if rec.edge().is_none() {
            self.pending_part = None;
            self.drag_part = None;
            self.move_node = Some(node);
            return Ok(Some(
                "picked up the whole craft — click empty space to move it there \
                 (Esc puts it back)"
                    .to_string(),
            ));
        }
        // One carry at a time — a catalog ghost gives way.
        self.pending_part = None;
        self.drag_part = None;
        self.move_node = Some(node);
        Ok(Some(format!(
            "picked up {} — click a craft part to reattach it (Esc puts it back)",
            self.node_label(node)
        )))
    }

    /// [`VabAction::Drop`]'s dispatch — commit the carried thing at
    /// the stored [`Self::hover`]. A drop that finds no surface keeps
    /// the carry: letting go is a deliberate cancel, not a missed
    /// click.
    fn drop_carried(&mut self) -> Result<Option<String>, VabError> {
        if let Some(node) = self.move_node {
            if self.session.node(node).is_some_and(|n| n.edge().is_none()) {
                // The carried root is the whole craft — it drops on
                // empty space only (a part under the cursor is a
                // refused mount, not a drop point).
                return self.drop_whole_craft(node).map(Some);
            }
            let Some(hit) = self.hover else {
                return Ok(Some(format!(
                    "still carrying {} — aim at a craft part to drop it",
                    self.node_label(node)
                )));
            };
            return self.move_planned(node, hit.node).map(Some);
        }
        if let Some(part) = self.pending_part.clone() {
            if self.session.is_empty() {
                return self.place_root(&part).map(Some);
            }
            let Some(hit) = self.hover else {
                return Ok(Some(format!(
                    "still carrying {} — aim at a craft part to place it",
                    self.display_name(&part)
                )));
            };
            return self.attach_planned(&part, hit.node).map(Some);
        }
        Ok(None)
    }

    /// The whole-craft drop — a carried ROOT can't mount a part (it
    /// has no parent edge); on empty space it translates the root so
    /// its origin lands at the view-pushed `drop_anchor` and the
    /// stack follows rigidly. Hovering a part or missing an anchor
    /// keeps the carry with an explanation.
    fn drop_whole_craft(&mut self, node: PartNodeId) -> Result<String, VabError> {
        if self.hover.is_some() {
            return Ok(
                "the craft root can't mount a part — aim at empty space to move the craft"
                    .to_string(),
            );
        }
        let Some(anchor) = self.drop_anchor else {
            return Ok("still carrying the craft — aim at empty space to move it".to_string());
        };
        let rot = self
            .session
            .node(node)
            .map(|n| n.transform().rotation)
            .unwrap_or(DQuat::IDENTITY);
        self.session
            .set_transform(node, PartTransform::new(anchor, rot))
            .map_err(VabError::Session)?;
        self.move_node = None;
        self.dirty = true;
        Ok("moved the whole craft — Ctrl+Z restores".to_string())
    }

    /// [`VabAction::PlaceAtHover`]'s dispatch — the catalog
    /// drag-drop landing: arm the part and resolve
    /// [`Self::drop_carried`] in one op.
    fn place_at_hover(&mut self, part: &PartId) -> Result<Option<String>, VabError> {
        if self.catalog.is_none() {
            return Err(VabError::NoCatalog);
        }
        // §3.3: same gate as `catalog_pick` — the headless/drag-drop
        // path cannot arm a tech-locked part either.
        let def = self
            .def_by_id(part)
            .ok_or(VabError::PartNotInCatalog { part: part.clone() })?;
        if !self.part_unlocked(def) {
            return Err(VabError::PartLocked {
                part: part.clone(),
                node: def.tech_requirement.clone(),
            });
        }
        self.pending_part = Some(part.clone());
        self.drop_carried()
    }

    /// The move half of the drop — [`Self::move_planned`] commits the
    /// plan [`Self::plan_move`] produced: one reattach, same node id,
    /// subtree carried. Dropping onto the node's own subtree puts it
    /// back where it was (the graceful read of "onto yourself").
    fn move_planned(&mut self, node: PartNodeId, parent: PartNodeId) -> Result<String, VabError> {
        // The carried root is the whole craft — it can never mount a
        // part, it can only land on empty space (drop_whole_craft).
        if self.session.node(node).is_some_and(|n| n.edge().is_none()) {
            return Ok(
                "the craft root can't mount a part — click empty space to move the whole craft"
                    .to_string(),
            );
        }
        if self.session.subtree_ids(node).contains(&parent) {
            self.move_node = None;
            return Ok(format!("put {} back where it was", self.node_label(node)));
        }
        let part = self
            .session
            .node(node)
            .map(|n| n.built().part_id.clone())
            .ok_or(VabError::Session(
                csp_phys::session::SessionError::UnknownNode { id: node },
            ))?;
        let plan = self.plan_move(node, parent);
        if plan.edges.is_empty() {
            return Err(VabError::NoCompatibleAttach { part, parent });
        }
        let defs = self.catalog.as_ref().ok_or(VabError::NoCatalog)?;
        self.session
            .reattach(defs, node, plan.edges[0].clone())
            .map_err(VabError::Session)?;
        let name = self.node_label(node);
        let onto = self.node_label(parent);
        self.move_node = None;
        self.selected = Some(node);
        self.dirty = true;
        Ok(format!("moved {name} onto {onto}"))
    }

    /// Attach `part` to `parent` through [`Self::plan_attach`]: the
    /// hovered face decides axial vs surface mount (subject to the
    /// attach-mode override), radial symmetry fans the plan out to
    /// `symmetry` copies. Success disarms the pending placement and
    /// selects the first placed copy; failure returns a typed error
    /// and leaves the arm/selection alone.
    fn attach_planned(&mut self, part: &PartId, parent: PartNodeId) -> Result<String, VabError> {
        let plan = self.plan_attach(part, parent);
        if plan.edges.is_empty() {
            return Err(VabError::NoCompatibleAttach {
                part: part.clone(),
                parent,
            });
        }
        self.check_part_count(self.session.len() + plan.edges.len())?;
        let name = self.display_name(part);
        let defs = self.catalog.as_ref().ok_or(VabError::NoCatalog)?;
        let built = self.stock_build(part)?;
        let mut placed = Vec::with_capacity(plan.edges.len());
        for edge in &plan.edges {
            let id = self
                .session
                .add_part(defs, built.clone(), edge.clone())
                .map_err(VabError::Session)?;
            placed.push(id);
        }
        if let Some(&first) = placed.first() {
            self.selected = Some(first);
        }
        self.pending_part = None;
        self.drag_part = None;
        self.dirty = true;
        let copies = placed.len();
        Ok(if copies > 1 {
            format!("attached {copies}× {name} on {}", self.node_label(parent))
        } else {
            format!("attached {name} on {}", self.node_label(parent))
        })
    }

    /// The would-be attach for `part` under `parent`, evaluated
    /// against the stored [`Self::hover`] (when it names `parent`),
    /// [`Self::attach_mode`], and [`Self::symmetry`] — the ghost's
    /// source of truth. No session mutation; the `transforms` are
    /// parent-local poses parallel to `edges`.
    ///
    /// - Hover on a cap (or `Axial` forced) → an authored axial snap:
    ///   the free node on that side mates the child node that needs
    ///   the least twist — the same preference [`Self::attach_auto`]
    ///   applies, restricted to the clicked side.
    /// - Hover on the side (or `Side`/`End` forced) → a surface mount:
    ///   a synthesized `surf:` node on the parent hull at the hit's
    ///   azimuth/height. `Side` keeps the child upright on a `surf:`
    ///   side node of its own; `End` mates the child's most axial
    ///   authored node so the part stands perpendicular to the hull.
    pub fn plan_attach(&self, part: &PartId, parent: PartNodeId) -> AttachPlan {
        let mut plan = AttachPlan::default();
        macro_rules! fail {
            ($why:expr) => {{
                plan.error = Some($why);
                return plan;
            }};
        }
        let Some(child_def) = self.def_by_id(part) else {
            fail!(format!("part '{part}' is not in the loaded catalog"));
        };
        let Ok(built) = self.stock_build(part) else {
            fail!(format!("part '{part}' cannot be built"));
        };
        let bound = self.session.bound_attach_nodes(parent);
        self.plan_edges(
            parent,
            child_def,
            &built,
            &bound,
            &std::collections::BTreeSet::new(),
            usize::from(self.symmetry.max(1)),
        )
    }

    /// The would-be move for the lifted `node` under `parent` —
    /// [`Self::plan_attach`]'s move half: the same face/hover
    /// planning, but the child is the node's *own* built part
    /// (variant/scale/stretch preserved), symmetry does not fan out
    /// (one reattach), the node's current edge frees BOTH its ends
    /// (re-attaching where it was must be a valid plan), and the
    /// child's nodes already mated to the node's own children are not
    /// candidates.
    pub fn plan_move(&self, node: PartNodeId, parent: PartNodeId) -> AttachPlan {
        let mut plan = AttachPlan::default();
        macro_rules! fail {
            ($why:expr) => {{
                plan.error = Some($why);
                return plan;
            }};
        }
        let Some(node_rec) = self.session.node(node) else {
            fail!(format!("node {node:?} is not placed"));
        };
        if node_rec.edge().is_none() {
            fail!(
                "the craft root can't mount a part — click empty space to move the whole craft"
                    .to_string()
            );
        }
        if self.session.subtree_ids(node).contains(&parent) {
            fail!("a part cannot attach to its own subtree".to_string());
        }
        let built = node_rec.built().clone();
        let Some(child_def) = self.def_by_id(&built.part_id) else {
            fail!(format!(
                "part '{}' is not in the loaded catalog",
                built.part_id
            ));
        };
        // Free the moved edge's ends: the parent node it hangs from
        // (only when the target IS that parent) and its own mount
        // node. Everything else `bound_attach_nodes` reports stays
        // bound — the node's children still own their mates.
        let mut bound = self.session.bound_attach_nodes(parent);
        let mut child_bound = self.session.bound_attach_nodes(node);
        if let Some(e) = node_rec.edge() {
            if e.parent == parent {
                bound.remove(&e.parent_node);
            }
            child_bound.remove(&e.child_node);
        }
        self.plan_edges(parent, child_def, &built, &bound, &child_bound, 1)
    }

    /// The shared planner for [`Self::plan_attach`] (new part) and
    /// [`Self::plan_move`] (lifted part): resolves the parent records,
    /// reads the stored hover + attach mode, and dispatches to the
    /// axial or surface half. `bound`/`child_bound` are the
    /// already-spoken-for node ids on the parent/child;
    /// `copies` is the radial fan-out (1 for a move).
    fn plan_edges(
        &self,
        parent: PartNodeId,
        child_def: &PartDef,
        built: &BuiltPart,
        bound: &std::collections::BTreeSet<String>,
        child_bound: &std::collections::BTreeSet<String>,
        copies: usize,
    ) -> AttachPlan {
        let mut plan = AttachPlan::default();
        macro_rules! fail {
            ($why:expr) => {{
                plan.error = Some($why);
                return plan;
            }};
        }
        let Some(parent_rec) = self.session.node(parent) else {
            fail!(format!("node {parent:?} is not placed"));
        };
        let parent_part = parent_rec.built().part_id.clone();
        let (p_scale, p_stretch) = (parent_rec.built().scale, parent_rec.built().stretch);
        let Some(parent_def) = self.def_by_id(&parent_part) else {
            fail!(format!("part '{parent_part}' is not in the loaded catalog"));
        };
        let hit = self.hover.filter(|h| h.node == parent);
        let mode = match self.attach_mode {
            AttachMode::Auto => match hit.map(|h| h.region) {
                Some(AttachRegion::Side) => AttachMode::Side,
                _ => AttachMode::Axial,
            },
            m => m,
        };
        match mode {
            AttachMode::Axial | AttachMode::Auto => self.plan_axial(
                parent,
                parent_def,
                child_def,
                built,
                bound,
                child_bound,
                hit,
                &mut plan,
            ),
            AttachMode::Side | AttachMode::End => self.plan_radial(
                parent,
                parent_def,
                child_def,
                built,
                bound,
                child_bound,
                hit,
                p_scale,
                p_stretch,
                mode,
                copies,
                &mut plan,
            ),
        }
        plan
    }

    /// The axial half of [`Self::plan_edges`] — the authored snap
    /// graph on the side the hit named (default: the down-stack face,
    /// the same preference `attach_auto` applies). Candidates are the
    /// (free parent node, free child node) pairs ordered by
    /// (side-match, least-twist); the first one the session's own
    /// `preview_attach` accepts wins.
    #[allow(clippy::too_many_arguments)] // the plan context is one unit; a params struct would obscure it
    fn plan_axial(
        &self,
        parent: PartNodeId,
        parent_def: &PartDef,
        child_def: &PartDef,
        built: &BuiltPart,
        bound: &std::collections::BTreeSet<String>,
        child_bound: &std::collections::BTreeSet<String>,
        hit: Option<HoverHit>,
        plan: &mut AttachPlan,
    ) {
        let Some(defs) = self.catalog() else {
            plan.error = Some("part catalog is not loaded".to_string());
            return;
        };
        // The parent's body-frame orientation — needed to know which
        // authored node faces which way after the ancestor fold.
        let parent_t = self
            .body_transforms()
            .get(&parent)
            .copied()
            .unwrap_or_else(|| {
                let mut chain = vec![parent];
                while let Some(e) = chain
                    .last()
                    .and_then(|id| self.session.node(*id))
                    .and_then(|n| n.edge())
                {
                    chain.push(e.parent);
                }
                let mut t = PartTransform::IDENTITY;
                for id in chain.iter().rev() {
                    if let Some(n) = self.session.node(*id) {
                        t = t.compose(n.transform());
                    }
                }
                t
            });
        // The preferred stack side: a cap hit names it, anything else
        // keeps "below" — the auto-placement's growth direction.
        let want_down = !matches!(hit.map(|h| h.region), Some(AttachRegion::TopCap));
        let mut candidates: Vec<(&str, &str, i32, f64)> = Vec::new();
        for pn in &parent_def.attach_nodes {
            if bound.contains(&pn.id) {
                continue;
            }
            // Same facing metric as `attach_auto`: the parent node's
            // body-frame outward normal — positive faces down-stack.
            let facing = -parent_t.apply_dir(pn.direction).z;
            let side_match = if want_down {
                facing > 0.0
            } else {
                facing < 0.0
            };
            for cn in &child_def.attach_nodes {
                // A moved part's children already own some of its
                // nodes — a node can't mate twice.
                if child_bound.contains(&cn.id) {
                    continue;
                }
                let twist = cn.direction.dot(-pn.direction);
                candidates.push((&pn.id, &cn.id, i32::from(side_match), twist));
            }
        }
        // Preferred side first, then least twist, authored order last.
        candidates.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.3.total_cmp(&a.3)));
        let mut last = None;
        for &(pn, cn, ..) in &candidates {
            let edge = AttachEdge::new(parent, pn, cn);
            match self.session.preview_attach(defs, built, &edge) {
                Ok(t) => {
                    plan.edges.push(edge);
                    plan.transforms.push(t);
                    return;
                }
                Err(e) => last = Some(e.to_string()),
            }
        }
        plan.error = Some(last.unwrap_or_else(|| {
            "no free attach node on that side — flip the camera or pick another face".to_string()
        }));
    }

    /// The surface-mount half of [`Self::plan_edges`]: a synthesized
    /// `surf:` parent node at the hit's azimuth/height, `copies`
    /// fanned around the parent's axis (`symmetry` for a new part, 1
    /// for a move). `Side` mounts the child upright on its own
    /// `surf:` side node; `End` mates the child's most axial *free*
    /// authored node so the part protrudes.
    #[allow(clippy::too_many_arguments)]
    fn plan_radial(
        &self,
        parent: PartNodeId,
        parent_def: &PartDef,
        child_def: &PartDef,
        built: &BuiltPart,
        bound: &std::collections::BTreeSet<String>,
        child_bound: &std::collections::BTreeSet<String>,
        hit: Option<HoverHit>,
        p_scale: f64,
        p_stretch: f64,
        mode: AttachMode,
        copies: usize,
        plan: &mut AttachPlan,
    ) {
        let Some(defs) = self.catalog() else {
            plan.error = Some("part catalog is not loaded".to_string());
            return;
        };
        let Some(hit) = hit else {
            plan.error =
                Some("aim at the part's side — surface mount needs a hull hit".to_string());
            return;
        };
        // The mount height: the hit's z, clamped inside the parent's
        // z extent so a grazing hit near a cap still lands on the hull.
        let (z_lo, z_hi) = collider_z_extent(&parent_def.collider, p_scale, p_stretch);
        let span = (z_hi - z_lo).max(0.0);
        let z = hit.local.z.clamp(z_lo + 0.05 * span, z_hi - 0.05 * span);
        let azimuth = libm::atan2(hit.local.y, hit.local.x);
        // `End` mode mates the child's authored node closest to its
        // own stack axis — resolved once up front so the parent
        // surface node's size class matches it exactly. Nodes a moved
        // part's children already own are not candidates.
        let end_node = (mode == AttachMode::End)
            .then(|| {
                child_def
                    .attach_nodes
                    .iter()
                    .filter(|n| !child_bound.contains(&n.id))
                    .max_by(|a, b| a.direction.z.abs().total_cmp(&b.direction.z.abs()))
                    .map(|n| (n.id.clone(), n.diameter_m * built.scale))
            })
            .flatten();
        if mode == AttachMode::End && end_node.is_none() {
            plan.error = Some(format!(
                "{} has no free attach node to mount by",
                self.display_name(&child_def.id)
            ));
            return;
        }
        // The child's mount size class — for `End` the mated authored
        // node's diameter (the size-class gate must pass); for `Side`
        // both ends are synthesized and simply agree.
        let mount_dia = end_node.as_ref().map(|(_, d)| *d).unwrap_or_else(|| {
            child_def
                .attach_nodes
                .first()
                .map(|n| n.diameter_m * built.scale)
                .unwrap_or(0.625)
        });
        let copies = copies.max(1);
        for k in 0..copies {
            let th = azimuth + k as f64 * 2.0 * core::f64::consts::PI / copies as f64;
            let (sy, cy) = libm::sincos(th);
            let u = DVec3::new(cy, sy, 0.0);
            // `surf:` ids store DEF-LOCAL values (scale-1 geometry):
            // `scaled_attach_node` applies the live scale/stretch at
            // every snap, so a later resize re-derives the mount point
            // instead of stranding it at today's size. The hit's z is
            // measured in the parent's scaled frame — divide it back
            // out; the diameter likewise divides by the side's own
            // scale so both ends still compare scaled-to-scaled.
            let r_p = collider_support_xy(&parent_def.collider, u);
            let parent_pos = DVec3::new(u.x * r_p, u.y * r_p, z / (p_scale * p_stretch));
            let parent_id = csp_phys::session::surface_node_id(parent_pos, u, mount_dia / p_scale);
            if bound.contains(&parent_id) {
                plan.error = Some("that surface spot is already occupied".to_string());
                return;
            }
            let edge = match mode {
                AttachMode::Side => {
                    // Upright mount: the child's synthesized node faces
                    // −u in its own local frame, so the snap rotation
                    // is the identity — the part keeps its +z axis.
                    let r_c = collider_support_xy(&child_def.collider, -u);
                    let child_id = csp_phys::session::surface_node_id(
                        DVec3::new(-u.x * r_c, -u.y * r_c, 0.0),
                        -u,
                        mount_dia / built.scale,
                    );
                    // A moved part's own children can already own this
                    // synthesized hull node — no double-mating.
                    if child_bound.contains(&child_id) {
                        plan.error = Some("the part's hull mount point is occupied".to_string());
                        return;
                    }
                    AttachEdge::new(parent, &parent_id, &child_id)
                }
                _ => {
                    // End-on mount: the pre-resolved authored node —
                    // the part protrudes perpendicular to the hull.
                    let Some((cn_id, _)) = &end_node else {
                        plan.error =
                            Some("the mounted part has no authored attach node".to_string());
                        return;
                    };
                    AttachEdge::new(parent, &parent_id, cn_id)
                }
            };
            match self.session.preview_attach(defs, built, &edge) {
                Ok(t) => {
                    plan.edges.push(edge);
                    plan.transforms.push(t);
                }
                Err(e) => {
                    plan.edges.clear();
                    plan.transforms.clear();
                    plan.error = Some(e.to_string());
                    return;
                }
            }
        }
    }

    /// Place `part` as the craft root — the shared half of
    /// [`VabAction::SetRoot`] and the empty-craft branch of
    /// [`VabAction::CatalogPick`]. Returns the confirmation line.
    fn place_root(&mut self, part_id: &PartId) -> Result<String, VabError> {
        self.check_part_count(self.session.len() + 1)?;
        let defs = self.catalog.as_ref().ok_or(VabError::NoCatalog)?;
        let built = self.stock_build(part_id)?;
        let id = self
            .session
            .add_root(defs, built)
            .map_err(VabError::Session)?;
        self.selected = Some(id);
        self.pending_part = None;
        self.dirty = true;
        Ok(format!(
            "placed {} as the craft root",
            self.display_name(part_id)
        ))
    }

    /// Snap `part` under `parent` — the shared half of
    /// [`VabAction::AttachAuto`], [`VabAction::CatalogPick`]'s
    /// selected-parent branch, and the pending-placement
    /// [`VabAction::Select`] consume. Success disarms the pending arm
    /// and selects the new part; failure leaves the arm (and the
    /// selection) alone. Returns the confirmation line.
    fn place_attach(&mut self, part: &PartId, parent: PartNodeId) -> Result<String, VabError> {
        self.check_part_count(self.session.len() + 1)?;
        let name = self.display_name(part);
        let id = self.attach_auto(part, parent)?;
        self.selected = Some(id);
        self.pending_part = None;
        self.dirty = true;
        Ok(format!("attached {name} under {}", self.node_label(parent)))
    }

    /// The stock `BuiltPart` for a catalog id — `variants[0]` is the
    /// authored baseline (validator: non-empty). §3.3: a tech-locked
    /// part refuses (`PartLocked`) and purchased upgrade tracks freeze
    /// into `upgrade_levels_at_build` at build, exactly like the
    /// sim-side `TechState::build_part` manufacture helper.
    fn stock_build(&self, part_id: &PartId) -> Result<BuiltPart, VabError> {
        let def = self.def_by_id(part_id).ok_or(VabError::PartNotInCatalog {
            part: part_id.clone(),
        })?;
        if !self.part_unlocked(def) {
            return Err(VabError::PartLocked {
                part: part_id.clone(),
                node: def.tech_requirement.clone(),
            });
        }
        let variant = def
            .variants
            .first()
            .map(|v| v.id.clone())
            .unwrap_or_else(|| VariantId::from("stock"));
        let mut built = BuiltPart::stock(part_id.clone(), variant);
        if let Some(tech) = &self.tech {
            // The family's purchased levels freeze at build — a
            // compose failure on a stale/crafted track map degrades
            // to stock, never blocks a legal placement.
            built.upgrade_levels_at_build = csp_phys::compose_upgrade_levels(
                def,
                tech.tree
                    .tracks_for_family(&def.family)
                    .map(|t| (t, tech.track_levels.get(&t.id).copied().unwrap_or(0))),
            )
            .unwrap_or_default();
        }
        Ok(built)
    }

    /// The stock build resolved — the catalog part's shape and stats
    /// at scale 1. `csp-view` resolves the placement ghost's mesh
    /// through it; `None` when the catalog/resources are not loaded
    /// or the def fails to resolve.
    pub fn resolve_stock(&self, part_id: &PartId) -> Option<EffectivePart> {
        self.resolve_built(&self.stock_build(part_id).ok()?)
    }

    /// Any `BuiltPart` resolved — the moved-part ghost's resolve:
    /// the node's actual variant/scale/stretch, not the stock build.
    /// `None` when the catalog/resources are not loaded or the def
    /// fails to resolve.
    pub fn resolve_built(&self, built: &BuiltPart) -> Option<EffectivePart> {
        let def = self.def_by_id(&built.part_id)?;
        let res = self.resources.as_ref()?;
        csp_phys::resolve::resolve_built_part(
            def,
            built,
            res,
            &csp_phys::FamilyQualification::FRESH,
        )
        .ok()
    }

    /// `(id, transform-relative-to-subtree-root)` for `node` and its
    /// descendants — the carried-subtree ghost's rigidity: the lifted
    /// node folds to the identity and each child's stored parent-local
    /// transform composes under its parent's, so the whole subtree
    /// moves as one rigid body wherever the lift lands.
    pub fn subtree_rel_transforms(&self, node: PartNodeId) -> Vec<(PartNodeId, PartTransform)> {
        let mut out = Vec::new();
        if !self.session.contains(node) {
            return out;
        }
        let mut stack = vec![(node, PartTransform::IDENTITY)];
        while let Some((id, rel)) = stack.pop() {
            out.push((id, rel));
            for c in self.session.children_ids(id) {
                if let Some(n) = self.session.node(c) {
                    stack.push((c, rel.compose(n.transform())));
                }
            }
        }
        out
    }

    /// Snap `part` under `parent` — the honest M3 auto-placement:
    /// enumerate the (parent-node, child-node) pairs and try them
    /// until `session.add_part` accepts one (size-class + direction
    /// rules live there).
    ///
    /// Candidate choice is what makes a stack buildable:
    ///
    /// - A stack node mates exactly one neighbour, so parent nodes an
    ///   edge already binds ([`CraftSession::bound_attach_nodes`]) are
    ///   not candidates at all — a mid-stack part's "top" is spoken
    ///   for by the part above it.
    /// - The child hangs on the side the parent node faces, so the
    ///   most body-downward-facing free node is preferred — "under"
    ///   means under (−z in craft frame, the stack's growth
    ///   direction).
    /// - Ties break to the smallest snap rotation (`d_c · −d_p`
    ///   descending — a top-to-bottom mate snaps with no twist while
    ///   a top-to-top one flips the child 180°).
    ///
    /// Without those rules a pod→tank→engine build mates
    /// `tank.top`→`engine.top`: the engine lands between pod and
    /// tank rotated so its nozzle points *into* the stack and its
    /// thrust into the pad — the M10-D "Demo Hopper" bug.
    fn attach_auto(&mut self, part: &PartId, parent: PartNodeId) -> Result<PartNodeId, VabError> {
        let defs = self.catalog.as_ref().ok_or(VabError::NoCatalog)?;
        let parent_part = self
            .session
            .node(parent)
            .ok_or(VabError::Session(
                csp_phys::session::SessionError::UnknownNode { id: parent },
            ))?
            .built()
            .part_id
            .clone();
        let parent_def = self
            .def_by_id(&parent_part)
            .ok_or(VabError::PartNotInCatalog {
                part: parent_part.clone(),
            })?;
        let child_def = self
            .def_by_id(part)
            .ok_or(VabError::PartNotInCatalog { part: part.clone() })?;
        let built = self.stock_build(part)?;
        let bound = self.session.bound_attach_nodes(parent);
        // Clone the candidate node records — the defs are borrowed
        // through `&self`, and the attempt loop needs `&mut
        // self.session`.
        let parent_nodes = parent_def.attach_nodes.clone();
        let child_nodes = child_def.attach_nodes.clone();
        // The parent's rotation into body frame — folded down its
        // ancestor chain — is what "downward-facing" is measured in.
        // (Local −z only agrees with stack-down while every ancestor
        // mounted unflipped; the fold stays correct through an
        // upside-down parent.)
        let mut chain = vec![parent];
        while let Some(e) = chain
            .last()
            .and_then(|id| self.session.node(*id))
            .and_then(|n| n.edge())
        {
            chain.push(e.parent);
        }
        let mut parent_t = PartTransform::IDENTITY;
        for id in chain.iter().rev() {
            if let Some(n) = self.session.node(*id) {
                parent_t = parent_t.compose(n.transform());
            }
        }
        let mut candidates: Vec<(&str, &str, f64, f64)> = Vec::new();
        for pn in &parent_nodes {
            if bound.contains(&pn.id) {
                continue;
            }
            // The node normal in body frame: the child lands on the
            // side it faces, so a large `facing` hangs the child low.
            let facing = -parent_t.apply_dir(pn.direction).z;
            for cn in &child_nodes {
                // `d_c · −d_p` is the cosine of the snap arc the
                // session computes — 1.0 snaps with no rotation,
                // −1.0 flips the child. Least-twist wins ties and
                // keeps the child's authored pose (engines mount
                // nozzle-down).
                let twist = cn.direction.dot(-pn.direction);
                candidates.push((&pn.id, &cn.id, facing, twist));
            }
        }
        // Stable sort — authored order breaks ties deterministically.
        candidates.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| b.3.total_cmp(&a.3)));
        let mut last = None;
        for &(pn, cn, ..) in &candidates {
            let edge = AttachEdge::new(parent, pn, cn);
            match self.session.add_part(defs, built.clone(), edge) {
                Ok(id) => return Ok(id),
                Err(e) => last = Some(e),
            }
        }
        Err(last.map_or(
            VabError::NoCompatibleAttach {
                part: part.clone(),
                parent,
            },
            VabError::Session,
        ))
    }

    /// Serialize the session to `CSPCRAFT` bytes — the save path the
    /// app's file store consumes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, csp_phys::session::CraftFileError> {
        self.session.to_bytes()
    }
}

/// The def's collider z-extent at a built scale — `(-h·s·t, +h·s·t)`
/// for the centered primitives, the scaled min/max z over the point
/// set for `Convex`. The surface-mount planner clamps the mount
/// height inside it.
fn collider_z_extent(collider: &csp_data::ColliderDef, scale: f64, stretch: f64) -> (f64, f64) {
    match collider {
        csp_data::ColliderDef::Box { half_extents_m } => {
            let h = half_extents_m.z * scale * stretch;
            (-h, h)
        }
        csp_data::ColliderDef::Cylinder { half_height_m, .. } => {
            let h = half_height_m * scale * stretch;
            (-h, h)
        }
        csp_data::ColliderDef::Convex { points } => {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for p in points {
                lo = lo.min(p.z * scale * stretch);
                hi = hi.max(p.z * scale * stretch);
            }
            if lo > hi { (-0.5, 0.5) } else { (lo, hi) }
        }
    }
}

/// The def's collider support radius in the xy plane along direction
/// `u` (unit, z ignored) at `scale 1` — the caller multiplies by the
/// built scale. `Cylinder` is exact; `Box`/`Convex` take the support
/// function `max(p·u)` over their vertices so a side mount lands on
/// the face, not inside it.
fn collider_support_xy(collider: &csp_data::ColliderDef, u: DVec3) -> f64 {
    match collider {
        csp_data::ColliderDef::Box { half_extents_m } => {
            half_extents_m.x * u.x.abs() + half_extents_m.y * u.y.abs()
        }
        csp_data::ColliderDef::Cylinder { radius_m, .. } => *radius_m,
        csp_data::ColliderDef::Convex { points } => points
            .iter()
            .map(|p| p.x * u.x + p.y * u.y)
            .fold(0.0_f64, f64::max),
    }
}

/// Requests the VAB panel emitted during `UiBuild` — drained by
/// `csp-app` next frame (the §2.5 one-frame hand-off).
#[derive(Clone, Debug, Default)]
pub struct VabUiOutput {
    /// A top-bar mode request.
    pub mode: Option<ModeSelect>,
    /// Save the session under this name (the bytes ride the outbox —
    /// `build_ui` serializes).
    pub save_name: Option<String>,
    /// Load the named craft file.
    pub load_name: Option<String>,
    /// The M4c launch button — spawn the working craft on the pad
    /// and switch to the flight view.
    pub launch: bool,
    /// Net warp-ladder steps from the top bar's −/+ buttons (the
    /// `DebugPanelOutput::warp_step` channel moved here when the
    /// scaffold panel became dev-gated).
    pub warp_step: i32,
    /// The M10 "Tutorial" button — re-open the first-flight checklist.
    /// Session state lives in `csp-view`, so this is folded back into
    /// the tutorial resource inside `build_ui`, not the app outbox.
    pub tutorial: bool,
    /// The "Help" button — toggle the controls card. Same fold-back
    /// convention as `tutorial` (the flag lives in `csp-view`).
    pub help: bool,
    /// The "Program" button — toggle the progression window. Same
    /// fold-back convention (the flag lives in `csp-view`).
    pub program: bool,
}

/// The mode bar — always-present top strip with the three view modes,
/// the warp readout (the player-visible half of the old debug panel —
/// its buttons ride the `warp_step` outbox), and the Tutorial/Help
/// affordances. Emits [`ModeSelect`] into the outbox; `csp-app` maps
/// it to `View::set_mode`. `keys` is the live keymap — the mode
/// labels print its bindings, not hardcoded names. Returns the drawn
/// window rect — the host adds it to [`vab_pointer_input`]'s overlay
/// mask so a click on the bar cannot reach the 3D scene behind it.
pub fn top_bar(
    ctx: &egui::Context,
    current: ModeSelect,
    warp: u32,
    keys: &crate::settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
    out: &mut VabUiOutput,
) -> egui::Rect {
    egui::Window::new("mode_bar")
        .title_bar(false)
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(0.0, 0.0))
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Clanker Space Program");
                ui.separator();
                for (mode, label) in [
                    (ModeSelect::Flight, "Flight".to_string()),
                    (
                        ModeSelect::Map,
                        format!(
                            "Map  [{}]",
                            keys.hint_labeled(crate::settings::KeyAction::ToggleMap, key_labels)
                        ),
                    ),
                    (
                        ModeSelect::Vab,
                        format!(
                            "VAB  [{}]",
                            keys.hint_labeled(crate::settings::KeyAction::ToggleVab, key_labels)
                        ),
                    ),
                ] {
                    if ui.selectable_label(current == mode, label).clicked() {
                        out.mode = Some(mode);
                    }
                }
                ui.separator();
                // M4c: spawn the working craft on the pad and fly it —
                // the vertical slice's fun gate.
                if ui.button("LAUNCH").clicked() {
                    out.launch = true;
                }
                ui.separator();
                // The warp readout stays player-visible now that the
                // debug panel is dev-gated; [,]/[.] step the ladder.
                ui.label(
                    egui::RichText::new(format!("warp ×{warp}"))
                        .color(egui::Color32::from_gray(200)),
                );
                if ui.small_button("−").clicked() {
                    out.warp_step -= 1;
                }
                if ui.small_button("+").clicked() {
                    out.warp_step += 1;
                }
                ui.separator();
                // M10: re-open the first-flight checklist — session
                // state folded back into the tutorial resource inside
                // `build_ui` (the checklist never rides the app outbox).
                if ui.button("Tutorial").clicked() {
                    out.tutorial = true;
                }
                // The controls card — same fold-back convention.
                if ui.button("? Help").clicked() {
                    out.help = true;
                }
                // The progression window — funds, science, research,
                // the vessel's experiment/reliability surface.
                if ui.button("Program").clicked() {
                    out.program = true;
                }
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING)
}

/// The whole VAB editor pass — palette, craft tree, inspector +
/// staging + readout, validation, craft-file bar, the action strip.
/// Runs only while the mode is `Vab`. Session ops apply immediately
/// through [`VabSession::apply`] (the §2.2-C channel); file IO and
/// mode switches ride `out` for next frame's app-side Input. `keys`
/// is the live keymap the hint bar prints bindings from. Returns the
/// editor's own window rects — the host folds them into
/// [`vab_pointer_input`]'s overlay mask alongside every *other*
/// floating window it drew this pass (the pointer pass runs last for
/// exactly that reason).
pub fn vab_editor(
    ctx: &egui::Context,
    vab: &mut VabSession,
    craft_files: &[String],
    keys: &crate::settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
    out: &mut VabUiOutput,
) -> Vec<egui::Rect> {
    vab.recompute();
    let mut panels = Vec::with_capacity(4);
    panels.push(palette_panel(ctx, vab));
    panels.push(inspector_panel(ctx, vab, craft_files, out));
    panels.push(action_bar(ctx, vab));
    vab_hint_bar(ctx, vab, keys, key_labels);
    panels
}

/// The pointer/hotkey half of the editor pass — the host calls it
/// LAST inside the egui pass so `masks` can carry the rect of every
/// floating window drawn this frame: the editor's own panels AND the
/// overlays drawn around them (top bar, help card, settings window,
/// crash dialog, tutorial card). Pointer events inside any mask are
/// the UI's business — they must never reach the 3D scene (a
/// right-click on the settings window used to subtree-delete the
/// craft part behind it). The KSP held-part gesture layer:
///
/// - publishes the viewport cursor (physical px) into the session so
///   `csp-view` can cast the CPU hover ray — `None` while the pointer
///   is over a panel or off the window;
/// - tracks the LMB press: a press that began on a placed part and
///   became a drag *lifts* the part for a move (the camera orbit is
///   suppressed by `orbit_block` for the press's whole life);
/// - resolves the LMB release: a lifted part dropped over the
///   viewport commits at the hover target ([`VabAction::Drop`]),
///   dropped over the palette is deleted (KSP's bin), dropped over
///   another panel stays held; a palette drag drops the same way and
///   cancels over a panel;
/// - right-click over the viewport puts the carried part back, or —
///   carrying nothing — removes the *hovered* part's subtree;
/// - `Del`/`Backspace` removes the selected part, `Esc` puts the
///   carried part back or deselects, `X` cycles radial symmetry 1→2→4.
///
/// `palette` is the parts-catalog window's rect separately — a lifted
/// part released over it is *deleted* (KSP's drag-to-bin), which is a
/// different resolution than the generic overlay mask's "stays held".
#[allow(clippy::too_many_lines)] // one gesture pass — each branch is one input's resolution
pub fn vab_pointer_input(
    ctx: &egui::Context,
    vab: &mut VabSession,
    masks: &[egui::Rect],
    palette: Option<egui::Rect>,
) {
    use egui::PointerButton;
    let (
        pos,
        pressed,
        primary_down,
        released,
        deciding,
        secondary,
        del,
        esc,
        sym_key,
        undo_key,
        redo_key,
    ) = ctx.input(|i| {
        (
            i.pointer.hover_pos(),
            i.pointer.button_pressed(PointerButton::Primary),
            i.pointer.primary_down(),
            i.pointer.button_released(PointerButton::Primary),
            i.pointer.is_decidedly_dragging(),
            i.pointer.secondary_clicked(),
            i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
            i.key_pressed(egui::Key::Escape),
            i.key_pressed(egui::Key::X),
            // `command` is Cmd on macOS, Ctrl elsewhere — egui's
            // platform-honest modifier.
            i.key_pressed(egui::Key::Z) && i.modifiers.command && !i.modifiers.shift,
            (i.key_pressed(egui::Key::Z) && i.modifiers.command && i.modifiers.shift)
                || (i.key_pressed(egui::Key::Y) && i.modifiers.command),
        )
    });
    // C14-F1: gate the hotkeys on TEXT-EDIT focus, never `InputState::focused`
    // (native window focus — true during all normal play, which dead-keyed
    // Del/Esc/X). Only a field actually taking typed text may hide them.
    let typing = ctx.text_edit_focused();
    let ppp = ctx.pixels_per_point();
    // The viewport is the content rect minus every masked window rect —
    // the mask list is geometric because egui's pointer state reads the
    // whole window regardless of which widget consumed the event (raw
    // `button_pressed`/`secondary_clicked` have no consumption path).
    let over_viewport =
        pos.is_some_and(|p| ctx.content_rect().contains(p) && !masks.iter().any(|r| r.contains(p)));
    vab.set_cursor_px(if over_viewport {
        pos.map(|p| (p.x as f64 * f64::from(ppp), p.y as f64 * f64::from(ppp)))
    } else {
        None
    });

    // The press anchor: which placed part (if any) the LMB press
    // began on — the drag-lift candidate — and whether the press
    // began over the viewport. Both anchor the gesture for its whole
    // life: a press over a panel that slides into the viewport never
    // becomes a scene gesture, and a viewport press that slides over
    // a panel keeps its orbit suppression.
    if pressed {
        vab.set_press_node(if over_viewport {
            vab.hover().map(|h| h.node)
        } else {
            None
        });
        vab.set_press_on_viewport(over_viewport);
    }
    if !primary_down {
        vab.set_press_node(None);
        vab.set_press_on_viewport(false);
    }

    // An LMB press carrying a part — or begun on one, about to lift
    // it — is a placement/move gesture, not a camera drag: `csp-view`
    // skips orbit deltas while this is set. The anchor is the PRESS
    // position, not the live cursor — mid-drag over the palette used
    // to drop the block and orbit while a part rode the cursor.
    vab.set_orbit_block(
        primary_down
            && vab.press_on_viewport()
            && (vab.carried().is_some() || vab.press_node().is_some()),
    );

    // The drag-lift: a press on a placed part that became a drag
    // picks the part up. One attempt per press — clearing
    // `press_node` here means a refused grab (or one on the carried
    // root that resolves) never refires, and a refusal falls through
    // to the camera orbit rather than a dead gesture.
    if primary_down
        && deciding
        && vab.carried().is_none()
        && let Some(n) = vab.press_node()
    {
        vab.set_press_node(None);
        let _ = vab.apply(VabAction::Grab(n));
    }

    if released {
        // A lifted part dropped over the palette is deleted — KSP's
        // drag-to-the-parts-list bin; over the viewport it commits at
        // the hover target; over any other panel it stays held.
        if let Some(node) = vab.move_node() {
            if pos.is_some_and(|p| palette.is_some_and(|r| r.contains(p))) {
                let _ = vab.apply(VabAction::Remove(node));
            } else if over_viewport {
                let _ = vab.apply(VabAction::Drop);
            }
        }
        // The palette drag resolves on release — through the same
        // `Drop` as a click, so a drop with nothing under it just
        // keeps the part held; over a panel it cancels.
        if vab.drag_part().is_some() {
            vab.set_drag_part(None);
            if over_viewport {
                let _ = vab.apply(VabAction::Drop);
            } else {
                let _ = vab.apply(VabAction::CancelPlacement);
            }
        }
        vab.set_press_node(None);
    }

    // Right-click over the viewport: carrying a part it puts it back
    // (the cancel); carrying nothing it removes the hovered part.
    if secondary && over_viewport {
        if vab.carried().is_some() {
            let _ = vab.apply(VabAction::CancelPlacement);
        } else if let Some(hit) = vab.hover() {
            let _ = vab.apply(VabAction::Remove(hit.node));
        }
    }

    // Hotkeys — gated on no text field wanting the input so a rename
    // or a save-slot edit never eats them.
    if typing {
        return;
    }
    if undo_key {
        let _ = vab.apply(VabAction::Undo);
    }
    if redo_key {
        let _ = vab.apply(VabAction::Redo);
    }
    if del && let Some(id) = vab.selected() {
        let _ = vab.apply(VabAction::Remove(id));
    }
    if esc {
        if vab.carried().is_some() {
            let _ = vab.apply(VabAction::CancelPlacement);
        } else {
            let _ = vab.apply(VabAction::Select(None));
        }
    }
    if sym_key {
        let next = match vab.symmetry() {
            1 => 2,
            2 => 4,
            _ => 1,
        };
        let _ = vab.apply(VabAction::SetSymmetry(next));
    }
}

/// The bottom-center action strip — always-visible placement
/// controls: radial symmetry, the attach-mode override, and the
/// obvious remove/cancel affordances the old hint-only UI lacked.
fn action_bar(ctx: &egui::Context, vab: &mut VabSession) -> egui::Rect {
    egui::Window::new("vab_actions")
        .title_bar(false)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -28.0))
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("symmetry");
                for n in [1u8, 2, 4] {
                    if ui
                        .selectable_label(vab.symmetry() == n, format!("×{n}"))
                        .clicked()
                    {
                        let _ = vab.apply(VabAction::SetSymmetry(n));
                    }
                }
                ui.separator();
                ui.label("mount");
                for (mode, label) in [
                    (AttachMode::Auto, "auto"),
                    (AttachMode::Axial, "stack"),
                    (AttachMode::Side, "side"),
                    (AttachMode::End, "end-on"),
                ] {
                    if ui
                        .selectable_label(vab.attach_mode() == mode, label)
                        .clicked()
                    {
                        let _ = vab.apply(VabAction::SetAttachMode(mode));
                    }
                }
                ui.separator();
                // The history controls live beside the mount strip —
                // undo is the destructive-gesture safety net.
                if ui
                    .add_enabled(vab.can_undo(), egui::Button::new("Undo (Ctrl+Z)"))
                    .clicked()
                {
                    let _ = vab.apply(VabAction::Undo);
                }
                if ui
                    .add_enabled(vab.can_redo(), egui::Button::new("Redo"))
                    .clicked()
                {
                    let _ = vab.apply(VabAction::Redo);
                }
                ui.separator();
                if let Some(id) = vab.selected()
                    && vab.carried().is_none()
                {
                    if ui.button("Pick up (move)").clicked() {
                        let _ = vab.apply(VabAction::Grab(id));
                    }
                    // "Remove this part only" — children promote to
                    // the grandparent; distinct from the subtree
                    // delete, which still eats everything below.
                    if ui.button("Remove").clicked() {
                        let _ = vab.apply(VabAction::Remove(id));
                    }
                    if vab.session().node(id).is_some_and(|n| n.edge().is_some())
                        && ui.button("Remove part only").clicked()
                    {
                        let _ = vab.apply(VabAction::RemoveOnly(id));
                    }
                }
                if vab.carried().is_some() && ui.button("Put back (Esc)").clicked() {
                    let _ = vab.apply(VabAction::CancelPlacement);
                }
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING)
}

/// One catalog row, snapshotted as owned data so the click handlers
/// below can mutate `vab` through `apply` while it renders.
struct PaletteRow {
    /// The catalog part.
    id: PartId,
    /// Loc-resolved display name (falls back to the id).
    name: String,
    /// One-line stat/module summary under the name.
    blurb: String,
    /// Browser category — the row's silhouette tint.
    cat: csp_data::PartCategory,
    /// The `(z, radius)` side profile the thumbnail draws.
    profile: Vec<(f64, f64)>,
    /// §3.3: `Some(node)` when the part's `tech_requirement` is not
    /// researched — the row draws dimmed with a "not researched"
    /// tag and cannot arm a placement.
    locked: Option<String>,
}

/// A one-line summary of what a part IS and DOES — the catalog's
/// "description" half: mass + cost plus each module's headline stat.
fn part_blurb(def: &PartDef) -> String {
    let mut mods: Vec<String> = Vec::new();
    for m in &def.modules {
        mods.push(match m {
            ModuleDef::Engine {
                thrust_vac_n,
                isp_vac_s,
                ..
            } => format!("{:.0} kN vac · Isp {isp_vac_s:.0} s", thrust_vac_n / 1000.0),
            ModuleDef::Tank { capacity } => format!(
                "holds {}",
                capacity
                    .iter()
                    .map(|(r, u)| format!("{u:.0} {}", r.as_str()))
                    .collect::<Vec<_>>()
                    .join(" + ")
            ),
            ModuleDef::Control { torque_nm, .. } => format!("{torque_nm:.0} N·m torque"),
            ModuleDef::Decoupler => "staging decoupler".to_string(),
            ModuleDef::DockPort { .. } => "docking port".to_string(),
            ModuleDef::CrewSeat { slots } => format!("{slots} crew seat(s)"),
            ModuleDef::Experiment { data_units, .. } => {
                format!("experiment · {data_units:.0} data")
            }
            ModuleDef::Drill { .. } => "resource drill".to_string(),
            ModuleDef::Converter { .. } => "ISRU converter".to_string(),
            ModuleDef::Lab { .. } => "science lab".to_string(),
            ModuleDef::Parachute {
                deployed_drag_area_m2,
                ..
            } => format!("parachute · {deployed_drag_area_m2:.0} m² CdA"),
        });
    }
    let mut s = format!("{:.0} kg · {:.0} funds", def.mass_dry_kg, def.cost_funds);
    if !mods.is_empty() {
        s.push_str(&format!(" — {}", mods.join(" · ")));
    }
    s
}

/// A side-view silhouette for the palette icon — `(z, radius)`
/// stations in ascending z, drawn mirrored about the part axis. Same
/// kind-override convention as the 3D kitbash classifier (`csp_view::
/// part_mesh`): an engine reads as its bell, a crew pod as its taper,
/// a cone collider as its authored profile.
fn def_profile(def: &PartDef) -> Vec<(f64, f64)> {
    let engine = def
        .modules
        .iter()
        .any(|m| matches!(m, ModuleDef::Engine { .. }));
    let crew = def
        .modules
        .iter()
        .any(|m| matches!(m, ModuleDef::CrewSeat { .. }));
    let (z0, z1, r) = match &def.collider {
        csp_data::ColliderDef::Box { half_extents_m } => (
            -half_extents_m.z,
            half_extents_m.z,
            half_extents_m.x.max(half_extents_m.y),
        ),
        csp_data::ColliderDef::Cylinder {
            radius_m,
            half_height_m,
        } => (-*half_height_m, *half_height_m, *radius_m),
        csp_data::ColliderDef::Convex { points } => {
            // Authored point set → per-z max ring radius — the same
            // silhouette the kitbash lathe produces.
            let mut zs: Vec<f64> = points.iter().map(|p| p.z).collect();
            zs.sort_by(f64::total_cmp);
            zs.dedup_by(|a, b| (*b - *a).abs() < 1e-9);
            let mut out = Vec::with_capacity(zs.len());
            for z in zs {
                let r = points
                    .iter()
                    .filter(|p| (p.z - z).abs() < 1e-9)
                    .map(|p| p.x.hypot(p.y))
                    .fold(0.0_f64, f64::max);
                out.push((z, r));
            }
            return out;
        }
    };
    if engine {
        // The kitbash bell profile (mount → throat → nozzle flare).
        let h = z1 - z0;
        return [
            (0.0, 0.97),
            (0.08, 0.83),
            (0.26, 0.58),
            (0.45, 0.46),
            (1.0, 0.58),
        ]
        .into_iter()
        .map(|(t, k)| (z0 + t * h, k * r))
        .collect();
    }
    if crew {
        // The capsule taper — full base radius, smoothstep to a
        // top-stack half radius.
        let h = z1 - z0;
        let rt = 0.5 * r;
        return [0.0, 0.15, 0.35, 0.55, 0.75, 0.9, 1.0]
            .into_iter()
            .map(|t: f64| {
                let u = ((t - 0.15) / 0.85).clamp(0.0, 1.0);
                (z0 + t * h, r + (rt - r) * u * u * (3.0 - 2.0 * u))
            })
            .collect();
    }
    vec![(z0, r), (z1, r)]
}

/// Draw a `(z, r)` profile as a filled axisymmetric silhouette inside
/// `rect` — the palette row's thumbnail. The axis runs vertically
/// (part +z up); the profile mirrors about it.
fn paint_part_icon(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    profile: &[(f64, f64)],
    tint: egui::Color32,
) {
    if profile.is_empty() {
        return;
    }
    let z_lo = profile.first().map_or(0.0, |s| s.0);
    let z_hi = profile.last().map_or(0.0, |s| s.0);
    let r_max = profile.iter().map(|s| s.1).fold(0.0_f64, f64::max);
    if r_max <= 0.0 || z_hi <= z_lo {
        return;
    }
    let pad = 2.0;
    let cx = rect.center().x;
    let h_half = (rect.height() * 0.5 - pad).max(1.0);
    let w_half = (rect.width() * 0.5 - pad).max(1.0);
    // Fit the silhouette into the rect preserving aspect: scale each
    // axis by its own fit so any part stays inside the cell.
    let k_r = (w_half / r_max as f32).min(1e6);
    let k_z = (h_half / ((z_hi - z_lo) * 0.5) as f32).min(1e6);
    let k = k_r.min(k_z) * 0.95;
    let map = |z: f64, r: f64| {
        let t = ((z - z_lo) / (z_hi - z_lo) - 0.5) as f32;
        egui::pos2(
            cx - r as f32 * k,
            rect.center().y - t * ((z_hi - z_lo) as f32 * k),
        )
    };
    // Right flank up, left flank back down — one closed polygon.
    let mut pts: Vec<egui::Pos2> = profile.iter().map(|&(z, r)| map(z, -r)).collect();
    pts.extend(profile.iter().rev().map(|&(z, r)| map(z, r)));
    ui.painter()
        .add(egui::Shape::convex_polygon(pts, tint, egui::Stroke::NONE));
}

/// The category tint — matches the 3D craft view's `category_color`
/// so palette icons and placed parts read identically.
fn category_tint(cat: csp_data::PartCategory) -> egui::Color32 {
    let c = match cat {
        csp_data::PartCategory::Engine => (216, 115, 51),
        csp_data::PartCategory::FuelTank => (115, 140, 179),
        csp_data::PartCategory::Control => (115, 191, 128),
        csp_data::PartCategory::Structure => (153, 153, 153),
        csp_data::PartCategory::Payload => (179, 153, 77),
        csp_data::PartCategory::Science => (140, 115, 204),
        csp_data::PartCategory::Utility => (166, 166, 140),
    };
    egui::Color32::from_rgb(c.0, c.1, c.2)
}

/// Left side: the part catalog browser + the craft tree — the
/// player's build surface. Each catalog row is a thumbnail + name +
/// key stats, click-to-add ([`VabAction::CatalogPick`]) or drag onto
/// the 3D craft (the drag resolves through
/// [`VabAction::PlaceAtHover`]). Returns the window rect so the
/// pointer pass can mask the viewport cursor.
fn palette_panel(ctx: &egui::Context, vab: &mut VabSession) -> egui::Rect {
    egui::Window::new("Parts")
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(0.0, 30.0))
        .resizable(true)
        .default_size([310.0, 620.0])
        .show(ctx, |ui| {
            ui.heading("Parts catalog");
            // Snapshot the catalog into owned rows — the click/drag
            // handlers below mutate `vab` through `apply`, so no
            // borrow may cross them.
            let grouped: Vec<(&'static str, Vec<PaletteRow>)> = {
                const CATS: [csp_data::PartCategory; 7] = [
                    csp_data::PartCategory::Control,
                    csp_data::PartCategory::Engine,
                    csp_data::PartCategory::FuelTank,
                    csp_data::PartCategory::Structure,
                    csp_data::PartCategory::Payload,
                    csp_data::PartCategory::Science,
                    csp_data::PartCategory::Utility,
                ];
                match vab.catalog() {
                    None => Vec::new(),
                    Some(c) => CATS
                        .iter()
                        .filter_map(|cat| {
                            let rows: Vec<PaletteRow> = c
                                .parts
                                .iter()
                                .filter(|p| p.def.category == *cat)
                                .map(|p| PaletteRow {
                                    id: p.def.id.clone(),
                                    name: vab.display_name(&p.def.id),
                                    blurb: part_blurb(&p.def),
                                    cat: *cat,
                                    profile: def_profile(&p.def),
                                    locked: (!vab.part_unlocked(&p.def))
                                        .then(|| p.def.tech_requirement.as_str().to_string()),
                                })
                                .collect();
                            (!rows.is_empty()).then_some((cat.name(), rows))
                        })
                        .collect(),
                }
            };
            let carried_label = match vab.carried() {
                Some(Carried::Placing(p)) => Some(format!("{} (new part)", vab.display_name(p))),
                Some(Carried::Moving(n)) => Some(format!("{} (moving)", vab.node_label(n))),
                None => None,
            };
            let empty = vab.session().is_empty();
            palette_banner(ui, vab, carried_label.as_deref(), empty);

            if grouped.is_empty() && vab.catalog().is_some() {
                ui.weak("catalog is empty");
            }
            egui::ScrollArea::vertical()
                .id_salt("vab_catalog")
                .max_height(380.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (cat, rows) in grouped {
                        egui::CollapsingHeader::new(cat)
                            .default_open(true)
                            .show(ui, |ui| {
                                for row in rows {
                                    let armed = vab.armed_part() == Some(&row.id);
                                    let verb = if empty {
                                        "pick up, then click the build area to place it"
                                    } else {
                                        "pick up, then click a craft part to attach it"
                                    };
                                    palette_row(ui, vab, &row, armed, verb);
                                }
                            });
                    }
                });
            ui.separator();
            ui.heading("Craft tree");
            egui::ScrollArea::vertical()
                .id_salt("vab_tree")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    craft_tree(ui, vab);
                });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING)
}

/// One palette row — silhouette icon, name, stat line — sensed for
/// both click (`CatalogPick`) and drag (arms `drag_part`; the drop
/// resolves in `vab_pointer_input`).
fn palette_row(ui: &mut egui::Ui, vab: &mut VabSession, row: &PaletteRow, armed: bool, verb: &str) {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(120.0), 42.0),
        egui::Sense::click_and_drag(),
    );
    let locked = row.locked.is_some();
    let tint = if locked {
        // §3.3: locked rows draw dimmed — visible but unavailable.
        let c = category_tint(row.cat);
        egui::Color32::from_rgb(
            (u32::from(c.r()) * 55 / 255) as u8,
            (u32::from(c.g()) * 55 / 255) as u8,
            (u32::from(c.b()) * 55 / 255) as u8,
        )
    } else {
        category_tint(row.cat)
    };
    // Row chrome: armed rows light up; hovered rows lift.
    let fill = if armed {
        egui::Color32::from_rgb(60, 90, 60)
    } else if resp.hovered() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    let icon_rect = egui::Rect::from_min_size(rect.min, egui::vec2(30.0, rect.height()));
    paint_part_icon(ui, icon_rect.shrink(1.0), &row.profile, tint);
    let name_col = if locked {
        ui.visuals().weak_text_color()
    } else {
        ui.visuals().text_color()
    };
    ui.painter().text(
        egui::pos2(rect.min.x + 36.0, rect.min.y + 8.0),
        egui::Align2::LEFT_TOP,
        &row.name,
        egui::FontId::proportional(13.5),
        name_col,
    );
    let blurb = row
        .locked
        .as_ref()
        .map(|n| format!("not researched — requires {n}"))
        .unwrap_or_else(|| row.blurb.clone());
    ui.painter().text(
        egui::pos2(rect.min.x + 36.0, rect.min.y + 24.0),
        egui::Align2::LEFT_TOP,
        &blurb,
        egui::FontId::proportional(11.0),
        ui.visuals().weak_text_color(),
    );
    if let Some(node) = &row.locked {
        resp.clone().on_hover_text(format!(
            "{} — not researched (requires {node})",
            row.id.as_str()
        ));
        // A locked row cannot arm — a click explains, a drag does
        // nothing (the op path refuses identically).
        if resp.clicked() {
            vab.set_notice(Some(format!(
                "{} is tech-locked — requires {node}",
                row.name
            )));
        }
        return;
    }
    resp.clone()
        .on_hover_text(format!("{} — click or drag to {verb}", row.id.as_str()));
    if resp.clicked() {
        let _ = vab.apply(VabAction::CatalogPick(row.id.clone()));
    }
    if resp.drag_started() {
        // Arming `pending_part` rides inside `set_drag_part` — the
        // banner, the ghost, and a stray mid-drag click all agree.
        vab.set_drag_part(Some(row.id.clone()));
    }
    if resp.dragged()
        && let Some(pos) = ui.ctx().input(|i| i.pointer.hover_pos())
    {
        // The carried part's name rides the cursor — the 3D ghost
        // shows where it would land.
        ui.ctx().debug_painter().text(
            pos + egui::vec2(14.0, 14.0),
            egui::Align2::LEFT_CENTER,
            &row.name,
            egui::FontId::proportional(13.0),
            egui::Color32::LIGHT_GREEN,
        );
    }
}

/// The palette's state banner — the one line that says what the next
/// click does BEFORE the player commits: carrying a part, what will
/// place it and what puts it back; otherwise how the pick-up flow
/// reads.
fn palette_banner(
    ui: &mut egui::Ui,
    vab: &mut VabSession,
    carried_label: Option<&str>,
    empty: bool,
) {
    if vab.catalog().is_none() {
        ui.colored_label(
            egui::Color32::RED,
            "part catalog not loaded — nothing can be placed",
        );
    } else if let Some(name) = carried_label {
        ui.group(|ui| {
            ui.colored_label(
                egui::Color32::LIGHT_GREEN,
                format!(
                    "Carrying {name} — click a craft part to place it; \
                     Esc or right-click puts it back."
                ),
            );
            if ui.small_button("put it back").clicked() {
                let _ = vab.apply(VabAction::CancelPlacement);
            }
        });
    } else if empty {
        ui.label("Empty craft — pick up a part, then click the build area to place it.");
    } else {
        ui.label(
            "Pick up a part (click or drag), hover the craft to snap it, \
             click to place. Drag a placed part to move it.",
        );
    }
}

/// The session tree as an indented selectable list — DFS from the
/// root. Clicking selects (the same `VabAction::Select` the GPU pick
/// lands on).
fn craft_tree(ui: &mut egui::Ui, vab: &mut VabSession) {
    let mut children: BTreeMap<PartNodeId, Vec<PartNodeId>> = BTreeMap::new();
    for (id, n) in vab.session().iter() {
        if let Some(e) = n.edge() {
            children.entry(e.parent).or_default().push(id);
        }
    }
    let mut pending: Vec<VabAction> = Vec::new();
    let mut stack: Vec<(PartNodeId, usize)> = Vec::new();
    if let Some(root) = vab.session().root() {
        stack.push((root, 0));
    }
    while let Some((id, depth)) = stack.pop() {
        if let Some(n) = vab.session().node(id) {
            // The loc display name, like the palette/notices — the raw
            // part id is the fallback, never the label.
            let name = vab.node_label(id);
            let is_sel = vab.selected() == Some(id);
            // How the part hangs: its edge's mount id — an authored
            // node name ("bottom") or a surface mount ("on the hull").
            let mount = n.edge().map(|e| {
                if e.parent_node
                    .starts_with(csp_phys::session::SURFACE_NODE_PREFIX)
                {
                    "hull".to_string()
                } else {
                    e.parent_node.clone()
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(depth as f32 * 12.0);
                let resp = ui.selectable_label(is_sel, name);
                if resp.clicked() {
                    pending.push(VabAction::Select(Some(id)));
                }
                resp.context_menu(|ui| {
                    if ui.button("select").clicked() {
                        pending.push(VabAction::Select(Some(id)));
                        ui.close();
                    }
                    if ui.button("pick up (move)").clicked() {
                        pending.push(VabAction::Grab(id));
                        ui.close();
                    }
                    if ui.button("make root").clicked() {
                        pending.push(VabAction::Reroot(id));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("remove subtree").clicked() {
                        pending.push(VabAction::Remove(id));
                        ui.close();
                    }
                    if ui.button("remove part only").clicked() {
                        pending.push(VabAction::RemoveOnly(id));
                        ui.close();
                    }
                });
                if let Some(m) = mount {
                    ui.weak(format!("@{m}"));
                }
                ui.weak(format!("s{}", n.stage()));
            });
        }
        if let Some(kids) = children.get(&id) {
            for c in kids.iter().rev() {
                stack.push((*c, depth + 1));
            }
        }
    }
    for a in pending {
        let _ = vab.apply(a);
    }
}

/// Right side: craft summary + file IO, the stage column with the
/// live readout, and the selected-part inspector.
fn inspector_panel(
    ctx: &egui::Context,
    vab: &mut VabSession,
    craft_files: &[String],
    out: &mut VabUiOutput,
) -> egui::Rect {
    egui::Window::new("Craft")
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(0.0, 30.0))
        .resizable(true)
        .default_size([320.0, 640.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                craft_section(ui, vab, craft_files, out);
                ui.separator();
                staging_section(ui, vab);
                ui.separator();
                selected_section(ui, vab);
                ui.separator();
                validation_section(ui, vab);
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING)
}

/// Craft name, totals, file save/load, mission check.
#[allow(clippy::too_many_lines)]
fn craft_section(
    ui: &mut egui::Ui,
    vab: &mut VabSession,
    craft_files: &[String],
    out: &mut VabUiOutput,
) {
    ui.heading("Craft");
    let mut name = vab.session().name().to_string();
    if ui
        .add(egui::TextEdit::singleline(&mut name).hint_text("craft name"))
        .changed()
    {
        let _ = vab.apply(VabAction::Rename(name.clone()));
    }
    if let Some(a) = vab.analysis() {
        let cap = vab
            .part_limit()
            .map(|l| format!("/{l}"))
            .unwrap_or_default();
        ui.label(format!(
            "parts {}{} · dry+wet {:.1} t · cost {:.0}",
            a.effective.len(),
            cap,
            a.launch_mass_kg / 1000.0,
            a.cost_funds,
        ));
        ui.label(format!(
            "Δv total  {:.0} m/s vac  ({:.0} m/s SL)",
            a.total_delta_v_m_per_s, a.total_delta_v_sl_m_per_s
        ));
        let check = vab.profile().evaluate(a);
        let (txt, ok) = if check.meets_profile {
            ("MEETS", true)
        } else {
            ("SHORT", false)
        };
        ui.label(format!(
            "profile {}: Δv margin {:+.0} m/s — {}",
            vab.profile().name,
            check.delta_v_margin_m_per_s,
            txt
        ));
        if !ok {
            ui.weak("short on Δv or launch TWR — see below");
        }
        if let Some(t) = check.launch_twr {
            ui.label(format!(
                "launch TWR {:.2} (min {:.2})  {}",
                t,
                vab.profile().min_launch_twr,
                if check.meets_launch_twr { "ok" } else { "LOW" }
            ));
        }
        // M9b §3.4: the profile's real confidence — `exp(−Λ_expected)`
        // over the burn segments, evaluated with zero qualification
        // units (the editor session has no save-side family table —
        // flight-tested families improve on this number).
        let specs: Vec<csp_phys::ReliabilitySpec> = a
            .effective
            .iter()
            .map(csp_phys::ReliabilitySpec::from_effective)
            .collect();
        if !specs.is_empty() {
            let quals = csp_phys::FamilyQualifications::default();
            let c = csp_phys::confidence(&specs, &quals, vab.profile());
            if c.is_finite() {
                ui.label(format!("confidence {:.1}% (unqualified family)", c * 100.0));
            }
        }
    }
    if let Some(e) = vab.analysis_error() {
        ui.colored_label(egui::Color32::RED, format!("resolve: {e}"));
    }

    // Mission profile stub — editable targets (§3.4's real
    // burn-segment confidence lands at M9b).
    ui.collapsing("Mission profile (stub)", |ui| {
        let mut dv = vab.profile().target_delta_v_m_per_s;
        ui.horizontal(|ui| {
            ui.label("target Δv m/s");
            if ui
                .add(egui::DragValue::new(&mut dv).range(0.0..=50_000.0))
                .changed()
            {
                let _ = vab.apply(VabAction::SetTargetDeltaV(dv));
            }
        });
        let mut twr = vab.profile().min_launch_twr;
        ui.horizontal(|ui| {
            ui.label("min launch TWR");
            if ui
                .add(egui::DragValue::new(&mut twr).range(0.0..=10.0).speed(0.05))
                .changed()
            {
                let _ = vab.apply(VabAction::SetMinLaunchTwr(twr));
            }
        });
        ui.weak(format!(
            "{} burn segments · g₀ ref {:.2} m/s²",
            vab.profile().burn_segments.len(),
            vab.profile().reference_g_m_s2
        ));
    });

    // Craft file IO — the bytes ride the outbox; the app owns the
    // file store. §6.3: hidden entirely in the demo build (no save
    // import/export); the session refuses the ops too — this row is
    // the honest UI reflection of that rule, not the enforcement.
    if vab.file_io() {
        ui.horizontal(|ui| {
            ui.label("save as");
            ui.add(egui::TextEdit::singleline(&mut vab.save_name).desired_width(120.0));
            if ui.button("Save").clicked() && !vab.save_name.is_empty() {
                out.save_name = Some(vab.save_name.clone());
            }
        });
        if !craft_files.is_empty() {
            egui::ComboBox::from_label("load craft")
                .selected_text("…")
                .show_ui(ui, |ui| {
                    for f in craft_files {
                        if ui.selectable_label(false, f).clicked() {
                            out.load_name = Some(f.clone());
                        }
                    }
                });
        }
    } else {
        ui.weak("craft save/load is off in the web demo");
    }
    if ui.button("New craft").clicked() {
        let _ = vab.apply(VabAction::New);
    }
    if let Some(n) = vab.notice() {
        ui.colored_label(egui::Color32::YELLOW, n);
    }
}

/// The stage column: firing order rows (highest stage fires first)
/// with each stage's parts and its live readout line.
fn staging_section(ui: &mut egui::Ui, vab: &mut VabSession) {
    ui.heading("Staging");
    ui.horizontal(|ui| {
        if ui.small_button("+ stage").clicked() {
            let top = vab
                .session()
                .populated_stages()
                .last()
                .copied()
                .unwrap_or(0);
            let _ = vab.apply(VabAction::InsertStage(top.saturating_add(1)));
        }
        if ui.small_button("− stage").clicked()
            && let Some(top) = vab.session().populated_stages().last().copied()
        {
            let _ = vab.apply(VabAction::RemoveStage(top));
        }
        if ui.small_button("compact").clicked() {
            let _ = vab.apply(VabAction::CompactStages);
        }
    });
    // Snapshot the stage rows — the labels below mutate `vab` through
    // `apply`, so no borrow of the analysis may cross them.
    type StageRow = (
        u32,
        Vec<(PartNodeId, String)>,
        usize,
        Option<(f64, f64, f64, f64)>,
    );
    let stage_rows: Vec<StageRow> = vab
        .analysis()
        .map(|a| {
            a.stages
                .iter()
                .map(|st| {
                    let parts = st
                        .part_ids
                        .iter()
                        // The loc display name (palette/notices
                        // convention); `node_label` falls back to the
                        // part id when no loc table is installed.
                        .map(|id| (*id, vab.node_label(*id)))
                        .collect();
                    let stats = (!st.engine_ids.is_empty()).then_some((
                        st.delta_v_m_per_s,
                        st.twr_surface,
                        st.burn_time_s,
                        st.thrust_vac_n,
                    ));
                    (st.stage, parts, st.separated_ids.len(), stats)
                })
                .collect()
        })
        .unwrap_or_default();
    for (stage_idx, parts, n_severed, stats) in &stage_rows {
        ui.separator();
        ui.label(format!("stage {stage_idx}"));
        for (id, name) in parts {
            let id = *id;
            let is_sel = vab.selected() == Some(id);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if ui
                    .selectable_label(is_sel, format!("{name}  #{}", id.0))
                    .clicked()
                {
                    let _ = vab.apply(VabAction::Select(Some(id)));
                }
            });
        }
        if *n_severed > 0 {
            ui.weak(format!("  severs {n_severed} part(s)"));
        }
        if let Some((dv, twr, burn, thrust)) = stats {
            ui.monospace(format!(
                "  Δv {dv:>7.0} m/s  TWR {twr:>5.2}  burn {burn:>5.0} s  F {:.0} kN",
                thrust / 1000.0,
            ));
        }
    }
}

/// The selected part's controls: variant, scale + stack length (per
/// the def's `ScaleRule`), stage, reroot/remove.
#[allow(clippy::too_many_lines)] // the inspector IS a list of sections — splitting adds indirection, not structure
fn selected_section(ui: &mut egui::Ui, vab: &mut VabSession) {
    ui.heading("Selected part");
    let Some(id) = vab.selected() else {
        ui.weak(if vab.carried().is_some() {
            "carrying a part — click a craft part to place it"
        } else {
            "none — click a part in the 3D view, the craft tree, or a stage row"
        });
        return;
    };
    // Snapshot the node's data as owned values — the closures below
    // mutate `vab` through `apply`, so no borrow may outlive this block.
    let Some(node) = vab.session().node(id) else {
        return;
    };
    let (stage_now, scale_now, stretch_now, variant_now) = (
        node.stage(),
        node.built().scale,
        node.built().stretch,
        node.built().variant.clone(),
    );
    let def_data = vab
        .def_of(id)
        .map(|d| (d.variants.clone(), d.scale.clone(), d.size_m));

    ui.label(format!("#{}  {}", id.0, vab.node_label(id)));
    ui.label(format!(
        "stage {stage_now} · scale {scale_now:.2} · length ×{stretch_now:.2} · variant {}",
        variant_now.as_str()
    ));
    // Resolved stats for the placed part — the same `effective` row
    // the reliability section indexes (ascending-id convention).
    if let Some(eff) = vab
        .session()
        .iter()
        .position(|(nid, _)| nid == id)
        .and_then(|i| vab.analysis().and_then(|a| a.effective.get(i)))
    {
        ui.weak(format!(
            "resolved {:.1} kg · {:.0} funds · {:.2}×{:.2}×{:.2} m",
            eff.weld_input.mass_kg, eff.cost_funds, eff.size_m.x, eff.size_m.y, eff.size_m.z,
        ));
    }

    // Stage controls.
    let mut stage = stage_now as i32;
    ui.horizontal(|ui| {
        ui.label("stage");
        if ui
            .add(egui::DragValue::new(&mut stage).range(0..=64))
            .changed()
        {
            let _ = vab.apply(VabAction::SetStage {
                node: id,
                stage: stage.max(0) as u32,
            });
        }
    });

    if let Some((variants, scale_rule, size_m)) = def_data {
        // Variant combo.
        egui::ComboBox::from_label("variant")
            .selected_text(variant_now.as_str())
            .show_ui(ui, |ui| {
                for v in &variants {
                    if ui
                        .selectable_label(v.id == variant_now, v.id.as_str())
                        .clicked()
                    {
                        let _ = vab.apply(VabAction::SetVariant {
                            node: id,
                            variant: v.id.clone(),
                        });
                    }
                }
            });
        // Scale control per the ScaleRule.
        match &scale_rule {
            ScaleRule::None => {
                ui.weak("fixed size (scale 1.0)");
            }
            ScaleRule::Discrete(sizes) => {
                egui::ComboBox::from_label("scale")
                    .selected_text(format!("{scale_now:.2}"))
                    .show_ui(ui, |ui| {
                        for s in sizes {
                            if ui
                                .selectable_label((scale_now - s).abs() < 1e-9, format!("{s:.2}"))
                                .clicked()
                            {
                                let _ = vab.apply(VabAction::SetScale {
                                    node: id,
                                    scale: *s,
                                });
                            }
                        }
                    });
            }
            ScaleRule::Continuous { min, max, .. } => {
                let mut s = scale_now;
                if ui
                    .add(
                        egui::Slider::new(&mut s, *min..=*max)
                            .text("scale")
                            .fixed_decimals(2),
                    )
                    .changed()
                {
                    let _ = vab.apply(VabAction::SetScale { node: id, scale: s });
                }
            }
        }
        // The stack-axis length control — `BuiltPart::stretch`, the
        // same rule's bounds as the uniform scale (`None` rules have
        // no length dimension at all). Shown in metres: the part's
        // authored z extent times the two factors.
        let (t_lo, t_hi) = match &scale_rule {
            ScaleRule::None => (1.0, 1.0),
            ScaleRule::Discrete(set) => (
                set.first().copied().unwrap_or(1.0),
                set.last().copied().unwrap_or(1.0),
            ),
            ScaleRule::Continuous { min, max, .. } => (*min, *max),
        };
        if t_hi > t_lo {
            let mut t = stretch_now;
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Slider::new(&mut t, t_lo..=t_hi)
                            .text("length")
                            .fixed_decimals(2),
                    )
                    .changed()
                {
                    let _ = vab.apply(VabAction::SetStretch {
                        node: id,
                        stretch: t,
                    });
                }
                ui.weak(format!("{:.2} m", size_m.z * scale_now * t));
            });
        }
    }

    reliability_section(ui, vab, id);
    selected_actions(ui, vab, id);
}

/// The inspector's destructive/structural button row — reroot,
/// remove-subtree, deselect.
fn selected_actions(ui: &mut egui::Ui, vab: &mut VabSession, id: PartNodeId) {
    ui.horizontal(|ui| {
        if ui.button("pick up (move)").clicked() {
            let _ = vab.apply(VabAction::Grab(id));
        }
        if ui.button("make root").clicked() {
            let _ = vab.apply(VabAction::Reroot(id));
        }
        if ui.button("remove subtree").clicked() {
            let _ = vab.apply(VabAction::Remove(id));
        }
        // The non-destructive sibling: the node's children promote to
        // the grandparent (a mid-stack delete keeps the engine stack).
        if vab.session().node(id).is_some_and(|n| n.edge().is_some())
            && ui.button("remove part only").clicked()
        {
            let _ = vab.apply(VabAction::RemoveOnly(id));
        }
        if ui.button("deselect").clicked() {
            let _ = vab.apply(VabAction::Select(None));
        }
    });
}

/// The node's resolved reliability spec — `analysis.effective[i]`
/// pairs with the session's ascending-`PartNodeId` iteration (the
/// `effective_index` convention `to_part_graph` sets).
fn selected_spec(vab: &VabSession, id: PartNodeId) -> Option<csp_phys::ReliabilitySpec> {
    let idx = vab.session().iter().position(|(nid, _)| nid == id)?;
    vab.analysis()
        .and_then(|a| a.effective.get(idx))
        .map(csp_phys::ReliabilitySpec::from_effective)
}

/// A readable one-line rendering of an authored stress rule — the
/// kind's metric vs its threshold, and the multiplier the rule
/// applies while active.
fn stress_rule_text(rule: &csp_data::StressRule) -> String {
    use csp_data::StressKind;
    let cond = match rule.kind {
        StressKind::OverThrottle => format!("throttle > {:.2}", rule.threshold),
        StressKind::BurnBeyondRated => format!("burn_s > rated + {:.0}s", rule.threshold),
        StressKind::Overtemperature => format!("T/Tmax > {:.2}", rule.threshold),
        StressKind::ExcessG => format!("g-load > {:.1}g", rule.threshold),
        StressKind::UllageNegativeIgnition => "ullage-negative ignition".to_string(),
        StressKind::ExcessRestarts => format!("restarts > {:.0}", rule.threshold),
    };
    format!("{cond} → λ×{:.2}", rule.multiplier)
}

/// §3.4's "displayed to the player" surface (the `regime_stress` spec
/// text: "hidden stress multipliers are bad design") — the selected
/// part's resolved reliability numbers plus its authored stress rules
/// evaluated by `csp_phys::stress_breakdown` under the editable probe
/// metrics. The active marks and the Π product are the same
/// evaluation the flight model applies — one function, one answer.
fn reliability_section(ui: &mut egui::Ui, vab: &mut VabSession, id: PartNodeId) {
    let Some(spec) = selected_spec(vab, id) else {
        return;
    };
    ui.collapsing("Reliability", |ui| {
        ui.monospace(format!(
            "MTBF {:.3e} s · family {}",
            spec.base_mtbf_s, spec.family
        ));
        ui.monospace(format!(
            "infant +{:.0}% · dormant ×{:.3}{}",
            spec.infant_mortality * 100.0,
            spec.dormant_stress,
            spec.engine
                .map(|e| format!(" · rated burn {:.0}s", e.rated_burn_s))
                .unwrap_or_default(),
        ));
        let modes = spec
            .failure_modes
            .iter()
            .map(|(m, w)| format!("{m:?} ×{w:.2}"))
            .collect::<Vec<_>>()
            .join(", ");
        ui.weak(format!("failure modes: {modes}"));
        if spec.regime_stress.is_empty() {
            ui.weak("no regime stress rules");
            return;
        }
        ui.separator();
        ui.weak("probe regime (hypothetical — not sim state):");
        let p = &mut vab.stress_probe;
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut p.throttle)
                    .range(0.0..=1.5)
                    .speed(0.02)
                    .prefix("thr "),
            );
            ui.add(
                egui::DragValue::new(&mut p.temp_fraction)
                    .range(0.0..=2.0)
                    .speed(0.02)
                    .prefix("T/Tmax "),
            );
            ui.add(
                egui::DragValue::new(&mut p.g_load)
                    .range(0.0..=100.0)
                    .speed(0.1)
                    .prefix("g "),
            );
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut p.burn_s)
                    .range(0.0..=1.0e9)
                    .speed(1.0)
                    .prefix("burn s "),
            );
            ui.add(
                egui::DragValue::new(&mut p.restarts)
                    .range(0.0..=1000.0)
                    .speed(1.0)
                    .prefix("restarts "),
            );
            ui.checkbox(&mut p.ullage_negative, "ullage−");
        });
        let probe = vab.stress_probe;
        for (rule, active) in csp_phys::stress_breakdown(&spec, &probe) {
            let text = stress_rule_text(rule);
            if active {
                ui.colored_label(egui::Color32::YELLOW, format!("▶ {text}"));
            } else {
                ui.weak(format!("   {text}"));
            }
        }
        ui.monospace(format!(
            "Π stress ×{:.3}  (operating λ = base × product × qual × infant)",
            csp_phys::stress_product(&spec, &probe)
        ));
    });
}

/// `validate`'s errors (red, launch-blocking) and warnings (yellow).
fn validation_section(ui: &mut egui::Ui, vab: &mut VabSession) {
    ui.heading("Validation");
    match vab.validation() {
        None => {
            ui.weak("no catalog — nothing to check");
        }
        Some(v) if v.errors.is_empty() && v.warnings.is_empty() => {
            ui.label("clean — launchable");
        }
        Some(v) => {
            for e in &v.errors {
                ui.colored_label(egui::Color32::RED, format!("{e}"));
            }
            for w in &v.warnings {
                ui.colored_label(egui::Color32::YELLOW, format!("{w}"));
            }
        }
    }
}

/// Bottom hint bar — same overlay idiom as `map_overlay`. Also the
/// viewport-side surface for an unresolvable craft: the scene draws
/// from `analysis.effective`, so a failed resolve paints an empty
/// viewport — without this the editor reads as a blank scene with no
/// cause (the panel's "resolve:" line sits off to the side in the
/// Craft window).
fn vab_hint_bar(
    ctx: &egui::Context,
    vab: &VabSession,
    keys: &crate::settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
) {
    let rect = ctx.content_rect();
    let problem = if let Some(e) = vab.analysis_error() {
        Some(format!("craft failed to resolve — {e}"))
    } else if !vab.session().is_empty() && vab.analysis().is_none() && vab.catalog().is_none() {
        // Reachable via a loaded craft file in a catalog-less session:
        // parts exist, nothing can resolve.
        Some("part catalog is not loaded — the craft cannot resolve".to_string())
    } else {
        None
    };
    egui::Area::new(egui::Id::unique("vab_hint"))
        .order(egui::Order::Background)
        .interactable(false)
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            let painter = ui.painter();
            let parts = vab.session().len();
            painter.text(
                egui::pos2(rect.left() + 10.0, rect.bottom() - 8.0),
                egui::Align2::LEFT_BOTTOM,
                format!(
                    "VAB  ·  {parts} part(s)  —  [{}] exit  drag empty space: orbit  \
                     wheel: zoom  click part: select  click/drag selected: move  \
                     drag root: move craft  Del/right-click: remove  Ctrl+Z: undo",
                    keys.hint_labeled(crate::settings::KeyAction::ToggleVab, key_labels)
                ),
                egui::FontId::proportional(13.0),
                egui::Color32::from_gray(180),
            );
            // The op notice rides the viewport too — the Craft window's
            // notice row can scroll out of sight, and "every action
            // produces a visible result or a clear notice" only holds
            // if the notice is where the player is looking.
            if let Some(n) = vab.notice() {
                painter.text(
                    egui::pos2(rect.left() + 10.0, rect.bottom() - 26.0),
                    egui::Align2::LEFT_BOTTOM,
                    n,
                    egui::FontId::proportional(13.0),
                    egui::Color32::YELLOW,
                );
            }
            // A carried part is the banner state — it outranks the
            // problem line (which still shows in the Craft window).
            // The plan's live refusal rides under it: the ghost is
            // red, this says *why*.
            match vab.carried() {
                Some(Carried::Placing(p)) => {
                    painter.text(
                        egui::pos2(rect.center().x, rect.top() + 46.0),
                        egui::Align2::CENTER_TOP,
                        format!(
                            "carrying {} — click a craft part to place it \
                             (Esc/right-click puts it back)",
                            vab.display_name(p)
                        ),
                        egui::FontId::proportional(15.0),
                        egui::Color32::LIGHT_GREEN,
                    );
                }
                Some(Carried::Moving(n)) => {
                    painter.text(
                        egui::pos2(rect.center().x, rect.top() + 46.0),
                        egui::Align2::CENTER_TOP,
                        format!(
                            "moving {} — click a craft part to reattach it \
                             (Esc/right-click puts it back, palette deletes)",
                            vab.node_label(n)
                        ),
                        egui::FontId::proportional(15.0),
                        egui::Color32::LIGHT_GREEN,
                    );
                }
                None => {
                    if let Some(msg) = &problem {
                        painter.text(
                            egui::pos2(rect.center().x, rect.top() + 46.0),
                            egui::Align2::CENTER_TOP,
                            msg,
                            egui::FontId::proportional(15.0),
                            egui::Color32::RED,
                        );
                    }
                }
            }
            if let Some(status) = vab.ghost_status() {
                painter.text(
                    egui::pos2(rect.center().x, rect.top() + 64.0),
                    egui::Align2::CENTER_TOP,
                    status,
                    egui::FontId::proportional(13.0),
                    egui::Color32::from_rgb(255, 120, 100),
                );
            }
        });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // A minimal catalog: one scalable tank (top/bottom nodes) + one
    // engine (top only) — enough to exercise the op channel without
    // touching `data/`.
    fn mini_catalog() -> LoadedParts {
        use csp_core::DVec3;
        use csp_data::{
            AeroDef, AttachNode, ColliderDef, FamilyId, LoadedPart, ModuleDef, PART_SCHEMA_VERSION,
            PartCategory, PartDef, PartVariant, QualCurve, QualKnot, ReliabilityDef, StructuralDef,
            TechNodeId, ThermalDef,
        };
        let node = |id: &str, z: f64| AttachNode {
            id: id.into(),
            position_m: DVec3::new(0.0, 0.0, z),
            direction: DVec3::new(0.0, 0.0, z.signum()),
            diameter_m: 1.0,
        };
        let def =
            |id: &str, cat: PartCategory, nodes: Vec<AttachNode>, mods: Vec<ModuleDef>| PartDef {
                schema_version: PART_SCHEMA_VERSION,
                id: PartId::from(id),
                family: FamilyId::from("f"),
                display_name: "n".into(),
                category: cat,
                mass_dry_kg: 10.0,
                cost_funds: 1.0,
                size_m: DVec3::new(1.0, 1.0, 1.0),
                build_resources: Vec::new(),
                attach_nodes: nodes,
                scale: ScaleRule::Continuous {
                    min: 0.5,
                    max: 2.0,
                    exponents: csp_data::ScaleExponents {
                        mass: 3.0,
                        cost: 3.0,
                        thrust: 3.0,
                        capacity: 3.0,
                        structural: 2.0,
                    },
                },
                collider: ColliderDef::Box {
                    half_extents_m: DVec3::new(0.5, 0.5, 0.5),
                },
                aero: AeroDef {
                    cd_curve: Vec::new(),
                    cl_curve: Vec::new(),
                    ref_len_m: 1.0,
                },
                thermal: ThermalDef {
                    heat_capacity_j_per_k: 1.0,
                    max_temp_k: 1.0,
                    emissivity: 0.5,
                    conduction_w_per_k: 1.0,
                },
                structural: StructuralDef {
                    max_axial_n: 1.0,
                    max_shear_n: 1.0,
                    max_torque_nm: 1.0,
                },
                variants: vec![PartVariant {
                    id: VariantId::from("stock"),
                    display_name: "v".into(),
                    cost_multiplier: 1.0,
                    stat_multipliers: Vec::new(),
                }],
                modules: mods,
                reliability: ReliabilityDef {
                    base_mtbf_s: 1.0e9,
                    infant_mortality: 0.0,
                    qualification_curve: QualCurve {
                        knots: vec![QualKnot {
                            qualification_units: 0.0,
                            hazard_multiplier: 1.0,
                        }],
                    },
                    regime_stress: Vec::new(),
                    dormant_stress: 0.0,
                    failure_modes: vec![(csp_data::FailureMode::SensorFault, 1.0)],
                },
                tech_requirement: TechNodeId::from("t"),
            };
        let tank = def(
            "t.tank",
            PartCategory::FuelTank,
            vec![node("top", 0.5), node("bottom", -0.5)],
            vec![ModuleDef::Tank {
                capacity: vec![(csp_data::ResourceId::from("fuel"), 10.0)],
            }],
        );
        let eng = def(
            "t.eng",
            PartCategory::Engine,
            vec![node("top", 0.5)],
            vec![],
        );
        LoadedParts {
            parts: [tank, eng]
                .into_iter()
                .map(|def| LoadedPart {
                    def,
                    path: "x".into(),
                })
                .collect(),
        }
    }

    fn mini_resources() -> ResourceFile {
        ResourceFile {
            schema_version: 1,
            resources: vec![csp_data::ResourceDef {
                id: csp_data::ResourceId::from("fuel"),
                display_name: "r".into(),
                mass_per_unit_kg: 5.0,
            }],
        }
    }

    /// Root + attach through the action channel, then recompute and
    /// save/load round-trip — the session-op surface the panels drive.
    #[test]
    fn action_channel_builds_and_roundtrips() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: root,
        })
        .unwrap();
        assert_eq!(vab.session().len(), 2);
        vab.recompute();
        assert!(vab.analysis().is_some());
        // Stable ids across save/load (M3 requirement).
        let bytes = vab.to_bytes().unwrap();
        vab.apply(VabAction::LoadBytes(bytes)).unwrap();
        vab.recompute();
        assert_eq!(vab.session().len(), 2);
    }

    /// M10-D regression — the "Demo Hopper" stack: pod→tank→engine
    /// through `AttachAuto` must mount the engine *nozzle-down* under
    /// the tank's free bottom node. The pre-fix enumeration took the
    /// first size-compatible pair in authored order — `tank.top` →
    /// `engine.top` — which lands the engine between pod and tank
    /// rotated 180°, thrusting into the pad.
    #[test]
    fn attach_auto_stacks_engine_nozzle_down() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();

        // The mid part mates the root's *bottom* node with its own
        // "top" — the no-twist pair — even though the root's free
        // "top" node is enumerated first. (Pre-fix first-fit put the
        // second part *above* the root, flipped.)
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let mid = vab.selected().unwrap();
        let mid_node = vab.session().node(mid).unwrap();
        let mid_edge = mid_node.edge().unwrap();
        assert_eq!(mid_edge.parent_node, "bottom");
        assert_eq!(mid_edge.child_node, "top");
        let mid_up = mid_node.transform().rotation.rotate(csp_core::DVec3::Z);
        assert!(
            mid_up.z > 1.0 - 1e-12,
            "mid part flipped: its +z maps to {mid_up:?}"
        );
        assert!(
            mid_node.transform().offset.z < 0.0,
            "mid part above its parent: {:?}",
            mid_node.transform()
        );

        // The engine: the mid tank's "top" node is bound by its own
        // edge, so the only sane mate is "bottom" → "top" at zero
        // rotation — nozzle down, thrust (+z, the resolved thrust
        // site's direction) pointing up through the stack.
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: mid,
        })
        .unwrap();
        let eng = vab.selected().unwrap();
        let eng_node = vab.session().node(eng).unwrap();
        let eng_edge = eng_node.edge().unwrap();
        assert_eq!(eng_edge.parent_node, "bottom");
        assert_eq!(eng_edge.child_node, "top");
        let eng_up = eng_node.transform().rotation.rotate(csp_core::DVec3::Z);
        assert!(
            eng_up.z > 1.0 - 1e-12,
            "engine flipped: its +z thrust axis maps to {eng_up:?}"
        );
        assert!(
            eng_node.transform().offset.z < 0.0,
            "engine above its parent: {:?}",
            eng_node.transform()
        );

        // Both of the mid tank's stack nodes are now bound — a second
        // engine has no honest mate and is refused, not overlapped.
        let r = vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: mid,
        });
        assert_eq!(
            r,
            Err(VabError::NoCompatibleAttach {
                part: PartId::from("t.eng"),
                parent: mid,
            })
        );
    }

    /// The held-part flow: a catalog click never places — it arms the
    /// ghost. The NEXT click commits: empty space on an empty craft
    /// drops the root, a craft part is the snap target. Empty space
    /// on a non-empty craft keeps the part held — KSP's "nothing on
    /// empty space".
    #[test]
    fn catalog_pick_arms_the_ghost_then_click_places() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        // Empty craft: the click arms, it does NOT place.
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        assert_eq!(vab.session().len(), 0);
        assert_eq!(
            vab.carried(),
            Some(Carried::Placing(&PartId::from("t.tank")))
        );
        // A background click drops the ghost as the root.
        vab.apply(VabAction::Select(None)).unwrap();
        assert_eq!(vab.session().len(), 1);
        assert!(vab.carried().is_none());
        let root = vab.session().root().unwrap();
        // Next part: armed ghost, then a click on the root attaches it.
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        assert_eq!(vab.session().len(), 1);
        vab.apply(VabAction::Select(Some(root))).unwrap();
        assert_eq!(vab.session().len(), 2);
        assert!(vab.pending_part().is_none());
        assert!(vab.notice().unwrap().contains("attached"));
        // A background click while the craft is non-empty keeps the
        // part held — the "cancels every time" trap is gone.
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        vab.apply(VabAction::Select(None)).unwrap();
        assert_eq!(vab.session().len(), 2);
        assert!(vab.pending_part().is_some());
    }

    /// The held part's honest "put it back" exits — the banner's
    /// `CancelPlacement`, re-clicking the armed catalog row, and a
    /// different row swapping the ghost — none of which places a
    /// part.
    #[test]
    fn held_part_exits() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        // A one-part craft through the held-part flow: arm, then drop.
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        vab.apply(VabAction::Select(None)).unwrap();
        assert_eq!(vab.session().len(), 1);
        // 1: the banner's cancel button (Esc/right-click land here).
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        assert!(vab.pending_part().is_some());
        vab.apply(VabAction::CancelPlacement).unwrap();
        assert!(vab.pending_part().is_none());
        assert_eq!(vab.session().len(), 1);
        // 2: re-clicking the armed row puts it back.
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        assert!(vab.pending_part().is_none());
        assert_eq!(vab.session().len(), 1);
        // 3: a different row swaps the held ghost.
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        assert_eq!(vab.pending_part(), Some(&PartId::from("t.tank")));
        assert_eq!(vab.session().len(), 1);
    }

    /// A refused pending attach keeps the arm: the error notice names
    /// the failure and the next click can try a different parent —
    /// losing the placement silently would be the old bug's shape.
    #[test]
    fn failed_pending_attach_keeps_the_arm() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let mid = vab.selected().unwrap();
        // Bind BOTH of mid's stack nodes — its top by its own edge,
        // its bottom by an engine.
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: mid,
        })
        .unwrap();
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        let r = vab.apply(VabAction::Select(Some(mid)));
        assert!(r.is_err());
        assert!(vab.pending_part().is_some());
        assert!(vab.notice().unwrap().contains("no compatible"));
        // Picking the root (free top node) instead completes it.
        vab.apply(VabAction::Select(Some(root))).unwrap();
        assert_eq!(vab.session().len(), 4);
        assert!(vab.pending_part().is_none());
    }

    /// Grab lifts a placed part onto the cursor; the drop reattaches
    /// it — the node keeps its id, its build, and its whole subtree.
    #[test]
    fn grab_lifts_and_drop_moves_the_real_node() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let mid = vab.selected().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: mid,
        })
        .unwrap();
        let eng = vab.selected().unwrap();
        // The lift removes nothing — the node stays in the session.
        vab.apply(VabAction::Grab(mid)).unwrap();
        assert_eq!(vab.move_node(), Some(mid));
        assert_eq!(vab.session().len(), 3);
        assert_eq!(vab.carried(), Some(Carried::Moving(mid)));
        // A drop with nothing under the cursor keeps it held.
        vab.apply(VabAction::Drop).unwrap();
        assert_eq!(vab.move_node(), Some(mid));
        // Drop it on the root's top cap — a real reattach.
        vab.set_hover(Some(HoverHit {
            node: root,
            local: DVec3::new(0.0, 0.0, 0.5),
            region: AttachRegion::TopCap,
        }));
        vab.apply(VabAction::Drop).unwrap();
        assert!(vab.move_node().is_none());
        assert_eq!(vab.selected(), Some(mid));
        let edge = vab.session().node(mid).unwrap().edge().unwrap().clone();
        assert_eq!(edge.parent, root);
        assert_eq!(edge.parent_node, "top");
        assert!(vab.session().node(mid).unwrap().transform().offset.z > 0.0);
        // The engine followed — same ids, same subtree.
        assert_eq!(vab.session().node(eng).unwrap().edge().unwrap().parent, mid);
        assert_eq!(vab.session().len(), 3);
    }

    /// The move's own-subtree drop puts the part back, a cancel puts
    /// it back untouched, and the root refuses to lift at all.
    #[test]
    fn move_cancel_and_self_drop_put_it_back() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let mid = vab.selected().unwrap();
        let mid_edge = vab.session().node(mid).unwrap().edge().unwrap().clone();
        // The root lifts the WHOLE craft — it anchors the build
        // frame, so the carry is the entire stack; Esc puts it back.
        vab.apply(VabAction::Grab(root)).unwrap();
        assert_eq!(vab.move_node(), Some(root));
        assert!(vab.notice().unwrap().contains("whole craft"));
        vab.apply(VabAction::CancelPlacement).unwrap();
        assert!(vab.move_node().is_none());
        // A cancel puts the lifted part back, edge untouched.
        vab.apply(VabAction::Grab(mid)).unwrap();
        assert_eq!(vab.move_node(), Some(mid));
        vab.apply(VabAction::CancelPlacement).unwrap();
        assert!(vab.move_node().is_none());
        assert_eq!(*vab.session().node(mid).unwrap().edge().unwrap(), mid_edge);
        // Dropping on its own subtree reads "put it back" too — a
        // child of mid can't be its parent.
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: mid,
        })
        .unwrap();
        let eng = vab.selected().unwrap();
        vab.apply(VabAction::Grab(mid)).unwrap();
        vab.apply(VabAction::Select(Some(eng))).unwrap();
        assert!(vab.move_node().is_none());
        assert_eq!(*vab.session().node(mid).unwrap().edge().unwrap(), mid_edge);
        // A click on the already-selected part lifts it — the
        // click-to-move affordance.
        vab.apply(VabAction::Select(Some(mid))).unwrap();
        assert!(vab.move_node().is_none());
        vab.apply(VabAction::Select(Some(mid))).unwrap();
        assert_eq!(vab.move_node(), Some(mid));
    }

    /// The move plan frees both ends of the lifted edge — a move that
    /// lands where the part already hangs is a valid plan — while
    /// nodes its children own stay bound.
    #[test]
    fn plan_move_frees_the_old_edge_not_the_children() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let mid = vab.selected().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: mid,
        })
        .unwrap();
        // Moving mid back under the root (its bottom is spoken for by
        // mid's current edge → freed by the plan): the plan exists,
        // and mid's own "bottom" — bound by the engine — cannot be
        // the child's mount node.
        vab.set_hover(Some(HoverHit {
            node: root,
            local: DVec3::new(0.0, 0.0, 0.5),
            region: AttachRegion::TopCap,
        }));
        let plan = vab.plan_move(mid, root);
        assert_eq!(plan.edges.len(), 1, "plan error: {:?}", plan.error);
        assert_eq!(plan.edges[0].parent_node, "top");
        assert_eq!(plan.edges[0].child_node, "top");
        // Into its own subtree — refused at the plan, not the commit.
        let plan = vab.plan_move(mid, vab.selected().unwrap());
        assert!(plan.edges.is_empty());
        assert!(plan.error.unwrap().contains("subtree"));
    }

    /// `display_name` resolves a def's `LocKey` through the installed
    /// loc table and falls back to the raw part id when none is.
    #[test]
    fn display_name_uses_loc_then_falls_back() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        assert_eq!(vab.display_name(&PartId::from("t.tank")), "t.tank");
        // The mini defs all key their name on LocKey "n".
        vab.set_loc(LocFile {
            schema_version: 1,
            strings: [("n".to_string(), "Test Tank".to_string())]
                .into_iter()
                .collect(),
        });
        assert_eq!(vab.display_name(&PartId::from("t.tank")), "Test Tank");
    }

    /// A bad op lands in `notice`, never panics — the panel's error path.
    #[test]
    fn refused_ops_surface_as_notice() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        let r = vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: PartNodeId(999),
        });
        assert!(r.is_err());
        assert!(vab.notice().is_some());
    }

    /// §6.3's part cap: placements that would take the craft past the
    /// configured limit are refused (`PartLimit`), at the limit
    /// still pass, and the refusal lands in `notice` like every op
    /// failure.
    #[test]
    fn part_limit_refuses_past_the_cap() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.set_part_limit(Some(2));
        assert_eq!(vab.part_limit(), Some(2));
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        // Second part — at the cap — still passes.
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        assert_eq!(vab.session().len(), 2);
        // Third part — past the cap — refuses with PartLimit.
        let parent = vab.selected().unwrap();
        let r = vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent,
        });
        assert_eq!(r, Err(VabError::PartLimit { limit: 2, count: 3 }));
        assert_eq!(vab.session().len(), 2);
        assert!(vab.notice().unwrap().contains("part limit"));
    }

    /// §6.3's part cap applies to imported craft files too — a
    /// `LoadBytes` whose session exceeds the limit is refused and the
    /// working craft is left untouched.
    #[test]
    fn part_limit_applies_to_loaded_craft() {
        // Build a 3-part craft in an uncapped session and serialize it.
        let mut src = VabSession::new(mini_catalog(), mini_resources());
        src.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let mut parent = src.session().root().unwrap();
        for _ in 0..2 {
            src.apply(VabAction::AttachAuto {
                part: PartId::from("t.tank"),
                parent,
            })
            .unwrap();
            parent = src.selected().unwrap();
        }
        assert_eq!(src.session().len(), 3);
        let bytes = src.to_bytes().unwrap();

        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.set_part_limit(Some(2));
        let r = vab.apply(VabAction::LoadBytes(bytes));
        assert_eq!(r, Err(VabError::PartLimit { limit: 2, count: 3 }));
        assert!(vab.session().is_empty());
    }

    /// §6.3's no-save rule: `file_io: false` refuses `LoadBytes`
    /// (import) with `FileIoDisabled` — export is gated at the panel
    /// (the file row hides) and the app (outbox drop).
    #[test]
    fn file_io_disabled_refuses_load() {
        let mut src = VabSession::new(mini_catalog(), mini_resources());
        src.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let bytes = src.to_bytes().unwrap();

        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        assert!(vab.file_io());
        vab.set_file_io(false);
        assert!(!vab.file_io());
        assert_eq!(
            vab.apply(VabAction::LoadBytes(bytes)),
            Err(VabError::FileIoDisabled)
        );
        assert!(vab.session().is_empty());
    }

    /// The surface-mount plan: a side-face hover produces a `surf:`
    /// parent node on the hull at the hit's azimuth, `PlaceAtHover`
    /// commits it, and the mounted child keeps its own +z axis
    /// (identity snap — the `Side` style's whole point).
    #[test]
    fn side_hover_plans_and_places_a_surface_mount() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.recompute();
        // Hover the root's +x hull face: a hit on the side AABB in
        // the node's scaled local frame (the mini catalog's collider
        // is a unit box).
        vab.set_hover(Some(HoverHit {
            node: root,
            local: DVec3::new(0.5, 0.0, 0.0),
            region: AttachRegion::Side,
        }));
        let plan = vab.plan_attach(&PartId::from("t.eng"), root);
        assert_eq!(plan.edges.len(), 1, "plan error: {:?}", plan.error);
        assert!(
            plan.edges[0]
                .parent_node
                .starts_with(csp_phys::session::SURFACE_NODE_PREFIX)
        );
        assert!(
            plan.edges[0]
                .child_node
                .starts_with(csp_phys::session::SURFACE_NODE_PREFIX)
        );
        vab.apply(VabAction::PlaceAtHover {
            part: PartId::from("t.eng"),
        })
        .unwrap();
        assert_eq!(vab.session().len(), 2);
        let eng = vab.selected().unwrap();
        let eng_node = vab.session().node(eng).unwrap();
        // Identity rotation — the mounted part stays upright.
        let up = eng_node.transform().rotation.rotate(DVec3::Z);
        assert!(up.z > 1.0 - 1e-12, "side mount flipped: {up:?}");
        // And it sits beside the root, on +x.
        assert!(eng_node.transform().offset.x > 0.5);
    }

    /// Radial symmetry: `SetSymmetry(4)` fans a surface-mount plan
    /// out to four copies around the parent's axis.
    #[test]
    fn symmetry_fans_a_surface_mount_to_copies() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::SetSymmetry(4)).unwrap();
        vab.set_hover(Some(HoverHit {
            node: root,
            local: DVec3::new(0.5, 0.0, 0.0),
            region: AttachRegion::Side,
        }));
        let plan = vab.plan_attach(&PartId::from("t.eng"), root);
        assert_eq!(plan.edges.len(), 4, "plan error: {:?}", plan.error);
        // Each copy mounts at a distinct azimuth — no two parent
        // surface ids collide.
        let ids: std::collections::BTreeSet<&str> =
            plan.edges.iter().map(|e| e.parent_node.as_str()).collect();
        assert_eq!(ids.len(), 4);
        vab.apply(VabAction::PlaceAtHover {
            part: PartId::from("t.eng"),
        })
        .unwrap();
        assert_eq!(vab.session().len(), 5);
    }

    /// `SetStretch` grows a part along its stack axis and re-snaps
    /// the part attached beneath it — the stretched parent carries
    /// its child rather than leaving it floating.
    #[test]
    fn stretch_moves_the_attached_child() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let child = vab.selected().unwrap();
        let before = vab.session().node(child).unwrap().transform().offset;
        vab.apply(VabAction::SetStretch {
            node: root,
            stretch: 2.0,
        })
        .unwrap();
        let after = vab.session().node(child).unwrap().transform().offset;
        assert_eq!(vab.session().node(root).unwrap().built().stretch, 2.0);
        // The root's bottom node moved 0.5m lower (z extent × 2), so
        // the child's transform moved with it.
        assert!(
            after.z < before.z - 0.4,
            "child did not follow the stretch: {before:?} -> {after:?}"
        );
    }

    /// A cap-face hover stacks axially on that side — the plan picks
    /// the authored node facing the hit, not just the first free one.
    #[test]
    fn cap_hover_stacks_on_the_hit_side() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.recompute();
        vab.set_hover(Some(HoverHit {
            node: root,
            local: DVec3::new(0.0, 0.0, 0.5),
            region: AttachRegion::TopCap,
        }));
        let plan = vab.plan_attach(&PartId::from("t.tank"), root);
        assert_eq!(plan.edges.len(), 1, "plan error: {:?}", plan.error);
        // A top-cap hit stacks on the parent's top node.
        assert_eq!(plan.edges[0].parent_node, "top");
        vab.apply(VabAction::PlaceAtHover {
            part: PartId::from("t.tank"),
        })
        .unwrap();
        let placed = vab.selected().unwrap();
        assert!(vab.session().node(placed).unwrap().transform().offset.z > 0.0);
    }

    /// The bottom-cap mirror of [`cap_hover_stacks_on_the_hit_side`]:
    /// once the camera can orbit under the craft, a −z cap hover must
    /// plan the parent's *bottom* node and land the part beneath it —
    /// the "can't place shit below" path end to end.
    #[test]
    fn bottom_cap_hover_plans_the_bottom_node() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.recompute();
        vab.set_hover(Some(HoverHit {
            node: root,
            local: DVec3::new(0.0, 0.0, -0.5),
            region: AttachRegion::BottomCap,
        }));
        let plan = vab.plan_attach(&PartId::from("t.tank"), root);
        assert_eq!(plan.edges.len(), 1, "plan error: {:?}", plan.error);
        // A bottom-cap hit stacks on the parent's bottom node.
        assert_eq!(plan.edges[0].parent_node, "bottom");
        vab.apply(VabAction::PlaceAtHover {
            part: PartId::from("t.tank"),
        })
        .unwrap();
        let placed = vab.selected().unwrap();
        assert!(vab.session().node(placed).unwrap().transform().offset.z < 0.0);
    }

    // ------------------------------------------------------------------
    // egui-pass tests — `vab_pointer_input` driven headlessly through
    // `Context::run_pass` with a fabricated `RawInput` (C14-F1/F2).
    // ------------------------------------------------------------------

    /// A `RawInput` sized to an 800×600 viewport carrying `events`.
    fn egui_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            focused: true,
            events,
            ..Default::default()
        }
    }

    fn key_press(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn secondary_click(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    /// C14-F1 regression: the hotkey gate used to read
    /// `InputState::focused` — the NATIVE window focus, true during all
    /// normal play — so Del/Esc/X were dead keys. With a focused window
    /// and no text edit, `Del` must remove the selected part.
    #[test]
    fn hotkeys_fire_under_plain_window_focus() {
        let ctx = egui::Context::default();
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::Select(Some(root))).unwrap();
        assert_eq!(vab.selected(), Some(root));

        ctx.run_pass(egui_input(vec![key_press(egui::Key::Delete)]), |ctx| {
            vab_pointer_input(ctx, &mut vab, &[], None);
        })
        .drop_without_applying_deltas();
        assert_eq!(
            vab.session().len(),
            0,
            "Del must remove the selected part — window focus is not typing"
        );
        // Same for the symmetry key — an X press cycles 1→2.
        ctx.run_pass(egui_input(vec![key_press(egui::Key::X)]), |ctx| {
            vab_pointer_input(ctx, &mut vab, &[], None);
        })
        .drop_without_applying_deltas();
        assert_eq!(vab.symmetry(), 2, "X must cycle radial symmetry");
    }

    /// The same hotkeys stay hidden only while a real text field holds
    /// focus — a save-name edit must never eat Del/X (and a plain
    /// `Event::Key` for X is not consumed by a `TextEdit`, so the gate
    /// itself is what suppresses it here).
    #[test]
    fn text_edit_focus_gates_hotkeys() {
        let ctx = egui::Context::default();
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        vab.apply(VabAction::Select(Some(vab.session().root().unwrap())))
            .unwrap();
        let mut name = String::new();
        let mut edit_id = egui::Id::NULL;
        // Frame 1: draw the field and claim focus for it.
        ctx.run_pass(egui_input(Vec::new()), |ctx| {
            egui::Window::new("editor_panel").show(ctx, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut name));
                edit_id = r.id;
                r.request_focus();
            });
        })
        .drop_without_applying_deltas();
        // Frame 2: focus established — X is gated by `text_edit_focused`.
        assert!(ctx.text_edit_focused());
        ctx.run_pass(egui_input(vec![key_press(egui::Key::X)]), |ctx| {
            egui::Window::new("editor_panel").show(ctx, |ui| {
                ui.add(egui::TextEdit::singleline(&mut name));
            });
            vab_pointer_input(ctx, &mut vab, &[], None);
        })
        .drop_without_applying_deltas();
        assert_eq!(
            vab.symmetry(),
            1,
            "X must not reach the game while a TextEdit holds focus"
        );
        // Focus released → the very same press fires again.
        ctx.run_pass(egui_input(Vec::new()), |ctx| {
            ctx.memory_mut(|m| m.surrender_focus(edit_id));
        })
        .drop_without_applying_deltas();
        assert!(!ctx.text_edit_focused());
        ctx.run_pass(egui_input(vec![key_press(egui::Key::X)]), |ctx| {
            vab_pointer_input(ctx, &mut vab, &[], None);
        })
        .drop_without_applying_deltas();
        assert_eq!(vab.symmetry(), 2, "X fires once the field surrenders");
    }

    /// C14-F2: pointer events inside a masked overlay rect resolve as
    /// UI — no cursor ray, no right-click remove — while a free point
    /// sees the scene. Enumerates EVERY overlay window the host draws
    /// over the VAB (top bar, help, settings, crash, tutorial, the
    /// editor's own panels): each must return its live rect so the
    /// host can mask it — a window returning `NOTHING` would click
    /// straight through to the craft.
    #[test]
    fn overlay_masks_block_pointer_but_free_viewport_sees_scene() {
        let ctx = egui::Context::default();
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.set_hover(Some(HoverHit {
            node: root,
            local: DVec3::new(0.0, 0.0, 0.5),
            region: AttachRegion::TopCap,
        }));
        // Draw every overlay the same pass order the host uses and
        // collect the rects exactly the way `build_ui` feeds `masks`.
        let mut out = VabUiOutput::default();
        let mut state = crate::settings::SettingsPanelState {
            open: true,
            ..Default::default()
        };
        let mut settings_out = crate::settings::SettingsPanelOutput::default();
        let mut tutorial = crate::tutorial::TutorialState::default();
        let mut help_open = true;
        let mut masks: Vec<egui::Rect> = Vec::new();
        ctx.run_pass(egui_input(Vec::new()), |ctx| {
            masks.push(top_bar(
                ctx,
                ModeSelect::Vab,
                1,
                &crate::settings::Keymap::default(),
                &std::collections::BTreeMap::new(),
                &mut out,
            ));
            masks.push(crate::help_panel(
                ctx,
                &crate::settings::Keymap::default(),
                &std::collections::BTreeMap::new(),
                &mut help_open,
            ));
            masks.extend(vab_editor(
                ctx,
                &mut vab,
                &[],
                &crate::settings::Keymap::default(),
                &std::collections::BTreeMap::new(),
                &mut out,
            ));
            masks.push(crate::settings::settings_panel(
                ctx,
                &mut state,
                &mut settings_out,
                &std::collections::BTreeMap::new(),
            ));
            masks.push(crate::crash_dialog(ctx, "test halt", None));
            masks.push(crate::tutorial::tutorial_overlay(
                ctx,
                &mut tutorial,
                &crate::settings::Keymap::default(),
                &std::collections::BTreeMap::new(),
                true,
            ));
        })
        .drop_without_applying_deltas();
        // Eight overlays: mode bar, help, palette, inspector, action
        // strip, settings, crash, tutorial — every one a positive rect.
        assert_eq!(masks.len(), 8, "overlay enumeration: {masks:?}");
        for (i, rect) in masks.iter().enumerate() {
            assert!(
                rect.is_positive(),
                "overlay mask {i} is empty — its window clicks through"
            );
            let pos = rect.center();
            ctx.run_pass(egui_input(secondary_click(pos)), |ctx| {
                vab_pointer_input(ctx, &mut vab, &masks, None);
            })
            .drop_without_applying_deltas();
            assert_eq!(
                vab.cursor_px(),
                None,
                "a click through overlay {i} at {pos:?} reached the viewport"
            );
            assert_eq!(
                vab.session().len(),
                1,
                "a right-click through overlay {i} deleted the craft"
            );
        }
        // The same click on free viewport resolves as the remove. Pick a
        // free point programmatically — the overlay union can cover most
        // of the screen when every panel is open, so a hardcoded point
        // breaks whenever a panel grows.
        let free = (0..60)
            .flat_map(|iy| {
                (0..80).map(move |ix| egui::pos2(ix as f32 * 10.0 + 2.0, iy as f32 * 10.0 + 2.0))
            })
            .find(|p| !masks.iter().any(|r| r.contains(*p)))
            .expect("the overlay union should leave some viewport free");
        ctx.run_pass(egui_input(secondary_click(free)), |ctx| {
            vab_pointer_input(ctx, &mut vab, &masks, None);
        })
        .drop_without_applying_deltas();
        assert!(vab.cursor_px().is_some());
        assert_eq!(vab.session().len(), 0);
    }

    /// The drag-to-bin rule keeps working with the split signature: a
    /// lifted part released over the palette rect is deleted, released
    /// over a non-palette mask it stays held.
    #[test]
    fn palette_rect_still_bins_lifted_parts() {
        let ctx = egui::Context::default();
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let mid = vab.selected().unwrap();
        vab.apply(VabAction::Grab(mid)).unwrap();
        assert_eq!(vab.move_node(), Some(mid));
        let palette = egui::Rect::from_min_size(egui::pos2(0.0, 30.0), egui::vec2(310.0, 620.0));
        let other_mask =
            egui::Rect::from_min_size(egui::pos2(500.0, 200.0), egui::vec2(200.0, 100.0));
        // Release over a NON-palette mask: stays held (masked, not binned).
        ctx.run_pass(
            egui_input(vec![
                egui::Event::PointerMoved(egui::pos2(600.0, 250.0)),
                egui::Event::PointerButton {
                    pos: egui::pos2(600.0, 250.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: egui::pos2(600.0, 250.0),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ctx| vab_pointer_input(ctx, &mut vab, &[palette, other_mask], Some(palette)),
        )
        .drop_without_applying_deltas();
        assert_eq!(
            vab.move_node(),
            Some(mid),
            "release over a non-palette window keeps the lift held"
        );
        // Release over the palette rect: the bin deletes the node.
        ctx.run_pass(
            egui_input(vec![
                egui::Event::PointerMoved(egui::pos2(150.0, 300.0)),
                egui::Event::PointerButton {
                    pos: egui::pos2(150.0, 300.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: egui::pos2(150.0, 300.0),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]),
            |ctx| vab_pointer_input(ctx, &mut vab, &[palette, other_mask], Some(palette)),
        )
        .drop_without_applying_deltas();
        assert_eq!(
            vab.session().len(),
            1,
            "release over the palette bins the lifted part"
        );
    }

    /// Phase-3 W3.2: the mid-stack subtree delete is undoable — the
    /// snapshot restores the session bit-identically (ids, transforms,
    /// stages, the id allocator), and Redo replays the removal.
    #[test]
    fn undo_restores_a_deleted_subtree() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let mid = vab.selected().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: mid,
        })
        .unwrap();
        assert_eq!(vab.session().len(), 3);
        let before = vab.session().clone();

        // Delete the mid part — the engine goes with it.
        vab.apply(VabAction::Remove(mid)).unwrap();
        assert_eq!(vab.session().len(), 1);
        assert!(vab.can_undo());
        // A no-op (Select) leaves the stack untouched.
        vab.apply(VabAction::Select(Some(root))).unwrap();
        let undo_depth = vab.undo_stack.len();

        vab.apply(VabAction::Undo).unwrap();
        assert_eq!(
            vab.session(),
            &before,
            "undo restores the craft bit-identically"
        );
        assert_eq!(vab.session().len(), 3);
        assert!(vab.can_redo());
        assert_eq!(vab.undo_stack.len(), undo_depth - 1);

        vab.apply(VabAction::Redo).unwrap();
        assert_eq!(vab.session().len(), 1);

        // Undoing past the floor is a quiet notice, not a panic —
        // drain the whole stack (4 frames: SetRoot + 2 AttachAuto +
        // Remove), then the next one reports empty.
        for _ in 0..6 {
            vab.apply(VabAction::Undo).unwrap();
        }
        assert!(vab.notice().unwrap().contains("nothing to undo"));

        // A new mutation clears the redo stack (the drained session
        // is empty — SetRoot is a history op too).
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        assert!(!vab.can_redo());
    }

    /// Phase-3 W3.3: the root lifts as the whole-craft carry — an
    /// empty-space drop lands it at the view's drop anchor (the stack
    /// follows rigidly), hovering a part refuses the mount, and
    /// `Translate` nudges the same rigid way.
    #[test]
    fn root_grab_moves_the_whole_craft() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: root,
        })
        .unwrap();
        let eng = vab.selected().unwrap();
        let eng_local = vab.session().node(eng).unwrap().transform();

        vab.apply(VabAction::Grab(root)).unwrap();
        assert_eq!(vab.carried(), Some(Carried::Moving(root)));

        // A drop ON a part refuses — the root cannot mount.
        vab.set_hover(Some(HoverHit {
            node: eng,
            local: DVec3::ZERO,
            region: AttachRegion::BottomCap,
        }));
        vab.apply(VabAction::Drop).unwrap();
        assert_eq!(
            vab.move_node(),
            Some(root),
            "a refused mount keeps the carry"
        );

        // Empty space: the craft lands at the pushed anchor.
        vab.set_hover(None);
        vab.set_drop_anchor(Some(DVec3::new(2.0, 3.0, -1.0)));
        vab.apply(VabAction::Drop).unwrap();
        assert!(vab.move_node().is_none());
        let t = vab.session().node(root).unwrap().transform();
        assert_eq!(t.offset, DVec3::new(2.0, 3.0, -1.0));
        // The subtree moved rigidly — the child's local transform is
        // byte-identical.
        assert_eq!(vab.session().node(eng).unwrap().transform(), eng_local);
        assert_eq!(
            vab.session().node(eng).unwrap().edge().unwrap().parent,
            root
        );

        // `Translate` is the same rigid move, programmatic.
        vab.apply(VabAction::Translate(DVec3::new(0.0, 0.0, 5.0)))
            .unwrap();
        assert_eq!(
            vab.session().node(root).unwrap().transform().offset,
            DVec3::new(2.0, 3.0, 4.0)
        );
        // And it is undoable.
        vab.apply(VabAction::Undo).unwrap();
        assert_eq!(
            vab.session().node(root).unwrap().transform().offset,
            DVec3::new(2.0, 3.0, -1.0)
        );
    }

    /// Phase-3 W3.4: `RemoveOnly` deletes just the mid part — the
    /// engine stack promotes onto the root instead of vanishing; the
    /// root refuses (no parent to promote to).
    #[test]
    fn remove_only_promotes_children() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::SetRoot(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.tank"),
            parent: root,
        })
        .unwrap();
        let mid = vab.selected().unwrap();
        vab.apply(VabAction::AttachAuto {
            part: PartId::from("t.eng"),
            parent: mid,
        })
        .unwrap();
        let eng = vab.selected().unwrap();

        vab.apply(VabAction::RemoveOnly(mid)).unwrap();
        assert_eq!(vab.session().len(), 2);
        assert!(vab.session().contains(eng));
        assert_eq!(
            vab.session().node(eng).unwrap().edge().unwrap().parent,
            root,
            "the engine promoted onto the grandparent"
        );
        // The root refuses RemoveOnly outright.
        assert!(vab.apply(VabAction::RemoveOnly(root)).is_err());
        assert_eq!(vab.session().len(), 2);
        // And the promote is undoable.
        vab.apply(VabAction::Undo).unwrap();
        assert_eq!(vab.session().len(), 3);
        assert!(vab.session().contains(mid));
    }
}
