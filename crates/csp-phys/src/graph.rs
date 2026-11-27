//! The attachment graph the §2.4 weld consumes.
//!
//! A `PartGraph` is a single rooted tree of [`PartNode`]s: each node's
//! [`PartTransform`] places the part's local frame relative to its
//! parent's, and the root's transform places it relative to the graph
//! (= body) frame. The tree is a `BTreeMap` keyed by [`PartNodeId`], so
//! iteration order is ascending ID regardless of insertion order —
//! §2.2's rule for anything that feeds state, and the mechanism behind
//! Stage 1's "bit-identical for a given `(graph, effective_parts)`"
//! guarantee — where the id labelling is part of "a given graph":
//! relabelling the same physical tree changes f64 summation order and
//! so is equivalent only to rounding, not bitwise.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use csp_core::{DQuat, DVec3};

/// Identity of a node in a [`PartGraph`]: a `u64` unique within the
/// graph, ordered for deterministic iteration. This is the graph's own
/// identity space — `csp-sim`'s §2.2 `PartInstanceId` arena keys on it
/// when a craft is manufactured, and a `DockSeam`'s `port_a`/`port_b`
/// will resolve through it at M6.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartNodeId(pub u64);

/// A rigid transform placing a child's frame inside its parent's:
/// `offset` is the child origin in parent-frame metres and `rotation`
/// carries child-local vectors into parent frame, i.e.
/// `p_parent = offset + rotation·(p_child)`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartTransform {
    pub offset: DVec3,
    pub rotation: DQuat,
}

impl PartTransform {
    /// `offset = 0`, `rotation = identity`.
    pub const IDENTITY: Self = Self {
        offset: DVec3::ZERO,
        rotation: DQuat::IDENTITY,
    };

    pub const fn new(offset: DVec3, rotation: DQuat) -> Self {
        Self { offset, rotation }
    }

    /// `self ∘ local`: the transform of a grandchild into `self`'s
    /// frame. The composed rotation is renormalized so unit-quaternion
    /// drift cannot accumulate along deep attachment chains — still
    /// exact IEEE + `libm`, hence deterministic.
    pub fn compose(self, local: Self) -> Self {
        Self {
            offset: self.offset + self.rotation.rotate(local.offset),
            rotation: (self.rotation * local.rotation).normalize(),
        }
    }

    /// The inverse transform: `self⁻¹` maps parent-frame points back
    /// into child frame — `self.compose(self.inverse())` is the
    /// identity to rounding (exact for exactly-unit rotations). The
    /// docking merge uses it to express a body-frame pose inside the
    /// seam's parent-node frame (`t_node⁻¹ ∘ T_relative`); undock's
    /// split path reads the same convention.
    ///
    /// The rotation is conjugated, not renormalized: a stored
    /// unit-within-tolerance quaternion stays one, and the op is
    /// exact IEEE + `libm`, hence deterministic.
    pub fn inverse(self) -> Self {
        let r = self.rotation.conjugate();
        Self {
            offset: r.rotate(-self.offset),
            rotation: r,
        }
    }

    /// Apply the transform to a child-local point.
    pub fn apply(self, p: DVec3) -> DVec3 {
        self.offset + self.rotation.rotate(p)
    }

    /// Rotate a child-local direction (no translation).
    pub fn apply_dir(self, d: DVec3) -> DVec3 {
        self.rotation.rotate(d)
    }

    /// The same transform with `rotation` renormalized to unit length.
    /// Composed rotations are renormalized per [`Self::compose`]; this
    /// exists for the one place a stored quat is used raw — the root
    /// transform — so a value at the edge of
    /// [`Self::UNIT_ROTATION_TOLERANCE`] cannot seed non-unit-ness into
    /// every descendant's composition.
    pub fn normalized(self) -> Self {
        Self {
            offset: self.offset,
            rotation: self.rotation.normalize(),
        }
    }

    /// Bound on `|q·q − 1|` for `rotation` to count as a unit
    /// quaternion: `1e-9` (≈ `|‖q‖ − 1| ≤ 5e-10`). Wide enough that
    /// legitimately composed/normalized quats always pass — drift from
    /// `normalize` chains stays under ~1e-15 — and tight enough that a
    /// scaled quat like `(0,0,0,2)` is rejected instead of silently
    /// doubling every rotation it performs.
    pub const UNIT_ROTATION_TOLERANCE: f64 = 1e-9;

    /// `rotation` is finite and a unit quaternion within
    /// [`Self::UNIT_ROTATION_TOLERANCE`]. Rejects the zero quat, NaN,
    /// and any scaled quat — a non-unit `rotation` used in `rotate`
    /// scales every vector it touches, which would corrupt mass
    /// properties silently.
    pub fn has_unit_rotation(self) -> bool {
        self.rotation.is_finite()
            && (self.rotation.dot(self.rotation) - 1.0).abs() <= Self::UNIT_ROTATION_TOLERANCE
    }

    /// Offset finite and rotation a unit quaternion — the weld's
    /// transform input gate. See [`Self::has_unit_rotation`].
    pub fn is_valid(self) -> bool {
        self.offset.is_finite() && self.has_unit_rotation()
    }
}

/// One node of the attachment tree. The node's identity *is* its
/// [`PartNodeId`] key in [`PartGraph::nodes`] — the node carries no
/// `id` field of its own, so a stored-id/key mismatch is impossible
/// by construction.
///
/// `effective_index` selects the node's resolved stats from the
/// `&[WeldPart]` slice `weld` is handed — keeping identity
/// (graph) and stats (data) in separate slots is what lets the same
/// graph re-weld when quantized tank contents change the effective
/// parts (§2.4 inertia-cache rule).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartNode {
    /// `None` iff this node is the graph root. Every non-root node must
    /// name an existing parent; the weld rejects dangling parents,
    /// cycles, and second roots.
    pub parent: Option<PartNodeId>,
    /// This node's frame relative to its parent's frame (relative to
    /// the graph frame for the root).
    pub transform: PartTransform,
    /// Index into the `effective: &[WeldPart]` weld argument.
    pub effective_index: u32,
    /// The attachment's snapped joint point, expressed in the
    /// **parent** node's local frame — the `p_J` the §2.4 joint-load
    /// torque arm is measured about (`structural::joint_loads`; C14-F4).
    /// Stamped at attach time (`CraftSession::to_part_graph` derives it
    /// from the parent's named `AttachNode`, the latch merge records
    /// the sealed port anchor); `None` means "not recorded" —
    /// hand-built graphs, pre-field saves — and the load model falls
    /// back to the part origin, the pre-field approximation. An edge
    /// record like `AttachEdge`: it lives on the child, is serialized
    /// with the craft, and is hashed as weld input (the joint load it
    /// feeds decides breakup).
    #[serde(default)]
    pub joint: Option<DVec3>,
}

impl PartNode {
    pub const fn new(
        parent: Option<PartNodeId>,
        transform: PartTransform,
        effective_index: u32,
    ) -> Self {
        Self {
            parent,
            transform,
            effective_index,
            joint: None,
        }
    }

    /// `self` with the [`Self::joint`] record stamped — the snapped
    /// attach point in the parent's local frame (see the field doc).
    pub const fn with_joint(mut self, joint: Option<DVec3>) -> Self {
        self.joint = joint;
        self
    }

    /// The attachment's body-frame attach point — the `p_J` the §2.4
    /// joint-load torque arm measures about: the recorded [`Self::joint`]
    /// carried into the parent's pose, the node's own origin when it is
    /// unrecorded (the pre-field approximation the field doc names). `id` is this
    /// node's key in `transforms`; the root has no attachment and
    /// answers its origin.
    pub fn attach_point(
        &self,
        id: PartNodeId,
        transforms: &BTreeMap<PartNodeId, PartTransform>,
    ) -> DVec3 {
        self.joint
            .and_then(|j| {
                let parent = self.parent?;
                let t = transforms.get(&parent)?;
                Some(t.apply(j))
            })
            .unwrap_or(transforms[&id].offset)
    }
}

/// A rooted part tree — the first `weld` argument of §2.4's
/// `csp_phys::weld(part_graph, &[WeldPart])`.
///
/// Construction is unordered by design (a map), but callers that build
/// a graph from an ordered craft-file node list simply insert in that
/// order — the result is identical for any insertion order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartGraph {
    /// Ascending-ID arena. `BTreeMap`, never `HashMap` (§2.2).
    pub nodes: BTreeMap<PartNodeId, PartNode>,
    /// The designated root. The body frame of the produced
    /// `WeldedBody` is the graph frame — the frame the root's
    /// `transform` is expressed in.
    pub root: PartNodeId,
}

impl PartGraph {
    pub fn new(root: PartNodeId) -> Self {
        Self {
            nodes: BTreeMap::new(),
            root,
        }
    }

    /// Insert `node` under `id`. The key is the node's only identity,
    /// so id and node cannot disagree. Returns the displaced node so a
    /// duplicate id is loud at the call site rather than silent.
    pub fn insert(&mut self, id: PartNodeId, node: PartNode) -> Option<PartNode> {
        self.nodes.insert(id, node)
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use core::f64::consts::{FRAC_PI_2, PI};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * b.abs().max(1.0)
    }

    fn assert_vec_close(a: DVec3, b: DVec3) {
        assert!(
            close(a.x, b.x) && close(a.y, b.y) && close(a.z, b.z),
            "expected {b:?}, got {a:?}"
        );
    }

    /// A chain of `depth` transforms folded by `compose` must land a
    /// leaf-frame point exactly where applying each local transform
    /// innermost-first puts it — the reference pose computed without
    /// `compose`. Depth 10 exceeds the deepest plausible attachment
    /// chains by a wide margin.
    #[test]
    fn deep_chain_compose_matches_sequential_apply() {
        let steps: Vec<PartTransform> = (0..10)
            .map(|i| {
                let f = f64::from(i);
                PartTransform::new(
                    DVec3::new(0.1 * f + 0.01, -0.05, 0.02 * f),
                    DQuat::from_axis_angle(DVec3::new(1.0, f + 1.0, -0.5), 0.05 * f + 0.01),
                )
            })
            .collect();
        let folded = steps[1..].iter().fold(steps[0], |acc, &t| acc.compose(t));
        // Reference: `p ↦ t1.apply(t2.apply(…tn.apply(p)))` — the
        // definition of the composed transform, computed directly.
        let p = DVec3::new(0.3, -0.7, 1.1);
        let expected = steps.iter().rev().fold(p, |acc, &t| t.apply(acc));
        assert_vec_close(folded.apply(p), expected);
        assert!(folded.has_unit_rotation());
    }

    /// `(a∘b)∘c` and `a∘(b∘c)` normalize at different intermediate
    /// steps, so they agree only to rounding — checked on the applied
    /// pose of several probe points, not on raw components.
    #[test]
    fn compose_is_associative_to_rounding() {
        let a = PartTransform::new(
            DVec3::new(1.0, -0.5, 2.0),
            DQuat::from_axis_angle(DVec3::X, 0.4),
        );
        let b = PartTransform::new(
            DVec3::new(-0.25, 0.75, 0.5),
            DQuat::from_axis_angle(DVec3::new(0.0, 1.0, 1.0), -0.9),
        );
        let c = PartTransform::new(
            DVec3::new(0.125, 0.0, -1.5),
            DQuat::from_axis_angle(DVec3::Y, 1.3),
        );
        let left = a.compose(b).compose(c);
        let right = a.compose(b.compose(c));
        for p in [
            DVec3::ZERO,
            DVec3::new(0.5, -0.25, 0.75),
            DVec3::new(-1.0, 2.0, 0.125),
        ] {
            assert_vec_close(left.apply(p), right.apply(p));
        }
    }

    /// Every `compose` renormalizes, so repeated composition cannot
    /// accumulate unit-quat drift: the norm stays within a few ulp of
    /// 1.0 even 64 levels deep.
    #[test]
    fn composed_rotation_stays_unit_over_depth() {
        let step = PartTransform::new(
            DVec3::new(0.05, -0.02, 0.01),
            DQuat::from_axis_angle(DVec3::new(1.0, 2.0, 3.0), 0.11),
        );
        let t = (0..64).fold(PartTransform::IDENTITY, |acc, _| acc.compose(step));
        assert!((t.rotation.norm() - 1.0).abs() < 1e-14);
        assert!(t.has_unit_rotation());
    }

    /// `apply_dir` rotates but never translates: a 120° rotation about
    /// the non-axis (1,1,1) cycles x→y→z even with a nonzero offset.
    #[test]
    fn apply_dir_rotates_without_translating() {
        let q = DQuat::from_axis_angle(DVec3::new(1.0, 1.0, 1.0), 2.0 * PI / 3.0);
        let t = PartTransform::new(DVec3::new(5.0, -3.0, 0.5), q);
        assert_vec_close(t.apply_dir(DVec3::X), DVec3::Y);
        assert_vec_close(t.apply_dir(DVec3::Y), DVec3::Z);
        assert_vec_close(t.apply_dir(DVec3::Z), DVec3::X);
        // The offset applies to points only: apply − offset == apply_dir.
        let p = DVec3::new(0.4, -1.2, 0.9);
        assert_vec_close(t.apply(p) - t.offset, t.apply_dir(p));
    }

    /// Each `is_valid` failure mode, and the tolerance boundary: a
    /// quat inside `UNIT_ROTATION_TOLERANCE` still counts as unit.
    #[test]
    fn is_valid_rejects_each_failure_mode() {
        let good = PartTransform::new(
            DVec3::new(1.0, 2.0, 3.0),
            DQuat::from_axis_angle(DVec3::Z, FRAC_PI_2),
        );
        assert!(good.is_valid());
        assert!(PartTransform::IDENTITY.is_valid());

        // Non-finite offset.
        assert!(!PartTransform::new(DVec3::new(f64::NAN, 0.0, 0.0), DQuat::IDENTITY).is_valid());
        assert!(
            !PartTransform::new(DVec3::new(0.0, f64::INFINITY, 0.0), DQuat::IDENTITY).is_valid()
        );

        // Non-finite, zero, and scaled rotations.
        for q in [
            DQuat::new(f64::NAN, 0.0, 0.0, 1.0),
            DQuat::new(0.0, 0.0, 0.0, 0.0),
            DQuat::new(0.0, 0.0, 0.0, 2.0),
            DQuat::new(1.0, 0.0, 0.0, -1.0), // norm² = 2
            DQuat::new(1.0, 2.0, 3.0, 4.0),  // norm² = 30
        ] {
            assert!(
                !PartTransform::new(DVec3::ZERO, q).is_valid(),
                "quat {q:?} must be rejected"
            );
        }

        // Inside tolerance: a unit quat scaled by (1 + 1e-11) — its
        // squared norm deviates by ≈2e-11 < 1e-9 — still valid.
        let slightly_off = DQuat::new(0.0, 0.0, 0.0, 1.0 + 1e-11);
        assert!(PartTransform::new(DVec3::ZERO, slightly_off).is_valid());
        // Just outside: scaled by (1 + 1e-6) → deviates ≈2e-6 > 1e-9.
        let too_far = DQuat::new(0.0, 0.0, 0.0, 1.0 + 1e-6);
        assert!(!PartTransform::new(DVec3::ZERO, too_far).is_valid());
    }

    /// `insert` keys the node; re-inserting under the same id returns
    /// the displaced node so duplicate ids are loud.
    #[test]
    fn insert_displaces_and_reports() {
        let mut g = PartGraph::new(PartNodeId(1));
        assert!(
            g.insert(
                PartNodeId(1),
                PartNode::new(None, PartTransform::IDENTITY, 0)
            )
            .is_none()
        );
        let displaced = g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 7),
        );
        assert!(displaced.is_some());
        assert_eq!(g.nodes[&PartNodeId(1)].effective_index, 7);
    }

    /// The attach point is the joint-load torque arm's `p_J`: a
    /// recorded joint reads through the PARENT's pose (a turned,
    /// offset parent carries it), an unrecorded one falls back to the
    /// node's own origin, and the root has no attachment.
    #[test]
    fn attach_point_reads_the_recorded_joint_through_the_parent() {
        let (root, child) = (PartNodeId(1), PartNodeId(2));
        let mut g = PartGraph::new(root);
        g.insert(root, PartNode::new(None, PartTransform::IDENTITY, 0));
        g.insert(
            child,
            PartNode::new(
                Some(root),
                PartTransform::new(DVec3::new(0.0, 0.0, 2.0), DQuat::IDENTITY),
                1,
            ),
        );
        // The parent sits at (10, 0, 0), quarter-turned about z — its
        // local +x is the body's +y.
        let quarter = DQuat::from_axis_angle(DVec3::Z, FRAC_PI_2);
        let transforms: BTreeMap<PartNodeId, PartTransform> = BTreeMap::from([
            (
                root,
                PartTransform::new(DVec3::new(10.0, 0.0, 0.0), quarter),
            ),
            (
                child,
                PartTransform::new(DVec3::new(10.0, 0.0, 2.0), quarter),
            ),
        ]);
        assert_eq!(
            g.nodes[&root].attach_point(root, &transforms),
            DVec3::new(10.0, 0.0, 0.0),
            "the root answers its origin"
        );
        assert_eq!(
            g.nodes[&child].attach_point(child, &transforms),
            DVec3::new(10.0, 0.0, 2.0),
            "unrecorded: the child's own origin"
        );
        let recorded = g.nodes[&child].clone();
        g.nodes
            .insert(child, recorded.with_joint(Some(DVec3::new(1.0, 0.0, 0.5))));
        assert_eq!(
            g.nodes[&child].attach_point(child, &transforms),
            DVec3::new(10.0, 1.0, 0.5),
            "recorded: the parent-frame point in the body frame"
        );
    }
}
