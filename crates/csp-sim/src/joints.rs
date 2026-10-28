//! M6-B: the §2.4 **transient joint** bookkeeping — the live
//! soft-capture joints ([`LiveJoint`]), the docking tunables
//! ([`DockingConfig`]), the per-tick capture detection + lifecycle,
//! and the §7.1 hash walk.
//!
//! §2.4's spec, verbatim: *"Joints are transients only. Live joints
//! exist for: docking soft-capture (≤ a few seconds), robotic
//! hinges/pistons, and decoupler-failure cases. Hard cap 16 live
//! joints per bubble. […] Runtime: joint creation beyond the cap
//! degrades to an immediate clean separation (no joint) and logs a
//! `Recoverable` error."* And §2.4's docking resolution, step 1:
//! *"Approach: the two bodies are a jointed island solved for the
//! soft-capture window (≤ `balance.ron: capture_window_ticks`,
//! default 192 = 3 s)."* (§2.9 names `RapierBackend` for the island
//! solve — v1 instead runs the joint inside `SimpleBackend`'s PGS
//! velocity pass; [`csp_phys::JointDef`]'s backend note is the
//! documented reading and this module's behaviour is independent of
//! which solver runs it.)
//!
//! # Lifecycle
//!
//! Per integer tick, inside [`World::step_bubble_range`]
//! (crate-root docs):
//!
//! 1. [`batch_joint_defs`] translates the live joints into the
//!    backend's per-tick `JointDef`s — body-frame anchors re-derived
//!    from each vessel's *current* craft graph, so a mid-window
//!    re-weld (jettison, breakup) can never leave an anchor welded
//!    to a stale frame.
//! 2. The backend solves the batch **with** the joints.
//! 3. [`lifecycle`] runs last in the tick, after
//!    breakup/impact/latch consequences: it dissolves joints whose
//!    window ran out (`formed_tick + capture_window_ticks`), whose
//!    vessel left `InBubble`, or whose port node vanished; then it
//!    scans ascending `InBubble` vessel pairs for new soft-captures.
//!
//! # State class (§7.2/§7.4)
//!
//! Live joints are **hashed but never serialized**: they change the
//! next tick's evolution (authoritative sim state →
//! [`hash_joints`]), and §7.4 refuses the save boundary while any
//! live joint exists, so no snapshot can ever contain one —
//! [`World::restore`] therefore builds them empty. The
//! [`joint_refused`](World) pair set is **hashed AND serialized**:
//! it provably outlives the transients — window expiry inserts a
//! refusal for a still-posed pair *after* the last live joint is
//! gone, which is exactly when the §7.4 boundary opens — and a
//! restore that dropped it would let the next detection pass
//! re-capture the refused pair, diverging the run on its first
//! tick.
//!
//! # The v1 simplifications (documented)
//!
//! - **At most one joint per vessel pair.** Two port pairs between
//!   the same bodies would over-constrain the island for no
//!   capture-fidelity gain; the first qualifying pair in the fixed
//!   enumeration wins.
//! - **The capture predicate is geometric**: matching `PortSize`,
//!   port faces within `capture_range_m`, facing within
//!   `acos(capture_min_alignment_dot)`, and relative speed at the
//!   port points `≤ capture_rel_speed_mps`.
//! - **Expiry is unconditional** in v1: a joint that never settles
//!   dissolves at the window's end — §2.4's clean separation.
//!
//! # M6-C: the latch (§2.4's "docking rigidizes", step 2)
//!
//! Each tick, after every consequence (impact, breakup, eviction)
//! and before the lifecycle's dissolve/detect pass, each live joint
//! is checked against the *settle* predicate: the joint's anchors
//! within `latch_separation_m` AND the relative point speed at them
//! within `latch_rel_speed_mps`. A settled pair **merges**
//! ([`apply_latch`]): the secondary's graph folds into the
//! primary's through [`csp_phys::merge_graphs`], one Stage-1 weld
//! runs synchronously on the merged graph, and the merged vessel
//! records a [`DockSeam`] — `port_a`/`port_b` naming the two sealed
//! port nodes, `seam_node` the seam edge's child (the split point
//! an undock fragments at), `latch_tick`, and the part-local capture
//! points that sealed. The island collapses; the constraint solver
//! stops running it — the §2.4 rigidize, verbatim.
//!
//! Primary/secondary: the **heavier** vessel absorbs (a station
//! absorbs a pod, never the reverse), ties breaking to `vessel_a`
//! (the smaller `VesselId`). The absorbed vessel leaves the arena
//! outright — its seams, temperatures, mask bits, and the seam edge
//! itself are carried into the merged bookkeeping; its id is never
//! reused (the arena counter is monotonic). The merged body keeps
//! the primary's pose (`pos`/`rot` — no drift on the surviving
//! frame) and takes the island's aggregate momentum per §2.9's
//! backend hand-off: `v = p_total / m_total`, and `ω` solving
//! `I_merged·ω = L` about the merged CoM — conserved exactly, which
//! is what the merge+split conservation test checks.
//!
//! [`DockSeam`] records are **hashed AND serialized** on the vessel
//! (§7.2's explicit list) — unlike `LiveJoint`s they outlive the
//! transient that produced them: a station carries one seam per
//! docked segment forever, and §2.4's undock step reads them.

use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use serde::{Deserialize, Serialize};

use csp_core::{DVec3, SimError, StateHasher};
use csp_data::{BalanceDef, PortSize};
use csp_phys::{BodyHandle, JointDef, JointKind, PHYSICS_DT, PartNodeId, PartTransform};

use crate::scripts::AutopilotState;
use crate::transfers::{ActiveTransfer, TransferId};
use crate::{ScheduledEvent, Vessel, VesselId, VesselState};

/// §2.4's hard cap mirror for data-less worlds (unit tests, the wasm
/// slice) — same role as [`crate::DEFAULT_MAX_VESSELS`]. The pinned
/// value is 16 live joints per bubble.
pub const DEFAULT_MAX_LIVE_JOINTS: u64 = 16;
/// `balance.ron: capture_window_ticks` mirror — the plan's default
/// 192 = 3 s at 64 Hz.
pub const DEFAULT_CAPTURE_WINDOW_TICKS: u64 = 192;
/// `balance.ron: dock_capture_range_m` mirror.
pub const DEFAULT_CAPTURE_RANGE_M: f64 = 0.5;
/// `balance.ron: dock_capture_angle_deg` mirror — 15° facing cone.
pub const DEFAULT_CAPTURE_ANGLE_DEG: f64 = 15.0;
/// `balance.ron: dock_capture_rel_speed_mps` mirror.
pub const DEFAULT_CAPTURE_REL_SPEED_MPS: f64 = 5.0;
/// `balance.ron: dock_soft_pull_per_s` mirror.
pub const DEFAULT_SOFT_PULL_PER_S: f64 = 8.0;
/// The `soft_pull_per_s` exclusive upper bound (1/s) — the
/// soft-capture reel-in is the explicit map
/// `c_{n+1} ≈ (1 − pull·dt)·c_n` through the solver's unclamped
/// bilateral PGS targets (`csp-phys::simple::solve_joint_rows`), so
/// `pull·dt ≥ 2` gives `|1 − pull·dt| ≥ 1`: the separation never
/// contracts — a bounded limit cycle injecting a full-strength
/// impulse every tick, and (with the shipped
/// `soft_max_correction_mps > latch_rel_speed_mps`) a joint that can
/// never read settled. Derived from the pinned timestep rather than
/// named as a magic number — `2/dt` at 64 Hz is 128/s; the
/// alternating-overshoot band below it (`pull·dt ≥ 1`, i.e. ≥ 64/s)
/// converges but is not the authored regime either.
/// `csp-data::validate_balance` mirrors this bound file-side — the
/// dependency runs `csp-sim → csp-data`, so the mirror is the only
/// direction available.
pub const SOFT_PULL_LIMIT_PER_S: f64 = 2.0 / PHYSICS_DT;
/// `balance.ron: dock_soft_max_correction_mps` mirror.
pub const DEFAULT_SOFT_MAX_CORRECTION_MPS: f64 = 1.0;
/// `balance.ron: dock_latch_separation_m` mirror.
pub const DEFAULT_LATCH_SEPARATION_M: f64 = 0.1;
/// `balance.ron: dock_latch_rel_speed_mps` mirror.
pub const DEFAULT_LATCH_REL_SPEED_MPS: f64 = 0.5;

/// The §2.4 docking tunables — construction config (loaded from
/// `balance.ron` in real worlds; the `DEFAULT_*` mirrors otherwise),
/// **serialized and hashed** like `caps`/`event_budget`: every field
/// changes state evolution (capture formation, joint compliance, the
/// cap), so an undocumented divergence here would red the wrong tests.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DockingConfig {
    /// §2.4's hard cap — live transient joints per bubble.
    pub max_live_joints: u64,
    /// `capture_window_ticks` — a joint's maximum life; it dissolves
    /// unmerged at `formed_tick + window` (the latch merge that would
    /// consume it inside the window is M6-C).
    pub capture_window_ticks: u64,
    /// Capture predicate: port faces within this distance (m).
    pub capture_range_m: f64,
    /// Capture predicate: the facing cone as a cosine — capture
    /// requires `dir_a · dir_b ≤ −capture_min_alignment_dot` (the
    /// port directions oppose). In `[0, 1]`; 1 is a perfect-facing
    /// requirement, 0 accepts any orientation.
    pub capture_min_alignment_dot: f64,
    /// Capture predicate: maximum relative speed at the port points
    /// (m/s) — a fast fly-through cannot capture.
    pub capture_rel_speed_mps: f64,
    /// Soft-capture compliance: exponential reel-in rate (1/s) — see
    /// [`JointKind::SoftCapture`]. Bounded above by
    /// [`SOFT_PULL_LIMIT_PER_S`] — the joint solve is an explicit map
    /// and diverges (limit-cycle buzz, never settles) at or past it.
    pub soft_pull_per_s: f64,
    /// Soft-capture compliance: the anchor-closing speed cap (m/s).
    pub soft_max_correction_mps: f64,
    /// M6-C latch settle predicate: the joint's anchors within this
    /// distance (m) count as mated — the first half of "settled".
    pub latch_separation_m: f64,
    /// M6-C latch settle predicate: relative point speed at the
    /// anchors at or below this (m/s) — the second half. Both must
    /// hold for the pair to rigidize inside the window.
    pub latch_rel_speed_mps: f64,
}

impl DockingConfig {
    /// The no-data defaults — the `DEFAULT_*` mirrors, which a
    /// `csp-bench` test pins to `data/balance.ron` so the two can
    /// never drift silently.
    pub const DEFAULT: Self = Self {
        max_live_joints: DEFAULT_MAX_LIVE_JOINTS,
        capture_window_ticks: DEFAULT_CAPTURE_WINDOW_TICKS,
        capture_range_m: DEFAULT_CAPTURE_RANGE_M,
        capture_min_alignment_dot: -1.0, // replaced below — see note
        capture_rel_speed_mps: DEFAULT_CAPTURE_REL_SPEED_MPS,
        soft_pull_per_s: DEFAULT_SOFT_PULL_PER_S,
        soft_max_correction_mps: DEFAULT_SOFT_MAX_CORRECTION_MPS,
        latch_separation_m: DEFAULT_LATCH_SEPARATION_M,
        latch_rel_speed_mps: DEFAULT_LATCH_REL_SPEED_MPS,
    };

    /// The `balance.ron` → config conversion (§8.3 — the file is the
    /// tunable source of truth). The authored facing *angle* becomes
    /// the stored *cosine*: `cos(deg·π/180)`. The authored domain is
    /// `(0°, 90°]` — outside it the cosine leaves the `[0, 1]`
    /// validity domain [`Self::is_valid`] checks, and the bad value
    /// would only surface at snapshot restore *after* running — so
    /// the conversion clamps to the valid edge instead of carrying
    /// an invalid config (the degenerate `0°` edge — perfect facing
    /// required — stays valid; non-finite input passes `clamp`
    /// through and `is_valid` still catches it).
    ///
    /// The remaining fields copy verbatim — the authored domain is
    /// [`Self::is_valid`]'s, enforced file-side by
    /// `csp_data::validate_balance` (which mirrors the
    /// [`SOFT_PULL_LIMIT_PER_S`] bound) and install-side by
    /// [`crate::World::set_docking_config`]'s rejection.
    pub fn from_balance(b: &BalanceDef) -> Self {
        Self {
            max_live_joints: b.max_live_joints_per_bubble,
            capture_window_ticks: b.capture_window_ticks,
            capture_range_m: b.dock_capture_range_m,
            capture_min_alignment_dot: alignment_dot(b.dock_capture_angle_deg.clamp(0.0, 90.0)),
            capture_rel_speed_mps: b.dock_capture_rel_speed_mps,
            soft_pull_per_s: b.dock_soft_pull_per_s,
            soft_max_correction_mps: b.dock_soft_max_correction_mps,
            latch_separation_m: b.dock_latch_separation_m,
            latch_rel_speed_mps: b.dock_latch_rel_speed_mps,
        }
    }

    /// The install/restore gate: corrupt config fails typed rather
    /// than silently diverging the run (`NaN` in the predicate would
    /// make every comparison false — a silent capture-off; a
    /// `soft_pull_per_s` at or past [`SOFT_PULL_LIMIT_PER_S`] would
    /// put the joint solve into a permanent limit cycle). Checked at
    /// snapshot restore and by [`crate::World::set_docking_config`].
    pub fn is_valid(&self) -> bool {
        self.capture_range_m.is_finite()
            && self.capture_range_m >= 0.0
            && self.capture_min_alignment_dot.is_finite()
            && (0.0..=1.0).contains(&self.capture_min_alignment_dot)
            && self.capture_rel_speed_mps.is_finite()
            && self.capture_rel_speed_mps >= 0.0
            && self.soft_pull_per_s.is_finite()
            && self.soft_pull_per_s >= 0.0
            && self.soft_pull_per_s < SOFT_PULL_LIMIT_PER_S
            && self.soft_max_correction_mps.is_finite()
            && self.soft_max_correction_mps >= 0.0
            && self.latch_separation_m.is_finite()
            && self.latch_separation_m >= 0.0
            && self.latch_rel_speed_mps.is_finite()
            && self.latch_rel_speed_mps >= 0.0
    }

    /// The `JointKind` a capture under this config creates — kept on
    /// the joint so a later config change (there is none in v1) could
    /// never retroactively change a live joint's law.
    fn joint_kind(&self) -> JointKind {
        JointKind::SoftCapture {
            pull_per_s: self.soft_pull_per_s,
            max_correction_mps: self.soft_max_correction_mps,
        }
    }
}

/// `cos(deg·π/180)` via libm — the §2.7 rule applied to the one
/// transcendental this module needs (config conversion only; the
/// per-tick predicate compares dots, never angles).
fn alignment_dot(deg: f64) -> f64 {
    libm::cos(deg * (core::f64::consts::PI / 180.0))
}

/// `DockingConfig::DEFAULT` with the angle field resolved — a `const`
/// cannot call `libm::cos`, so the canonical default lives here. The
/// `DEFAULT` associated const keeps a `−1.0` sentinel in the dot field;
/// [`default_config`] is what construction paths use.
pub fn default_config() -> DockingConfig {
    DockingConfig {
        capture_min_alignment_dot: alignment_dot(DEFAULT_CAPTURE_ANGLE_DEG),
        ..DockingConfig::DEFAULT
    }
}

/// One live transient joint — §2.4's "joints are transients only".
/// Created when two `InBubble` vessels' port faces satisfy the
/// capture predicate at the end of a bubble tick; solved inside the
/// next tick's `backend.step` batch; dissolved on window expiry,
/// vessel departure, or a lost port node. **Hashed, never
/// serialized** — see the module doc's state-class note (no serde
/// derives: nothing may write one to a snapshot).
#[derive(Clone, Debug, PartialEq)]
pub struct LiveJoint {
    /// The two vessels — `vessel_a < vessel_b`, the canonical pair
    /// order (also the `batch_vids` order: the batch is ascending-id).
    pub vessel_a: VesselId,
    pub vessel_b: VesselId,
    /// The port-face graph nodes on each vessel — kept for the M6-C
    /// `DockSeam` record, and used to re-derive the body-frame anchor
    /// every tick: a re-weld (jettison, breakup) changes the frame,
    /// and a vanished node is itself a dissolution trigger.
    pub node_a: PartNodeId,
    pub node_b: PartNodeId,
    /// Port capture points in each node's part-local frame — the
    /// resolved `csp_phys::DockPortSpec::local_pos`.
    pub local_a: DVec3,
    pub local_b: DVec3,
    /// The matched size class — carried for the seam record.
    pub size: PortSize,
    /// The constraint law + compliance, handed to the backend
    /// verbatim each tick.
    pub kind: JointKind,
    /// The tick the joint formed — the §2.4 capture window counts
    /// from here.
    pub formed_tick: u64,
}

/// One port face in the bubble frame — the capture predicate's view
/// of a `DockPortSpec` on an `InBubble` body.
struct PortFace {
    /// The graph node carrying the port (for [`LiveJoint::node_a`]).
    node: PartNodeId,
    /// The port-face capture point, part-local — the joint anchor's
    /// stored form (re-posed through `body_transforms` each tick).
    local: DVec3,
    /// World capture point this tick.
    pos_w: DVec3,
    /// World facing direction — the node's unit `direction` re-posed.
    dir_w: DVec3,
    /// The size class (matched at formation).
    size: PortSize,
}

/// A vessel's port faces in world frame — empty for craft-less,
/// non-`InBubble`, or port-less vessels (the common fast path: one
/// `any` scan before `body_transforms` is even built). Faces
/// enumerate in ascending `(PartNodeId, dock_ports index)` — the
/// detection order's first half.
fn world_ports(vessel: &Vessel) -> Result<Vec<PortFace>, SimError> {
    let (Some(craft), Some(body)) = (&vessel.craft, vessel.body()) else {
        return Ok(Vec::new());
    };
    if !craft.parts.iter().any(|p| !p.dock_ports.is_empty()) {
        return Ok(Vec::new());
    }
    // Same error class as the external-loads path: a craft that
    // welded cannot fail `body_transforms` — a failure is corrupted
    // state, not bad input.
    let transforms = csp_phys::weld::body_transforms(&craft.graph, &craft.parts)
        .map_err(|e| SimError::Fatal(e.reason()))?;
    let mut out = Vec::new();
    for (id, node) in &craft.graph.nodes {
        let Some(part) = craft.parts.get(node.effective_index as usize) else {
            continue;
        };
        let t = &transforms[id];
        for spec in &part.dock_ports {
            out.push(PortFace {
                node: *id,
                local: spec.local_pos,
                pos_w: body.pos + body.rot.rotate(t.apply(spec.local_pos)),
                dir_w: body.rot.rotate(t.apply_dir(spec.local_dir)),
                size: spec.size,
            });
        }
    }
    Ok(out)
}

/// Relative point speed at two port faces (m/s): each body's anchor
/// point velocity `v + ω_world × (p − x̄)` — the face-relative speed
/// a closing vessel actually shows its mate.
fn port_rel_speed(a: &csp_phys::WeldedBody, pa: DVec3, b: &csp_phys::WeldedBody, pb: DVec3) -> f64 {
    let va = a.vel + a.rot.rotate(a.ang_vel).cross(pa - a.com_world());
    let vb = b.vel + b.rot.rotate(b.ang_vel).cross(pb - b.com_world());
    (va - vb).norm()
}

/// The full §-predicate over one port pair: matching size class,
/// faces within range, opposing directions inside the facing cone,
/// and the closing-speed gate.
fn capture_match(
    fa: &PortFace,
    fb: &PortFace,
    a: &csp_phys::WeldedBody,
    b: &csp_phys::WeldedBody,
    cfg: &DockingConfig,
) -> bool {
    fa.size == fb.size
        && fa.pos_w.distance(fb.pos_w) <= cfg.capture_range_m
        && fa.dir_w.dot(fb.dir_w) <= -cfg.capture_min_alignment_dot
        && port_rel_speed(a, fa.pos_w, b, fb.pos_w) <= cfg.capture_rel_speed_mps
}

/// The first capture-eligible port pair between two vessels in the
/// fixed `(node_a, port_a index, node_b, port_b index)` enumeration
/// — `None` when no pair satisfies the predicate.
fn find_capture<'a>(
    pa: &'a [PortFace],
    pb: &'a [PortFace],
    a: &csp_phys::WeldedBody,
    b: &csp_phys::WeldedBody,
    cfg: &DockingConfig,
) -> Option<(&'a PortFace, &'a PortFace)> {
    for fa in pa {
        for fb in pb {
            if capture_match(fa, fb, a, b, cfg) {
                return Some((fa, fb));
            }
        }
    }
    None
}

/// Per-tick translation of the world's live joints into the backend's
/// `JointDef`s for this `step` call. `batch_vids` is the batch's
/// ascending-id layout — the `BodyHandle` order. A joint whose vessel
/// is not in the batch, or whose port node vanished from the craft,
/// contributes nothing this tick; the end-of-tick [`lifecycle`]
/// dissolves it. Returned in live-joint order — the backend re-sorts
/// into its own fixed order anyway (see `generate_joint_constraints`).
pub fn batch_joint_defs(
    joints: &[LiveJoint],
    vessels: &BTreeMap<VesselId, Vessel>,
    batch_vids: &[VesselId],
) -> Result<Vec<JointDef>, SimError> {
    if joints.is_empty() {
        return Ok(Vec::new());
    }
    let index: BTreeMap<VesselId, u32> = batch_vids
        .iter()
        .enumerate()
        .map(|(i, v)| (*v, i as u32))
        .collect();
    // `body_transforms` once per jointed vessel — the anchor's
    // body-frame pose follows the *current* craft, so a mid-window
    // re-weld is handled by construction.
    let mut transforms: BTreeMap<VesselId, BTreeMap<PartNodeId, PartTransform>> = BTreeMap::new();
    for j in joints {
        for vid in [j.vessel_a, j.vessel_b] {
            if index.contains_key(&vid)
                && !transforms.contains_key(&vid)
                && let Some(v) = vessels.get(&vid)
                && let Some(craft) = &v.craft
            {
                transforms.insert(
                    vid,
                    csp_phys::weld::body_transforms(&craft.graph, &craft.parts)
                        .map_err(|e| SimError::Fatal(e.reason()))?,
                );
            }
        }
    }
    let mut out = Vec::with_capacity(joints.len());
    for j in joints {
        let (Some(&ia), Some(&ib)) = (index.get(&j.vessel_a), index.get(&j.vessel_b)) else {
            continue;
        };
        let (Some(ta), Some(tb)) = (
            transforms.get(&j.vessel_a).and_then(|m| m.get(&j.node_a)),
            transforms.get(&j.vessel_b).and_then(|m| m.get(&j.node_b)),
        ) else {
            continue;
        };
        out.push(JointDef {
            body_a: BodyHandle(ia),
            body_b: BodyHandle(ib),
            local_anchor_a: ta.apply(j.local_a),
            local_anchor_b: tb.apply(j.local_b),
            kind: j.kind,
        });
    }
    Ok(out)
}

/// The end-of-tick joint lifecycle: dissolve dead joints, prune stale
/// refusals, then detect new soft-captures under the §2.4 cap.
/// `tick` is the tick that just completed (`t + 1` in the range loop —
/// joint ages are measured in completed ticks).
pub fn lifecycle(
    vessels: &BTreeMap<VesselId, Vessel>,
    joints: &mut Vec<LiveJoint>,
    refused: &mut BTreeSet<(VesselId, VesselId)>,
    cfg: &DockingConfig,
    tick: u64,
    degraded: &mut Vec<SimError>,
) -> Result<(), SimError> {
    // 1. Dissolve. A joint lives only while the capture window runs,
    // both vessels are `InBubble`, and both port nodes still exist in
    // their crafts. (Vessel removal — impact, debris eviction,
    // breakup of the whole graph — fails the `InBubble` check.)
    // A joint that dies of **window expiry** — not departure — joins
    // the refused set: the transient's clean separation is a *dead
    // approach*, and without that record the very next detection pass
    // would re-capture the still-posed pair so the "transient" would
    // never end. (M6-C's latch consumes a successful capture inside
    // the window, so expiry only ever means the approach failed.)
    let mut expired: Vec<(VesselId, VesselId)> = Vec::new();
    joints.retain(|j| {
        let alive = port_alive(vessels.get(&j.vessel_a), j.node_a)
            && port_alive(vessels.get(&j.vessel_b), j.node_b);
        if alive && tick >= j.formed_tick + cfg.capture_window_ticks {
            expired.push((j.vessel_a, j.vessel_b));
        }
        alive && tick < j.formed_tick + cfg.capture_window_ticks
    });
    for pair in expired {
        refused.insert(pair);
    }
    // 2. Refusal pruning: a refused pair stays refused only while it
    //    remains inside capture geometry — the §2.4 degradation is a
    //    clean separation *for that approach*. Once the pair exits
    //    the predicate a later re-approach re-attempts (and re-refuses
    //    if the cap is still full — one `Recoverable` per approach,
    //    never a per-tick storm).
    let mut ports_cache: BTreeMap<VesselId, Vec<PortFace>> = BTreeMap::new();
    refused.retain(|&(va, vb)| {
        pair_has_capture(vessels, &mut ports_cache, cfg, va, vb).unwrap_or(false)
    });
    // 3. Detection, ascending `(vessel_a, vessel_b)` — the §2.2 fixed
    //    order. At most one joint per pair (v1): the first eligible
    //    port pair wins.
    let jointed: BTreeSet<(VesselId, VesselId)> =
        joints.iter().map(|j| (j.vessel_a, j.vessel_b)).collect();
    for (i, (va, a)) in vessels.iter().enumerate() {
        if !matches!(a.state, VesselState::InBubble(_)) {
            continue;
        }
        for (vb, b) in vessels.iter().skip(i + 1) {
            if !matches!(b.state, VesselState::InBubble(_))
                || jointed.contains(&(*va, *vb))
                || refused.contains(&(*va, *vb))
            {
                continue;
            }
            let Some((fa, fb)) = pair_capture(vessels, &mut ports_cache, cfg, *va, *vb)? else {
                continue;
            };
            let pair = (*va, *vb);
            if joints.len() as u64 >= cfg.max_live_joints {
                // §2.4, verbatim: "joint creation beyond the cap
                // degrades to an immediate clean separation (no
                // joint) and logs a Recoverable error" — the pair is
                // refused for this approach (recorded so the log
                // fires once, not per tick); the hull-contact
                // machinery does the separating.
                refused.insert(pair);
                degraded.push(SimError::Recoverable(
                    "joint cap reached: soft-capture degraded to clean separation",
                ));
                continue;
            }
            joints.push(LiveJoint {
                vessel_a: *va,
                vessel_b: *vb,
                node_a: fa.node,
                node_b: fb.node,
                local_a: fa.local,
                local_b: fb.local,
                size: fa.size,
                kind: cfg.joint_kind(),
                formed_tick: tick,
            });
        }
    }
    Ok(())
}

/// A joint's port is alive while its vessel is `InBubble` and the
/// port node still exists in the craft graph (a jettisoned or
/// destroyed port kills the joint; a surviving node keeps it across
/// re-welds — the anchor is re-posed, not remembered).
fn port_alive(vessel: Option<&Vessel>, node: PartNodeId) -> bool {
    vessel
        .and_then(|v| v.craft.as_ref())
        .is_some_and(|c| c.graph.nodes.contains_key(&node))
        && vessel.is_some_and(|v| matches!(v.state, VesselState::InBubble(_)))
}

/// `ports_cache` lazy fill — computes a vessel's world port faces
/// once per lifecycle call.
fn ensure_ports(
    ports_cache: &mut BTreeMap<VesselId, Vec<PortFace>>,
    vessels: &BTreeMap<VesselId, Vessel>,
    vid: VesselId,
) -> Result<(), SimError> {
    if let std::collections::btree_map::Entry::Vacant(e) = ports_cache.entry(vid) {
        let faces = vessels
            .get(&vid)
            .map_or_else(|| Ok(Vec::new()), world_ports)?;
        e.insert(faces);
    }
    Ok(())
}

/// The capture candidate for a vessel pair — the bodies + port lists
/// resolved through the cache. Shared by refusal pruning
/// ([`pair_has_capture`]) and detection ([`pair_capture`]).
fn pair_capture(
    vessels: &BTreeMap<VesselId, Vessel>,
    ports_cache: &mut BTreeMap<VesselId, Vec<PortFace>>,
    cfg: &DockingConfig,
    va: VesselId,
    vb: VesselId,
) -> Result<Option<(PortFace, PortFace)>, SimError> {
    let (Some(a), Some(b)) = (vessels.get(&va), vessels.get(&vb)) else {
        return Ok(None);
    };
    let (Some(ba), Some(bb)) = (a.body(), b.body()) else {
        return Ok(None);
    };
    // Fill both entries through sequential mutable borrows, then read
    // both through shared borrows — the map cannot lend `pa` and `pb`
    // mutably at once.
    ensure_ports(ports_cache, vessels, va)?;
    ensure_ports(ports_cache, vessels, vb)?;
    let pa = &ports_cache[&va][..];
    let pb = &ports_cache[&vb][..];
    Ok(find_capture(pa, pb, ba, bb, cfg).map(|(fa, fb)| {
        (
            PortFace {
                node: fa.node,
                local: fa.local,
                pos_w: fa.pos_w,
                dir_w: fa.dir_w,
                size: fa.size,
            },
            PortFace {
                node: fb.node,
                local: fb.local,
                pos_w: fb.pos_w,
                dir_w: fb.dir_w,
                size: fb.size,
            },
        )
    }))
}

/// Does any port pair of `(va, vb)` satisfy the capture predicate?
/// The refusal set's pruning test — infallible-facing wrapper: a
/// vessel that cannot produce transforms prunes its refusal rather
/// than wedging it (the same call site surfaces the `Fatal` on the
/// detection path when the pair is actually live).
fn pair_has_capture(
    vessels: &BTreeMap<VesselId, Vessel>,
    ports_cache: &mut BTreeMap<VesselId, Vec<PortFace>>,
    cfg: &DockingConfig,
    va: VesselId,
    vb: VesselId,
) -> Result<bool, SimError> {
    pair_capture(vessels, ports_cache, cfg, va, vb).map(|o| o.is_some())
}

/// The §7.1 walk for the live-joint state — hashed (it changes the
/// next tick's evolution). The live joints are never serialized
/// (§7.4 refuses the boundary while any exist, so the serialized
/// stream can never contain one); the refused pairs ARE — they can
/// outlive the last joint (see the module doc's state-class note).
pub fn hash_joints(
    h: &mut StateHasher,
    joints: &[LiveJoint],
    refused: &BTreeSet<(VesselId, VesselId)>,
) {
    h.u32(joints.len() as u32);
    for j in joints {
        h.u64(j.vessel_a.0);
        h.u64(j.vessel_b.0);
        h.u64(j.node_a.0);
        h.u64(j.node_b.0);
        h.dvec3(j.local_a);
        h.dvec3(j.local_b);
        h.stable_enum(&j.size);
        h.stable_enum(&j.kind);
        match j.kind {
            JointKind::SoftCapture {
                pull_per_s,
                max_correction_mps,
            } => {
                h.f64(pull_per_s);
                h.f64(max_correction_mps);
            }
        }
        h.u64(j.formed_tick);
    }
    h.u32(refused.len() as u32);
    for (a, b) in refused {
        h.u64(a.0);
        h.u64(b.0);
    }
}

// ------------------------------------------------------------------
// M6-C: the latch merge (§2.4 "docking rigidizes", step 2)
// ------------------------------------------------------------------

/// §2.4's recorded seam — one per latched pair, kept on the merged
/// vessel for its life. `port_a`/`port_b` are the merged-graph ids of
/// the two nodes carrying the sealed ports; `seam_node` is the seam
/// edge's child — the absorbed segment's subtree top in the merged
/// graph — so `csp_phys::subtree_set(graph, seam_node)` is exactly
/// what an `Undock` at this seam detaches via the breakup path.
///
/// **Hashed AND serialized** (§7.2's explicit list): unlike
/// [`LiveJoint`]s the record outlives the transient — a 1000-part
/// station is one rigid body with ~30 of these. Undock consumes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DockSeam {
    /// The retaining (primary) side's port-carrying node — the seam
    /// edge's parent.
    pub port_a: PartNodeId,
    /// The absorbed (secondary) side's port-carrying node — the
    /// merged-graph id, not its original.
    pub port_b: PartNodeId,
    /// The seam edge's child: the absorbed segment's former root,
    /// remapped — the subtree top the undock split fragments at.
    pub seam_node: PartNodeId,
    /// The capture points that sealed, in each port node's part-local
    /// frame (`local_a` on the primary side, `local_b` on the
    /// secondary) — identifies which face on a multi-port part
    /// sealed, mirroring [`LiveJoint`]'s anchors.
    pub local_a: DVec3,
    /// See [`Self::local_a`].
    pub local_b: DVec3,
    /// The tick the latch merged at (`t + 1` of the settling tick —
    /// the same completed-tick convention [`LiveJoint::formed_tick`]).
    pub latch_tick: u64,
}

/// The settle predicate's view of one live joint: where its anchors
/// sit this tick. `None` when a port node or body is unavailable —
/// the caller treats it as "not settled" (the lifecycle's own
/// dissolution owns the dead-joint case).
fn joint_anchors(
    vessels: &BTreeMap<VesselId, Vessel>,
    j: &LiveJoint,
    transforms: &mut BTreeMap<VesselId, BTreeMap<PartNodeId, PartTransform>>,
) -> Result<Option<(csp_phys::WeldedBody, DVec3, csp_phys::WeldedBody, DVec3)>, SimError> {
    let mut bodies = [None, None];
    let mut anchors = [DVec3::ZERO, DVec3::ZERO];
    for (k, (vid, node, local)) in [
        (j.vessel_a, j.node_a, j.local_a),
        (j.vessel_b, j.node_b, j.local_b),
    ]
    .iter()
    .enumerate()
    {
        let Some(v) = vessels.get(vid) else {
            return Ok(None);
        };
        let (Some(craft), Some(body)) = (&v.craft, v.body()) else {
            return Ok(None);
        };
        if let std::collections::btree_map::Entry::Vacant(e) = transforms.entry(*vid) {
            e.insert(
                csp_phys::weld::body_transforms(&craft.graph, &craft.parts)
                    .map_err(|e| SimError::Fatal(e.reason()))?,
            );
        }
        let Some(t) = transforms[vid].get(node) else {
            return Ok(None);
        };
        anchors[k] = body.pos + body.rot.rotate(t.apply(*local));
        bodies[k] = Some(body.clone());
    }
    let (Some(a), Some(b)) = (bodies[0].clone(), bodies[1].clone()) else {
        return Ok(None);
    };
    Ok(Some((a, anchors[0], b, anchors[1])))
}

/// §2.4's rigidize predicate for one live joint: the anchors have
/// been reeled within `latch_separation_m` AND the relative point
/// speed at them is within `latch_rel_speed_mps`. Both gates are
/// measured on this tick's post-step poses — a settled pair merges
/// inside the capture window rather than expiring with it.
pub fn latch_ready(
    vessels: &BTreeMap<VesselId, Vessel>,
    j: &LiveJoint,
    cfg: &DockingConfig,
) -> Result<bool, SimError> {
    let mut transforms = BTreeMap::new();
    let Some((a, pa, b, pb)) = joint_anchors(vessels, j, &mut transforms)? else {
        return Ok(false);
    };
    Ok(pa.distance(pb) <= cfg.latch_separation_m
        && port_rel_speed(&a, pa, &b, pb) <= cfg.latch_rel_speed_mps)
}

/// The §2.4 latch merge: fold the joint's secondary vessel into the
/// primary — one `WeldedBody` with a recorded [`DockSeam`]. See the
/// module doc for primary/secondary assignment, the momentum stamp,
/// and the bookkeeping. `tick` is the completed tick (`t + 1` of the
/// settling tick) — `DockSeam::latch_tick`'s convention.
///
/// Returns `true` when the merge ran. `false` when the joint's
/// vessels no longer qualify (one left the bubble, broke up, or was
/// merged away by an earlier latch this tick) — the caller's
/// lifecycle dissolves the dead joint. A merge that cannot run
/// (corrupt seam inputs) degrades to the transient's clean
/// separation — joint removed, pair refused, `Recoverable` logged —
/// the §2.4 rule for a transient that cannot complete, never a stall.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
pub(crate) fn apply_latch(
    vessels: &mut BTreeMap<VesselId, Vessel>,
    physics: &mut crate::Physics,
    joints: &mut Vec<LiveJoint>,
    refused: &mut BTreeSet<(VesselId, VesselId)>,
    tracked: &mut BTreeSet<VesselId>,
    event_heap: &BinaryHeap<ScheduledEvent>,
    transfers: &mut BTreeMap<TransferId, ActiveTransfer>,
    autopilots: &mut BTreeMap<VesselId, AutopilotState>,
    science: &mut crate::research::ScienceState,
    economy: &mut crate::economy::EconomyState,
    joint: &LiveJoint,
    tick: u64,
    degraded: &mut Vec<SimError>,
    shadow: &mut crate::TickShadow,
) -> Result<bool, SimError> {
    let (Some(a), Some(b)) = (vessels.get(&joint.vessel_a), vessels.get(&joint.vessel_b)) else {
        return Ok(false);
    };
    let (Some(ba), Some(bb)) = (a.body(), b.body()) else {
        return Ok(false);
    };
    // Primary/secondary by mass — the heavier absorbs; ties to the
    // smaller VesselId (canonical order).
    let (prim, sec) = if bb.mass_kg > ba.mass_kg {
        (joint.vessel_b, joint.vessel_a)
    } else {
        (joint.vessel_a, joint.vessel_b)
    };
    let (node_p, local_p) = if prim == joint.vessel_a {
        (joint.node_a, joint.local_a)
    } else {
        (joint.node_b, joint.local_b)
    };
    let (node_s, local_s) = if prim == joint.vessel_a {
        (joint.node_b, joint.local_b)
    } else {
        (joint.node_a, joint.local_a)
    };
    let Some(vp) = vessels.get(&prim) else {
        return Ok(false);
    };
    let Some(vs) = vessels.get(&sec) else {
        return Ok(false);
    };
    let (Some(craft_p), Some(craft_s)) = (vp.craft.clone(), vs.craft.clone()) else {
        return Ok(false);
    };
    let (Some(bp), Some(bs)) = (vp.body().cloned(), vs.body().cloned()) else {
        return Ok(false);
    };
    // The secondary's graph-frame pose inside the primary's —
    // inverse pose composition in PartTransform form. Computed once:
    // it feeds both the seam (below) and the momentum stamp (which
    // needs `rel`'s rotation and the secondary CoM mapped into the
    // merged frame).
    let rel = PartTransform::new(
        bp.rot.conjugate().rotate(bs.pos - bp.pos),
        (bp.rot.conjugate() * bs.rot).normalize(),
    );
    // …into the port node's frame: `seam = t_port⁻¹ ∘ rel`. The seam
    // is the secondary's *graph frame* inside `attach_node`'s frame —
    // `merge_graphs` itself composes the secondary ROOT's stored
    // transform onto it (`seam ∘ t'_root`), because `t'_root` is not
    // identity in general: `fragment_graph_of` writes the subtree's
    // merged-frame pose into the fragment root at undock, and
    // dropping it would rigidly displace the whole absorbed segment.
    let transforms_p = csp_phys::weld::body_transforms(&craft_p.graph, &craft_p.parts)
        .map_err(|e| SimError::Fatal(e.reason()))?;
    let Some(t_port) = transforms_p.get(&node_p).copied() else {
        // The port node vanished mid-tick — the lifecycle owns
        // the dissolution; nothing to merge.
        return Ok(false);
    };
    let mut merge = match csp_phys::merge_graphs(
        &craft_p.graph,
        &craft_s.graph,
        node_p,
        node_s,
        t_port.inverse().compose(rel),
        u32::try_from(craft_p.parts.len())
            .map_err(|_| SimError::Fatal("latch: part count exceeds u32::MAX"))?,
    ) {
        Ok(m) => m,
        Err(e) => {
            // Corrupt seam inputs — degrade to the §2.4 clean
            // separation (remove the joint, refuse the pair, log
            // once) rather than stalling the window or panicking.
            joints.retain(|j| j != joint);
            refused.insert((joint.vessel_a, joint.vessel_b));
            degraded.push(SimError::Recoverable(e.reason()));
            return Ok(false);
        }
    };
    // C14-F4: the seam edge's joint point is the primary-side sealed
    // anchor — `local_p` is already in `attach_node`'s (the seam
    // edge's parent's) local frame, exactly the `PartNode::joint`
    // convention `joint_loads` reads for the torque arm.
    if let Some(n) = merge.graph.nodes.get_mut(&merge.seam_node) {
        n.joint = Some(local_p);
    }
    let mut parts_m = craft_p.parts.clone();
    parts_m.extend(craft_s.parts.iter().cloned());
    // M7-C: the frozen part-cost table merges positionally exactly
    // like `parts` — a node's cost rides its `effective_index`.
    let mut part_costs_m = craft_p.part_costs.clone();
    part_costs_m.extend(craft_s.part_costs.iter().cloned());
    // M9b: the frozen spec table merges positionally exactly like
    // `parts`/`part_costs` — same `effective_index` convention — but
    // only when BOTH halves carry a table covering their own `parts`.
    // A specless half absorbed by a spec'd one has no positional
    // representation (the absorbed side's specs would bind at the
    // other half's `effective_index` slots). Dropping to an empty
    // table was the old answer — but the loss is PERMANENT: every
    // reader gates on `specs_parallel`, and the undock/breakup split
    // clones the emptied table into both fragments, so one
    // mixed-spec latch silently killed the §3.4 model for both
    // halves forever. The merge refuses the pair instead — the same
    // clean-separation degradation a corrupt seam takes — so a
    // spec'd craft keeps its model rather than losing it silently.
    let p_specd = crate::reliability::specs_parallel(&craft_p);
    let s_specd = crate::reliability::specs_parallel(&craft_s);
    if p_specd != s_specd {
        joints.retain(|j| j != joint);
        refused.insert((joint.vessel_a, joint.vessel_b));
        degraded.push(SimError::Recoverable(
            "latch: mixed reliability-spec coverage is unmergeable",
        ));
        return Ok(false);
    }
    let reliability_table_m = if p_specd && s_specd {
        let mut t = craft_p.reliability.clone();
        t.extend(craft_s.reliability.iter().cloned());
        t
    } else {
        Vec::new()
    };
    // M7-B: the identity table merges by the same rule — both halves
    // must carry a table covering their own `parts` for the merged
    // positions to bind (the reliability table's precedent); either
    // half anonymous drops the record, never a wrong binding.
    let part_ids_m = if crate::reliability::part_ids_parallel(&craft_p)
        && crate::reliability::part_ids_parallel(&craft_s)
    {
        let mut t = craft_p.part_ids.clone();
        t.extend(craft_s.part_ids.iter().cloned());
        t
    } else {
        Vec::new()
    };
    let mut body_m =
        csp_phys::weld(&merge.graph, &parts_m).map_err(|e| SimError::Fatal(e.reason()))?;
    // §2.9's hand-off: the merged body keeps the primary's pose and
    // takes the island's aggregate momentum. `vel` is the CoM
    // velocity on both bodies, so `p_total` is direct; `ω` solves
    // `I_m·ω = L` about the merged CoM — the angular bookkeeping is
    // done in the primary's frame (rotations conjugate cleanly).
    let (m_p, m_s) = (bp.mass_kg, bs.mass_kg);
    let rel_rot = rel.rotation;
    let com_s_in_m = rel.apply(bs.com_local);
    let r_p = bp.com_local - body_m.com_local;
    let r_s = com_s_in_m - body_m.com_local;
    let v_p_local = bp.rot.conjugate().rotate(bp.vel);
    let v_s_local = bp.rot.conjugate().rotate(bs.vel);
    let l = bp.inertia_local.mul_vec(bp.ang_vel)
        + r_p.cross(v_p_local * m_p)
        + bs.inertia_local
            .rotate(rel_rot)
            .mul_vec(rel_rot.rotate(bs.ang_vel))
        + r_s.cross(v_s_local * m_s);
    body_m.pos = bp.pos;
    body_m.rot = bp.rot;
    body_m.vel = (bp.vel * m_p + bs.vel * m_s) / (m_p + m_s);
    body_m.ang_vel = body_m
        .inertia_local
        .inverse()
        .map(|i| i.mul_vec(l))
        .unwrap_or(DVec3::ZERO);
    body_m.throttle = bp.throttle;
    // A merged body is a fresh rigid body — settle restarts (the
    // primary's countdown cannot carry across a mass-property change).
    body_m.settle = csp_phys::SettleState::default();
    // The seam record (§7.2): which two ports sealed, the split
    // point, the latch tick.
    let seam = DockSeam {
        port_a: node_p,
        port_b: merge.port_b,
        seam_node: merge.seam_node,
        local_a: local_p,
        local_b: local_s,
        latch_tick: tick,
    };
    // The absorbed vessel's own seam records travel with the merge —
    // every PartNodeId field maps through the same remap the graph
    // took.
    let map = |id: PartNodeId| merge.remap.get(&id).copied().unwrap_or(id);
    // M6-D: tank contents and seated crew pool into the merged
    // vessel through the same remap (the absorbed tanks/seats keep
    // their nodes' new ids); in-flight transfers naming the absorbed
    // id are redirected below so a stream already in the pipe lands
    // in the merged vessel.
    let (tanks_m, crew_m) = crate::transfers::merge_vessel_records(vp, vs, &map);
    // M9b: reliability records pool through the same remap — the
    // primary's pending publishes stay valid (same `VesselId`); the
    // absorbed side's named the dead id, so their flags clear for
    // the publish slot to re-derive under the merged binding (the
    // stale events fire as tolerated no-ops).
    let mut reliability_m = vp.reliability.clone();
    for (id, rec) in &vs.reliability {
        let mut r = rec.clone();
        r.pending_failure_tick = None;
        reliability_m.insert(map(*id), r);
    }
    // §3.1 `Parachute`: deploy records pool through the same remap —
    // a canopy already out on the absorbed side stays out on the
    // merged hull.
    let mut deployed_m = vp.deployed.clone();
    for (id, t) in &vs.deployed {
        deployed_m.insert(map(*id), *t);
    }
    // M8-B: the merged hull keeps either half's colony opt-in — it
    // still sits where a joined half sat (the join record is
    // per-vessel, so a second latched member of the same colony
    // cannot stack the same join twice: `or` reads one).
    let joined_m = vp.joined_colony.or(vs.joined_colony);
    // M7-A: science records pool through the same remap, and the
    // absorbed vessel's live projects re-seat onto the merged id
    // (their lab nodes remap like the records — `map` preserves
    // primary ids, so primary projects are untouched).
    crate::research::shadow_science(science, &mut shadow.science);
    let science_m = crate::research::merge_vessel_science(vp, vs, &map, &mut science.projects);
    let mut seams = vp.seams.clone();
    seams.extend(vs.seams.iter().map(|s| DockSeam {
        port_a: map(s.port_a),
        port_b: map(s.port_b),
        seam_node: map(s.seam_node),
        local_a: s.local_a,
        local_b: s.local_b,
        latch_tick: s.latch_tick,
    }));
    seams.push(seam);
    // Temperatures and aero mask merge through the same remap —
    // the mask is positional over ascending node order, so each
    // merged bit comes from whichever side the node lived on.
    let mut temps = vp.thermal.part_temps_k.clone();
    for (id, t) in &vs.thermal.part_temps_k {
        temps.insert(map(*id), *t);
    }
    let pos_p: BTreeMap<PartNodeId, usize> = craft_p
        .graph
        .nodes
        .keys()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();
    let pos_s: BTreeMap<PartNodeId, usize> = craft_s
        .graph
        .nodes
        .keys()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();
    let inv: BTreeMap<PartNodeId, PartNodeId> =
        merge.remap.iter().map(|(old, new)| (*new, *old)).collect();
    // The merged bit comes from whichever side the node lived on; a
    // node neither source knows (the seam) reads aero-active.
    let mask = csp_phys::remap_config_mask(merge.graph.nodes.keys().copied(), |id| {
        if let Some(&i) = pos_p.get(&id) {
            vp.aero_mask.active(i)
        } else if let Some(&i) = inv.get(&id).and_then(|old| pos_s.get(old)) {
            vs.aero_mask.active(i)
        } else {
            true
        }
    })
    .0;
    // §2.4.1: the merged body's attached aero table must be for its
    // own graph + mask — rebuild inline when the environment has an
    // atmosphere (the apply_breakup convention), else attach nothing;
    // the caches for both ids are dropped below so `ensure_aero`
    // re-derives the rest.
    if physics.backend.environment().atmosphere.is_some() {
        let craft_m = crate::CraftDef {
            hash_memo: Default::default(),
            graph: merge.graph.clone(),
            parts: parts_m.clone(),
            part_costs: part_costs_m.clone(),
            reliability: reliability_table_m.clone(),
            part_ids: part_ids_m.clone(),
        };
        body_m.aero = Some(Box::new(
            csp_phys::aero::build_aero_tables(
                &body_m,
                &craft_m.graph,
                &craft_m.parts,
                csp_phys::ConfigMask(mask),
                crate::craft_graph_hash(&craft_m),
            )
            .map_err(|e| SimError::Fatal(e.reason()))?,
        ));
    }
    // Commit: rewrite the primary's record, absorb the secondary's.
    {
        let v = vessels
            .get_mut(&prim)
            .ok_or(SimError::Fatal("latch: primary vessel vanished"))?;
        v.craft = Some(crate::CraftDef {
            hash_memo: Default::default(),
            graph: merge.graph,
            parts: parts_m,
            part_costs: part_costs_m,
            reliability: reliability_table_m,
            part_ids: part_ids_m,
        });
        v.state = VesselState::InBubble(body_m);
        v.seams = seams;
        v.thermal.part_temps_k = temps;
        v.aero_mask = csp_phys::ConfigMask(mask);
        v.tank_contents = tanks_m;
        v.crew = crew_m;
        v.science = science_m;
        v.joined_colony = joined_m;
        v.reliability = reliability_m;
        v.deployed = deployed_m;
    }
    let absorbed_tracked = tracked.remove(&sec);
    if absorbed_tracked {
        tracked.insert(prim);
    }
    // Same inline-removal invariant as the impact/eviction sites in
    // `step_bubble_range`: the absorbed vessel is `InBubble`, and
    // vessel-named event kinds (SoiTransition, SoiRescan, DebrisDecay,
    // ManeuverNodeStart) are scheduled ONLY for on-rails conics —
    // dropped at `ToBubble` — so no pending event can name it.
    debug_assert!(
        !event_heap
            .iter()
            .any(|e| crate::event_names_vessel(&e.kind, &sec)),
        "latch absorb: a pending event outlived its conic"
    );
    // The absorb drops the secondary's `maneuver` slot and
    // `debris_since` bookkeeping with the vessel: a scheduled/burning
    // node on the secondary dies with its id (the merged vessel keeps
    // the PRIMARY's maneuver), and a debris counter that was counting
    // the absorbed vessel simply stops — the mass is in the merged
    // body now, not orbiting as debris. The record's crew list still
    // names the seats the merge just pooled — the roster re-seat
    // below reads it.
    let absorbed = vessels.remove(&sec);
    // M9a §2.10: a bound autopilot keyed on `sec` dies with the
    // absorbed vessel — the `remove_vessel`/dead-vessels-drain rule
    // applied at the absorb, so the restore gate's `autopilots ⊆
    // vessels` invariant holds of every post-latch save (an outliving
    // record could only ever sit inert: the bound vessel's telemetry
    // no longer resolves). `scripts` records still survive — they
    // are global, not per-vessel state. The shadow capture is lazy
    // like `economy`'s below: only a merge whose secondary carried a
    // record mutates the arena.
    if autopilots.contains_key(&sec) {
        if shadow.autopilots.is_none() {
            shadow.autopilots = Some(autopilots.clone());
        }
        autopilots.remove(&sec);
    }
    // In-flight transfers naming the absorbed id redirect to the
    // merged survivor — its tanks (and the reservation they held)
    // traveled with the merge.
    crate::transfers::remap_transfer_vessels(transfers, sec, prim);
    // Roster half of the seat merge: the absorbed vessel's members
    // re-seat onto the surviving primary — the `split`/fragment
    // path's mirror, keeping the roster↔seat pairing the restore
    // gate enforces.
    if let Some(vs) = &absorbed
        && !vs.crew.is_empty()
    {
        crate::economy::shadow_economy(economy, &mut shadow.economy);
        crate::economy::reseat_roster(economy, sec, prim, &vs.crew);
    }
    physics.aero.remove(&prim);
    physics.aero.remove(&sec);
    // The seam's own joint is consumed; any other joint touching the
    // absorbed vessel is dead with it.
    joints.retain(|j| j.vessel_a != sec && j.vessel_b != sec);
    // Refused pairs naming the absorbed id are moot — the vessel is
    // gone; the pair can never form again.
    refused.retain(|&(x, y)| x != sec && y != sec);
    Ok(true)
}

/// The per-tick latch driver: every live joint is checked against
/// [`latch_ready`] in `joints` order (formation order — the §2.2
/// fixed order); a settled pair merges through [`apply_latch`]. Runs
/// after the tick's consequences and before [`lifecycle`]'s
/// dissolve/detect pass — §2.4's "the island collapses; the
/// constraint solver stops running".
///
/// Index discipline: `apply_latch` mutates `joints` — a merge removes
/// the consumed joint AND any other joint naming the absorbed vessel
/// (at ANY index), and the clean-separation degrade removes the
/// checked joint itself. A removal below `i` compacts unchecked
/// joints into lower slots, so the scan restarts after a merge;
/// `latch_ready` is deterministic inside the pass (no poses move),
/// and joints already checked just re-fail — nothing merges twice.
#[allow(clippy::too_many_arguments)]
pub(crate) fn latch_pass(
    vessels: &mut BTreeMap<VesselId, Vessel>,
    physics: &mut crate::Physics,
    joints: &mut Vec<LiveJoint>,
    refused: &mut BTreeSet<(VesselId, VesselId)>,
    tracked: &mut BTreeSet<VesselId>,
    event_heap: &BinaryHeap<ScheduledEvent>,
    transfers: &mut BTreeMap<TransferId, ActiveTransfer>,
    autopilots: &mut BTreeMap<VesselId, AutopilotState>,
    science: &mut crate::research::ScienceState,
    economy: &mut crate::economy::EconomyState,
    cfg: &DockingConfig,
    tick: u64,
    degraded: &mut Vec<SimError>,
    shadow: &mut crate::TickShadow,
) -> Result<(), SimError> {
    let mut i = 0;
    while i < joints.len() {
        if !latch_ready(vessels, &joints[i], cfg)? {
            i += 1;
            continue;
        }
        let joint = joints[i].clone();
        if apply_latch(
            vessels, physics, joints, refused, tracked, event_heap, transfers, autopilots, science,
            economy, &joint, tick, degraded, shadow,
        )? {
            // The merge removed the consumed joint — and every other
            // joint touching the absorbed vessel, at any index —
            // compacting unchecked joints into lower slots. Restart:
            // rechecking earlier joints is sound (the predicate is
            // fixed for the tick) and a joint that survived at a
            // shifted-down slot is never skipped.
            i = 0;
            continue;
        }
        // A non-merge splits two ways: the corrupt-input degrade
        // REMOVED this joint — the element now at `i` was never
        // checked, so the index must not advance — while the
        // dead-vessel early-outs left it in place — step past.
        if joints.get(i) == Some(&joint) {
            i += 1;
        }
    }
    Ok(())
}
