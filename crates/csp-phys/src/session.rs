//! The M3 editor session model — the VAB's working craft (PLAN §2.2
//! category C "session state", M3 row "part snap graph; staging; …
//! craft save/load with stable `PartId`s").
//!
//! # Placement (a documented decision, not an oversight)
//!
//! §2.1 assigns "the VAB session model" to `csp-ui` — and it stays
//! correct to say the *working instance* lives there: a `CraftSession`
//! value is §2.2-C session state owned by `csp-ui`, never hashed, never
//! in the command log. The *type* lives in `csp-phys` instead because
//! everything it manipulates is already here — [`PartGraph`] /
//! [`PartTransform`] ([`crate::graph`]) and [`BuiltPart`]
//! ([`crate::resolve`]) — and because its product
//! [`CraftSession::to_part_graph`] feeds `resolve_parts`/`weld` in this
//! same crate. The alternative drags `csp-phys` + `csp-data` + a codec
//! into the egui crate and, worse, forces every headless consumer —
//! `csp-bench` scenario checks, the §2.11 design solver in `csp-ai`,
//! session-class MCP `craft.load`/`editor.build` tools — to depend on a
//! UI crate. Nothing session-shaped needs `csp-sim`: `CraftDef` is
//! assembled at the `csp-sim`/`csp-app` boundary from this module's
//! `(PartGraph, Vec<BuiltPart>)` output. §2.1's dep rules are
//! untouched: `csp-phys` already allows `csp-core`, `csp-data`,
//! `serde`, `bincode`.
//!
//! # The model
//!
//! A `CraftSession` is a single rooted tree of [`SessionNode`]s keyed
//! by [`PartNodeId`] — the same identity space the §2.4 weld graph
//! uses, so `csp-sim`'s `PartInstanceId` arena keys on it verbatim at
//! manufacture (graph.rs's doc). Each node carries:
//!
//! - a [`BuiltPart`] (the §3.1 manufacture record — part def, scale,
//!   variant, frozen upgrades);
//! - an optional [`AttachEdge`] (`None` iff root): which `AttachNode`
//!   of the parent mates which `AttachNode` of the child — the
//!   §3.1 `(part_node, node_id)` pair craft files reference;
//! - the child's [`PartTransform`] in the parent frame;
//! - a `stage: u32` (see staging below).
//!
//! `PartNodeId`s are `u64`s allocated from a monotonic `next_id`
//! counter that is **serialized with the craft file** — ids are stable
//! across save/load and never reused, which is the hard M3 requirement
//! (staging and per-instance state key on them). Removing a part frees
//! nothing; the counter only climbs.
//!
//! # Snap semantics (§3.1 `AttachNode`)
//!
//! `add_part`/`reattach` take an [`AttachEdge`] naming the parent node
//! and one attach node on each def. The session computes the snap
//! transform itself: the minimal-arc rotation sending the child's node
//! direction onto the *opposite* of the parent's (`rotation·d_c =
//! −d_p`), then the offset putting the child's node position on the
//! parent's (`offset = p_p − rotation·p_c`). Positions and diameters
//! are the defs' values scaled by each part's own `s` — identical to
//! the resolver's attach-node scaling (`position·s`, `diameter·s`,
//! direction unchanged). The size class must match: `d_p`/`d_c` within
//! [`ATTACH_DIAMETER_REL_TOLERANCE`] or the attach is refused — §3.1's
//! "a 1.25 m node does not snap a 0.625 m node without an adapter;
//! the check is editor policy" is enforced here, at the boundary where
//! it can still be a typed error. After placement the editor may
//! fine-adjust via [`CraftSession::set_transform`]; the stored edge
//! remains as provenance.
//!
//! # Staging
//!
//! KSP's rule: **higher stage numbers fire earlier** — the firing
//! order is strictly descending (`stage_order`). New parts default to
//! stage `0`, the last-fired floor, and indices are capped at
//! [`MAX_STAGE_INDEX`] (`set_stage`/`insert_stage` refuse more — a
//! saturating add may never merge two stages). A
//! [`ModuleDef::Decoupler`] part
//! severs *all its child edges* when its stage fires ("the attach node
//! below this part severs" — the decoupler stays with the parent side;
//! radial children depart too — a documented simplification for v1's
//! single-child decouplers).
//!
//! [`CraftSession::validate`] returns a [`Validation`] with two lists.
//! *Errors* make the craft unlaunchable: an empty craft (matching
//! `to_part_graph`'s `EmptySession` refusal), missing defs, stored
//! `built` records failing the shared scale/variant/upgrade gate,
//! out-of-cap stage indices, or bindings naming attach nodes the
//! defs don't carry — the last four reachable only through corrupt
//! files or callers mutating behind the session's back. *Warnings*
//! are editor surfacing: an empty stage index in the firing sequence,
//! a decoupler with no children to sever, a post-scale diameter
//! mismatch, **an engine whose stage leaves it no propellant**, and
//! **an engine whose supply the pooled feed can strand across the
//! stage boundary**. Both propellant warnings are computed over the
//! engine's *supply region*: the tree component still attached after
//! every decoupler in a *higher* stage has severed (a decoupler in
//! the engine's own stage fires with it — KSP's simultaneous
//! fire-and-separate — so it does not starve it). The first is a
//! *capability* check: if no region tank carries a resource the
//! engine burns, that's a warning. The second (C13-F2) is a
//! *depletion* check the capability test cannot express: the pooled
//! feed debits tanks ascending `PartNodeId` regardless of stage
//! boundaries, so when the last tank in drain order holding a burned
//! resource sits outside the region, a burn can leave the stage dry
//! while propellant still aboard is jettisoned — the starved stage
//! row `readout` reports. Both are warnings, never errors — the
//! plan's staging text is thin and a borderline craft must still
//! save.
//!
//! # Craft file format
//!
//! `to_bytes`/`from_bytes`: magic [`CRAFT_FILE_MAGIC`] (§7.2's
//! `CSPSAVE\0` convention), then a `bincode` body of
//! `CraftFile { schema_version, name, next_id, root, nodes }` with
//! nodes in ascending-id order — byte-canonical, so equal sessions
//! serialize identically and save→load→save is a fixed point.
//! `bincode`, not the §7.2 CBOR+deflate save envelope: the craft file
//! is a session-domain artifact (§2.2-C — outside the save's state
//! hash), `bincode` is the plan's other pinned codec, and it is
//! already on `csp-phys`'s §2.1 allowlist where `ciborium`/
//! `miniz_oxide` are not. RON is excluded per §7.7 (no portable
//! NaN/inf round-trip; the RON dump is write-only). Unknown schema
//! versions are refused per the §7.7 pre-1.0 policy — no migration
//! functions before the first public build.
//!
//! # Determinism
//!
//! `BTreeMap`/`BTreeSet` everywhere, ascending-id iteration for
//! anything order-sensitive, `libm` for the one transcendental
//! (`acos` in the snap arc), no I/O, no clock. `to_part_graph` emits
//! `built[i]` bound to the node with `effective_index == i` in
//! ascending-id order — the positional contract `weld` and
//! `resolve_parts` share, so a session round-trips to the weld
//! deterministically.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use csp_core::{DQuat, DVec3, SimError};
use csp_data::{
    AttachNode, LoadedParts, ModuleDef, PartDef, PartId, ResourceId, ScaleRule, StatPath, VariantId,
};

use crate::graph::{PartGraph, PartNode, PartNodeId, PartTransform};
use crate::resolve::BuiltPart;

/// Body schema version of the craft file — the §3.1 first-field
/// convention carried into the codec. §7.7 pre-1.0: an unknown version
/// is *refused*, never migrated. Version 2 added `BuiltPart::stretch`
/// (the stack-axis length factor) and [`SURFACE_NODE_PREFIX`] attach
/// ids; version-1 bodies decode against the old `BuiltPart` layout and
/// so are refused rather than silently misread.
pub const CRAFT_SCHEMA_VERSION: u32 = 2;

/// Craft-file magic — §7.2's `CSPSAVE\0` convention, one byte longer.
pub const CRAFT_FILE_MAGIC: &[u8] = b"CSPCRAFT\0";

/// Relative tolerance on the attach-node size-class check: two nodes
/// snap when `|d_p − d_c| ≤ tol · max(d_p, d_c)` at their scaled
/// diameters. 1e-6 is loose enough that authored 1.25 m vs scaled
/// 0.625·2 m counts as a match and a real size-class jump (1.25 vs
/// 0.625) does not — the §3.1 adapter rule made a number.
pub const ATTACH_DIAMETER_REL_TOLERANCE: f64 = 1e-6;

/// An attach direction shorter than this cannot orient a snap —
/// the defence-in-depth gate on defs that skipped the validator's
/// `‖d‖ = 1` rule. Not a unit-ness *requirement*: the arc math
/// normalizes its inputs, so authored ±1e-6 off-unit normals snap
/// exactly as intended.
const MIN_DIRECTION_NORM: f64 = 1e-9;

/// The dot-product band inside which two unit directions count as
/// parallel/antiparallel for the snap arc — wider than f64 noise on
/// normalized authored vectors, far tighter than any real authoring
/// case.
const ARC_PARALLEL_EPS: f64 = 1e-12;

/// Cap on individually reported [`SessionWarning::EmptyStage`] gaps.
/// Crafted files can still carry stage indices above
/// [`MAX_STAGE_INDEX`] — a file staging a part at `u32::MAX` spans
/// ~4 G empty indices, and iterating the span would OOM `validate`.
/// Gaps past the cap fold into one
/// [`SessionWarning::EmptyStagesOmitted`] summary.
const MAX_EMPTY_STAGE_WARNINGS: usize = 8;

/// Highest legal stage index — the cap `set_stage`/`insert_stage`
/// enforce and `validate` re-checks. Stage indices were unbounded
/// `u32`s, which let `insert_stage`'s `saturating_add(1)` silently
/// merge two stages at `u32::MAX`; bounding the index space makes
/// saturation unreachable through the ops (999 stages is far past
/// any real staging scheme). Crafted files can still carry larger
/// indices — `validate` flags them as [`SessionError::InvalidStage`]
/// rather than trusting the op-level cap.
pub const MAX_STAGE_INDEX: u32 = 999;

/// Which attach node of the parent mates which attach node of the
/// child — the §3.1 `(part_node, node_id)` pair a craft file
/// references. Stored on the child; `None` on the root. Also the
/// placement request for [`CraftSession::add_part`]/[`CraftSession::reattach`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachEdge {
    /// The node this part hangs off.
    pub parent: PartNodeId,
    /// `AttachNode::id` on the parent's def.
    pub parent_node: String,
    /// `AttachNode::id` on this part's def.
    pub child_node: String,
}

impl AttachEdge {
    /// `AttachEdge::new(parent, "bottom", "top")`.
    pub fn new(parent: PartNodeId, parent_node: &str, child_node: &str) -> Self {
        Self {
            parent,
            parent_node: parent_node.to_string(),
            child_node: child_node.to_string(),
        }
    }
}

/// One placed part — the session's node record. Fields are private so
/// the edge/transform/stage invariants the ops uphold cannot be
/// bypassed; reads go through the accessors.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionNode {
    built: BuiltPart,
    /// `None` iff this node is the root.
    edge: Option<AttachEdge>,
    /// Frame in the parent's frame (the graph frame for the root).
    transform: PartTransform,
    /// Stage index — higher fires earlier.
    stage: u32,
}

impl SessionNode {
    /// The §3.1 manufacture record (def id, scale, variant, frozen
    /// upgrades).
    pub fn built(&self) -> &BuiltPart {
        &self.built
    }

    /// The attachment edge to the parent — `None` on the root.
    pub fn edge(&self) -> Option<&AttachEdge> {
        self.edge.as_ref()
    }

    /// The node's transform in its parent's frame.
    pub fn transform(&self) -> PartTransform {
        self.transform
    }

    /// The node's stage assignment.
    pub fn stage(&self) -> u32 {
        self.stage
    }
}

/// The editor's working craft — see the module doc for invariants and
/// the file format. `Default` is an empty unnamed session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CraftSession {
    /// Display name for the save slot — payload, not identity.
    name: String,
    /// The root node — `None` iff `nodes` is empty.
    root: Option<PartNodeId>,
    /// Ascending-id arena (`BTreeMap`, §2.2). Iteration is the
    /// canonical order `to_part_graph` and the codec use.
    nodes: BTreeMap<PartNodeId, SessionNode>,
    /// Monotonic allocator — serialized, never decreases, ids never
    /// reused (M3's stable-`PartId` requirement).
    next_id: u64,
}

/// Why a session op was refused — typed, never a panic (§2.13). These
/// are *editor* failures: the session stays consistent, the op just
/// did not happen. Nothing here reaches the sim, so no `SimError`
/// mapping exists — the UI surfaces the message.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionError {
    /// `to_part_graph`/commit on a session with no parts.
    EmptySession,
    /// The named node is not in the session.
    UnknownNode {
        /// The id that missed.
        id: PartNodeId,
    },
    /// `add_root` on a session that already has a root.
    AlreadyRooted {
        /// The existing root.
        root: PartNodeId,
    },
    /// No `PartDef` in the catalog for a `built.part_id`. `node` is
    /// `None` when the failing record was never placed (add-time) and
    /// `Some` when [`CraftSession::validate`] names a placed node.
    UnknownPartDef {
        /// The placed node carrying the bad id, when there is one.
        node: Option<PartNodeId>,
        /// The unresolved def id.
        part_id: PartId,
    },
    /// An attach binding names a node id the def does not carry.
    /// `which` is `"parent"` or `"child"` — which side of the edge.
    UnknownAttachNode {
        /// The child-side node (the edge lives on it). On `add_part`
        /// failure this is the id the node *would have* received — the
        /// allocator is not advanced on error, so it remains free.
        node: PartNodeId,
        /// `"parent"` or `"child"`.
        which: &'static str,
        /// The missing `AttachNode::id`.
        node_id: String,
    },
    /// The snap check failed — mismatched size classes or a direction
    /// that cannot orient anything. `detail` names which.
    IncompatibleAttach {
        /// The parent end of the refused edge.
        parent: PartNodeId,
        /// The part being attached.
        child_part: PartId,
        /// Which constraint failed.
        detail: &'static str,
    },
    /// `reattach` would make `node` an ancestor of itself.
    WouldCycle {
        /// The subtree being moved.
        node: PartNodeId,
        /// The requested new parent (inside that subtree).
        parent: PartNodeId,
    },
    /// `reattach` on the root — the root has no parent edge to
    /// rewrite (its ancestor chain tops out at itself, so the generic
    /// check reports `WouldCycle`, which misreads as "the move loops
    /// the tree"). Translate it with [`Self::set_transform`] or
    /// re-anchor the tree with [`Self::reroot`].
    RootImmovable {
        /// The root node a reattach named.
        node: PartNodeId,
    },
    /// `add_part`/`reattach`/`remove_and_reparent` named an attach
    /// node another edge already mates — one stack node mates exactly
    /// one neighbour (a second edge would weld coincident,
    /// interpenetrating colliders). The VAB planners filter bound
    /// nodes up front; this is the model-level invariant every other
    /// writer (MCP `craft.build`, the design solver, a crafted file
    /// surfaced by `validate`) gets for free.
    AttachNodeOccupied {
        /// The placed node carrying the occupied attach node.
        node: PartNodeId,
        /// `"parent"` or `"child"` — which side of the edge.
        which: &'static str,
        /// The already-bound `AttachNode::id` (or `surf:` id).
        node_id: String,
    },
    /// `remove_and_reparent` refused — nothing was removed. `child`
    /// names the child that found no free size-class-matched node on
    /// the grandparent; `None` when `node` is the root, which has no
    /// parent to promote its children to.
    ReparentFailed {
        /// The node whose single-part removal was refused.
        node: PartNodeId,
        /// The child that could not promote, when relevant.
        child: Option<PartNodeId>,
    },
    /// `set_transform` got a non-finite or non-unit transform — the
    /// same gate the weld applies, one boundary earlier.
    InvalidTransform {
        /// The node.
        node: PartNodeId,
    },
    /// `set_scale`/`add_part` scale precheck — mirrors
    /// `resolve::scale_factors`' gate; the resolver stays authoritative
    /// at commit.
    InvalidScale {
        /// The node (`UnknownNode` would have fired first).
        node: PartNodeId,
        /// The rejected factor.
        scale: f64,
        /// Which constraint failed.
        rule: &'static str,
    },
    /// The def carries no such variant.
    UnknownVariant {
        /// The node.
        node: PartNodeId,
        /// The missing variant id.
        variant: VariantId,
    },
    /// A frozen `upgrade_levels_at_build` entry the resolver would
    /// refuse at commit — `check_built`'s third arm (after scale and
    /// variant): the [`StatPath`] must resolve on the def
    /// (`StatPath::base_value`) and the composed multiplier must be
    /// finite and `> 0`. Without the gate a bad `TrackLevels` would
    /// serialize into the craft file and fail later at
    /// `resolve::fold_multipliers`.
    InvalidUpgrade {
        /// The node carrying the record.
        node: PartNodeId,
        /// The offending entry's stat path.
        stat: StatPath,
        /// Which constraint failed.
        detail: &'static str,
    },
    /// A stage index above [`MAX_STAGE_INDEX`] — from `set_stage`,
    /// `insert_stage`, or a crafted file surfaced by `validate`.
    /// Stage indices are bounded so a saturating add can never
    /// silently merge two stages.
    InvalidStage {
        /// The node being restaged — `None` for `insert_stage`, which
        /// names no node.
        node: Option<PartNodeId>,
        /// The offending index.
        stage: u32,
        /// Which constraint failed.
        detail: &'static str,
    },
    /// `next_id` hit `u64::MAX` — ids are never reused, so the session
    /// refuses rather than collide. Unreachable in practice; typed so
    /// the allocator has no wraparound path.
    IdSpaceExhausted,
    /// The `effective_index` space (`u32`, per [`PartNode`]) overflowed
    /// in `to_part_graph` — the craft exceeds 4 G parts. Unreachable;
    /// typed per §2.13 rather than silently truncated.
    IndexOverflow,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySession => write!(f, "session: craft is empty"),
            Self::UnknownNode { id } => write!(f, "session: no node {id:?}"),
            Self::AlreadyRooted { root } => {
                write!(f, "session: craft already rooted at {root:?}")
            }
            Self::UnknownPartDef { node, part_id } => match node {
                Some(n) => write!(f, "session: node {n:?} names unknown def {part_id}"),
                None => write!(f, "session: unknown def {part_id}"),
            },
            Self::UnknownAttachNode {
                node,
                which,
                node_id,
            } => write!(
                f,
                "session: {which} attach node {node_id:?} missing (edge child {node:?})"
            ),
            Self::IncompatibleAttach {
                parent,
                child_part,
                detail,
            } => write!(
                f,
                "session: cannot attach {child_part} under {parent:?}: {detail}"
            ),
            Self::WouldCycle { node, parent } => write!(
                f,
                "session: attaching {node:?} under {parent:?} would cycle the tree"
            ),
            Self::RootImmovable { node } => write!(
                f,
                "session: node {node:?} is the craft root — it has no parent edge \
                 to rewrite (set_transform moves it, reroot re-anchors the tree)"
            ),
            Self::AttachNodeOccupied {
                node,
                which,
                node_id,
            } => write!(
                f,
                "session: {which} attach node {node_id:?} on {node:?} is already \
                 bound — one attach node mates exactly one neighbour"
            ),
            Self::ReparentFailed { node, child } => match child {
                Some(c) => write!(
                    f,
                    "session: removing {node:?} alone would leave child {c:?} \
                     without a free size-class-matched node on the grandparent"
                ),
                None => write!(
                    f,
                    "session: the root {node:?} has no parent — its children \
                     cannot promote (remove the subtree or reroot first)"
                ),
            },
            Self::InvalidTransform { node } => {
                write!(f, "session: invalid transform on node {node:?}")
            }
            Self::InvalidScale { node, scale, rule } => {
                write!(f, "session: node {node:?} scale {scale}: {rule}")
            }
            Self::UnknownVariant { node, variant } => {
                write!(f, "session: node {node:?} has no variant {variant}")
            }
            Self::InvalidUpgrade { node, stat, detail } => write!(
                f,
                "session: node {node:?} upgrade on {}: {detail}",
                stat.name()
            ),
            Self::InvalidStage {
                node,
                stage,
                detail,
            } => match node {
                Some(n) => write!(f, "session: node {n:?} stage {stage}: {detail}"),
                None => write!(f, "session: stage {stage}: {detail}"),
            },
            Self::IdSpaceExhausted => write!(f, "session: part id space exhausted"),
            Self::IndexOverflow => {
                write!(f, "session: node count exceeds the u32 weld index space")
            }
        }
    }
}

impl std::error::Error for SessionError {}

impl fmt::Display for SessionWarning {
    /// One line per warning — the VAB's validation panel renders these
    /// verbatim.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyStage { stage } => {
                write!(f, "stage {stage} holds no parts — a wasted activation")
            }
            Self::EmptyStagesOmitted { count } => {
                write!(f, "{count} further empty stages not listed")
            }
            Self::EngineWithoutPropellant {
                node,
                stage,
                missing,
            } => {
                let names: Vec<&str> = missing.iter().map(|r| r.as_str()).collect();
                write!(
                    f,
                    "engine {node:?} (stage {stage}) has no {} in its post-separation supply",
                    names.join(", ")
                )
            }
            Self::DecouplerSeversNothing { node, stage } => {
                write!(f, "decoupler {node:?} (stage {stage}) severs nothing")
            }
            Self::CrossStagePropellantDrain {
                node,
                stage,
                resources,
            } => {
                let names: Vec<&str> = resources.iter().map(|r| r.as_str()).collect();
                write!(
                    f,
                    "engine {node:?} (stage {stage}) can starve at separation — the pooled \
                     feed drains its {} before fuel that drops with the earlier stage",
                    names.join(", ")
                )
            }
            Self::AttachDiameterMismatch { child } => {
                write!(f, "node {child:?}'s attach sizes disagree after scaling")
            }
            Self::AttachNodeOccupied { node, node_id } => write!(
                f,
                "attach node {node_id:?} on node {node:?} is bound by two edges"
            ),
        }
    }
}

/// A `validate` warning — editor surfacing, never launch-blocking (see
/// the module doc's staging section).
#[derive(Clone, Debug, PartialEq)]
pub enum SessionWarning {
    /// A stage index inside the firing sequence holds no parts — a
    /// wasted activation in flight. At most
    /// [`MAX_EMPTY_STAGE_WARNINGS`] of these are emitted per
    /// `validate`; further gaps collapse into
    /// [`Self::EmptyStagesOmitted`].
    EmptyStage {
        /// The unpopulated index.
        stage: u32,
    },
    /// The populated-stage span holds more empty indices than the
    /// per-stage cap reports — `count` is how many gaps were *not*
    /// individually listed. Exists because stage indices are unbounded
    /// `u32`s (a crafted file can stage a part at `u32::MAX`); the cap
    /// keeps `validate` output bounded.
    EmptyStagesOmitted {
        /// Unreported empty-stage count.
        count: u64,
    },
    /// An engine staged at `stage` has propellant resources no tank in
    /// its post-separation supply region carries (module doc).
    EngineWithoutPropellant {
        /// The engine node.
        node: PartNodeId,
        /// Its stage.
        stage: u32,
        /// The uncovered `Engine.propellants` resources, authored order.
        missing: Vec<ResourceId>,
    },
    /// A decoupler with no children severs nothing on firing.
    DecouplerSeversNothing {
        /// The decoupler node.
        node: PartNodeId,
        /// Its stage.
        stage: u32,
    },
    /// The pooled feed does not respect stage boundaries: it drains
    /// every attached tank ascending `PartNodeId`, so where the last
    /// propellant sits is fixed by drain order, not by the decoupler
    /// layout. When the last tank in that order holding a resource
    /// this engine burns sits *outside* its post-separation supply
    /// region — in a subtree an earlier stage drops — a burn can
    /// empty the region while fuel the departing half still carries
    /// is jettisoned: the starved stage row the readout reports
    /// honestly and [`Self::EngineWithoutPropellant`] cannot see (it
    /// checks tank *capability* in the region, never cross-stage
    /// depletion — C13-F2). A real crossfeed/feed-priority model is
    /// 7.x feature work.
    CrossStagePropellantDrain {
        /// The engine whose post-separation supply is exposed.
        node: PartNodeId,
        /// Its stage.
        stage: u32,
        /// The propellant resources that can strand in departing
        /// tanks, authored order.
        resources: Vec<ResourceId>,
    },
    /// A stored edge's scaled size classes disagree — possible after a
    /// `set_scale` post-attach. Permitted (the weld only needs
    /// topology + transform) but flagged: the §3.1 size-class policy
    /// says it should not have snapped.
    AttachDiameterMismatch {
        /// The child-side node carrying the edge.
        child: PartNodeId,
    },
    /// Two edges bind the same attach node on `node` — a crafted file
    /// can bypass the op-level one-neighbour-per-node rule
    /// ([`SessionError::AttachNodeOccupied`]); the craft welds
    /// (coincident, interpenetrating colliders) but the flag is the
    /// honest read that its geometry is corrupt.
    AttachNodeOccupied {
        /// The placed node carrying the over-bound attach node.
        node: PartNodeId,
        /// The shared `AttachNode::id` (or `surf:` id).
        node_id: String,
    },
}

/// `validate` output: `errors` make the craft unlaunchable, `warnings`
/// are advisory. Construction is the session's business — only
/// `is_launchable` and the lists are public API surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Validation {
    /// Hard failures (unknown defs, bindings to missing attach nodes).
    pub errors: Vec<SessionError>,
    /// Advisory findings (staging, size classes).
    pub warnings: Vec<SessionWarning>,
}

impl Validation {
    /// `errors` is empty — the craft may commit to a `PartGraph`.
    pub fn is_launchable(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Why a craft file could not load (or encode) — the typed boundary
/// for §7.2-class persisted data. Maps to `SimError::SaveBlocking`: a
/// corrupt craft file refuses to load, it never half-loads (§7.7's
/// "refused with a message").
#[derive(Clone, Debug, PartialEq)]
pub enum CraftFileError {
    /// Fewer bytes than the magic, or a different magic.
    BadMagic,
    /// The bincode body did not decode.
    Decode(String),
    /// Bytes remain after the decoded body — a canonical file has none.
    Trailing {
        /// Undecoded byte count.
        extra: usize,
    },
    /// `schema_version` is not [`CRAFT_SCHEMA_VERSION`] — refused, not
    /// migrated (§7.7 pre-1.0 policy).
    UnsupportedSchemaVersion {
        /// What the file declared.
        found: u32,
        /// What this build reads.
        expected: u32,
    },
    /// Two node records carry the same id.
    DuplicateNode {
        /// The repeated id.
        id: u64,
    },
    /// `nodes` is non-empty but `root` is `None` or names no node.
    MissingRoot,
    /// More than one node is parentless — a forest, not a tree.
    MultipleRoots {
        /// A second parentless node (lowest id).
        second: u64,
    },
    /// A node's `edge.parent` is not in the file.
    DanglingParent {
        /// The node whose parent dangles.
        node: u64,
        /// The missing parent id.
        parent: u64,
    },
    /// Parent links never reach the root — a cycle (dangling parents
    /// are caught by [`Self::DanglingParent`] first).
    Cycle {
        /// One node on/unreachable past the cycle (lowest id).
        node: u64,
    },
    /// `next_id` is not greater than every live id — reloading would
    /// reissue a live `PartNodeId`, breaking stability.
    NextIdCollision {
        /// The file's counter.
        next_id: u64,
        /// The largest live id.
        max_id: u64,
    },
    /// A stored transform fails [`PartTransform::is_valid`].
    BadTransform {
        /// The node carrying it.
        node: u64,
    },
    /// The root record carries an edge, or a non-root carries none —
    /// the root is parentless by definition.
    BadRootEdge {
        /// The offending node.
        node: u64,
    },
    /// Encoding failed (unreachable for this schema — `bincode`
    /// returns `Result`, so the failure is typed rather than unwrapped).
    Encode(String),
}

impl CraftFileError {
    /// Static reason text for the `SimError` mapping — logs and bug
    /// bundles only, never hashed (§2.13).
    pub fn reason(&self) -> &'static str {
        match self {
            Self::BadMagic => "craft file: bad magic",
            Self::Decode(_) => "craft file: body did not decode",
            Self::Trailing { .. } => "craft file: trailing bytes",
            Self::UnsupportedSchemaVersion { .. } => "craft file: unsupported schema version",
            Self::DuplicateNode { .. } => "craft file: duplicate node id",
            Self::MissingRoot => "craft file: missing root",
            Self::MultipleRoots { .. } => "craft file: multiple roots",
            Self::DanglingParent { .. } => "craft file: dangling parent id",
            Self::Cycle { .. } => "craft file: parent cycle",
            Self::NextIdCollision { .. } => "craft file: next_id collides with a live id",
            Self::BadTransform { .. } => "craft file: invalid transform",
            Self::BadRootEdge { .. } => "craft file: root edge inconsistency",
            Self::Encode(_) => "craft file: encode failed",
        }
    }
}

impl fmt::Display for CraftFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(msg) | Self::Encode(msg) => write!(f, "{}: {msg}", self.reason()),
            Self::UnsupportedSchemaVersion { found, expected } => write!(
                f,
                "{}: file is v{found}, this build reads v{expected}",
                self.reason()
            ),
            other => write!(f, "{}", other.reason()),
        }
    }
}

impl std::error::Error for CraftFileError {}

impl From<CraftFileError> for SimError {
    fn from(e: CraftFileError) -> Self {
        SimError::SaveBlocking(e.reason())
    }
}

/// The serialized body — private, `schema_version` first per §3.1.
/// Kept separate from `CraftSession` so there is exactly one
/// serialization truth and the in-memory invariants (map, allocator)
/// stay enforced on load rather than trusted from disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CraftFile {
    schema_version: u32,
    name: String,
    next_id: u64,
    root: Option<u64>,
    nodes: Vec<CraftFileNode>,
}

/// One serialized node — the flat record; `edge` absent iff root.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CraftFileNode {
    id: u64,
    edge: Option<AttachEdge>,
    transform: PartTransform,
    stage: u32,
    built: BuiltPart,
}

/// Look up `part_id` in the loaded set — the editor's catalog is small
/// (one file per part), so a linear scan is the honest choice over a
/// maintained index.
fn find_def<'a>(defs: &'a LoadedParts, part_id: &PartId) -> Option<&'a PartDef> {
    defs.parts.iter().map(|p| &p.def).find(|d| d.id == *part_id)
}

/// The synthesized surface-mount node convention. A def authors its
/// axial snap points; a *radial* (side) mount has no authored node on
/// the parent to name — the mount point is wherever the player put it.
/// The session records such a mount as an `AttachEdge` node id with
/// this prefix, carrying the full node inline:
/// `surf:<x>:<y>:<z>:<nx>:<ny>:<nz>:<d>` — seven f64 fields in
/// shortest round-trip form: the surface point in part-local frame,
/// the outward normal, and the size class. Decoded by
/// [`scaled_attach_node`]/[`decode_surface_node`]; the values stored
/// are already at the node's built scale+stretch, so decode applies
/// no further scaling.
pub const SURFACE_NODE_PREFIX: &str = "surf:";

/// Encode a synthetic surface-mount node as an [`AttachEdge`] node id.
/// `position`/`direction` are DEF-LOCAL — the mounting part's scale-1
/// frame (the caller resolves them); `diameter_m` is the def-local
/// size class the mate must match. Storing def-local values is what
/// lets a resize move surface mounts: [`scaled_attach_node`] applies
/// the live scale/stretch at every use, so the frozen id never pins
/// an old size.
pub fn surface_node_id(position: DVec3, direction: DVec3, diameter_m: f64) -> String {
    let mut s = String::with_capacity(96);
    s.push_str(SURFACE_NODE_PREFIX);
    for (i, v) in [
        position.x,
        position.y,
        position.z,
        direction.x,
        direction.y,
        direction.z,
        diameter_m,
    ]
    .iter()
    .enumerate()
    {
        if i > 0 {
            s.push(':');
        }
        s.push_str(&format!("{v}"));
    }
    s
}

/// Decode a [`SURFACE_NODE_PREFIX`] id into an [`AttachNode`]. `None`
/// when the id is not a surface id or any field fails to parse —
/// malformed surface ids resolve like any other unknown node id
/// (typed `UnknownAttachNode` at attach/validate time).
pub fn decode_surface_node(node_id: &str) -> Option<AttachNode> {
    let body = node_id.strip_prefix(SURFACE_NODE_PREFIX)?;
    let mut fields = [0.0_f64; 7];
    let mut it = body.split(':');
    for f in fields.iter_mut() {
        *f = it.next()?.parse::<f64>().ok()?;
        if !f.is_finite() {
            return None;
        }
    }
    if it.next().is_some() || fields[6] <= 0.0 {
        return None;
    }
    Some(AttachNode {
        id: node_id.to_string(),
        position_m: DVec3::new(fields[0], fields[1], fields[2]),
        direction: DVec3::new(fields[3], fields[4], fields[5]),
        diameter_m: fields[6],
    })
}

/// A def's attach node resolved to the node's built scale — the same
/// scaling the resolver applies (`position·(s, s, s·t)`, `diameter·s`,
/// direction unchanged; resolve.rs `resolve_envelopes`). `node_id` may
/// also be a [`SURFACE_NODE_PREFIX`] synthetic surface-mount id —
/// those decode inline and scale identically: the id carries
/// DEF-LOCAL coordinates (the mounting part's scale-1 geometry), so a
/// resize re-derives the snap point instead of stranding it at the
/// mount-time size. Kept local so snapping does not need a full
/// resolve (no `ResourceFile`/qual inputs on the placement path).
pub fn scaled_attach_node(
    def: &PartDef,
    node_id: &str,
    scale: f64,
    stretch: f64,
) -> Option<AttachNode> {
    if let Some(n) = decode_surface_node(node_id) {
        return Some(AttachNode {
            id: n.id,
            position_m: DVec3::new(
                n.position_m.x * scale,
                n.position_m.y * scale,
                n.position_m.z * scale * stretch,
            ),
            direction: n.direction,
            diameter_m: n.diameter_m * scale,
        });
    }
    def.attach_nodes
        .iter()
        .find(|n| n.id == node_id)
        .map(|n| AttachNode {
            id: n.id.clone(),
            position_m: DVec3::new(
                n.position_m.x * scale,
                n.position_m.y * scale,
                n.position_m.z * scale * stretch,
            ),
            direction: n.direction,
            diameter_m: n.diameter_m * scale,
        })
}

/// A deterministic unit vector orthogonal to `v` (non-zero, finite):
/// `v` crossed with its least-aligned basis axis, which maximizes
/// `‖v × b‖` and can never degenerate.
fn orthogonal_axis(v: DVec3) -> DVec3 {
    let basis = if v.x.abs() <= v.y.abs() && v.x.abs() <= v.z.abs() {
        DVec3::X
    } else if v.y.abs() <= v.z.abs() {
        DVec3::Y
    } else {
        DVec3::Z
    };
    v.cross(basis)
}

/// The minimal arc taking unit direction `from` to unit direction
/// `to` — identity when parallel, a deterministic half-turn about
/// [`orthogonal_axis`] when antiparallel, `libm::acos` otherwise (the
/// session's only transcendental). Inputs are normalized here so
/// authored directions inside the validator's `‖d‖ = 1 ± 1e-6` band
/// produce an exact-unit result.
fn rotation_arc(from: DVec3, to: DVec3) -> DQuat {
    let f = from / from.norm();
    let t = to / to.norm();
    let d = f.dot(t);
    if d > 1.0 - ARC_PARALLEL_EPS {
        return DQuat::IDENTITY;
    }
    if d < -1.0 + ARC_PARALLEL_EPS {
        return DQuat::from_axis_angle(orthogonal_axis(f), core::f64::consts::PI);
    }
    DQuat::from_axis_angle(f.cross(t), libm::acos(d.clamp(-1.0, 1.0)))
}

/// The snap transform placing a child under its parent through the
/// named attach nodes: `rotation·d_c = −d_p` (outward normals oppose)
/// and `offset + rotation·p_c = p_p` (nodes coincide in the parent
/// frame). Both nodes are the *scaled* defs' — see
/// [`scaled_attach_node`].
pub fn snap_transform(parent_node: &AttachNode, child_node: &AttachNode) -> PartTransform {
    let rotation = rotation_arc(child_node.direction, -parent_node.direction);
    let offset = parent_node.position_m - rotation.rotate(child_node.position_m);
    PartTransform { offset, rotation }.normalized()
}

/// The shared `built`-vs-`def` sanity gate for the mutating ops:
/// scale rule and variant membership. The resolver re-checks
/// everything at commit — this is the editor-side boundary where a bad
/// record is refused before it becomes (or mutates) a node. `node` is
/// the id errors report: the live node for `set_*`, the pending
/// allocation for `add_*` (on error the allocator is not advanced, so
/// the id remains free — the variant doc says "would have received").
fn check_built(def: &PartDef, built: &BuiltPart, node: PartNodeId) -> Result<(), SessionError> {
    let bad_scale = |rule: &'static str| SessionError::InvalidScale {
        node,
        scale: built.scale,
        rule,
    };
    if !built.scale.is_finite() || built.scale <= 0.0 {
        return Err(bad_scale("session: scale must be finite and positive"));
    }
    match &def.scale {
        ScaleRule::None => {
            if built.scale != 1.0 {
                return Err(bad_scale(
                    "session: part does not scale (scale must be 1.0)",
                ));
            }
        }
        ScaleRule::Discrete(set) => {
            if !set.contains(&built.scale) {
                return Err(bad_scale("session: scale is not one of the authored sizes"));
            }
        }
        ScaleRule::Continuous { min, max, .. } => {
            if built.scale < *min || built.scale > *max {
                return Err(bad_scale("session: scale outside the authored range"));
            }
        }
    }
    // The stack-axis stretch factor rides the same rule's bounds (the
    // resolver's `scale_factors` enforces the identical range at
    // commit): `None` pins 1.0, `Discrete`/`Continuous` bound to the
    // authored min/max.
    if !built.stretch.is_finite() || built.stretch <= 0.0 {
        return Err(bad_scale("session: stretch must be finite and positive"));
    }
    let (t_lo, t_hi) = match &def.scale {
        ScaleRule::None => (1.0, 1.0),
        ScaleRule::Discrete(set) => (
            set.first().copied().unwrap_or(1.0),
            set.last().copied().unwrap_or(1.0),
        ),
        ScaleRule::Continuous { min, max, .. } => (*min, *max),
    };
    if built.stretch < t_lo || built.stretch > t_hi {
        return Err(bad_scale("session: stretch outside the authored range"));
    }
    if !def.variants.iter().any(|v| v.id == built.variant) {
        return Err(SessionError::UnknownVariant {
            node,
            variant: built.variant.clone(),
        });
    }
    // Third arm: the frozen upgrade entries. `fold_multipliers`
    // enforces both rules at commit — the stat path must resolve on
    // this def (`StatPath::base_value`) and the composed multiplier
    // must be positive and finite — but an ungated record would
    // serialize into the craft file and only fail on launch, so the
    // boundary check runs them here too.
    for (path, tl) in &built.upgrade_levels_at_build {
        if path.base_value(def).is_none() {
            return Err(SessionError::InvalidUpgrade {
                node,
                stat: *path,
                detail: "session: upgrade stat path does not resolve on this part",
            });
        }
        if !tl.multiplier.is_finite() || tl.multiplier <= 0.0 {
            return Err(SessionError::InvalidUpgrade {
                node,
                stat: *path,
                detail: "session: upgrade multiplier is not positive and finite",
            });
        }
    }
    Ok(())
}

/// Whether scaled diameters `a`/`b` match within
/// [`ATTACH_DIAMETER_REL_TOLERANCE`]. `a = b = 0` passes (`0 ≤ 0`) —
/// unreachable in practice: valid defs carry positive attach diameters
/// and the callers gate non-finite values first.
fn diameters_match(a: f64, b: f64) -> bool {
    (a - b).abs() <= ATTACH_DIAMETER_REL_TOLERANCE * a.max(b)
}

/// Report the empty-stage gaps inside `populated` (ascending). The gap
/// *count* is computed arithmetically and the individual reporting
/// walks only as far as [`MAX_EMPTY_STAGE_WARNINGS`] — a crafted file
/// can stage a part at `u32::MAX`, where a naïve `lo..=hi` scan emits
/// ~4 G warnings and OOMs `validate`. Remaining gaps fold into one
/// [`SessionWarning::EmptyStagesOmitted`].
fn report_stage_gaps(populated: &[u32], out: &mut Validation) {
    let (Some(&lo), Some(&hi)) = (populated.first(), populated.last()) else {
        return;
    };
    let total_empty = u64::from(hi) - u64::from(lo) + 1 - populated.len() as u64;
    let mut reported = 0usize;
    'gaps: for pair in populated.windows(2) {
        for stage in pair[0] + 1..pair[1] {
            if reported == MAX_EMPTY_STAGE_WARNINGS {
                break 'gaps;
            }
            out.warnings.push(SessionWarning::EmptyStage { stage });
            reported += 1;
        }
    }
    let omitted = total_empty - reported as u64;
    if omitted > 0 {
        out.warnings
            .push(SessionWarning::EmptyStagesOmitted { count: omitted });
    }
}

/// The parent → children index in one O(n) pass — the shared shape
/// `body_transforms`, `validate_occupancy`, `from_file`, the staging
/// checks, and `subtree_ids` all need (C15-F5). Iterate `nodes` (a
/// `BTreeMap`) in ascending-id order so each child list is already
/// sorted — the same order `children_ids`'s per-node scan yields.
/// `children_ids` stays the one-off accessor; callers that visit more
/// than one node's children build this once.
fn children_map(
    nodes: &BTreeMap<PartNodeId, SessionNode>,
) -> BTreeMap<PartNodeId, Vec<PartNodeId>> {
    let mut children: BTreeMap<PartNodeId, Vec<PartNodeId>> = BTreeMap::new();
    for (id, n) in nodes {
        if let Some(e) = &n.edge {
            children.entry(e.parent).or_default().push(*id);
        }
    }
    children
}

/// `true` when `ancestor` is `id` or appears on `id`'s parent chain —
/// the `reattach` cycle check. Terminates because the maintained
/// invariant is a tree.
fn is_ancestor_of(
    nodes: &BTreeMap<PartNodeId, SessionNode>,
    ancestor: PartNodeId,
    id: PartNodeId,
) -> bool {
    let mut cur = Some(id);
    while let Some(n) = cur {
        if n == ancestor {
            return true;
        }
        cur = nodes
            .get(&n)
            .and_then(|s| s.edge.as_ref())
            .map(|e| e.parent);
    }
    false
}

impl CraftSession {
    /// An empty, unnamed session.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty named session.
    pub fn with_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// The craft's display name (file payload, not identity).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Rename the craft.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Placed-part count.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// No parts placed.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The root node id — `None` while the session is empty.
    pub fn root(&self) -> Option<PartNodeId> {
        self.root
    }

    /// Read one node.
    pub fn node(&self, id: PartNodeId) -> Option<&SessionNode> {
        self.nodes.get(&id)
    }

    /// Ascending-id iteration — the canonical order.
    pub fn iter(&self) -> impl Iterator<Item = (PartNodeId, &SessionNode)> {
        self.nodes.iter().map(|(id, n)| (*id, n))
    }

    /// Whether `id` is placed.
    pub fn contains(&self, id: PartNodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// The node's direct children, ascending id — an O(n) scan per
    /// call, fine for one-off lookups. Anything walking more than one
    /// node's children builds [`children_map`] once instead (C15-F5).
    pub fn children_ids(&self, id: PartNodeId) -> Vec<PartNodeId> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.edge.as_ref().is_some_and(|e| e.parent == id))
            .map(|(k, _)| *k)
            .collect()
    }

    /// The attach-node ids on `id`'s def an edge already binds: the
    /// node `id` itself hangs from (its edge's `child_node`) plus the
    /// parent-side node of every child edge. A stack node mates
    /// exactly one neighbour — [`Self::add_part`]/[`Self::reattach`]
    /// enforce the rule ([`SessionError::AttachNodeOccupied`]) and the
    /// VAB's planners additionally filter this set up front so the
    /// ghost previews the same verdict the commit reaches.
    pub fn bound_attach_nodes(&self, id: PartNodeId) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Some(e) = self.nodes.get(&id).and_then(|n| n.edge.as_ref()) {
            out.insert(e.child_node.clone());
        }
        for child in self.children_ids(id) {
            if let Some(e) = self.nodes.get(&child).and_then(|n| n.edge.as_ref()) {
                out.insert(e.parent_node.clone());
            }
        }
        out
    }

    /// `id` plus its whole descendant subtree, ascending id — the set
    /// `remove_part` takes out and the UI's subtree selection mirrors.
    pub fn subtree_ids(&self, id: PartNodeId) -> Vec<PartNodeId> {
        let children = children_map(&self.nodes);
        let mut out = BTreeSet::new();
        let mut frontier = vec![id];
        while let Some(n) = frontier.pop() {
            if out.insert(n)
                && let Some(kids) = children.get(&n)
            {
                frontier.extend(kids.iter().copied());
            }
        }
        out.into_iter().collect()
    }

    /// Body (graph) frame transform per node, ascending id — the fold
    /// of parent-local [`PartTransform`]s down the tree, i.e.
    /// `p_body = t(parent)·…·t(node)` applied to part-local points.
    /// The VAB's craft view and GPU-pick boxes need body-frame poses;
    /// the seam lives here because the tree walk is session-internal
    /// (`children_ids` is O(n) per call — the index below is one pass).
    /// The root's own stored transform composes on top of identity,
    /// matching `to_part_graph`'s handling of the root.
    pub fn body_transforms(&self) -> BTreeMap<PartNodeId, PartTransform> {
        let mut out = BTreeMap::new();
        let Some(root) = self.root else {
            return out;
        };
        let children = children_map(&self.nodes);
        let mut stack = vec![(root, PartTransform::IDENTITY)];
        while let Some((id, up)) = stack.pop() {
            let Some(n) = self.nodes.get(&id) else {
                continue;
            };
            let t = up.compose(n.transform);
            out.insert(id, t);
            if let Some(kids) = children.get(&id) {
                for c in kids {
                    stack.push((*c, t));
                }
            }
        }
        out
    }

    /// Allocate the next node id. The counter only advances on a
    /// successful insert — a refused op consumes nothing.
    fn alloc(&self) -> Result<PartNodeId, SessionError> {
        if self.next_id == u64::MAX {
            return Err(SessionError::IdSpaceExhausted);
        }
        Ok(PartNodeId(self.next_id))
    }

    /// Place the root part — the first node of a session. `built` is
    /// sanity-gated against its def (scale rule, variant) at this
    /// boundary; the resolver remains authoritative at commit.
    /// New parts land in stage `0` (the last-fired floor); the editor
    /// moves them with [`Self::set_stage`].
    pub fn add_root(
        &mut self,
        defs: &LoadedParts,
        built: BuiltPart,
    ) -> Result<PartNodeId, SessionError> {
        if let Some(root) = self.root {
            return Err(SessionError::AlreadyRooted { root });
        }
        let def = find_def(defs, &built.part_id).ok_or_else(|| SessionError::UnknownPartDef {
            node: None,
            part_id: built.part_id.clone(),
        })?;
        let id = self.alloc()?;
        check_built(def, &built, id)?;
        let node = SessionNode {
            built,
            edge: None,
            transform: PartTransform::IDENTITY,
            stage: 0,
        };
        self.next_id += 1;
        self.nodes.insert(id, node);
        self.root = Some(id);
        Ok(id)
    }

    /// Snap a new part under `edge.parent` through the named attach
    /// nodes. Computes the snap transform per [`snap_transform`] —
    /// the size classes must match ([`ATTACH_DIAMETER_REL_TOLERANCE`])
    /// and both named nodes must exist on their defs. On any error the
    /// session is unchanged (the id counter included).
    pub fn add_part(
        &mut self,
        defs: &LoadedParts,
        built: BuiltPart,
        edge: AttachEdge,
    ) -> Result<PartNodeId, SessionError> {
        let id = self.alloc()?;
        let child_def =
            find_def(defs, &built.part_id).ok_or_else(|| SessionError::UnknownPartDef {
                node: None,
                part_id: built.part_id.clone(),
            })?;
        check_built(child_def, &built, id)?;
        self.check_parent_slot_free(&edge)?;
        let transform = self.compute_attach(defs, id, &built, &edge)?;
        let node = SessionNode {
            built,
            edge: Some(edge),
            transform,
            stage: 0,
        };
        self.next_id += 1;
        self.nodes.insert(id, node);
        Ok(id)
    }

    /// The shared add/reattach path: validate the edge ends against the
    /// defs and compute the child's transform under the parent. `built`
    /// is the child-side record (for the scaled child node); `child` is
    /// the node the error reports.
    fn compute_attach(
        &self,
        defs: &LoadedParts,
        child: PartNodeId,
        built: &BuiltPart,
        edge: &AttachEdge,
    ) -> Result<PartTransform, SessionError> {
        let parent_node_rec = self
            .nodes
            .get(&edge.parent)
            .ok_or(SessionError::UnknownNode { id: edge.parent })?;
        let parent_def = find_def(defs, &parent_node_rec.built.part_id).ok_or_else(|| {
            SessionError::UnknownPartDef {
                node: Some(edge.parent),
                part_id: parent_node_rec.built.part_id.clone(),
            }
        })?;
        let child_def =
            find_def(defs, &built.part_id).ok_or_else(|| SessionError::UnknownPartDef {
                node: Some(child),
                part_id: built.part_id.clone(),
            })?;
        let pn = scaled_attach_node(
            parent_def,
            &edge.parent_node,
            parent_node_rec.built.scale,
            parent_node_rec.built.stretch,
        )
        .ok_or_else(|| SessionError::UnknownAttachNode {
            node: child,
            which: "parent",
            node_id: edge.parent_node.clone(),
        })?;
        let cn = scaled_attach_node(child_def, &edge.child_node, built.scale, built.stretch)
            .ok_or_else(|| SessionError::UnknownAttachNode {
                node: child,
                which: "child",
                node_id: edge.child_node.clone(),
            })?;
        if !pn.direction.is_finite() || pn.direction.norm() < MIN_DIRECTION_NORM {
            return Err(SessionError::IncompatibleAttach {
                parent: edge.parent,
                child_part: built.part_id.clone(),
                detail: "session: parent attach direction is not finite/non-zero",
            });
        }
        if !cn.direction.is_finite() || cn.direction.norm() < MIN_DIRECTION_NORM {
            return Err(SessionError::IncompatibleAttach {
                parent: edge.parent,
                child_part: built.part_id.clone(),
                detail: "session: child attach direction is not finite/non-zero",
            });
        }
        // `position_m` gates (R14-F3): a non-finite node position
        // yields a NaN-offset transform from `snap_transform`, which
        // `to_bytes` would write verbatim and `from_file` refuse —
        // a session could save a file it cannot reload. Checked here,
        // the boundary where it can still be a typed error.
        if !pn.position_m.is_finite() {
            return Err(SessionError::IncompatibleAttach {
                parent: edge.parent,
                child_part: built.part_id.clone(),
                detail: "session: parent attach position is non-finite",
            });
        }
        if !cn.position_m.is_finite() {
            return Err(SessionError::IncompatibleAttach {
                parent: edge.parent,
                child_part: built.part_id.clone(),
                detail: "session: child attach position is non-finite",
            });
        }
        if !pn.diameter_m.is_finite()
            || !cn.diameter_m.is_finite()
            || !diameters_match(pn.diameter_m, cn.diameter_m)
        {
            return Err(SessionError::IncompatibleAttach {
                parent: edge.parent,
                child_part: built.part_id.clone(),
                detail: "session: attach-node size classes do not match",
            });
        }
        let transform = snap_transform(&pn, &cn);
        // Output gate: with the inputs above checked the snap is
        // finite, but the stored transform is what the file and the
        // weld consume — verify it against the same `is_valid` rule
        // `set_transform` enforces before any caller stores it.
        if !transform.is_valid() {
            return Err(SessionError::IncompatibleAttach {
                parent: edge.parent,
                child_part: built.part_id.clone(),
                detail: "session: snap produced an invalid transform",
            });
        }
        Ok(transform)
    }

    /// The parent-side half of the occupancy rule — one stack node
    /// mates exactly one neighbour, so an edge onto an already-bound
    /// parent node refuses. Shared verbatim by [`Self::add_part`] and
    /// [`Self::preview_attach`] so the preview oracle can never `Ok`
    /// where the commit would refuse (C15-F1). `reattach` runs the
    /// same rule with its own exemption — the slot the moving edge's
    /// old binding frees — so it keeps its dedicated site.
    fn check_parent_slot_free(&self, edge: &AttachEdge) -> Result<(), SessionError> {
        if self
            .bound_attach_nodes(edge.parent)
            .contains(&edge.parent_node)
        {
            return Err(SessionError::AttachNodeOccupied {
                node: edge.parent,
                which: "parent",
                node_id: edge.parent_node.clone(),
            });
        }
        Ok(())
    }

    /// The transform `add_part` would compute for `edge` — the same
    /// checks, no session mutation. The editor's ghost preview uses it
    /// so the drop lands exactly what was previewed.
    pub fn preview_attach(
        &self,
        defs: &LoadedParts,
        built: &BuiltPart,
        edge: &AttachEdge,
    ) -> Result<PartTransform, SessionError> {
        // Occupancy precedes `compute_attach` exactly as `add_part`
        // orders it — a doubly-rejecting edge reports the same error
        // the commit would.
        self.check_parent_slot_free(edge)?;
        self.compute_attach(defs, PartNodeId(self.next_id), built, edge)
    }

    /// Remove `id` and its whole subtree (the §2.4 editor rule for a
    /// rooted tree — a node cannot outlive its parent edge). Removing
    /// the root empties the session. Returns the removed ids,
    /// ascending. `next_id` does not move — ids are never reused.
    pub fn remove_part(&mut self, id: PartNodeId) -> Result<Vec<PartNodeId>, SessionError> {
        if !self.nodes.contains_key(&id) {
            return Err(SessionError::UnknownNode { id });
        }
        let removed = self.subtree_ids(id);
        for r in &removed {
            self.nodes.remove(r);
        }
        if self.root == Some(id) {
            self.root = None;
        }
        Ok(removed)
    }

    /// Remove `id` WITHOUT its subtree — the "delete this part only"
    /// complement to [`Self::remove_part`]: each direct child promotes
    /// onto `id`'s parent, re-snapped through a free
    /// size-class-matched attach node (the slot `id` vacated is tried
    /// first, then authored order). Atomic: when `id` is the root (no
    /// parent to promote to) or any child finds no mate on the
    /// grandparent, the op refuses and nothing is removed. Returns
    /// `[id]` — the single removed node.
    pub fn remove_and_reparent(
        &mut self,
        defs: &LoadedParts,
        id: PartNodeId,
    ) -> Result<Vec<PartNodeId>, SessionError> {
        let node = self
            .nodes
            .get(&id)
            .ok_or(SessionError::UnknownNode { id })?;
        let Some(edge) = node.edge.clone() else {
            return Err(SessionError::ReparentFailed {
                node: id,
                child: None,
            });
        };
        let children = self.children_ids(id);
        if children.is_empty() {
            // A leaf promotes nothing — identical to remove_part.
            return self.remove_part(id);
        }
        let parent_rec = self
            .nodes
            .get(&edge.parent)
            .ok_or(SessionError::UnknownNode { id: edge.parent })?;
        let parent_def = find_def(defs, &parent_rec.built.part_id).ok_or_else(|| {
            SessionError::UnknownPartDef {
                node: Some(edge.parent),
                part_id: parent_rec.built.part_id.clone(),
            }
        })?;
        // Plan every promotion before mutating: candidates are the
        // grandparent's free attach nodes, `id`'s own slot freed, in
        // vacated-slot-first then authored order.
        let mut bound = self.bound_attach_nodes(edge.parent);
        bound.remove(&edge.parent_node);
        let mut planned: Vec<(PartNodeId, AttachEdge, PartTransform)> =
            Vec::with_capacity(children.len());
        for c in children {
            let (c_built, c_child_node) = {
                let cn = self
                    .nodes
                    .get(&c)
                    .ok_or(SessionError::UnknownNode { id: c })?;
                let ce = cn.edge.clone().ok_or(SessionError::UnknownNode { id: c })?;
                (cn.built.clone(), ce.child_node)
            };
            let mut candidates: Vec<&str> = vec![edge.parent_node.as_str()];
            candidates.extend(
                parent_def
                    .attach_nodes
                    .iter()
                    .map(|n| n.id.as_str())
                    .filter(|nid| *nid != edge.parent_node),
            );
            let mut found = None;
            for pn_id in candidates {
                if bound.contains(pn_id) {
                    continue;
                }
                let trial = AttachEdge::new(edge.parent, pn_id, &c_child_node);
                if let Ok(t) = self.compute_attach(defs, c, &c_built, &trial) {
                    bound.insert(trial.parent_node.clone());
                    found = Some((trial, t));
                    break;
                }
            }
            let Some((trial, t)) = found else {
                return Err(SessionError::ReparentFailed {
                    node: id,
                    child: Some(c),
                });
            };
            planned.push((c, trial, t));
        }
        self.nodes.remove(&id);
        for (c, e, t) in planned {
            let n = self
                .nodes
                .get_mut(&c)
                .ok_or(SessionError::UnknownNode { id: c })?;
            n.edge = Some(e);
            n.transform = t;
        }
        Ok(vec![id])
    }

    /// Re-snap `id` (with its subtree) under a new parent — the "move"
    /// half of editor drag-and-drop. Rejects a move under the node's
    /// own subtree (`WouldCycle`); the root refuses outright
    /// (`RootImmovable` — it has no parent edge; translate it with
    /// [`Self::set_transform`], re-anchor with [`Self::reroot`]).
    /// An edge onto an already-mated attach node refuses with
    /// [`SessionError::AttachNodeOccupied`], except the slot the
    /// moving edge's own old binding frees.
    pub fn reattach(
        &mut self,
        defs: &LoadedParts,
        id: PartNodeId,
        edge: AttachEdge,
    ) -> Result<(), SessionError> {
        let node = self
            .nodes
            .get(&id)
            .ok_or(SessionError::UnknownNode { id })?;
        // The root tops every ancestor chain, so it always tripped
        // the cycle check — report the real reason instead (the
        // `WouldCycle` message misreads for what is "the root is the
        // anchor"). Free moves go through `set_transform`;
        // re-anchoring goes through `reroot`.
        if node.edge.is_none() {
            return Err(SessionError::RootImmovable { node: id });
        }
        if is_ancestor_of(&self.nodes, id, edge.parent) {
            return Err(SessionError::WouldCycle {
                node: id,
                parent: edge.parent,
            });
        }
        // Occupancy: the new edge must not double-bind a parent node
        // another edge already mates — except the slot the moving
        // edge's own old binding frees (re-attaching where it already
        // hangs is a valid plan). Same on the child side: the node's
        // children keep their mates, so the new `child_node` must be
        // one they did not take; the node's old `child_node` frees.
        let mut bound = self.bound_attach_nodes(edge.parent);
        if let Some(old) = &node.edge
            && old.parent == edge.parent
        {
            bound.remove(&old.parent_node);
        }
        if bound.contains(&edge.parent_node) {
            return Err(SessionError::AttachNodeOccupied {
                node: edge.parent,
                which: "parent",
                node_id: edge.parent_node.clone(),
            });
        }
        let mut child_bound = self.bound_attach_nodes(id);
        if let Some(old) = &node.edge {
            child_bound.remove(&old.child_node);
        }
        if child_bound.contains(&edge.child_node) {
            return Err(SessionError::AttachNodeOccupied {
                node: id,
                which: "child",
                node_id: edge.child_node.clone(),
            });
        }
        let built = node.built.clone();
        let transform = self.compute_attach(defs, id, &built, &edge)?;
        let node = self
            .nodes
            .get_mut(&id)
            .ok_or(SessionError::UnknownNode { id })?;
        node.edge = Some(edge);
        node.transform = transform;
        // The root cannot move here: `edge.parent` sits in the existing
        // tree, so the top of its ancestor chain is the current root.
        // Root moves are `reroot`'s job.
        Ok(())
    }

    /// Re-root the tree at `new_root` — the VAB's reroot tool. The
    /// path `new_root → … → old_root` is reversed edge by edge: each
    /// former parent becomes the child of its former child, carrying
    /// the **inverse** of the old transform and the same attach-node
    /// pair with roles swapped. Because inversion preserves every
    /// node's body-frame pose, `to_part_graph` (and therefore the
    /// weld) is identical across a reroot to f64 rounding —
    /// bit-identical for the authored attachment set (axis-aligned
    /// snaps and identity rotations), while an arbitrarily rotated
    /// chain can drift ~1 ulp through the inverse compose. Only the
    /// identity of the root changes.
    pub fn reroot(&mut self, new_root: PartNodeId) -> Result<(), SessionError> {
        let old_root = self.root.ok_or(SessionError::EmptySession)?;
        if !self.nodes.contains_key(&new_root) {
            return Err(SessionError::UnknownNode { id: new_root });
        }
        if new_root == old_root {
            return Ok(());
        }
        // Walk new_root's ancestor chain up to the old root. The `None`
        // arm is unreachable under the maintained tree invariant (every
        // chain ends at the root) — refuse rather than trust it.
        let mut path = vec![new_root];
        let mut cur = new_root;
        while cur != old_root {
            let Some(e) = self.nodes.get(&cur).and_then(|n| n.edge.as_ref()) else {
                return Err(SessionError::UnknownNode { id: cur });
            };
            cur = e.parent;
            path.push(cur);
        }
        // Snapshot every path node's *original* edge/transform before
        // mutating — pair (i) rewrites what pair (i+1) would otherwise
        // read.
        let mut original = Vec::with_capacity(path.len());
        for id in &path {
            let n = self
                .nodes
                .get(id)
                .ok_or(SessionError::UnknownNode { id: *id })?;
            original.push((n.edge.clone(), n.transform));
        }
        // `new_root`'s body-frame transform under the old root — folded
        // top-down — becomes its stored root transform, so no part's
        // graph-frame pose moves.
        let mut body_t = PartTransform::IDENTITY;
        for (_, t) in original.iter().rev() {
            body_t = body_t.compose(*t);
        }
        // Reverse each (lower=path[i], upper=path[i+1]) pair: upper
        // re-hangs under lower with the inverse of lower's original
        // transform and the same attach-node pair, roles swapped.
        for i in 0..path.len() - 1 {
            let (lower, upper) = (path[i], path[i + 1]);
            let (Some(edge), t) = &original[i] else {
                // Only the old root lacks an edge, and it is path's last
                // element — never a `lower`. Refuse rather than trust it.
                return Err(SessionError::UnknownNode { id: lower });
            };
            let inv_rot = t.rotation.conjugate();
            let inverse = PartTransform {
                offset: -inv_rot.rotate(t.offset),
                rotation: inv_rot,
            };
            let upper_node = self
                .nodes
                .get_mut(&upper)
                .ok_or(SessionError::UnknownNode { id: upper })?;
            upper_node.edge = Some(AttachEdge {
                parent: lower,
                parent_node: edge.child_node.clone(),
                child_node: edge.parent_node.clone(),
            });
            upper_node.transform = inverse;
        }
        let nr = self
            .nodes
            .get_mut(&new_root)
            .ok_or(SessionError::UnknownNode { id: new_root })?;
        nr.edge = None;
        nr.transform = body_t;
        self.root = Some(new_root);
        Ok(())
    }

    /// Set a node's local transform directly — free-position/surface
    /// adjustment after the snap. Gated by [`PartTransform::is_valid`]
    /// so what reaches `to_part_graph` always welds.
    pub fn set_transform(
        &mut self,
        id: PartNodeId,
        transform: PartTransform,
    ) -> Result<(), SessionError> {
        if !transform.is_valid() {
            return Err(SessionError::InvalidTransform { node: id });
        }
        let node = self
            .nodes
            .get_mut(&id)
            .ok_or(SessionError::UnknownNode { id })?;
        node.transform = transform;
        Ok(())
    }

    /// Recompute a node's stored transform from its edge at the
    /// *current* built scale/stretch of both ends — no placement gate
    /// (the edge is already bound), just the snap re-evaluated so
    /// geometry follows a resize. Missing defs or a malformed edge
    /// leave the stored transform alone (the session stays
    /// consistent; `validate` reports the record).
    fn resnap_node(&mut self, defs: &LoadedParts, id: PartNodeId) {
        let Some((built, edge)) = self
            .nodes
            .get(&id)
            .map(|n| (n.built.clone(), n.edge.clone()))
        else {
            return;
        };
        let Some(edge) = edge else { return };
        let Some(parent_rec) = self.nodes.get(&edge.parent) else {
            return;
        };
        let Some(parent_def) = find_def(defs, &parent_rec.built.part_id) else {
            return;
        };
        let Some(child_def) = find_def(defs, &built.part_id) else {
            return;
        };
        let pn = scaled_attach_node(
            parent_def,
            &edge.parent_node,
            parent_rec.built.scale,
            parent_rec.built.stretch,
        );
        let cn = scaled_attach_node(child_def, &edge.child_node, built.scale, built.stretch);
        if let (Some(pn), Some(cn)) = (pn, cn) {
            let t = snap_transform(&pn, &cn);
            if t.is_valid()
                && let Some(n) = self.nodes.get_mut(&id)
            {
                n.transform = t;
            }
        }
    }

    /// Re-snap a node and every direct child — the shared tail of the
    /// resize ops: a part that grows or stretches carries its
    /// attachments' snap points with it (the edges are already bound,
    /// so only positions/rotations re-evaluate — the diameter gate is
    /// a *placement* rule; `validate` keeps reporting post-resize
    /// mismatches as warnings).
    fn resnap_self_and_children(&mut self, defs: &LoadedParts, id: PartNodeId) {
        self.resnap_node(defs, id);
        for child in self.children_ids(id) {
            self.resnap_node(defs, child);
        }
    }

    /// Editor scale change — prechecked against the def's `ScaleRule`
    /// (same conditions `resolve::scale_factors` enforces); the
    /// resolver re-validates at commit. The node and its direct
    /// children re-snap at the new size so the stack stays mated.
    pub fn set_scale(
        &mut self,
        defs: &LoadedParts,
        id: PartNodeId,
        scale: f64,
    ) -> Result<(), SessionError> {
        let node = self
            .nodes
            .get(&id)
            .ok_or(SessionError::UnknownNode { id })?;
        let mut built = node.built.clone();
        built.scale = scale;
        let def = find_def(defs, &built.part_id).ok_or_else(|| SessionError::UnknownPartDef {
            node: Some(id),
            part_id: built.part_id.clone(),
        })?;
        check_built(def, &built, id)?;
        self.nodes
            .get_mut(&id)
            .ok_or(SessionError::UnknownNode { id })?
            .built = built;
        self.resnap_self_and_children(defs, id);
        Ok(())
    }

    /// Editor stack-axis (length) change — [`BuiltPart::stretch`],
    /// gated by the same `ScaleRule` bounds `check_built` enforces and
    /// the resolver re-checks. The node and its direct children
    /// re-snap, same as [`Self::set_scale`].
    pub fn set_stretch(
        &mut self,
        defs: &LoadedParts,
        id: PartNodeId,
        stretch: f64,
    ) -> Result<(), SessionError> {
        let node = self
            .nodes
            .get(&id)
            .ok_or(SessionError::UnknownNode { id })?;
        let mut built = node.built.clone();
        built.stretch = stretch;
        let def = find_def(defs, &built.part_id).ok_or_else(|| SessionError::UnknownPartDef {
            node: Some(id),
            part_id: built.part_id.clone(),
        })?;
        check_built(def, &built, id)?;
        self.nodes
            .get_mut(&id)
            .ok_or(SessionError::UnknownNode { id })?
            .built = built;
        self.resnap_self_and_children(defs, id);
        Ok(())
    }

    /// Editor variant change — the variant must exist on the def.
    pub fn set_variant(
        &mut self,
        defs: &LoadedParts,
        id: PartNodeId,
        variant: VariantId,
    ) -> Result<(), SessionError> {
        let node = self
            .nodes
            .get(&id)
            .ok_or(SessionError::UnknownNode { id })?;
        let mut built = node.built.clone();
        built.variant = variant;
        let def = find_def(defs, &built.part_id).ok_or_else(|| SessionError::UnknownPartDef {
            node: Some(id),
            part_id: built.part_id.clone(),
        })?;
        check_built(def, &built, id)?;
        self.nodes
            .get_mut(&id)
            .ok_or(SessionError::UnknownNode { id })?
            .built = built;
        Ok(())
    }

    /// Assign `id` to stage `stage`. Indices are bounded by
    /// [`MAX_STAGE_INDEX`] — larger values are refused so no stage
    /// can sit at the `u32` saturation point where [`Self::insert_stage`]
    /// would merge it with the next insertion. Gaps are legal
    /// mid-edit and surface as [`SessionWarning::EmptyStage`] in
    /// `validate`.
    pub fn set_stage(&mut self, id: PartNodeId, stage: u32) -> Result<(), SessionError> {
        if stage > MAX_STAGE_INDEX {
            return Err(SessionError::InvalidStage {
                node: Some(id),
                stage,
                detail: "session: stage index exceeds the maximum",
            });
        }
        let node = self
            .nodes
            .get_mut(&id)
            .ok_or(SessionError::UnknownNode { id })?;
        node.stage = stage;
        Ok(())
    }

    /// The node's stage, if placed.
    pub fn stage_of(&self, id: PartNodeId) -> Option<u32> {
        self.nodes.get(&id).map(|n| n.stage)
    }

    /// Parts assigned to `stage`, ascending id.
    pub fn parts_in_stage(&self, stage: u32) -> Vec<PartNodeId> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.stage == stage)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Populated stage indices, ascending.
    pub fn populated_stages(&self) -> Vec<u32> {
        self.nodes
            .values()
            .map(|n| n.stage)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Firing order — populated stages **descending** (higher fires
    /// earlier, the KSP rule the module doc fixes).
    pub fn stage_order(&self) -> Vec<u32> {
        let mut s = self.populated_stages();
        s.reverse();
        s
    }

    /// The stage `spacebar` would fire next — the highest populated.
    pub fn next_stage(&self) -> Option<u32> {
        self.populated_stages().last().copied()
    }

    /// Open a gap at `at`: every part with `stage ≥ at` moves up one.
    /// `at` is capped by [`MAX_STAGE_INDEX`], and an insert that
    /// would push a part already at the cap is **refused**, not
    /// saturated — two stages can never silently merge (the
    /// `u32::MAX` `saturating_add` behaviour this replaced). Atomic:
    /// both checks run before any mutation. Nodes carrying an
    /// out-of-cap stage (only reachable via a crafted file —
    /// `validate` flags them) are left where they are.
    pub fn insert_stage(&mut self, at: u32) -> Result<(), SessionError> {
        if at > MAX_STAGE_INDEX {
            return Err(SessionError::InvalidStage {
                node: None,
                stage: at,
                detail: "session: stage index exceeds the maximum",
            });
        }
        if self
            .nodes
            .values()
            .any(|n| n.stage >= at && n.stage == MAX_STAGE_INDEX)
        {
            return Err(SessionError::InvalidStage {
                node: None,
                stage: at,
                detail: "session: insert would push a part past the stage cap",
            });
        }
        for n in self.nodes.values_mut() {
            if n.stage >= at && n.stage < MAX_STAGE_INDEX {
                n.stage += 1;
            }
        }
        Ok(())
    }

    /// Delete stage `at`: parts in it fold to `at − 1` (stage 0 is the
    /// floor — its parts stay), stages above it shift down one.
    pub fn remove_stage(&mut self, at: u32) {
        for n in self.nodes.values_mut() {
            if n.stage > at {
                n.stage -= 1;
            } else if n.stage == at && at > 0 {
                n.stage = at - 1;
            }
        }
    }

    /// Reindex populated stages to `0..k` preserving order — the
    /// staging panel's "tidy" op; empty gaps disappear. An out-of-cap
    /// stage index (crafted file only — `validate` flags it and the
    /// ops gate it) is **refused**, not remapped into a legal-looking
    /// dense index: silently rewriting it would launder the defect
    /// `validate` is meant to report (C15-F4).
    pub fn compact_stages(&mut self) -> Result<(), SessionError> {
        if let Some((id, stage)) = self
            .nodes
            .iter()
            .find(|(_, n)| n.stage > MAX_STAGE_INDEX)
            .map(|(id, n)| (*id, n.stage))
        {
            return Err(SessionError::InvalidStage {
                node: Some(id),
                stage,
                detail: "session: stage index exceeds the maximum",
            });
        }
        let order = self.populated_stages();
        let remap: BTreeMap<u32, u32> = order
            .iter()
            .enumerate()
            .map(|(i, s)| (*s, i as u32))
            .collect();
        for n in self.nodes.values_mut() {
            if let Some(new) = remap.get(&n.stage) {
                n.stage = *new;
            }
        }
        Ok(())
    }

    /// The weld-shaped view of the session (module doc): a `PartGraph`
    /// plus the `BuiltPart` slice positionally bound by
    /// `effective_index`, both in ascending-`PartNodeId` order — the
    /// canonicalization §2.2's determinism rules demand, so
    /// `resolve_parts` + `weld` reproduce this craft bit-identically.
    /// Staging and attach-node ids are editor metadata and do not
    /// enter the weld input (the graph carries topology + transform;
    /// the file carries the rest). `defs` supplies the part catalog so
    /// each edge's `joint` record — the snapped attach point the §2.4
    /// joint-load torque arm measures about (C14-F4) — resolves to the
    /// parent node's scaled `AttachNode::position_m`; an edge naming a
    /// missing node leaves `joint` empty (the `validate` pass already
    /// flags it) and the load model falls back to the part origin.
    pub fn to_part_graph(
        &self,
        defs: &LoadedParts,
    ) -> Result<(PartGraph, Vec<BuiltPart>), SessionError> {
        let root = self.root.ok_or(SessionError::EmptySession)?;
        let mut graph = PartGraph::new(root);
        let mut built = Vec::with_capacity(self.nodes.len());
        for (i, (id, n)) in self.nodes.iter().enumerate() {
            let effective_index = u32::try_from(i).map_err(|_| SessionError::IndexOverflow)?;
            // The snapped joint point in the PARENT node's local frame
            // — the same scaled position `compute_attach` snapped the
            // child onto (recomputed here so a rescaled or file-loaded
            // session derives it identically).
            let joint = n.edge.as_ref().and_then(|e| {
                let rec = self.nodes.get(&e.parent)?;
                let def = find_def(defs, &rec.built.part_id)?;
                scaled_attach_node(def, &e.parent_node, rec.built.scale, rec.built.stretch)
                    .map(|pn| pn.position_m)
            });
            graph.insert(
                *id,
                PartNode::new(
                    n.edge.as_ref().map(|e| e.parent),
                    n.transform,
                    effective_index,
                )
                .with_joint(joint),
            );
            built.push(n.built.clone());
        }
        Ok((graph, built))
    }

    /// The two-tier check the VAB's validate button and the commit
    /// path share — see the module doc. `defs` is the part catalog;
    /// an empty session, a node whose def is missing, whose stored
    /// `built` fails the shared scale/variant/upgrade gate, or whose
    /// stage index exceeds [`MAX_STAGE_INDEX`] lands in `errors`.
    pub fn validate(&self, defs: &LoadedParts) -> Validation {
        let mut out = Validation::default();
        // An empty session is not launchable — `to_part_graph`
        // refuses it (`EmptySession`), so `validate` must agree
        // rather than report a clean bill on a craft that cannot
        // commit.
        if self.nodes.is_empty() {
            out.errors.push(SessionError::EmptySession);
        }
        let mut def_of: BTreeMap<PartNodeId, &PartDef> = BTreeMap::new();
        for (id, n) in &self.nodes {
            // Out-of-cap stage index — only reachable via a crafted
            // file (the ops gate it), flagged like the other
            // file-bypassed checks below.
            if n.stage > MAX_STAGE_INDEX {
                out.errors.push(SessionError::InvalidStage {
                    node: Some(*id),
                    stage: n.stage,
                    detail: "session: stage index exceeds the maximum",
                });
            }
            match find_def(defs, &n.built.part_id) {
                Some(d) => {
                    def_of.insert(*id, d);
                    // A crafted file can carry a `built` record no op
                    // ever gated (NaN scale, ghost variant) — re-run the
                    // shared boundary check so a non-resolvable craft is
                    // an error here, not a landmine for `resolve_parts`.
                    if let Err(e) = check_built(d, &n.built, *id) {
                        out.errors.push(e);
                    }
                }
                None => out.errors.push(SessionError::UnknownPartDef {
                    node: Some(*id),
                    part_id: n.built.part_id.clone(),
                }),
            }
        }
        // Binding sanity (a corrupt file could name absent attach nodes)
        // and the post-scale size-class warning.
        for (id, n) in &self.nodes {
            let Some(edge) = &n.edge else { continue };
            let Some(child_def) = def_of.get(id).copied() else {
                continue;
            };
            let Some(parent_def) = def_of.get(&edge.parent).copied() else {
                continue;
            };
            let node_known = |def: &PartDef, node_id: &str| {
                def.attach_nodes.iter().any(|a| a.id == node_id)
                    || decode_surface_node(node_id).is_some()
            };
            if !node_known(parent_def, &edge.parent_node) {
                out.errors.push(SessionError::UnknownAttachNode {
                    node: *id,
                    which: "parent",
                    node_id: edge.parent_node.clone(),
                });
            }
            if !node_known(child_def, &edge.child_node) {
                out.errors.push(SessionError::UnknownAttachNode {
                    node: *id,
                    which: "child",
                    node_id: edge.child_node.clone(),
                });
            }
            let (parent_scale, parent_stretch) = self
                .nodes
                .get(&edge.parent)
                .map(|p| (p.built.scale, p.built.stretch))
                .unwrap_or((1.0, 1.0));
            match (
                scaled_attach_node(parent_def, &edge.parent_node, parent_scale, parent_stretch),
                scaled_attach_node(child_def, &edge.child_node, n.built.scale, n.built.stretch),
            ) {
                (Some(pn), Some(cn)) if !diameters_match(pn.diameter_m, cn.diameter_m) => {
                    out.warnings
                        .push(SessionWarning::AttachDiameterMismatch { child: *id });
                }
                _ => {}
            }
        }
        self.validate_occupancy(&mut out);
        self.validate_staging(&def_of, &mut out);
        out
    }

    /// The occupancy half of `validate` — kept separate like the
    /// staging half so the rule list (module doc) reads in one place.
    /// One attach node mates exactly one neighbour; the ops enforce
    /// it, a crafted file bypasses them. Tally the bound ids per node
    /// (its own `child_node` + each child's `parent_node`) and flag
    /// duplicates.
    fn validate_occupancy(&self, out: &mut Validation) {
        let children = children_map(&self.nodes);
        for (id, n) in &self.nodes {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            let mut dup: BTreeSet<&str> = BTreeSet::new();
            if let Some(e) = &n.edge
                && !seen.insert(e.child_node.as_str())
            {
                dup.insert(e.child_node.as_str());
            }
            if let Some(kids) = children.get(id) {
                for c in kids {
                    if let Some(e) = self.nodes.get(c).and_then(|n| n.edge.as_ref())
                        && !seen.insert(e.parent_node.as_str())
                    {
                        dup.insert(e.parent_node.as_str());
                    }
                }
            }
            for node_id in dup {
                out.warnings.push(SessionWarning::AttachNodeOccupied {
                    node: *id,
                    node_id: node_id.to_string(),
                });
            }
        }
    }

    /// The staging half of `validate` — kept separate so the rule list
    /// (module doc) reads in one place.
    fn validate_staging(&self, def_of: &BTreeMap<PartNodeId, &PartDef>, out: &mut Validation) {
        // Gaps inside the populated range are empty stages — reported
        // under a cap because stage indices are unbounded `u32`s (see
        // `report_stage_gaps`; a `u32::MAX` stage must not OOM this).
        report_stage_gaps(&self.populated_stages(), out);
        let has = |id: &PartNodeId, f: &dyn Fn(&ModuleDef) -> bool| {
            def_of.get(id).is_some_and(|d| d.modules.iter().any(f))
        };
        // The decoupler check and every engine's `supply_region` flood
        // share one child index — `children_ids`'s per-node O(n) scan
        // inside either loop would make `validate` quadratic (C15-F5).
        let children = children_map(&self.nodes);
        for (id, n) in &self.nodes {
            if has(id, &|m| matches!(m, ModuleDef::Decoupler))
                && children.get(id).is_none_or(|kids| kids.is_empty())
            {
                out.warnings.push(SessionWarning::DecouplerSeversNothing {
                    node: *id,
                    stage: n.stage,
                });
            }
            let Some(def) = def_of.get(id).copied() else {
                continue;
            };
            for m in &def.modules {
                let ModuleDef::Engine { propellants, .. } = m else {
                    continue;
                };
                let region = self.supply_region(def_of, &children, *id, n.stage);
                let missing = self.missing_propellants(def_of, &region, propellants);
                if !missing.is_empty() {
                    out.warnings.push(SessionWarning::EngineWithoutPropellant {
                        node: *id,
                        stage: n.stage,
                        missing,
                    });
                }
                let stranded = self.cross_stage_drain(def_of, &region, propellants);
                if !stranded.is_empty() {
                    out.warnings
                        .push(SessionWarning::CrossStagePropellantDrain {
                            node: *id,
                            stage: n.stage,
                            resources: stranded,
                        });
                }
            }
        }
    }

    /// The engine's *supply region* at `stage`: the tree component
    /// still connected after every decoupler in a *higher* stage has
    /// severed its child edges — everything the session holds but the
    /// region misses has already departed when `stage` fires.
    /// `children` is the caller's one-pass [`children_map`] — the flood
    /// runs once per engine, so rebuilding the index inside it would
    /// be O(engines · n).
    fn supply_region(
        &self,
        def_of: &BTreeMap<PartNodeId, &PartDef>,
        children: &BTreeMap<PartNodeId, Vec<PartNodeId>>,
        engine: PartNodeId,
        stage: u32,
    ) -> BTreeSet<PartNodeId> {
        // Decouplers that have fired by the time `stage` activates:
        // firing order is strictly descending, so only a decoupler at a
        // *higher* index has already severed. A same-stage decoupler
        // fires *with* the engine — KSP's simultaneous fire-and-separate
        // — and the optimistic reading keeps the default
        // "everything stage 0" layout warning-free.
        let fired: BTreeSet<PartNodeId> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.stage > stage)
            .filter(|(id, _)| {
                def_of
                    .get(id)
                    .is_some_and(|d| d.modules.iter().any(|m| matches!(m, ModuleDef::Decoupler)))
            })
            .map(|(id, _)| *id)
            .collect();
        // Flood the tree from `engine`, never crossing a severed edge
        // (decoupler → child).
        let mut region = BTreeSet::from([engine]);
        let mut frontier = vec![engine];
        while let Some(n) = frontier.pop() {
            if let Some(kids) = children.get(&n) {
                for c in kids {
                    if !fired.contains(&n) && region.insert(*c) {
                        frontier.push(*c);
                    }
                }
            }
            if let Some(p) = self
                .nodes
                .get(&n)
                .and_then(|s| s.edge.as_ref())
                .map(|e| e.parent)
                && !fired.contains(&p)
                && region.insert(p)
            {
                frontier.push(p);
            }
        }
        region
    }

    /// Engine fuel check: `region` — the engine's [`Self::supply_region`]
    /// at its stage — is searched for `Tank` capacity covering each
    /// `Engine.propellants` resource. Returns the uncovered resources
    /// (authored order).
    fn missing_propellants(
        &self,
        def_of: &BTreeMap<PartNodeId, &PartDef>,
        region: &BTreeSet<PartNodeId>,
        propellants: &[(ResourceId, f64)],
    ) -> Vec<ResourceId> {
        let mut available = BTreeSet::new();
        for id in region {
            let Some(d) = def_of.get(id) else { continue };
            for m in &d.modules {
                if let ModuleDef::Tank { capacity } = m {
                    for (r, _) in capacity {
                        available.insert(r.clone());
                    }
                }
            }
        }
        propellants
            .iter()
            .map(|(r, _)| r.clone())
            .filter(|r| !available.contains(r))
            .collect()
    }

    /// Cross-stage depletion check (C13-F2): the pooled feed debits
    /// every attached tank ascending `PartNodeId` (the drain order
    /// `transfers::debit`, the readout, and flight's `consume_drawn`
    /// share), so the *last* units of a resource always sit in the
    /// highest-id tank holding it. When that tank is outside
    /// `region` — in a subtree an earlier stage drops — a burn can
    /// empty this engine's post-separation supply while propellant
    /// still aboard is jettisoned with the departing half. Returns
    /// the resources that can strand that way (authored order).
    /// Companion to [`Self::missing_propellants`]: that one checks
    /// capability in the region, this one checks drain order across
    /// the stage boundary — an engine can pass the first and still
    /// reach separation dry.
    fn cross_stage_drain(
        &self,
        def_of: &BTreeMap<PartNodeId, &PartDef>,
        region: &BTreeSet<PartNodeId>,
        propellants: &[(ResourceId, f64)],
    ) -> Vec<ResourceId> {
        propellants
            .iter()
            .filter(|(_, frac)| *frac > 0.0)
            .map(|(r, _)| r)
            .filter(|r| {
                // Ascending-id iteration leaves the highest id in
                // each slot — the pool's last units of `r` sit in the
                // higher of the two.
                let mut last_inside = None;
                let mut last_outside = None;
                for id in self.nodes.keys() {
                    let holds = def_of.get(id).is_some_and(|d| {
                        d.modules.iter().any(|m| match m {
                            ModuleDef::Tank { capacity } => capacity.iter().any(|(cr, _)| cr == *r),
                            _ => false,
                        })
                    });
                    if !holds {
                        continue;
                    }
                    if region.contains(id) {
                        last_inside = Some(*id);
                    } else {
                        last_outside = Some(*id);
                    }
                }
                // Stranded only when both sides hold `r` and the last
                // tank in drain order departs. An empty `inside` is
                // `missing_propellants`' case, not this one's; an
                // empty `outside` means nothing the engine burns ever
                // leaves the craft.
                matches!((last_inside, last_outside), (Some(i), Some(o)) if o > i)
            })
            .cloned()
            .collect()
    }

    /// Serialize to the canonical craft-file bytes (module doc):
    /// magic + bincode body, nodes ascending. Equal sessions produce
    /// identical bytes; save→load→save is a fixed point.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CraftFileError> {
        let file = CraftFile {
            schema_version: CRAFT_SCHEMA_VERSION,
            name: self.name.clone(),
            next_id: self.next_id,
            root: self.root.map(|r| r.0),
            nodes: self
                .nodes
                .iter()
                .map(|(id, n)| CraftFileNode {
                    id: id.0,
                    edge: n.edge.clone(),
                    transform: n.transform,
                    stage: n.stage,
                    built: n.built.clone(),
                })
                .collect(),
        };
        let body = bincode::serde::encode_to_vec(&file, bincode::config::standard())
            .map_err(|e| CraftFileError::Encode(e.to_string()))?;
        let mut out = Vec::with_capacity(CRAFT_FILE_MAGIC.len() + body.len());
        out.extend_from_slice(CRAFT_FILE_MAGIC);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Load a craft file written by [`Self::to_bytes`]. Refuses on any
    /// structural defect — a corrupt file loads nothing rather than a
    /// half-session (§7.7 "refused with a message").
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CraftFileError> {
        let body = bytes
            .strip_prefix(CRAFT_FILE_MAGIC)
            .ok_or(CraftFileError::BadMagic)?;
        let (file, read): (CraftFile, usize) =
            bincode::serde::decode_from_slice(body, bincode::config::standard())
                .map_err(|e| CraftFileError::Decode(e.to_string()))?;
        if read != body.len() {
            return Err(CraftFileError::Trailing {
                extra: body.len() - read,
            });
        }
        if file.schema_version != CRAFT_SCHEMA_VERSION {
            return Err(CraftFileError::UnsupportedSchemaVersion {
                found: file.schema_version,
                expected: CRAFT_SCHEMA_VERSION,
            });
        }
        Self::from_file(file)
    }

    /// Structural validation + reconstruction of a decoded file —
    /// deterministic first-error: file order for the per-node record
    /// scan, ascending id for the structural checks after it.
    fn from_file(file: CraftFile) -> Result<Self, CraftFileError> {
        let mut nodes: BTreeMap<PartNodeId, SessionNode> = BTreeMap::new();
        for rec in &file.nodes {
            if nodes.contains_key(&PartNodeId(rec.id)) {
                return Err(CraftFileError::DuplicateNode { id: rec.id });
            }
            if !rec.transform.is_valid() {
                return Err(CraftFileError::BadTransform { node: rec.id });
            }
            nodes.insert(
                PartNodeId(rec.id),
                SessionNode {
                    built: rec.built.clone(),
                    edge: rec.edge.clone(),
                    transform: rec.transform,
                    stage: rec.stage,
                },
            );
        }
        if nodes.is_empty() {
            // An empty craft carries no root — a `Some` here is a bogus
            // pointer, not a legal file.
            if file.root.is_some() {
                return Err(CraftFileError::MissingRoot);
            }
            return Ok(Self {
                name: file.name,
                root: None,
                nodes,
                next_id: file.next_id,
            });
        }
        let root = PartNodeId(file.root.ok_or(CraftFileError::MissingRoot)?);
        if !nodes.contains_key(&root) {
            return Err(CraftFileError::MissingRoot);
        }
        // Exactly one parentless node, and it is the root; every
        // non-root edge names a live parent.
        for (id, n) in &nodes {
            match &n.edge {
                None => {
                    if *id != root {
                        return Err(CraftFileError::MultipleRoots { second: id.0 });
                    }
                }
                Some(e) => {
                    if *id == root {
                        return Err(CraftFileError::BadRootEdge { node: id.0 });
                    }
                    if !nodes.contains_key(&e.parent) {
                        return Err(CraftFileError::DanglingParent {
                            node: id.0,
                            parent: e.parent.0,
                        });
                    }
                }
            }
        }
        // Reachability from the root — anything left over is a cycle
        // (dangling parents already failed above). The parent→children
        // index is built once so the walk is O(n), not a full map scan
        // per popped node (the shared `children_map` shape).
        let children = children_map(&nodes);
        let mut seen = BTreeSet::from([root]);
        let mut frontier = vec![root];
        while let Some(p) = frontier.pop() {
            if let Some(kids) = children.get(&p) {
                for id in kids {
                    if seen.insert(*id) {
                        frontier.push(*id);
                    }
                }
            }
        }
        if let Some(orphan) = nodes.keys().find(|id| !seen.contains(id)) {
            return Err(CraftFileError::Cycle { node: orphan.0 });
        }
        let max_id = file.nodes.iter().map(|n| n.id).max().unwrap_or(0);
        if file.next_id <= max_id {
            return Err(CraftFileError::NextIdCollision {
                next_id: file.next_id,
                max_id,
            });
        }
        Ok(Self {
            name: file.name,
            root: Some(root),
            nodes,
            next_id: file.next_id,
        })
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use csp_data::{
        AeroDef, ColliderDef, FamilyId, LoadedPart, PART_SCHEMA_VERSION, PartCategory, PartVariant,
        QualCurve, QualKnot, ReliabilityDef, ScaleExponents, StressRule, StructuralDef, TechNodeId,
        ThermalDef, ThrottleRange,
    };

    /// Minimal synthetic def: ±z stack nodes `top`/`bottom` at `±half_h`
    /// of `diameter`, one stock variant, whatever modules the test needs.
    fn def(id: &str, diameter: f64, half_h: f64, modules: Vec<ModuleDef>) -> PartDef {
        PartDef {
            schema_version: PART_SCHEMA_VERSION,
            id: PartId::from(id),
            family: FamilyId::from("test.family"),
            display_name: "t.name".into(),
            category: PartCategory::Utility,
            mass_dry_kg: 100.0,
            cost_funds: 100.0,
            size_m: DVec3::new(diameter, diameter, 2.0 * half_h),
            build_resources: Vec::new(),
            attach_nodes: vec![
                AttachNode {
                    id: "top".into(),
                    position_m: DVec3::new(0.0, 0.0, half_h),
                    direction: DVec3::Z,
                    diameter_m: diameter,
                },
                AttachNode {
                    id: "bottom".into(),
                    position_m: DVec3::new(0.0, 0.0, -half_h),
                    direction: -DVec3::Z,
                    diameter_m: diameter,
                },
            ],
            scale: ScaleRule::None,
            collider: ColliderDef::Box {
                half_extents_m: DVec3::new(diameter / 2.0, diameter / 2.0, half_h),
            },
            aero: AeroDef {
                cd_curve: vec![csp_data::AeroKnot {
                    mach: 0.0,
                    coeff: 0.5,
                }],
                cl_curve: vec![csp_data::AeroKnot {
                    mach: 0.0,
                    coeff: 0.0,
                }],
                ref_len_m: 2.0 * half_h,
            },
            thermal: ThermalDef {
                heat_capacity_j_per_k: 1_000.0,
                max_temp_k: 1_000.0,
                emissivity: 0.5,
                conduction_w_per_k: 10.0,
            },
            structural: StructuralDef {
                max_axial_n: 1e6,
                max_shear_n: 1e5,
                max_torque_nm: 1e4,
            },
            variants: vec![
                PartVariant {
                    id: VariantId::from("stock"),
                    display_name: "t.stock".into(),
                    cost_multiplier: 1.0,
                    stat_multipliers: Vec::new(),
                },
                PartVariant {
                    id: VariantId::from("alt"),
                    display_name: "t.alt".into(),
                    cost_multiplier: 1.0,
                    stat_multipliers: Vec::new(),
                },
            ],
            modules,
            reliability: ReliabilityDef {
                base_mtbf_s: 100_000.0,
                infant_mortality: 0.0,
                qualification_curve: QualCurve {
                    knots: vec![QualKnot {
                        qualification_units: 0.0,
                        hazard_multiplier: 1.0,
                    }],
                },
                regime_stress: Vec::<StressRule>::new(),
                dormant_stress: 0.0,
                failure_modes: Vec::new(),
            },
            tech_requirement: TechNodeId::from("tech.test"),
        }
    }

    fn catalog(defs: Vec<PartDef>) -> LoadedParts {
        LoadedParts {
            parts: defs
                .into_iter()
                .map(|d| LoadedPart {
                    path: PathBuf::from(format!("{}.ron", d.id.as_str())),
                    def: d,
                })
                .collect(),
        }
    }

    /// The three-def stack catalog: a structural "hull" part (0.5 m
    /// half-height, 1.0 m class), a tank, an engine (45:55 like the
    /// authored set), and a decoupler.
    fn stack_catalog() -> LoadedParts {
        catalog(vec![
            def("test.hull", 1.0, 0.5, Vec::new()),
            def(
                "test.tank",
                1.0,
                1.0,
                vec![ModuleDef::Tank {
                    capacity: vec![
                        (ResourceId::from("liquid_fuel"), 10.0),
                        (ResourceId::from("oxidizer"), 10.0),
                    ],
                }],
            ),
            def(
                "test.engine",
                1.0,
                0.5,
                vec![ModuleDef::Engine {
                    thrust_vac_n: 10_000.0,
                    isp_vac_s: 300.0,
                    isp_sl_s: 280.0,
                    throttle_range: ThrottleRange { min: 0.0, max: 1.0 },
                    gimbal_deg: 0.0,
                    ignitions: None,
                    rated_burn_s: 60.0,
                    propellants: vec![
                        (ResourceId::from("liquid_fuel"), 0.45),
                        (ResourceId::from("oxidizer"), 0.55),
                    ],
                    ullage_required: false,
                }],
            ),
            def("test.decoupler", 1.0, 0.25, vec![ModuleDef::Decoupler]),
            def("test.small", 0.5, 0.5, Vec::new()),
        ])
    }

    fn stock(id: &str) -> BuiltPart {
        BuiltPart::stock(PartId::from(id), VariantId::from("stock"))
    }

    /// A three-part vertical stack on the stack catalog.
    fn stack() -> (
        CraftSession,
        LoadedParts,
        PartNodeId,
        PartNodeId,
        PartNodeId,
    ) {
        let defs = stack_catalog();
        let mut s = CraftSession::with_name("stack");
        let pod = s.add_root(&defs, stock("test.hull")).unwrap();
        let tank = s
            .add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(pod, "bottom", "top"),
            )
            .unwrap();
        let eng = s
            .add_part(
                &defs,
                stock("test.engine"),
                AttachEdge::new(tank, "bottom", "top"),
            )
            .unwrap();
        (s, defs, pod, tank, eng)
    }

    #[test]
    fn snap_offsets_are_hand_computed() {
        let (s, _, pod, tank, eng) = stack();
        // hull bottom −0.5 − tank top +1.0 → −1.5; tank bottom −1.0 −
        // engine top +0.5 → −1.5. Identity rotations all the way down.
        assert_eq!(
            s.node(tank).unwrap().transform().offset,
            DVec3::new(0.0, 0.0, -1.5)
        );
        assert_eq!(s.node(tank).unwrap().transform().rotation, DQuat::IDENTITY);
        assert_eq!(
            s.node(eng).unwrap().transform().offset,
            DVec3::new(0.0, 0.0, -1.5)
        );
        assert_eq!(s.node(pod).unwrap().edge(), None);
        assert_eq!(s.node(tank).unwrap().edge().unwrap().parent_node, "bottom");
    }

    #[test]
    fn snap_obeys_scale_and_radial_arc() {
        let defs = catalog(vec![def("test.hull", 1.0, 0.5, Vec::new()), {
            let mut d = def("test.side", 1.0, 0.5, Vec::new());
            d.attach_nodes = vec![AttachNode {
                id: "mount".into(),
                position_m: DVec3::new(-0.25, 0.0, 0.0),
                direction: -DVec3::X,
                diameter_m: 1.0,
            }];
            d
        }]);
        let mut s = CraftSession::new();
        let pod = s.add_root(&defs, stock("test.hull")).unwrap();
        // Radial: child "mount" (−x, at local x −0.25) onto hull "top"
        // (+z at z=0.5): rotation maps −x → −z, offset puts mount there.
        let side = s
            .add_part(
                &defs,
                stock("test.side"),
                AttachEdge::new(pod, "top", "mount"),
            )
            .unwrap();
        let t = s.node(side).unwrap().transform();
        assert!(t.is_valid());
        // The child node lands on the parent node, directions opposed.
        let pn = DVec3::new(0.0, 0.0, 0.5);
        let cn = DVec3::new(-0.25, 0.0, 0.0);
        assert!((t.apply(cn) - pn).norm() < 1e-12);
        assert!((t.apply_dir(-DVec3::X) - (-DVec3::Z)).norm() < 1e-12);
    }

    #[test]
    fn add_part_errors_are_typed() {
        let defs = stack_catalog();
        let mut s = CraftSession::new();
        // attach before any root exists.
        assert!(matches!(
            s.add_part(
                &defs,
                stock("test.hull"),
                AttachEdge::new(PartNodeId(9), "bottom", "top"),
            ),
            Err(SessionError::UnknownNode { .. })
        ));
        // unknown def id.
        assert!(matches!(
            s.add_root(&defs, stock("test.missing")),
            Err(SessionError::UnknownPartDef { node: None, .. })
        ));
        let pod = s.add_root(&defs, stock("test.hull")).unwrap();
        assert!(matches!(
            s.add_root(&defs, stock("test.hull")),
            Err(SessionError::AlreadyRooted { root }) if root == pod
        ));
        // bad attach-node names on either side.
        assert!(matches!(
            s.add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(pod, "nope", "top"),
            ),
            Err(SessionError::UnknownAttachNode {
                which: "parent",
                ..
            })
        ));
        assert!(matches!(
            s.add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(pod, "bottom", "nope"),
            ),
            Err(SessionError::UnknownAttachNode { which: "child", .. })
        ));
        // size-class mismatch: 0.5 m small part on a 1.0 m node.
        assert!(matches!(
            s.add_part(
                &defs,
                stock("test.small"),
                AttachEdge::new(pod, "bottom", "top"),
            ),
            Err(SessionError::IncompatibleAttach { .. })
        ));
        // Refused ops consumed no ids — the next good add still gets 1.
        let tank = s
            .add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(pod, "bottom", "top"),
            )
            .unwrap();
        assert_eq!(tank, PartNodeId(1));
    }

    #[test]
    fn remove_part_takes_subtree_and_ids_never_recycle() {
        let (mut s, defs, pod, tank, eng) = stack();
        assert!(matches!(
            s.remove_part(PartNodeId(99)),
            Err(SessionError::UnknownNode { .. })
        ));
        let removed = s.remove_part(tank).unwrap();
        assert_eq!(removed, vec![tank, eng]);
        assert!(!s.contains(tank) && !s.contains(eng));
        // Re-adding allocates a fresh id — never the removed ones.
        let again = s
            .add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(pod, "bottom", "top"),
            )
            .unwrap();
        assert_eq!(again, PartNodeId(3));
        // Removing the root empties the session.
        assert_eq!(s.remove_part(pod).unwrap().len(), 2);
        assert!(s.is_empty());
        assert_eq!(s.root(), None);
        // ...and ids still don't recycle after a clear.
        let n = s.add_root(&defs, stock("test.hull")).unwrap();
        assert_eq!(n, PartNodeId(4));
    }

    #[test]
    fn reattach_moves_subtree_and_rejects_cycles() {
        let (mut s, defs, pod, tank, eng) = stack();
        // Attach the engine directly under the pod's FREE top node
        // (pod.bottom is bound by the tank — the occupancy rule now
        // refuses that edge): top↔top opposes via a 180° roll, so
        // +0.5 − (−0.5) = +1.0 above the pod.
        s.reattach(&defs, eng, AttachEdge::new(pod, "top", "top"))
            .unwrap();
        assert_eq!(s.node(eng).unwrap().edge().unwrap().parent, pod);
        // (the roll leaves ~1e-17 residue in x)
        assert!(
            (s.node(eng).unwrap().transform().offset - DVec3::new(0.0, 0.0, 1.0)).norm() < 1e-12
        );
        assert_eq!(s.children_ids(pod), vec![tank, eng]);
        // The root is not a cycle — it's the typed `RootImmovable`
        // refusal (it has no parent edge at all).
        assert!(matches!(
            s.reattach(&defs, pod, AttachEdge::new(pod, "bottom", "top")),
            Err(SessionError::RootImmovable { .. })
        ));
        assert!(matches!(
            s.reattach(&defs, pod, AttachEdge::new(tank, "top", "top")),
            Err(SessionError::RootImmovable { .. })
        ));
        // A genuine cycle: put the engine back under the tank, then
        // try the tank under its own engine.
        s.reattach(&defs, eng, AttachEdge::new(tank, "bottom", "top"))
            .unwrap();
        assert!(matches!(
            s.reattach(&defs, tank, AttachEdge::new(eng, "bottom", "top")),
            Err(SessionError::WouldCycle { .. })
        ));
        // Missing nodes on either end are typed.
        assert!(matches!(
            s.reattach(&defs, PartNodeId(99), AttachEdge::new(pod, "bottom", "top")),
            Err(SessionError::UnknownNode { .. })
        ));
        assert!(matches!(
            s.reattach(&defs, eng, AttachEdge::new(PartNodeId(98), "bottom", "top")),
            Err(SessionError::UnknownNode { .. })
        ));
    }

    #[test]
    fn reroot_reverses_the_path_and_preserves_geometry() {
        let (mut s, ..) = stack();
        let pod = s.root().unwrap();
        let tank = s.children_ids(pod)[0];
        let eng = s.children_ids(tank)[0];
        s.reroot(eng).unwrap();
        assert_eq!(s.root(), Some(eng));
        assert_eq!(s.node(eng).unwrap().edge(), None);
        assert_eq!(s.node(tank).unwrap().edge().unwrap().parent, eng);
        assert_eq!(s.node(pod).unwrap().edge().unwrap().parent, tank);
        // Inverse transforms: what was a −1.5 child offset becomes the
        // parent's +1.5 — every body-frame pose is preserved.
        assert_eq!(
            s.node(tank).unwrap().transform().offset,
            DVec3::new(0.0, 0.0, 1.5)
        );
        assert_eq!(
            s.node(pod).unwrap().transform().offset,
            DVec3::new(0.0, 0.0, 1.5)
        );
        // The new root's stored transform reproduces its old body pose
        // (chain composition of the old local transforms).
        let rt = s.node(eng).unwrap().transform();
        assert!(rt.is_valid());
        assert!(rt.offset.z < 0.0, "engine was below the old root");
        // Edge bindings swapped roles on the reversed edges.
        let e = s.node(pod).unwrap().edge().unwrap();
        assert_eq!(e.parent_node, "top");
        assert_eq!(e.child_node, "bottom");
        assert_eq!(s.subtree_ids(eng).len(), 3);
        assert!(matches!(
            s.reroot(PartNodeId(77)),
            Err(SessionError::UnknownNode { .. })
        ));
    }

    #[test]
    fn set_scale_variant_and_transform_gates() {
        let defs = catalog(vec![{
            let mut d = def("test.scalable", 1.0, 0.5, Vec::new());
            d.scale = ScaleRule::Continuous {
                min: 0.5,
                max: 2.0,
                exponents: ScaleExponents {
                    mass: 3.0,
                    cost: 3.0,
                    thrust: 0.0,
                    capacity: 3.0,
                    structural: 2.0,
                },
            };
            d
        }]);
        let mut s = CraftSession::new();
        let n = s.add_root(&defs, stock("test.scalable")).unwrap();
        s.set_scale(&defs, n, 2.0).unwrap();
        assert_eq!(s.node(n).unwrap().built().scale, 2.0);
        assert!(matches!(
            s.set_scale(&defs, n, 3.0),
            Err(SessionError::InvalidScale { .. })
        ));
        s.set_variant(&defs, n, VariantId::from("alt")).unwrap();
        assert_eq!(s.node(n).unwrap().built().variant.as_str(), "alt");
        assert!(matches!(
            s.set_variant(&defs, n, VariantId::from("ghost")),
            Err(SessionError::UnknownVariant { .. })
        ));
        // Non-unit rotation is refused at the session boundary.
        let bad = PartTransform::new(DVec3::ZERO, DQuat::new(0.0, 0.0, 0.0, 2.0));
        assert!(matches!(
            s.set_transform(n, bad),
            Err(SessionError::InvalidTransform { .. })
        ));
        // A scaled part's node moves with it: validate() flags the now-
        // mismatched size class rather than hiding it.
    }

    #[test]
    fn staging_order_insert_remove_compact() {
        let (mut s, _, pod, tank, eng) = stack();
        s.set_stage(tank, 1).unwrap();
        s.set_stage(eng, 2).unwrap();
        // Firing order is descending: 2 (engine) before 1 before 0.
        assert_eq!(s.stage_order(), vec![2, 1, 0]);
        assert_eq!(s.next_stage(), Some(2));
        assert_eq!(s.parts_in_stage(0), vec![pod]);
        // insert at 1 pushes the old stage 1/2 up.
        s.insert_stage(1).unwrap();
        assert_eq!(s.stage_of(tank), Some(2));
        assert_eq!(s.stage_of(eng), Some(3));
        // remove 2: tank folds to 1, engine shifts 3 → 2.
        s.remove_stage(2);
        assert_eq!(s.stage_of(tank), Some(1));
        assert_eq!(s.stage_of(eng), Some(2));
        // Sparse out to stage 9 then compact back to dense.
        s.set_stage(eng, 9).unwrap();
        s.compact_stages().unwrap();
        assert_eq!(s.stage_order(), vec![2, 1, 0]);
        assert_eq!(s.stage_of(eng), Some(2));
    }

    /// C15-F4: a crafted out-of-cap stage index refuses `compact_stages`
    /// instead of being laundered into a legal-looking dense index —
    /// and the session is left untouched so `validate`'s error stands.
    #[test]
    fn compact_stages_refuses_out_of_cap_index() {
        let (mut s, _, _, tank, _) = stack();
        // Only a crafted file can carry this — `set_stage`/`insert_stage`
        // gate at MAX_STAGE_INDEX; `validate` flags it as InvalidStage.
        s.nodes.get_mut(&tank).unwrap().stage = u32::MAX;
        assert!(matches!(
            s.compact_stages(),
            Err(SessionError::InvalidStage {
                stage: u32::MAX,
                ..
            })
        ));
        // Nothing remapped: the defect still reads exactly what the
        // file authored, so `validate` keeps reporting it.
        assert_eq!(s.stage_of(tank), Some(u32::MAX));
    }

    /// C15-F1: `preview_attach` runs the same parent-side occupancy
    /// gate `add_part` enforces — the pub oracle can never `Ok` where
    /// the commit would refuse `AttachNodeOccupied`.
    #[test]
    fn preview_attach_enforces_parent_occupancy() {
        let (s, defs, pod, _, _) = stack();
        // `pod.bottom` is bound by the tank — preview must refuse the
        // same way `add_part` does (the doc's "the same checks" claim).
        let edge = AttachEdge::new(pod, "bottom", "top");
        assert_eq!(
            s.preview_attach(&defs, &stock("test.tank"), &edge),
            Err(SessionError::AttachNodeOccupied {
                node: pod,
                which: "parent",
                node_id: "bottom".to_string(),
            })
        );
        // A free parent node still previews, and the transform is the
        // one `add_part` would commit.
        let free = AttachEdge::new(pod, "top", "bottom");
        let built = stock("test.hull");
        let previewed = s.preview_attach(&defs, &built, &free).unwrap();
        let mut s2 = s.clone();
        let id = s2.add_part(&defs, built, free).unwrap();
        assert_eq!(s2.node(id).unwrap().transform(), previewed);
    }

    #[test]
    fn validate_flags_empty_stage_and_starved_engine() {
        let defs = stack_catalog();
        let mut s = CraftSession::with_name("v");
        let pod = s.add_root(&defs, stock("test.hull")).unwrap();
        let tank = s
            .add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(pod, "bottom", "top"),
            )
            .unwrap();
        let dec = s
            .add_part(
                &defs,
                stock("test.decoupler"),
                AttachEdge::new(tank, "bottom", "top"),
            )
            .unwrap();
        let eng = s
            .add_part(
                &defs,
                stock("test.engine"),
                AttachEdge::new(dec, "bottom", "top"),
            )
            .unwrap();
        // Decoupler stage 1 severs the engine; engine fires at stage 0 —
        // its supply region (below the decoupler) has no tanks.
        s.set_stage(dec, 1).unwrap();
        let v = s.validate(&defs);
        assert!(v.is_launchable());
        assert!(
            v.warnings.iter().any(|w| matches!(
                w,
                SessionWarning::EngineWithoutPropellant { node, .. } if *node == eng
            )),
            "engine below a severed decoupler must warn: {v:?}"
        );
        // Engine firing *before* the decoupler (higher stage) keeps its
        // tank — no propellant warning.
        s.set_stage(eng, 2).unwrap();
        let v = s.validate(&defs);
        assert!(
            !v.warnings
                .iter()
                .any(|w| matches!(w, SessionWarning::EngineWithoutPropellant { .. })),
            "engine firing pre-separation sees its tank: {v:?}"
        );
        // A stage gap (nothing at 1 after compacting differently) warns.
        s.set_stage(eng, 5).unwrap();
        let v = s.validate(&defs);
        let empties: Vec<u32> = v
            .warnings
            .iter()
            .filter_map(|w| match w {
                SessionWarning::EmptyStage { stage } => Some(*stage),
                _ => None,
            })
            .collect();
        assert_eq!(empties, vec![2, 3, 4]);
        // A leaf decoupler warns that it severs nothing.
        let leaf = s
            .add_part(
                &defs,
                stock("test.decoupler"),
                AttachEdge::new(eng, "bottom", "top"),
            )
            .unwrap();
        let v = s.validate(&defs);
        assert!(v.warnings.iter().any(|w| matches!(
            w,
            SessionWarning::DecouplerSeversNothing { node, .. } if *node == leaf
        )));
    }

    /// C13-F2: the pooled feed debits tanks ascending `PartNodeId`
    /// regardless of stage boundaries, so in the canonical top-down
    /// stack — sustainer tank at the lowest id — the stage-1 burn
    /// drains the sustainer's supply *through* the decoupler before
    /// the booster's own tank empties. `EngineWithoutPropellant`
    /// cannot see it (the region keeps its tank — capability is
    /// fine); the drain-order check must say so instead.
    #[test]
    fn validate_warns_cross_stage_propellant_drain() {
        let defs = stack_catalog();
        let mut s = CraftSession::new();
        let pod = s.add_root(&defs, stock("test.hull")).unwrap();
        let tank = s
            .add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(pod, "bottom", "top"),
            )
            .unwrap();
        let eng = s
            .add_part(
                &defs,
                stock("test.engine"),
                AttachEdge::new(tank, "bottom", "top"),
            )
            .unwrap();
        let dec = s
            .add_part(
                &defs,
                stock("test.decoupler"),
                AttachEdge::new(eng, "bottom", "top"),
            )
            .unwrap();
        let btank = s
            .add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(dec, "bottom", "top"),
            )
            .unwrap();
        let beng = s
            .add_part(
                &defs,
                stock("test.engine"),
                AttachEdge::new(btank, "bottom", "top"),
            )
            .unwrap();
        s.set_stage(dec, 1).unwrap();
        s.set_stage(beng, 1).unwrap();
        let v = s.validate(&defs);
        // The sustainer's post-separation region keeps its own tank —
        // no capability warning — but the ascending-id drain strands
        // it: both authored resources flag.
        assert!(
            !v.warnings.iter().any(|w| matches!(
                w,
                SessionWarning::EngineWithoutPropellant { node, .. } if *node == eng
            )),
            "region keeps its tank — capability passes: {v:?}"
        );
        assert!(
            v.warnings.iter().any(|w| matches!(
                w,
                SessionWarning::CrossStagePropellantDrain { node, resources, .. }
                    if *node == eng && resources.len() == 2
            )),
            "sustainer supply must flag cross-stage drain: {v:?}"
        );
        // The booster engine's region is the whole craft — nothing
        // departs it, so no drain warning for it.
        assert!(
            !v.warnings.iter().any(|w| matches!(
                w,
                SessionWarning::CrossStagePropellantDrain { node, .. } if *node == beng
            )),
            "{v:?}"
        );
        // Single-stage: nothing sits outside any region — silent.
        let (s2, defs2, ..) = stack();
        assert!(
            !s2.validate(&defs2)
                .warnings
                .iter()
                .any(|w| matches!(w, SessionWarning::CrossStagePropellantDrain { .. })),
        );
        // A late-added tank aboard the surviving half holds the last
        // fuel in drain order (higher id than the booster's), so the
        // order then preserves the sustainer's supply and the warning
        // clears.
        s.add_part(
            &defs,
            stock("test.tank"),
            AttachEdge::new(pod, "top", "bottom"),
        )
        .unwrap();
        let v = s.validate(&defs);
        assert!(
            !v.warnings.iter().any(|w| matches!(
                w,
                SessionWarning::CrossStagePropellantDrain { node, .. } if *node == eng
            )),
            "inside-last drain order must clear the warning: {v:?}"
        );
    }

    /// A crafted file can stage a part at `u32::MAX` — the ops refuse
    /// indices past [`MAX_STAGE_INDEX`], so this is a *file-level*
    /// attack: `validate` flags it as `InvalidStage` AND must still
    /// bound its empty-stage reporting, not emit ~4 G warnings (OOM).
    #[test]
    fn validate_bounds_empty_stage_warnings() {
        let defs = stack_catalog();
        let mut s = CraftSession::new();
        let pod = s.add_root(&defs, stock("test.hull")).unwrap();
        s.add_part(
            &defs,
            stock("test.tank"),
            AttachEdge::new(pod, "bottom", "top"),
        )
        .unwrap();
        // The ops cap stage indices — forge the file a crafted load
        // would produce.
        let mut f = file_of(&s);
        f.nodes[1].stage = u32::MAX;
        let forged = CraftSession::from_bytes(&encode(&f)).unwrap();
        let v = forged.validate(&defs);
        assert!(!v.is_launchable());
        assert!(
            v.errors.iter().any(
                |e| matches!(e, SessionError::InvalidStage { stage, .. } if *stage == u32::MAX)
            ),
            "out-of-cap stage must error: {v:?}"
        );
        // The 0..=u32::MAX span holds 4_294_967_294 gaps for these two
        // parts: the cap report individually, one summary carries the rest.
        assert_eq!(v.warnings.len(), MAX_EMPTY_STAGE_WARNINGS + 1);
        let empties = v
            .warnings
            .iter()
            .filter(|w| matches!(w, SessionWarning::EmptyStage { .. }))
            .count();
        assert_eq!(empties, MAX_EMPTY_STAGE_WARNINGS);
        assert!(
            v.warnings.iter().any(|w| matches!(
                w,
                SessionWarning::EmptyStagesOmitted { count }
                    if *count == 4_294_967_294 - MAX_EMPTY_STAGE_WARNINGS as u64
            )),
            "the remaining gaps must collapse into one summary: {v:?}"
        );
    }

    /// A crafted file whose `built` records bypassed the op-time gates
    /// (NaN scale, ghost variant) loads — the file schema cannot gate
    /// semantics — but `validate` re-runs the shared `built`-vs-`def`
    /// check and marks the craft unlaunchable.
    #[test]
    fn validate_flags_ungated_built_records() {
        let defs = stack_catalog();
        let (s, ..) = stack();
        let mut f = file_of(&s);
        f.nodes[0].built.scale = f64::NAN;
        f.nodes[1].built.variant = VariantId::from("ghost");
        let forged = CraftSession::from_bytes(&encode(&f)).unwrap();
        let v = forged.validate(&defs);
        assert!(!v.is_launchable());
        assert!(
            v.errors
                .iter()
                .any(|e| matches!(e, SessionError::InvalidScale { .. })),
            "NaN scale must error: {v:?}"
        );
        assert!(
            v.errors
                .iter()
                .any(|e| matches!(e, SessionError::UnknownVariant { .. })),
            "ghost variant must error: {v:?}"
        );
    }

    #[test]
    fn validate_errors_on_missing_defs_and_bindings() {
        let defs = stack_catalog();
        let (s, ..) = stack();
        let v = s.validate(&defs);
        assert!(v.errors.is_empty() && v.warnings.is_empty(), "{v:?}");
        // A node whose part_id is absent from the catalog is a hard error
        // per node (tank + engine here).
        let partial = catalog(vec![def("test.hull", 1.0, 0.5, Vec::new())]);
        let v = s.validate(&partial);
        assert_eq!(v.errors.len(), 2);
        assert!(!v.is_launchable());
        // A forged binding naming an attach node the def lacks — only
        // reachable through a crafted file, since add/reattach gate it.
        let mut f = file_of(&s);
        f.nodes[1].edge.as_mut().unwrap().parent_node = "missing".into();
        let forged = CraftSession::from_bytes(&encode(&f)).unwrap();
        let v = forged.validate(&defs);
        assert!(v.errors.iter().any(|e| matches!(
            e,
            SessionError::UnknownAttachNode {
                which: "parent",
                ..
            }
        )));
    }

    #[test]
    fn to_part_graph_is_canonical_and_bound_positionally() {
        let (s, defs, pod, tank, eng) = stack();
        let (g, built) = s.to_part_graph(&defs).unwrap();
        assert_eq!(g.root, pod);
        assert_eq!(g.nodes.len(), 3);
        // built[i] is the node with effective_index == i, ascending ids.
        for (i, (id, n)) in g.nodes.iter().enumerate() {
            assert_eq!(n.effective_index, i as u32);
            match i {
                0 => assert_eq!(*id, pod),
                1 => {
                    assert_eq!(*id, tank);
                    assert_eq!(n.parent, Some(pod));
                }
                _ => {
                    assert_eq!(*id, eng);
                    assert_eq!(n.parent, Some(tank));
                }
            }
        }
        assert_eq!(built[0].part_id.as_str(), "test.hull");
        assert_eq!(built[1].part_id.as_str(), "test.tank");
        assert_eq!(built[2].part_id.as_str(), "test.engine");
        // Empty session cannot produce a graph.
        assert!(matches!(
            CraftSession::new().to_part_graph(&defs),
            Err(SessionError::EmptySession)
        ));
    }

    #[test]
    fn save_load_roundtrip_preserves_everything() {
        let (mut s, defs, _, tank, eng) = stack();
        s.set_stage(tank, 1).unwrap();
        s.set_stage(eng, 1).unwrap();
        s.set_variant(&defs, eng, VariantId::from("alt")).unwrap();
        // Remove a part mid-edit so the id set has a hole — stability
        // must survive it.
        let spare = s
            .add_part(
                &defs,
                stock("test.hull"),
                AttachEdge::new(eng, "bottom", "top"),
            )
            .unwrap();
        s.remove_part(spare).unwrap();

        let bytes = s.to_bytes().unwrap();
        assert!(bytes.starts_with(CRAFT_FILE_MAGIC));
        let loaded = CraftSession::from_bytes(&bytes).unwrap();
        assert_eq!(loaded, s);
        assert_eq!(loaded.name(), "stack");
        assert_eq!(loaded.stage_of(tank), Some(1));
        // Canonical bytes: save → load → save is a fixed point.
        assert_eq!(loaded.to_bytes().unwrap(), bytes);
        // The hole is preserved: the next alloc on the *loaded* session
        // skips the removed id (next_id round-tripped).
        let mut loaded = loaded;
        let n = loaded
            .add_part(
                &defs,
                stock("test.hull"),
                AttachEdge::new(eng, "bottom", "top"),
            )
            .unwrap();
        assert_eq!(n, PartNodeId(spare.0 + 1));
        // Two identical constructions serialize identically.
        let (s2, ..) = stack();
        let mut s2 = s2;
        s2.set_stage(tank, 1).unwrap();
        s2.set_stage(eng, 1).unwrap();
        s2.set_variant(&defs, eng, VariantId::from("alt")).unwrap();
        let spare2 = s2
            .add_part(
                &defs,
                stock("test.hull"),
                AttachEdge::new(eng, "bottom", "top"),
            )
            .unwrap();
        s2.remove_part(spare2).unwrap();
        assert_eq!(s2.to_bytes().unwrap(), bytes);
    }

    /// Every corruption class the loader must refuse — files are built
    /// by hand through the private record so even unreachable-by-ops
    /// states are exercised.
    #[test]
    fn load_rejects_corrupt_files() {
        let (s, ..) = stack();
        let good = s.to_bytes().unwrap();
        // Bad magic / truncation.
        assert_eq!(
            CraftSession::from_bytes(&good[..4]),
            Err(CraftFileError::BadMagic)
        );
        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert_eq!(
            CraftSession::from_bytes(&bad_magic),
            Err(CraftFileError::BadMagic)
        );
        // Truncated body → Decode.
        let cut = good.len() - 5;
        assert!(matches!(
            CraftSession::from_bytes(&good[..cut]),
            Err(CraftFileError::Decode(_))
        ));
        // Trailing garbage.
        let mut trailed = good.clone();
        trailed.extend_from_slice(&[0xEE; 4]);
        assert_eq!(
            CraftSession::from_bytes(&trailed),
            Err(CraftFileError::Trailing { extra: 4 })
        );
    }

    fn encode(file: &CraftFile) -> Vec<u8> {
        let mut out = CRAFT_FILE_MAGIC.to_vec();
        out.extend_from_slice(
            &bincode::serde::encode_to_vec(file, bincode::config::standard()).unwrap(),
        );
        out
    }

    fn file_of(s: &CraftSession) -> CraftFile {
        // Round-trip the honest session into its record for tampering.
        let bytes = s.to_bytes().unwrap();
        let body = &bytes[CRAFT_FILE_MAGIC.len()..];
        bincode::serde::decode_from_slice(body, bincode::config::standard())
            .unwrap()
            .0
    }

    #[test]
    fn load_rejects_structural_corruption() {
        let (s, _, pod, _, _) = stack();
        // Unsupported schema version.
        let mut f = file_of(&s);
        f.schema_version = 99;
        assert_eq!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::UnsupportedSchemaVersion {
                found: 99,
                expected: CRAFT_SCHEMA_VERSION
            })
        );
        // Duplicate node id.
        let mut f = file_of(&s);
        let dup = f.nodes[1].clone();
        f.nodes.push(dup);
        assert!(matches!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::DuplicateNode { .. })
        ));
        // Root names a node that isn't there.
        let mut f = file_of(&s);
        f.root = Some(999);
        assert_eq!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::MissingRoot)
        );
        // A root pointer on an empty craft is bogus, not "no root".
        let mut f = file_of(&s);
        f.nodes.clear();
        f.root = Some(pod.0);
        assert_eq!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::MissingRoot)
        );
        // Dangling parent.
        let mut f = file_of(&s);
        f.nodes[2].edge = Some(AttachEdge::new(PartNodeId(77), "a", "b"));
        assert!(matches!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::DanglingParent { parent: 77, .. })
        ));
        // Cycle: make the root point at node 2 — root gains an edge.
        let mut f = file_of(&s);
        f.nodes[0].edge = Some(AttachEdge::new(PartNodeId(2), "a", "b"));
        assert!(matches!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::BadRootEdge { node }) if node == pod.0
        ));
        // Two parentless nodes.
        let mut f = file_of(&s);
        f.nodes[2].edge = None;
        assert!(matches!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::MultipleRoots { .. })
        ));
        // Real cycle: 1↔2 edges pointing at each other (both non-root).
        let mut f = file_of(&s);
        f.nodes[1].edge = Some(AttachEdge::new(PartNodeId(2), "a", "b"));
        f.nodes[2].edge = Some(AttachEdge::new(PartNodeId(1), "a", "b"));
        assert!(matches!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::Cycle { .. })
        ));
        // next_id below a live id.
        let mut f = file_of(&s);
        f.next_id = 1;
        assert_eq!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::NextIdCollision {
                next_id: 1,
                max_id: 2
            })
        );
        // Non-unit rotation in the file.
        let mut f = file_of(&s);
        f.nodes[1].transform.rotation = DQuat::new(0.0, 0.0, 0.0, 0.0);
        assert!(matches!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::BadTransform { .. })
        ));
    }

    /// R14-F3: a non-finite `position_m` on either side of an edge is
    /// refused at `compute_attach` — before the fix, `snap_transform`
    /// produced a NaN-offset transform that `to_bytes` wrote verbatim
    /// and `from_file` refused: a session could save a file it could
    /// not reload.
    #[test]
    fn attach_rejects_nonfinite_node_positions() {
        let broken_child = {
            let mut d = def("test.broken.child", 1.0, 0.5, Vec::new());
            d.attach_nodes[0].position_m = DVec3::new(f64::NAN, 0.0, 0.5);
            d
        };
        let broken_parent = {
            let mut d = def("test.broken.parent", 1.0, 0.5, Vec::new());
            d.attach_nodes[1].position_m = DVec3::new(0.0, f64::INFINITY, -0.5);
            d
        };
        let defs = catalog(vec![
            def("test.hull", 1.0, 0.5, Vec::new()),
            broken_child,
            broken_parent,
        ]);
        let mut s = CraftSession::new();
        let pod = s.add_root(&defs, stock("test.hull")).unwrap();
        // Child-side NaN position.
        assert!(matches!(
            s.add_part(
                &defs,
                stock("test.broken.child"),
                AttachEdge::new(pod, "bottom", "top"),
            ),
            Err(SessionError::IncompatibleAttach { .. })
        ));
        // Parent-side inf position — the broken part roots fine (a
        // root has no edge), it just can't be attached under.
        let bp = s.add_root(&defs, stock("test.broken.parent"));
        assert!(matches!(bp, Err(SessionError::AlreadyRooted { .. })));
        let mut s2 = CraftSession::new();
        let bp = s2.add_root(&defs, stock("test.broken.parent")).unwrap();
        assert!(matches!(
            s2.add_part(
                &defs,
                stock("test.hull"),
                AttachEdge::new(bp, "bottom", "top"),
            ),
            Err(SessionError::IncompatibleAttach { .. })
        ));
        // Refused attaches consumed no ids.
        assert_eq!(s.len(), 1);
        assert_eq!(s2.len(), 1);
    }

    /// R14-F3's round-trip half: every transform the ops can store is
    /// `is_valid`, so `to_bytes` → `from_bytes` on an op-built session
    /// can never hit `BadTransform`. (The file-level gate itself is
    /// covered by `load_rejects_structural_corruption`; here a forged
    /// NaN *offset* proves the same net catches what the ops now
    /// refuse.)
    #[test]
    fn op_built_session_always_reloads() {
        let (mut s, ..) = stack();
        let eng = s.iter().map(|(id, _)| id).last().unwrap();
        s.set_transform(
            eng,
            PartTransform::new(DVec3::new(1.0, -2.0, 0.5), DQuat::IDENTITY),
        )
        .unwrap();
        for (_, n) in s.iter() {
            assert!(n.transform().is_valid());
        }
        let bytes = s.to_bytes().unwrap();
        assert!(CraftSession::from_bytes(&bytes).is_ok());
        // The loader's net is still live for crafted files.
        let mut f = file_of(&s);
        f.nodes[1].transform.offset = DVec3::new(f64::NAN, 0.0, 0.0);
        assert!(matches!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::BadTransform { .. })
        ));
    }

    /// R14-F4: `check_built`'s upgrade arm — an unresolvable
    /// `StatPath` or a non-positive/non-finite multiplier fails at
    /// the boundary (`add_root`/`add_part`), and a crafted file
    /// carrying the same record fails at `validate`.
    #[test]
    fn check_built_rejects_invalid_upgrade_levels() {
        use crate::resolve::TrackLevels;

        let defs = stack_catalog();
        // Unresolvable path: the hull def carries no Engine/Control,
        // so `ThrustVacN` has no `base_value` on it.
        let mut b = stock("test.hull");
        b.upgrade_levels_at_build.insert(
            StatPath::ThrustVacN,
            TrackLevels {
                levels: 1,
                multiplier: 1.1,
            },
        );
        let mut s = CraftSession::new();
        assert!(matches!(
            s.add_root(&defs, b),
            Err(SessionError::InvalidUpgrade {
                stat: StatPath::ThrustVacN,
                ..
            })
        ));
        // Non-finite and non-positive multipliers on a path that does
        // resolve (`MassDryKg` is present on every def).
        for m in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let mut b = stock("test.hull");
            b.upgrade_levels_at_build.insert(
                StatPath::MassDryKg,
                TrackLevels {
                    levels: 1,
                    multiplier: m,
                },
            );
            assert!(
                matches!(
                    s.add_root(&defs, b),
                    Err(SessionError::InvalidUpgrade { .. })
                ),
                "multiplier {m} must fail"
            );
        }
        // A clean record is accepted (the engine part's MassDryKg).
        let mut b = stock("test.engine");
        b.upgrade_levels_at_build.insert(
            StatPath::MassDryKg,
            TrackLevels {
                levels: 2,
                multiplier: 0.9,
            },
        );
        // ...rooted on a fresh session since `s` still has no root
        // (all adds above failed — allocator untouched).
        let n = s.add_root(&defs, b).unwrap();
        assert_eq!(n, PartNodeId(0));
        // The same record reaching `validate` through a forged file
        // is flagged there too — `ControlTorqueNm` does not resolve on
        // the engine def (no Control module).
        let mut f = file_of(&s);
        f.nodes[0].built.upgrade_levels_at_build.insert(
            StatPath::ControlTorqueNm,
            TrackLevels {
                levels: 1,
                multiplier: 1.1,
            },
        );
        let forged = CraftSession::from_bytes(&encode(&f)).unwrap();
        let v = forged.validate(&defs);
        assert!(
            v.errors.iter().any(|e| matches!(
                e,
                SessionError::InvalidUpgrade {
                    stat: StatPath::ControlTorqueNm,
                    ..
                }
            )),
            "forged unresolvable upgrade must error: {v:?}"
        );
    }

    /// An empty session validates as unlaunchable — `to_part_graph`
    /// refuses it (`EmptySession`) so `validate` must agree (R14-F7).
    #[test]
    fn empty_session_is_a_validation_error() {
        let defs = stack_catalog();
        let v = CraftSession::new().validate(&defs);
        assert!(!v.is_launchable());
        assert_eq!(v.errors, vec![SessionError::EmptySession]);
    }

    /// R14-F8: stage indices are capped — `set_stage` and
    /// `insert_stage` refuse inputs past [`MAX_STAGE_INDEX`], and an
    /// insert with no headroom at the cap fails instead of merging.
    #[test]
    fn stage_indices_are_capped() {
        let (mut s, _, _, tank, eng) = stack();
        assert!(matches!(
            s.set_stage(tank, MAX_STAGE_INDEX + 1),
            Err(SessionError::InvalidStage { .. })
        ));
        assert!(matches!(
            s.set_stage(tank, u32::MAX),
            Err(SessionError::InvalidStage { .. })
        ));
        assert_eq!(s.stage_of(tank), Some(0)); // unchanged on refusal
        assert!(matches!(
            s.insert_stage(MAX_STAGE_INDEX + 1),
            Err(SessionError::InvalidStage { .. })
        ));
        // Legal cap value works.
        s.set_stage(tank, MAX_STAGE_INDEX).unwrap();
        assert_eq!(s.stage_of(tank), Some(MAX_STAGE_INDEX));
        // An insert that would push the capped part past the cap is
        // refused — never saturated into a silent merge.
        assert!(matches!(
            s.insert_stage(0),
            Err(SessionError::InvalidStage { .. })
        ));
        assert_eq!(s.stage_of(tank), Some(MAX_STAGE_INDEX));
        // Inserting at the cap itself also fails while a part sits
        // there — every legal insert preserves it, never merges.
        assert!(matches!(
            s.insert_stage(MAX_STAGE_INDEX),
            Err(SessionError::InvalidStage { .. })
        ));
        // And a normal insert still shifts.
        s.set_stage(tank, 2).unwrap();
        s.set_stage(eng, 3).unwrap();
        s.insert_stage(2).unwrap();
        assert_eq!(s.stage_of(tank), Some(3));
        assert_eq!(s.stage_of(eng), Some(4));
    }

    /// C9-session-F1: synthetic `surf:` ids carry DEF-LOCAL values —
    /// `scaled_attach_node` applies the live scale/stretch at every
    /// use, so a resize moves the mount point instead of stranding it
    /// at the mount-time size.
    #[test]
    fn surface_node_scales_with_the_build() {
        let d = def("surf.host", 1.0, 1.0, vec![]);
        let id = surface_node_id(DVec3::new(1.0, 0.0, 2.0), DVec3::X, 0.5);
        // scale=1: the def-local values pass through.
        let n = scaled_attach_node(&d, &id, 1.0, 1.0).unwrap();
        assert_eq!(n.position_m, DVec3::new(1.0, 0.0, 2.0));
        assert_eq!(n.diameter_m, 0.5);
        // scale=2, stretch=1.5: position·(s, s, s·t), diameter·s —
        // identical rule to authored nodes.
        let n = scaled_attach_node(&d, &id, 2.0, 1.5).unwrap();
        assert_eq!(n.position_m, DVec3::new(2.0, 0.0, 6.0));
        assert_eq!(n.diameter_m, 1.0);
    }

    /// C9-session-F2: `from_file`'s cycle rejection survives the
    /// single-pass rewrite — an orphan 2-cycle still fails `Cycle`
    /// (the reachability walk is now over a prebuilt child index).
    #[test]
    fn from_file_orphan_cycle_still_rejected() {
        let (s, _, _, _, _) = stack();
        let mut f = file_of(&s);
        // Point the root's edge at node 2 — the two-node {1,2} loop is
        // unreachable from root and must still read `Cycle`.
        let nid = PartNodeId(f.nodes[0].id);
        f.nodes[1].edge = Some(AttachEdge::new(nid, "a", "b"));
        f.nodes[0].edge = Some(AttachEdge::new(PartNodeId(f.nodes[1].id), "a", "b"));
        assert!(matches!(
            CraftSession::from_bytes(&encode(&f)),
            Err(CraftFileError::Cycle { .. }) | Err(CraftFileError::BadRootEdge { .. })
        ));
    }

    /// PRD-10 M4: one attach node mates exactly one neighbour —
    /// `add_part` refuses a second edge onto a bound node, `reattach`
    /// refuses both parent- and child-side double binds, and the
    /// moving edge's own old ends are exempt.
    #[test]
    fn attach_node_occupancy_is_enforced() {
        let (mut s, defs, pod, tank, eng) = stack();
        // Second edge onto pod.bottom — already mated to tank.top.
        assert!(matches!(
            s.add_part(
                &defs,
                stock("test.tank"),
                AttachEdge::new(pod, "bottom", "top"),
            ),
            Err(SessionError::AttachNodeOccupied {
                node,
                which: "parent",
                ref node_id,
            }) if node == pod && node_id == "bottom"
        ));
        // Reattach to the slot the moving edge frees is legal.
        s.reattach(&defs, eng, AttachEdge::new(tank, "bottom", "top"))
            .unwrap();
        // Reattach eng onto tank's side? no — onto pod.bottom is
        // occupied (by tank's edge — the MOVED node is eng, not tank).
        assert!(matches!(
            s.reattach(&defs, eng, AttachEdge::new(pod, "bottom", "top")),
            Err(SessionError::AttachNodeOccupied {
                node,
                which: "parent",
                ..
            }) if node == pod
        ));
        // Child side: tank's "bottom" is bound by eng's edge —
        // reattaching tank with child_node "bottom" double-binds it.
        assert!(matches!(
            s.reattach(&defs, tank, AttachEdge::new(pod, "bottom", "bottom")),
            Err(SessionError::AttachNodeOccupied {
                node,
                which: "child",
                ref node_id,
            }) if node == tank && node_id == "bottom"
        ));
        // The parent side alone is still legal when the child node is
        // a free one — tank re-snaps back where it was.
        s.reattach(&defs, tank, AttachEdge::new(pod, "bottom", "top"))
            .unwrap();
    }

    /// The root has no parent edge — reattach reports the typed
    /// `RootImmovable` (the old `WouldCycle` misread as a loop).
    #[test]
    fn reattach_root_is_typed() {
        let (mut s, defs, pod, tank, _) = stack();
        assert!(matches!(
            s.reattach(&defs, pod, AttachEdge::new(tank, "top", "bottom")),
            Err(SessionError::RootImmovable { node }) if node == pod
        ));
    }

    /// `remove_and_reparent` deletes just the mid part — the engine
    /// promotes onto the grandparent's freed slot, re-snapped.
    #[test]
    fn remove_and_reparent_promotes_children() {
        let (mut s, defs, pod, tank, eng) = stack();
        let removed = s.remove_and_reparent(&defs, tank).unwrap();
        assert_eq!(removed, vec![tank]);
        assert_eq!(s.len(), 2);
        // eng's edge now names pod; it kept its child node and landed
        // on the slot the tank vacated (pod.bottom/top: −0.5 − 0.5 = −1.0).
        let e = s.node(eng).unwrap().edge().unwrap().clone();
        assert_eq!(e.parent, pod);
        assert_eq!(e.parent_node, "bottom");
        assert_eq!(e.child_node, "top");
        let t = s.node(eng).unwrap().transform();
        assert_eq!(t.offset, DVec3::new(0.0, 0.0, -1.0));
        // Root refuses — no parent to promote to.
        assert!(matches!(
            s.remove_and_reparent(&defs, pod),
            Err(SessionError::ReparentFailed { node, child: None }) if node == pod
        ));
        // A leaf is a plain remove.
        let (mut s, defs, _, _, eng) = stack();
        assert_eq!(s.remove_and_reparent(&defs, eng).unwrap(), vec![eng]);
        assert_eq!(s.len(), 2);
    }

    /// A crafted file can bypass the ops' occupancy rule — `validate`
    /// flags the double-bound node so it never validates clean.
    #[test]
    fn validate_flags_a_double_bound_node() {
        let (s, defs, pod, _tank, _) = stack();
        let mut f = file_of(&s);
        // Duplicate tank's node record under a fresh id sharing
        // pod.bottom — two edges bind the same attach node.
        let mut dup = f.nodes[1].clone();
        dup.id = f.nodes.iter().map(|n| n.id).max().unwrap() + 1;
        dup.edge = Some(AttachEdge::new(PartNodeId(pod.0), "bottom", "top"));
        f.next_id = dup.id + 1;
        f.nodes.push(dup);
        let s = CraftSession::from_bytes(&encode(&f)).unwrap();
        let v = s.validate(&defs);
        assert!(v.warnings.iter().any(|w| matches!(
            w,
            SessionWarning::AttachNodeOccupied { node, node_id }
                if *node == pod && node_id == "bottom"
        )));
    }
}
