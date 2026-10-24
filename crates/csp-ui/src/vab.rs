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

use std::collections::BTreeMap;
use std::fmt;

use csp_data::{
    LoadedParts, LocFile, ModuleDef, PartDef, PartId, ResourceFile, ScaleRule, VariantId,
};
use csp_phys::graph::PartTransform;
use csp_phys::readout::CraftAnalysis;
use csp_phys::session::{AttachEdge, CraftSession, Validation};
use csp_phys::{BuiltPart, MissionProfile, PartNodeId};

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
    /// The palette's one-click "add part" — context picks the honest
    /// op so a catalog click always does something visible:
    ///
    /// - empty craft → `part` becomes the root;
    /// - a part is selected → `part` attaches under it;
    /// - otherwise → arms *pending placement*: the next
    ///   [`Self::Select`]`(Some)` is consumed as the parent pick (the
    ///   banner says "click where it attaches" — from the 3D view, the
    ///   craft tree, or a stage row), and `Select(None)` cancels.
    ///   Re-clicking the armed catalog row cancels too.
    CatalogPick(PartId),
    /// Disarm a pending placement without placing anything (the
    /// palette banner's cancel button).
    CancelPlacement,
    /// Remove `node` and its subtree.
    Remove(PartNodeId),
    /// Re-root the tree at `node` (the VAB's reroot tool).
    Reroot(PartNodeId),
    /// Select `Some` node for the inspector, or clear the selection.
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
        }
    }
}

impl std::error::Error for VabError {}

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
    /// The armed pending placement (see [`VabAction::CatalogPick`]):
    /// `Some(part)` means the next `Select(Some)` is consumed as that
    /// part's parent pick, and `Select(None)`/[`VabAction::CancelPlacement`]
    /// disarms. Session state — never serialized into the craft file.
    pending_part: Option<PartId>,
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
    /// Craft-generation counter — bumped by `VabAction::New` and a
    /// successful `LoadBytes`. The 3D view watches it so the camera's
    /// one-shot fit re-arms on a craft that is not the one it framed
    /// (edits don't bump it — only replacing the craft does).
    craft_epoch: u64,
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
            craft_epoch: 0,
        }
    }
}

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

    /// The armed pending placement, if any (see
    /// [`VabAction::CatalogPick`]) — the palette banner and hint bar
    /// surface it.
    pub fn pending_part(&self) -> Option<&PartId> {
        self.pending_part.as_ref()
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
    pub fn apply(&mut self, action: VabAction) -> Result<(), VabError> {
        let r = self.apply_inner(action);
        match &r {
            Ok(msg) => self.notice = msg.clone(),
            Err(e) => self.notice = Some(e.to_string()),
        }
        r.map(|_| ())
    }

    /// The op dispatch — see [`Self::apply`]. `Ok(Some(_))` is a
    /// player-facing confirmation; `Ok(None)` clears the notice (a
    /// quiet op — selection, rename keystroke, profile drag — whose
    /// result is already visible where it happened).
    fn apply_inner(&mut self, action: VabAction) -> Result<Option<String>, VabError> {
        Ok(match action {
            VabAction::New => {
                self.session = CraftSession::with_name(self.session.name());
                self.selected = None;
                self.pending_part = None;
                self.craft_epoch += 1;
                self.dirty = true;
                Some("new craft — click a catalog part to place the root".to_string())
            }
            VabAction::SetRoot(part_id) => Some(self.place_root(&part_id)?),
            VabAction::AttachAuto { part, parent } => Some(self.place_attach(&part, parent)?),
            VabAction::CatalogPick(part) => self.catalog_pick(&part)?,
            VabAction::CancelPlacement => {
                if self.pending_part.take().is_some() {
                    Some("placement cancelled".to_string())
                } else {
                    None
                }
            }
            VabAction::Remove(node) => Some(self.remove_node(node)?),
            VabAction::Reroot(node) => {
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
        })
    }

    /// The palette's one-click add — [`VabAction::CatalogPick`]'s
    /// dispatch. The catalog check runs before ANY branch so a
    /// catalog-less session refuses honestly instead of arming a
    /// placement that can never resolve.
    fn catalog_pick(&mut self, part: &PartId) -> Result<Option<String>, VabError> {
        if self.catalog.is_none() {
            return Err(VabError::NoCatalog);
        }
        Ok(Some(if self.session.is_empty() {
            self.place_root(part)?
        } else if let Some(parent) = self.selected() {
            self.place_attach(part, parent)?
        } else if self.pending_part.as_ref() == Some(part) {
            // Re-clicking the armed row toggles it off.
            self.pending_part = None;
            "placement cancelled".to_string()
        } else {
            // Refuse to arm a part the catalog doesn't carry — arming
            // a bad id would park the editor in a state whose only
            // exits are cancel and a guaranteed-failing attach.
            self.def_by_id(part)
                .ok_or(VabError::PartNotInCatalog { part: part.clone() })?;
            let msg = format!(
                "placing {} — click the craft part to attach it under \
                 (3D view, craft tree, or stage row)",
                self.display_name(part)
            );
            self.pending_part = Some(part.clone());
            msg
        }))
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
        self.dirty = true;
        Ok(if removed.len() > 1 {
            format!("removed {label} + subtree ({} parts)", removed.len())
        } else {
            format!("removed {label}")
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
        self.craft_epoch += 1;
        self.dirty = true;
        Ok(format!("loaded craft '{}'", self.session.name()))
    }

    /// [`VabAction::Select`]'s dispatch. An armed pending placement
    /// consumes `Some(node)` as its parent pick — the banner's "click
    /// where it attaches" made literal; a failed attach keeps the arm
    /// so the next click can try a different parent. `None` (a
    /// background click) deselects and disarms.
    fn select(&mut self, sel: Option<PartNodeId>) -> Result<Option<String>, VabError> {
        let target = sel.filter(|id| self.session.contains(*id));
        if let (Some(part), Some(parent)) = (self.pending_part.clone(), target) {
            return self.place_attach(&part, parent).map(Some);
        }
        self.selected = target;
        Ok(if target.is_none() && self.pending_part.take().is_some() {
            // Background click — the natural cancel.
            Some("placement cancelled".to_string())
        } else {
            None
        })
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
    /// authored baseline (validator: non-empty).
    fn stock_build(&self, part_id: &PartId) -> Result<BuiltPart, VabError> {
        let def = self.def_by_id(part_id).ok_or(VabError::PartNotInCatalog {
            part: part_id.clone(),
        })?;
        let variant = def
            .variants
            .first()
            .map(|v| v.id.clone())
            .unwrap_or_else(|| VariantId::from("stock"));
        Ok(BuiltPart::stock(part_id.clone(), variant))
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
}

/// The mode bar — always-present top strip with the three view modes,
/// the warp readout (the player-visible half of the old debug panel —
/// its buttons ride the `warp_step` outbox), and the Tutorial/Help
/// affordances. Emits [`ModeSelect`] into the outbox; `csp-app` maps
/// it to `View::set_mode`. `keys` is the live keymap — the mode
/// labels print its bindings, not hardcoded names.
pub fn top_bar(
    ctx: &egui::Context,
    current: ModeSelect,
    warp: u32,
    keys: &crate::settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
    out: &mut VabUiOutput,
) {
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
            });
        });
}

/// The whole VAB editor pass — palette, craft tree, inspector +
/// staging + readout, validation, craft-file bar. Runs only while the
/// mode is `Vab`. Session ops apply immediately through
/// [`VabSession::apply`] (the §2.2-C channel); file IO and mode
/// switches ride `out` for next frame's app-side Input. `keys` is the
/// live keymap the hint bar prints bindings from.
pub fn vab_editor(
    ctx: &egui::Context,
    vab: &mut VabSession,
    craft_files: &[String],
    keys: &crate::settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
    out: &mut VabUiOutput,
) {
    vab.recompute();
    palette_panel(ctx, vab);
    inspector_panel(ctx, vab, craft_files, out);
    vab_hint_bar(ctx, vab, keys, key_labels);
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
        });
    }
    let mut s = format!("{:.0} kg · {:.0} funds", def.mass_dry_kg, def.cost_funds);
    if !mods.is_empty() {
        s.push_str(&format!(" — {}", mods.join(" · ")));
    }
    s
}

/// Left side: the part catalog browser + the craft tree — the
/// player's build surface. Every catalog row is a one-click add
/// ([`VabAction::CatalogPick`]); the banner above the list says what
/// the next click does, so the flow needs no docs.
fn palette_panel(ctx: &egui::Context, vab: &mut VabSession) {
    egui::Window::new("Parts")
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(0.0, 30.0))
        .resizable(true)
        .default_size([300.0, 620.0])
        .show(ctx, |ui| {
            ui.heading("Parts catalog");
            // Snapshot the catalog into owned rows — the click handlers
            // below mutate `vab` through `apply`, so no borrow may
            // cross them.
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
                                })
                                .collect();
                            (!rows.is_empty()).then_some((cat.name(), rows))
                        })
                        .collect(),
                }
            };
            let pending_name = vab.pending_part().map(|p| vab.display_name(p));
            let empty = vab.session().is_empty();
            let sel_name = vab.selected().map(|id| vab.node_label(id));
            palette_banner(ui, vab, pending_name.as_deref(), empty, sel_name.as_deref());

            if grouped.is_empty() && vab.catalog().is_some() {
                ui.weak("catalog is empty");
            }
            egui::ScrollArea::vertical()
                .id_salt("vab_catalog")
                .max_height(360.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (cat, rows) in grouped {
                        egui::CollapsingHeader::new(cat)
                            .default_open(true)
                            .show(ui, |ui| {
                                for row in rows {
                                    let armed = pending_name.is_some()
                                        && vab.pending_part() == Some(&row.id);
                                    let verb = if empty {
                                        "place as the craft root"
                                    } else if sel_name.is_some() {
                                        "attach under the selected part"
                                    } else {
                                        "start placing (then click the parent)"
                                    };
                                    let hover = format!("{} — click to {verb}", row.id.as_str());
                                    let name_hit = ui
                                        .selectable_label(
                                            armed,
                                            egui::RichText::new(&row.name).strong(),
                                        )
                                        .on_hover_text(&hover)
                                        .clicked();
                                    // The blurb is the same part's row —
                                    // a click on it must do what the
                                    // name does, not dead-end.
                                    let blurb_hit = ui
                                        .add(
                                            egui::Label::new(
                                                egui::RichText::new(format!("   {}", row.blurb))
                                                    .weak(),
                                            )
                                            .sense(egui::Sense::click()),
                                        )
                                        .on_hover_text(&hover)
                                        .clicked();
                                    if name_hit || blurb_hit {
                                        let _ = vab.apply(VabAction::CatalogPick(row.id.clone()));
                                    }
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
        });
}

/// The palette's state banner — the one line that says what the next
/// catalog click does BEFORE the player commits. The old "select a
/// parent part first" notice answered after the fact and left the
/// attach step undiscoverable; pending placement turns the same
/// gesture into the guided "click a part, then click where it
/// attaches" flow.
fn palette_banner(
    ui: &mut egui::Ui,
    vab: &mut VabSession,
    pending_name: Option<&str>,
    empty: bool,
    sel_name: Option<&str>,
) {
    if vab.catalog().is_none() {
        ui.colored_label(
            egui::Color32::RED,
            "part catalog not loaded — nothing can be placed",
        );
    } else if let Some(name) = pending_name {
        ui.group(|ui| {
            ui.colored_label(
                egui::Color32::LIGHT_GREEN,
                format!(
                    "Placing {name} — click the craft part to attach it under \
                     (3D view, craft tree below, or a stage row)."
                ),
            );
            if ui.small_button("cancel placement").clicked() {
                let _ = vab.apply(VabAction::CancelPlacement);
            }
        });
    } else if empty {
        ui.label("Empty craft — click a part to place it as the root.");
    } else if let Some(sel) = sel_name {
        ui.label(format!(
            "Click a part to attach it under the selected {sel}."
        ));
    } else {
        ui.label(
            "Click a part, then click the craft part it attaches under \
             (3D view, craft tree, or stage row).",
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
            ui.horizontal(|ui| {
                ui.add_space(depth as f32 * 12.0);
                if ui.selectable_label(is_sel, name).clicked() {
                    pending.push(VabAction::Select(Some(id)));
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
) {
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
        });
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

/// The selected part's controls: variant, scale (per the def's
/// `ScaleRule`), stage, reroot/remove.
fn selected_section(ui: &mut egui::Ui, vab: &mut VabSession) {
    ui.heading("Selected part");
    let Some(id) = vab.selected() else {
        ui.weak(if vab.pending_part().is_some() {
            "placing a part — click the craft part it attaches under"
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
    let (stage_now, scale_now, variant_now) = (
        node.stage(),
        node.built().scale,
        node.built().variant.clone(),
    );
    let def_data = vab
        .def_of(id)
        .map(|d| (d.variants.clone(), d.scale.clone()));

    ui.label(format!("#{}  {}", id.0, vab.node_label(id)));
    ui.label(format!(
        "stage {stage_now} · scale {scale_now:.2} · variant {}",
        variant_now.as_str()
    ));

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

    if let Some((variants, scale_rule)) = def_data {
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
    }

    reliability_section(ui, vab, id);
    selected_actions(ui, vab, id);
}

/// The inspector's destructive/structural button row — reroot,
/// remove-subtree, deselect.
fn selected_actions(ui: &mut egui::Ui, vab: &mut VabSession, id: PartNodeId) {
    ui.horizontal(|ui| {
        if ui.button("make root").clicked() {
            let _ = vab.apply(VabAction::Reroot(id));
        }
        if ui.button("remove subtree").clicked() {
            let _ = vab.apply(VabAction::Remove(id));
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
                    "VAB  ·  {parts} part(s)  —  [{}] exit  drag: orbit  wheel: zoom  \
                     click part: select/attach  catalog click: add part",
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
            // An armed placement is the banner state — it outranks the
            // problem line (which still shows in the Craft window).
            if let Some(p) = vab.pending_part() {
                painter.text(
                    egui::pos2(rect.center().x, rect.top() + 46.0),
                    egui::Align2::CENTER_TOP,
                    format!(
                        "placing {} — click a craft part to attach it under \
                         (click empty space to cancel)",
                        vab.display_name(p)
                    ),
                    egui::FontId::proportional(15.0),
                    egui::Color32::LIGHT_GREEN,
                );
            } else if let Some(msg) = &problem {
                painter.text(
                    egui::pos2(rect.center().x, rect.top() + 46.0),
                    egui::Align2::CENTER_TOP,
                    msg,
                    egui::FontId::proportional(15.0),
                    egui::Color32::RED,
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

    /// The palette's one-click add drives the whole build flow: empty
    /// craft → root; a selection → immediate attach under it; neither
    /// → the placement ARMS (pending) and the next `Select` is
    /// consumed as the parent pick — from a tree row, a stage row, or
    /// the 3D view's pick, all of which land on `Select`.
    #[test]
    fn catalog_pick_drives_the_build_flow() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        // Empty craft → the click places the root.
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        let root = vab.session().root().unwrap();
        assert_eq!(vab.selected(), Some(root));
        assert!(vab.notice().unwrap().contains("root"));
        // The root stays selected → the next click attaches under it.
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        assert_eq!(vab.session().len(), 2);
        let mid = vab.selected().unwrap();
        assert_ne!(mid, root);
        // Deselect → a catalog click arms pending placement rather
        // than dying on "select a parent first".
        vab.apply(VabAction::Select(None)).unwrap();
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        assert_eq!(vab.pending_part(), Some(&PartId::from("t.eng")));
        assert!(vab.notice().unwrap().contains("placing"));
        assert_eq!(vab.session().len(), 2); // armed, not placed
        // The next Select is the parent pick — the guided
        // "click a part, then click where it attaches".
        vab.apply(VabAction::Select(Some(mid))).unwrap();
        assert_eq!(vab.session().len(), 3);
        assert!(vab.pending_part().is_none());
        assert!(vab.notice().unwrap().contains("attached"));
        // The new part is selected, so the NEXT catalog click targets
        // it as parent without an extra click — here honestly refused
        // (the engine's only node is bound by its own edge), with the
        // error notice to say why.
        assert_ne!(vab.selected(), Some(mid));
        let r = vab.apply(VabAction::CatalogPick(PartId::from("t.tank")));
        assert!(r.is_err());
        assert_eq!(vab.session().len(), 3);
        assert!(vab.notice().unwrap().contains("no compatible"));
    }

    /// Pending placement has three honest exits — a background click
    /// (`Select(None)`), the banner's `CancelPlacement`, and
    /// re-clicking the armed catalog row — and none of them places a
    /// part.
    #[test]
    fn pending_placement_cancels_three_ways() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        vab.apply(VabAction::Select(None)).unwrap();
        // 1: background click cancels.
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        assert!(vab.pending_part().is_some());
        vab.apply(VabAction::Select(None)).unwrap();
        assert!(vab.pending_part().is_none());
        assert_eq!(vab.session().len(), 1);
        // 2: the banner's cancel button.
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        vab.apply(VabAction::CancelPlacement).unwrap();
        assert!(vab.pending_part().is_none());
        assert_eq!(vab.session().len(), 1);
        // 3: re-clicking the armed row toggles it off.
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        assert!(vab.pending_part().is_none());
        assert_eq!(vab.session().len(), 1);
    }

    /// A refused pending attach keeps the arm: the error notice names
    /// the failure and the next click can try a different parent —
    /// losing the placement silently would be the old bug's shape.
    #[test]
    fn failed_pending_attach_keeps_the_arm() {
        let mut vab = VabSession::new(mini_catalog(), mini_resources());
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        vab.apply(VabAction::CatalogPick(PartId::from("t.tank")))
            .unwrap();
        let mid = vab.selected().unwrap();
        // Bind the mid tank's bottom node too — then it has no honest
        // mate left for the armed engine.
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        vab.apply(VabAction::Select(None)).unwrap();
        vab.apply(VabAction::CatalogPick(PartId::from("t.eng")))
            .unwrap();
        let r = vab.apply(VabAction::Select(Some(mid)));
        assert!(r.is_err());
        assert!(vab.pending_part().is_some());
        assert!(vab.notice().unwrap().contains("no compatible"));
        // Picking the root (free top node) instead completes it.
        let root = vab.session().root().unwrap();
        vab.apply(VabAction::Select(Some(root))).unwrap();
        assert_eq!(vab.session().len(), 4);
        assert!(vab.pending_part().is_none());
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
}
