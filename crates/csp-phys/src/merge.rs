//! §2.4 docking step 2 — the **latch merge**: two settled
//! soft-capture part graphs combine into one, so a single Stage-1
//! weld produces the merged `WeldedBody`.
//!
//! # Structure
//!
//! The merge is a pure graph edit — same discipline as the weld
//! (ascending `PartNodeId`, `BTreeMap`, plain `f64`):
//!
//! - The **primary** graph's nodes are kept verbatim — its frame is
//!   the merged frame, so the retained vessel's side of a later
//!   undock is bitwise its original craft (the merge only *added*
//!   nodes).
//! - Every **secondary** node is remapped to `base + old_id` where
//!   `base = max(primary ids) + 1`, so the whole secondary subtree
//!   sorts above every primary node and the two id spaces can never
//!   collide.
//! - The secondary's **root** becomes a child of `attach_node` (the
//!   primary-side port node): `parent = attach_node`,
//!   `transform = seam ∘ t'_root` — the caller's `seam` carries the
//!   secondary's *graph frame* inside `attach_node`'s frame, and the
//!   merge composes the root's own stored transform onto it. The
//!   compose is load-bearing: `t'_root` is not identity in general —
//!   `fragment_graph_of` writes the subtree's merged-frame pose into
//!   the fragment root at undock, so a split-off segment re-docking
//!   carries its in-frame pose in its root. Dropping it would
//!   displace the entire absorbed segment by `t'_root`. That one new
//!   edge is the docking seam — the attachment through which the
//!   secondary's rigid motion reached the pair. Attaching under the
//!   port node (not the primary root)
//!   is what makes the ordinary §2.4 machinery read the seam
//!   correctly: `failed_joints` treats it like any other attachment
//!   (an overload detaches the docked segment), and a thermal
//!   destruction of the port node detaches the subtree with it.
//! - The secondary's internal edges are stored transforms, untouched
//!   — the weld composes them against the seam edge exactly as it
//!   composes any attachment chain.
//!
//! The caller supplies `seam` — the secondary's **graph frame**
//! (its body frame) expressed inside `attach_node`'s frame. For a
//! live latch the sim computes it from the settled poses:
//! `t_port⁻¹ ∘ T_rel`, where `T_rel` is the secondary's graph-frame
//! pose inside the primary's (`R_a⁻¹·(x_b − x_a)`, `R_a⁻¹·R_b`).
//! The merge then composes the secondary root's own stored transform
//! onto it: the recorded edge is `seam ∘ t'_root`, so every merged
//! secondary node lands at `T_rel ∘ t'_m` — callers must NOT
//! pre-compose the root transform into `seam` themselves. For the
//! M6 merge test the seam is *derived* the same way — from authored
//! body poses through the latch's formula — and the merged graph is
//! then *bitwise* the same `PartGraph` a VAB session would author
//! for the assembled station, so `weld(merged) ==
//! weld(from-scratch)` bit-identically — the exit criterion the
//! plan names.
//!
//! # The record
//!
//! [`merge_graphs`] returns a [`MergedGraph`] carrying everything the
//! sim's `DockSeam` needs: `seam_node` (the remapped secondary root —
//! the seam edge's child, the split point an undock fragments at),
//! `port_b` (the secondary-side port node, remapped), and `remap`
//! (secondary id → merged id), which the sim also uses to carry the
//! absorbed vessel's own seam records, temperatures, and config mask
//! into the merged bookkeeping.
//!
//! The inverse operation is not here: undock splits through the
//! ordinary §2.4 breakup machinery — [`crate::structural`]'s
//! `subtree_set`/`fragment_graph_of` at `seam_node` — by plan
//! ("the graph splits at the seam via the breakup path").

use std::collections::BTreeMap;
use std::fmt;

use csp_core::SimError;

use crate::graph::{PartGraph, PartNode, PartNodeId, PartTransform};

/// Why two graphs cannot merge — the typed boundary between bad seam
/// arguments and the pure edit (§2.13). Maps onto
/// `SimError::Recoverable` at the latch: a merge that cannot run
/// degrades to the transient's clean separation like a cap refusal,
/// never a panic and never a half-written graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeError {
    /// `attach_node` is not a node of the primary graph.
    AttachNodeMissing(PartNodeId),
    /// `port_b` is not a node of the secondary graph.
    PortNodeMissing(PartNodeId),
    /// The secondary graph is empty or its `root` is not a node.
    SecondaryShape(&'static str),
    /// A non-root secondary node names no parent or a parent outside
    /// the graph — malformed input the merge reports here rather than
    /// letting the merged weld reject it as `Unreachable`.
    SecondaryParent {
        node: PartNodeId,
        parent: Option<PartNodeId>,
    },
    /// `seam` failed [`PartTransform::is_valid`] — non-finite offset
    /// or a non-unit rotation. A corrupt seam would weld, silently,
    /// into mis-scaled geometry.
    InvalidSeam,
    /// A remapped id or effective index overflowed — `base + id`
    /// exceeded `u64`/`u32`. Carried names the overflowing counter.
    IndexOverflow(&'static str),
}

impl MergeError {
    /// Static reason text for the `SimError` mapping — logs only,
    /// never hashed (§2.13).
    pub fn reason(self) -> &'static str {
        match self {
            Self::AttachNodeMissing(_) => "merge: attach node not in primary graph",
            Self::PortNodeMissing(_) => "merge: port node not in secondary graph",
            Self::SecondaryShape(r) => r,
            Self::SecondaryParent { .. } => "merge: secondary node has a broken parent",
            Self::InvalidSeam => "merge: seam transform is not finite-unit",
            Self::IndexOverflow(f) => f,
        }
    }
}

impl fmt::Display for MergeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason())
    }
}

impl std::error::Error for MergeError {}

impl From<MergeError> for SimError {
    fn from(e: MergeError) -> Self {
        SimError::Recoverable(e.reason())
    }
}

/// The merge result — see the module doc. `graph` welds to the merged
/// `WeldedBody`; the rest is the seam bookkeeping the sim's
/// `DockSeam` consumes.
#[derive(Clone, Debug, PartialEq)]
pub struct MergedGraph {
    /// The merged part graph in the primary's frame — a legal
    /// `PartGraph` like any authored craft (the merge test's point:
    /// `weld` on it is bit-identical to welding the same graph
    /// authored from scratch).
    pub graph: PartGraph,
    /// Every secondary node id → its merged id, ascending old id —
    /// the map the sim uses to carry the absorbed vessel's own
    /// `DockSeam` records, temperature record, and aero-mask bits
    /// into the merged bookkeeping.
    pub remap: BTreeMap<PartNodeId, PartNodeId>,
    /// The remapped id of the secondary's former root — the seam
    /// edge's child. `subtree_set(&graph, seam_node)` is everything
    /// the merge brought in; an undock at this seam fragments there.
    pub seam_node: PartNodeId,
    /// `port_b` under the remap — the seam record's secondary-side
    /// port node.
    pub port_b: PartNodeId,
}

/// §2.4's latch merge — fold `secondary` into `primary` behind the
/// new seam edge `attach_node → secondary.root`.
///
/// - `attach_node`: the primary-side node carrying the sealed port —
///   the seam edge's parent.
/// - `port_b`: the secondary-side node carrying the sealed port —
///   recorded, remapped, in [`MergedGraph::port_b`].
/// - `seam`: the secondary **graph frame**'s transform in
///   `attach_node`'s frame — `t_port⁻¹ ∘ T_rel` for a live latch
///   (see the module doc); an authored literal in the merge test.
///   The merge composes the secondary root's own stored transform
///   onto it — the recorded seam edge is `seam ∘ t'_root` — so the
///   subtree lands at `T_rel ∘ t'_m` even when `t'_root` is
///   non-identity (the undock-fragment case).
/// - `effective_base`: `primary.parts.len()` — every secondary
///   `effective_index` shifts by it so the merged nodes index into
///   `parts_primary ++ parts_secondary` (the sim concatenates the
///   slices itself; `merge_graphs` is a graph edit, and keeping the
///   parts vec out of the signature keeps the function pure over
///   exactly what it transforms).
///
/// Errors instead of panicking on every malformed input: missing
/// attach/port nodes, a malformed secondary graph, a non-unit seam,
/// or an id/index overflow.
pub fn merge_graphs(
    primary: &PartGraph,
    secondary: &PartGraph,
    attach_node: PartNodeId,
    port_b: PartNodeId,
    seam: PartTransform,
    effective_base: u32,
) -> Result<MergedGraph, MergeError> {
    if !primary.nodes.contains_key(&attach_node) {
        return Err(MergeError::AttachNodeMissing(attach_node));
    }
    if secondary.nodes.is_empty() {
        return Err(MergeError::SecondaryShape("merge: empty secondary graph"));
    }
    if !secondary.nodes.contains_key(&secondary.root) {
        return Err(MergeError::SecondaryShape(
            "merge: secondary root is not a node",
        ));
    }
    if secondary.nodes[&secondary.root].parent.is_some() {
        return Err(MergeError::SecondaryShape(
            "merge: secondary root has a parent",
        ));
    }
    if !secondary.nodes.contains_key(&port_b) {
        return Err(MergeError::PortNodeMissing(port_b));
    }
    if !seam.is_valid() {
        return Err(MergeError::InvalidSeam);
    }
    // The remap base: every merged secondary id sorts above every
    // primary id, so the weld's ascending-id summation order is the
    // primary subtree's order followed by the secondary's — the order
    // a from-scratch graph of the same layout produces.
    let base = primary
        .nodes
        .keys()
        .next_back()
        .map(|id| id.0)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(MergeError::IndexOverflow("merge: id base exceeds u64::MAX"))?;
    // Pass 1 — the id remap, computed in full before any node is
    // written so parent references resolve in one pass regardless of
    // the secondary's internal id order.
    let mut remap = BTreeMap::new();
    for &id in secondary.nodes.keys() {
        let merged = base.checked_add(id.0).ok_or(MergeError::IndexOverflow(
            "merge: remapped id exceeds u64::MAX",
        ))?;
        remap.insert(id, PartNodeId(merged));
    }
    // Pass 2 — the graph edit itself.
    let mut graph = primary.clone();
    for (&id, node) in &secondary.nodes {
        let merged_id = remap[&id];
        let eff =
            node.effective_index
                .checked_add(effective_base)
                .ok_or(MergeError::IndexOverflow(
                    "merge: effective index exceeds u32::MAX",
                ))?;
        let mapped = if id == secondary.root {
            // The seam edge: the secondary's root hangs under the
            // primary-side port node at `seam ∘ t'_root` — `seam`
            // carries the secondary's *frame*; the root's own stored
            // transform (non-identity for fragment graphs — an
            // undock split writes the merged-frame pose there) rides
            // on top, so the subtree lands at `T_rel ∘ t'_m`.
            PartNode::new(Some(attach_node), seam.compose(node.transform), eff)
        } else {
            let Some(&parent) = node.parent.as_ref().and_then(|p| remap.get(p)) else {
                return Err(MergeError::SecondaryParent {
                    node: id,
                    parent: node.parent,
                });
            };
            PartNode::new(Some(parent), node.transform, eff)
        };
        graph.insert(merged_id, mapped);
    }
    let seam_node = remap[&secondary.root];
    let port_b = remap[&port_b];
    Ok(MergedGraph {
        graph,
        remap,
        seam_node,
        port_b,
    })
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::part::{ColliderShape, ForceSite, ForceSiteKind, WeldPart};
    use crate::tensor::SymMat3;
    use crate::weld::weld;
    use csp_core::{DQuat, DVec3};

    fn test_thermal() -> csp_data::ThermalDef {
        csp_data::ThermalDef {
            heat_capacity_j_per_k: 100_000.0,
            max_temp_k: 1_200.0,
            emissivity: 0.8,
            conduction_w_per_k: 50.0,
        }
    }

    fn box_part(mass_kg: f64, half: DVec3) -> WeldPart {
        WeldPart {
            experiments: Vec::new(),
            lab: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(mass_kg, half),
            collider: ColliderShape::Box { half_extents: half },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: test_thermal(),
        }
    }

    /// A two-node primary: root `0` (hub) + port node `1` off +z.
    fn primary() -> (PartGraph, Vec<WeldPart>) {
        let mut g = PartGraph::new(PartNodeId(0));
        g.insert(
            PartNodeId(0),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(1),
            PartNode::new(
                Some(PartNodeId(0)),
                PartTransform::new(DVec3::new(0.0, 0.0, 1.0), DQuat::IDENTITY),
                1,
            ),
        );
        (
            g,
            vec![
                box_part(500.0, DVec3::new(0.5, 0.5, 0.5)),
                box_part(100.0, DVec3::new(0.25, 0.25, 0.5)),
            ],
        )
    }

    /// A two-node secondary: root `0` (a second hub) + child `1`.
    fn secondary() -> (PartGraph, Vec<WeldPart>) {
        let mut g = PartGraph::new(PartNodeId(0));
        g.insert(
            PartNodeId(0),
            PartNode::new(
                None,
                PartTransform::new(DVec3::new(0.125, -0.25, 0.5), DQuat::IDENTITY),
                0,
            ),
        );
        g.insert(
            PartNodeId(1),
            PartNode::new(
                Some(PartNodeId(0)),
                PartTransform::new(DVec3::new(0.0, 0.5, 0.25), DQuat::IDENTITY),
                1,
            ),
        );
        (
            g,
            vec![
                box_part(800.0, DVec3::new(0.5, 0.5, 0.5)),
                box_part(50.0, DVec3::new(0.25, 0.25, 0.25)),
            ],
        )
    }

    /// The authored seam transform used across the tests — a
    /// translation plus an exact-unit flip so every composition is
    /// bitwise predictable.
    fn seam() -> PartTransform {
        PartTransform::new(DVec3::new(0.25, -0.5, 0.75), DQuat::new(0.0, 1.0, 0.0, 0.0))
    }

    /// The M6 merge test's graph half: the merged graph is **bitwise**
    /// the PartGraph an author would build for the assembled craft —
    /// primary nodes verbatim, secondary nodes remapped above the
    /// primary's max id, the seam edge verbatim.
    #[test]
    fn merge_graph_is_the_authored_station_bitwise() {
        let (gp, _) = primary();
        let (gs, _) = secondary();
        let seam = seam();
        let m = merge_graphs(&gp, &gs, PartNodeId(1), PartNodeId(1), seam, 2).unwrap();

        // The reference: the same four parts authored as ONE graph —
        // primary {0,1} untouched; secondary {0,1} → {2,3} (base 2);
        // merged root under the port node at `seam ∘ t'_root` — the
        // secondary root's own transform `(0.125, −0.25, 0.5) / I`
        // composed into the edge: the y-flip maps it to
        // (−0.125, −0.25, −0.5), so the edge offset is
        // (0.25, −0.5, 0.75) + (−0.125, −0.25, −0.5).
        let mut expect = gp.clone();
        expect.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(
                    DVec3::new(0.125, -0.75, 0.25),
                    DQuat::new(0.0, 1.0, 0.0, 0.0),
                ),
                2,
            ),
        );
        expect.insert(
            PartNodeId(3),
            PartNode::new(
                Some(PartNodeId(2)),
                PartTransform::new(DVec3::new(0.0, 0.5, 0.25), DQuat::IDENTITY),
                3,
            ),
        );
        assert_eq!(
            m.graph, expect,
            "merged graph must be bitwise the authored one"
        );
        assert_eq!(m.seam_node, PartNodeId(2));
        assert_eq!(m.port_b, PartNodeId(3));
        assert_eq!(
            m.remap,
            BTreeMap::from([
                (PartNodeId(0), PartNodeId(2)),
                (PartNodeId(1), PartNodeId(3)),
            ])
        );
    }

    /// A three-node primary with a two-deep port chain — hub root
    /// `0`, mid `1`, port node `2` — on exact-unit flips and dyadic
    /// offsets so every composition below is bitwise exact. `t_port`
    /// (the node-2 body transform) is non-identity *and* composed:
    /// the seam math must see through the whole chain.
    fn chained_primary() -> (PartGraph, Vec<WeldPart>) {
        let mut g = PartGraph::new(PartNodeId(0));
        g.insert(
            PartNodeId(0),
            PartNode::new(
                None,
                PartTransform::new(DVec3::new(0.5, -0.25, 0.0), DQuat::IDENTITY),
                0,
            ),
        );
        g.insert(
            PartNodeId(1),
            PartNode::new(
                Some(PartNodeId(0)),
                PartTransform::new(DVec3::new(0.0, 0.0, 1.0), DQuat::new(1.0, 0.0, 0.0, 0.0)),
                1,
            ),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.25, -0.5, 0.75), DQuat::new(0.0, 0.0, 1.0, 0.0)),
                2,
            ),
        );
        (
            g,
            vec![
                box_part(500.0, DVec3::new(0.5, 0.5, 0.5)),
                box_part(100.0, DVec3::new(0.25, 0.25, 0.5)),
                box_part(60.0, DVec3::new(0.125, 0.125, 0.25)),
            ],
        )
    }

    /// A two-node secondary whose ROOT transform is non-identity in
    /// BOTH offset and rotation — the shape a `fragment_graph_of`
    /// split-off carries (its root holds the merged-frame pose), the
    /// reachability path for the seam-edge compose.
    fn rooted_secondary() -> (PartGraph, Vec<WeldPart>) {
        let mut g = PartGraph::new(PartNodeId(0));
        g.insert(
            PartNodeId(0),
            PartNode::new(
                None,
                PartTransform::new(
                    DVec3::new(0.125, -0.25, 0.5),
                    DQuat::new(1.0, 0.0, 0.0, 0.0),
                ),
                0,
            ),
        );
        g.insert(
            PartNodeId(1),
            PartNode::new(
                Some(PartNodeId(0)),
                PartTransform::new(DVec3::new(0.0, 0.5, 0.25), DQuat::new(0.0, 1.0, 0.0, 0.0)),
                1,
            ),
        );
        (
            g,
            vec![
                box_part(800.0, DVec3::new(0.5, 0.5, 0.5)),
                box_part(50.0, DVec3::new(0.25, 0.25, 0.25)),
            ],
        )
    }

    /// Exact transform equality — every quantity in this fixture is
    /// dyadic with exact-unit rotations, so all arithmetic is exact
    /// and `==` is the right gate (not approximate). ±0 sign bits on
    /// zero components are deliberately invisible: `inverse`'s
    /// conjugate negates zero lanes, so equivalent compose
    /// associations can legitimately differ in the sign OF a zero,
    /// which `rotate`/`apply` then absorb without consequence. The
    /// bitwise gate is the weld comparison below, where the mass
    /// properties are all nonzero.
    fn assert_transform_exact(a: PartTransform, b: PartTransform, ctx: &str) {
        assert_eq!(a, b, "{ctx}: merged subtree transform drifted");
    }

    /// The M6 exit criterion, derived: the seam is computed the way
    /// `apply_latch` computes it — `t_port⁻¹ ∘ T_rel` from authored
    /// body poses — over a primary whose port node sits two levels
    /// deep (non-identity `t_port`) and a secondary whose ROOT
    /// transform is non-identity (the undock-fragment shape). Every
    /// merged secondary node must land at `T_rel ∘ t'_m` — the seam
    /// edge carries the secondary's root transform, not just its
    /// frame — and `weld(merged)` is bit-identical to welding the
    /// same station authored from scratch.
    #[test]
    fn merge_weld_is_bit_identical_to_from_scratch_weld() {
        let (gp, parts_p) = chained_primary();
        let (gs, parts_s) = rooted_secondary();
        // Authored body poses for the settled pair — the latch's
        // inputs. Exact-unit flips keep every composition bitwise.
        let (pos_p, rot_p) = (DVec3::new(-2.0, 3.0, 1.0), DQuat::IDENTITY);
        let (pos_s, rot_s) = (
            pos_p + DVec3::new(2.0, -1.0, 3.0),
            DQuat::new(0.0, 1.0, 0.0, 0.0),
        );
        // The `apply_latch` seam computation, verbatim: the
        // secondary's graph-frame pose inside the primary's, then
        // inside the port node's frame.
        let rel = PartTransform::new(
            rot_p.conjugate().rotate(pos_s - pos_p),
            (rot_p.conjugate() * rot_s).normalize(),
        );
        let transforms_p = crate::weld::body_transforms(&gp, &parts_p).unwrap();
        let t_port = transforms_p[&PartNodeId(2)];
        let m = merge_graphs(
            &gp,
            &gs,
            PartNodeId(2),
            PartNodeId(1),
            t_port.inverse().compose(rel),
            3,
        )
        .unwrap();
        let mut merged_parts = parts_p.clone();
        merged_parts.extend(parts_s.iter().cloned());

        // The derived placement gate: EVERY merged secondary node
        // sits at `T_rel ∘ t'_m` — the absorbed segment is rigidly
        // where its free body was, not displaced by its root's own
        // transform.
        let transforms_s = crate::weld::body_transforms(&gs, &parts_s).unwrap();
        let transforms_m = crate::weld::body_transforms(&m.graph, &merged_parts).unwrap();
        for (old, new) in &m.remap {
            assert_transform_exact(
                transforms_m[new],
                rel.compose(transforms_s[old]),
                "merged subtree must land at T_rel ∘ t'_m",
            );
        }

        // The same five parts as ONE authored craft: the primary
        // nodes verbatim; the secondary subtree at its composed
        // pose — the seam edge is `t_port⁻¹ ∘ (T_rel ∘ t'_root)`,
        // the same value `seam ∘ t'_root` produces bitwise under
        // exact-unit arithmetic.
        let mut graph_ref = gp.clone();
        graph_ref.insert(
            PartNodeId(3),
            PartNode::new(
                Some(PartNodeId(2)),
                t_port
                    .inverse()
                    .compose(rel.compose(transforms_s[&gs.root])),
                3,
            ),
        );
        graph_ref.insert(
            PartNodeId(4),
            PartNode::new(Some(PartNodeId(3)), gs.nodes[&PartNodeId(1)].transform, 4),
        );
        let parts_ref = merged_parts.clone();
        assert_eq!(
            m.graph, graph_ref,
            "merged graph must be bitwise the authored one"
        );

        let w_merged = weld(&m.graph, &merged_parts).unwrap();
        let w_ref = weld(&graph_ref, &parts_ref).unwrap();
        // Bit-level comparison like weld.rs's assert_bit_identical —
        // == on f64 cannot see +0/-0 or NaN payloads.
        assert_eq!(w_merged.mass_kg.to_bits(), w_ref.mass_kg.to_bits());
        for (a, b) in [
            (w_merged.com_local.x, w_ref.com_local.x),
            (w_merged.com_local.y, w_ref.com_local.y),
            (w_merged.com_local.z, w_ref.com_local.z),
            (w_merged.inertia_local.xx, w_ref.inertia_local.xx),
            (w_merged.inertia_local.yy, w_ref.inertia_local.yy),
            (w_merged.inertia_local.zz, w_ref.inertia_local.zz),
            (w_merged.inertia_local.xy, w_ref.inertia_local.xy),
            (w_merged.inertia_local.xz, w_ref.inertia_local.xz),
            (w_merged.inertia_local.yz, w_ref.inertia_local.yz),
        ] {
            assert_eq!(a.to_bits(), b.to_bits(), "mass-property field drifted");
        }
        assert_eq!(w_merged.force_sites, w_ref.force_sites);
        assert_eq!(w_merged.collider, w_ref.collider);
        assert_eq!(w_merged.part_index, w_ref.part_index);
        // And the union property the criterion is really about: the
        // merged weld's mass is the two source welds' total — nothing
        // dropped, nothing fabricated.
        let w_a = weld(&gp, &parts_p).unwrap();
        let w_b = weld(&gs, &parts_s).unwrap();
        assert_eq!(w_merged.mass_kg, w_a.mass_kg + w_b.mass_kg);
    }

    /// A seam with a rotation: the composed solve lands the
    /// secondary's parts at `t_port ∘ seam ∘ t'_root` — the seam
    /// carries the secondary's frame and the root's own stored
    /// transform rides inside the edge — verified through the weld's
    /// own `body_transforms` (the same compose path the weld uses).
    #[test]
    fn merged_subtree_lands_at_composed_pose() {
        let (gp, parts_p) = primary();
        let (gs, parts_s) = secondary();
        let seam = seam();
        let m = merge_graphs(&gp, &gs, PartNodeId(1), PartNodeId(1), seam, 2).unwrap();
        let mut parts_m = parts_p.clone();
        parts_m.extend(parts_s.iter().cloned());
        let t = crate::weld::body_transforms(&m.graph, &parts_m).unwrap();
        // t_port = identity-offset (0,0,1): the secondary root's body
        // transform is t_port.compose(seam ∘ t'_root) — the root's own
        // offset `(0.125, −0.25, 0.5)` folded into the edge.
        let t_port = PartTransform::new(DVec3::new(0.0, 0.0, 1.0), DQuat::IDENTITY);
        let t_root_s = gs.nodes[&gs.root].transform;
        let expected = t_port.compose(seam.compose(t_root_s));
        assert_eq!(t[&PartNodeId(2)], expected);
        // And the secondary child composes on top of it.
        assert_eq!(
            t[&PartNodeId(3)],
            expected.compose(PartTransform::new(
                DVec3::new(0.0, 0.5, 0.25),
                DQuat::IDENTITY
            ))
        );
    }

    /// Every `MergeError` arm is reachable.
    #[test]
    fn merge_rejects_each_failure_mode() {
        let (gp, _) = primary();
        let (gs, _) = secondary();
        assert!(matches!(
            merge_graphs(&gp, &gs, PartNodeId(9), PartNodeId(1), seam(), 2),
            Err(MergeError::AttachNodeMissing(_))
        ));
        assert!(matches!(
            merge_graphs(&gp, &gs, PartNodeId(1), PartNodeId(9), seam(), 2),
            Err(MergeError::PortNodeMissing(_))
        ));
        let bad_seam = PartTransform::new(DVec3::new(f64::NAN, 0.0, 0.0), DQuat::IDENTITY);
        assert!(matches!(
            merge_graphs(&gp, &gs, PartNodeId(1), PartNodeId(1), bad_seam, 2),
            Err(MergeError::InvalidSeam)
        ));
        let empty = PartGraph::new(PartNodeId(0));
        assert!(matches!(
            merge_graphs(&gp, &empty, PartNodeId(1), PartNodeId(0), seam(), 2),
            Err(MergeError::SecondaryShape(_))
        ));
        // A secondary whose non-root node has no parent in the graph.
        let mut orphan = PartGraph::new(PartNodeId(0));
        orphan.insert(
            PartNodeId(0),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        orphan.insert(
            PartNodeId(7),
            PartNode::new(Some(PartNodeId(6)), PartTransform::IDENTITY, 0),
        );
        assert!(matches!(
            merge_graphs(&gp, &orphan, PartNodeId(1), PartNodeId(0), seam(), 2),
            Err(MergeError::SecondaryParent { .. })
        ));
        // A secondary root that names a parent is malformed input.
        let mut bad_root = PartGraph::new(PartNodeId(0));
        bad_root.insert(
            PartNodeId(0),
            PartNode::new(Some(PartNodeId(5)), PartTransform::IDENTITY, 0),
        );
        assert!(matches!(
            merge_graphs(&gp, &bad_root, PartNodeId(1), PartNodeId(0), seam(), 2),
            Err(MergeError::SecondaryShape(_))
        ));
        // effective_index overflow.
        let mut big = PartGraph::new(PartNodeId(0));
        big.insert(
            PartNodeId(0),
            PartNode::new(None, PartTransform::IDENTITY, u32::MAX - 1),
        );
        assert!(matches!(
            merge_graphs(&gp, &big, PartNodeId(1), PartNodeId(0), seam(), 2),
            Err(MergeError::IndexOverflow(_))
        ));
    }

    /// Force sites + collider hulls carry through the merge indexed at
    /// the remapped ids — the `part_index` entries land under the new
    /// ids, not the old.
    #[test]
    fn merged_part_index_uses_remapped_ids() {
        let (gp, mut parts_p) = primary();
        let (gs, parts_s) = secondary();
        parts_p[0].force_sites = vec![ForceSite::new(
            DVec3::ZERO,
            DVec3::Z,
            ForceSiteKind::Thrust,
            10.0,
        )];
        let m = merge_graphs(&gp, &gs, PartNodeId(1), PartNodeId(1), seam(), 2).unwrap();
        let mut parts_m = parts_p.clone();
        parts_m.extend(parts_s.iter().cloned());
        let w = weld(&m.graph, &parts_m).unwrap();
        assert!(w.part_index.contains_key(&PartNodeId(2)));
        assert!(w.part_index.contains_key(&PartNodeId(3)));
        assert!(!w.part_index.contains_key(&PartNodeId(4)));
        // The primary's site still lands first (ascending id order).
        assert_eq!(w.force_sites.len(), 1);
    }

    /// §2.4's station claim at scale: repeated merges fold a
    /// thousand-part station into ONE graph that welds to one
    /// `WeldedBody` — the live-joint cap bounds only transients, the
    /// merged graph carries a seam's subtree per dock and keeps
    /// growing.
    #[test]
    fn thousand_part_station_merges_into_one_body() {
        let (mut station, mut parts) = primary();
        // 999 more one-part craft docked on — each merge hangs the
        // newcomer under the latest node, chain-style.
        for _ in 0..999u64 {
            let mut craft = PartGraph::new(PartNodeId(0));
            craft.insert(
                PartNodeId(0),
                PartNode::new(None, PartTransform::IDENTITY, 0),
            );
            let attach = *station.nodes.keys().next_back().unwrap();
            let m = merge_graphs(
                &station,
                &craft,
                attach,
                PartNodeId(0),
                seam(),
                u32::try_from(parts.len()).unwrap(),
            )
            .unwrap();
            station = m.graph;
            parts.push(box_part(10.0, DVec3::new(0.1, 0.1, 0.1)));
        }
        assert_eq!(station.nodes.len(), 1001);
        let w = weld(&station, &parts).unwrap();
        // One body, every part indexed, mass is the sum.
        assert_eq!(w.part_index.len(), 1001);
        assert_eq!(w.mass_kg, 500.0 + 100.0 + 999.0 * 10.0);
    }
}
