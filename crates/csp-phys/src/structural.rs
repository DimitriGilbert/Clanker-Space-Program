//! §2.4 breakup — the structural-overload evaluation and the
//! fragment split that turns a failed attachment into a debris body.
//!
//! # Joint-load model
//!
//! A welded craft is one rigid body; the joint between a node and its
//! parent is what transmits the force/torque the node's *subtree*
//! needs to keep moving with that body. For each non-root node `n`
//! the joint reaction is the difference between what the subtree's
//! rigid motion requires and what the subtree's real external forces
//! already supply:
//!
//! ```text
//! F_J = M_S·a(c_S) − Σᵢ F_ext,i                         (linear)
//! τ_J = I_J·α + ω×(I_J·ω) + M_S·(c_S−p_J)×a(p_J)
//!       − Σᵢ (r_i−p_J)×F_ext,i                          (about p_J)
//! ```
//!
//! where `S` is the subtree rooted at `n`, `a(x)` is the rigid-body
//! point acceleration `a_c + α×(x−c) + ω×(ω×(x−c))` measured from the
//! tick's actual Δv/Δω (so contact, thrust, aero, and gravity all
//! enter `a_c` — gravity cancels out of `F_J` identically, which is
//! what makes uniform gravity produce no joint load), `p_J` is the
//! node's body-frame attach point (`PartNode::joint` — the snapped
//! `AttachNode` position, falling back to `PartTransform::offset`
//! when unrecorded), `I_J` the subtree inertia about `p_J`, and
//! `F_ext,i` the external forces applied to part `i` this tick
//! (gravity, per-part aero, thrust sites, ground-contact impulses
//! ÷ dt).
//!
//! `F_J` decomposes against the attachment axis (the dominant axis
//! of the AABB-centre separation between the node and its parent —
//! the same convention the M4b conduction net uses) into
//! `axial = |F_J·axis|` and `shear = ‖F_J − axis(F_J·axis)‖`; the
//! attachment torque is `‖τ_J‖`. The **child** part's
//! `StructuralDef` limits govern its own attachment — a joint fails
//! when any of the three exceeds its authored limit.
//!
//! All quantities are evaluated in **body frame**, post-step (the
//! same convention the M4b thermal tick documents): the measured
//! accelerations are `(v₁−v₀)/dt` rotated into body frame and
//! `(ω₁−ω₀)/dt`, an O(dt) reading of the tick's real motion.
//!
//! Subtree aggregates (mass `M_S`, first moment `M_S·c_S`, inertia
//! about the body origin `A_S`, summed force `F_S`, summed moment
//! `T_S`) accumulate bottom-up in one pass — O(parts) total, never
//! per-subtree re-summation.
//!
//! # Fragmentation (§2.4)
//!
//! "Structural/thermal failure splits the part graph at the failed
//! node → two Stage-1 welds, momentum distributed from the parent's
//! rigid-body state." [`fragment_graph`] extracts a subtree as a
//! free-standing [`PartGraph`] whose root keeps its *body-frame*
//! transform, so a fragment weld's `com_local` IS the subtree's CoM
//! in the parent body frame. [`fragment_velocity`] stamps the
//! fragment's CoM velocity as the parent's rigid-body point velocity
//! at that CoM — `v + ω×(c_S − c)` — which conserves linear and
//! angular momentum *exactly* in continuous terms (the retained
//! fragment gets the same formula from the jettison convention);
//! §2.4's defined tolerances are checked by the sim-side test.
//!
//! Determinism: identical discipline to the weld — a fixed traversal
//! order (`joint_loads`' DFS preorder, `BTreeMap` key order
//! elsewhere), plain `f64`, no hash maps.

use std::collections::BTreeMap;

use csp_core::DVec3;

use crate::graph::{PartGraph, PartNodeId, PartTransform};
use crate::part::WeldPart;
use crate::tensor::SymMat3;
use crate::weld::{WeldError, WeldedBody};

/// One external force applied to a part, body frame: `force` applied
/// at `at` (the point the moment arm is taken from). Produced by
/// [`crate::simple::SimpleBackend::external_loads`]; consumed by
/// [`joint_loads`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartForce {
    /// Application point, body frame.
    pub at: DVec3,
    /// Force vector, body frame, newtons.
    pub force: DVec3,
}

/// The measured load at one attachment joint — compared against the
/// child part's `structural` limits by [`failed_joints`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointLoad {
    /// The child node whose attachment carried the load.
    pub node: PartNodeId,
    /// `|F_J·axis|` along the attach axis (N).
    pub axial_n: f64,
    /// `‖F_J ⊥ axis‖` (N).
    pub shear_n: f64,
    /// `‖τ_J‖` about the attach point (N·m).
    pub torque_nm: f64,
}

/// Per-node subtree aggregates — accumulated bottom-up.
#[derive(Clone, Copy)]
struct Agg {
    mass: f64,
    com_m: DVec3,
    inertia_origin: SymMat3,
    force: DVec3,
    moment: DVec3,
}

/// Evaluate every non-root attachment's load — see the module doc
/// for the model. `accel_body`/`alpha_body` are the tick's measured
/// accelerations in body frame (`(v₁−v₀)/dt` conjugated by `rot`,
/// `(ω₁−ω₀)/dt`); `ext` is the per-node external force list from the
/// backend. Returns loads in **depth-first preorder** from the graph
/// root (a parent ahead of its children — the traversal order the
/// split driver's subtree bookkeeping needs), NOT ascending
/// `PartNodeId`. [`failed_joints`] preserves that order; sim
/// consumers that need id order re-sort by `(depth, id)`.
#[allow(clippy::too_many_lines)]
pub fn joint_loads(
    body: &WeldedBody,
    graph: &PartGraph,
    parts: &[WeldPart],
    ext: &BTreeMap<PartNodeId, Vec<PartForce>>,
    accel_body: DVec3,
    alpha_body: DVec3,
    transforms: &BTreeMap<PartNodeId, PartTransform>,
) -> Result<Vec<JointLoad>, WeldError> {
    // Part AABBs for the attach-axis convention — the thermal net's
    // per-node hull bounds (same data the conduction edge axis uses).
    let aabbs: BTreeMap<PartNodeId, (DVec3, DVec3)> = body
        .thermal
        .nodes
        .iter()
        .map(|n| (n.node, (n.aabb_lo, n.aabb_hi)))
        .collect();

    // Children map + preorder — a node's descendants follow it, so
    // reversed order is children-before-parent (bottom-up).
    let mut children: BTreeMap<PartNodeId, Vec<PartNodeId>> = BTreeMap::new();
    for (id, node) in &graph.nodes {
        if let Some(p) = node.parent {
            children.entry(p).or_default().push(*id);
        }
    }
    let mut order = Vec::with_capacity(graph.nodes.len());
    let mut stack = vec![graph.root];
    while let Some(id) = stack.pop() {
        order.push(id);
        if let Some(cs) = children.get(&id) {
            stack.extend(cs.iter().rev().copied());
        }
    }

    // Own contribution per node, then subtree sums bottom-up.
    let omega = body.ang_vel;
    let mut aggs: BTreeMap<PartNodeId, Agg> = BTreeMap::new();
    for id in &order {
        let node = &graph.nodes[id];
        let t = transforms[id];
        let ep = &parts[node.effective_index as usize];
        let c_i = t.apply(ep.com);
        let mut agg = Agg {
            mass: ep.mass_kg,
            com_m: c_i * ep.mass_kg,
            inertia_origin: ep.inertia.rotate(t.rotation).shift(ep.mass_kg, c_i),
            force: DVec3::ZERO,
            moment: DVec3::ZERO,
        };
        if let Some(forces) = ext.get(id) {
            for f in forces {
                agg.force = agg.force + f.force;
                agg.moment = agg.moment + f.at.cross(f.force);
            }
        }
        aggs.insert(*id, agg);
    }
    for &id in order.iter().skip(1).rev() {
        let a = aggs[&id];
        let parent = graph.nodes[&id].parent.ok_or(WeldError::InternalInvariant(
            "structural: non-root node missing a parent",
        ))?;
        let p = aggs.get_mut(&parent).ok_or(WeldError::InternalInvariant(
            "structural: subtree parent missing from aggregates",
        ))?;
        p.mass += a.mass;
        p.com_m = p.com_m + a.com_m;
        p.inertia_origin = p.inertia_origin + a.inertia_origin;
        p.force = p.force + a.force;
        p.moment = p.moment + a.moment;
    }

    let com = body.com_local;
    let point_accel = |x: DVec3| {
        let r = x - com;
        accel_body + alpha_body.cross(r) + omega.cross(omega.cross(r))
    };

    let mut loads = Vec::new();
    for id in &order {
        let node = &graph.nodes[id];
        let Some(parent) = node.parent else { continue };
        let a = aggs[id];
        if a.mass <= 0.0 {
            continue; // a massless subtree carries no joint load
        }
        let c_s = a.com_m / a.mass;
        // Inertia about the subtree CoM, then about the joint point.
        let i_cs = a.inertia_origin.shift(-a.mass, c_s);
        // `p_j` is the edge's snapped attach point in body frame —
        // `PartNode::joint` records it in the PARENT's local frame
        // (C14-F4: the load's torque arm is measured about the point
        // the joint physically mates at, not the child's origin, which
        // can sit a full part-length away). A node with no recorded
        // joint — hand-built graphs, pre-field saves — falls back to
        // the child origin, the pre-field approximation.
        let p_j = node
            .joint
            .and_then(|j| transforms.get(&parent).map(|t| t.apply(j)))
            .unwrap_or(transforms[id].offset);
        let i_pj = i_cs.shift(a.mass, c_s - p_j);
        let f_j = a.force * -1.0 + point_accel(c_s) * a.mass;
        let tau_j = i_pj.mul_vec(alpha_body)
            + omega.cross(i_pj.mul_vec(omega))
            + (c_s - p_j).cross(point_accel(p_j)) * a.mass
            - (a.moment - p_j.cross(a.force));

        // Attach axis: dominant axis of the AABB-centre separation,
        // signed toward the child (the thermal net's convention).
        // When the centres coincide the dominant-axis rule would pick
        // an arbitrary +X (`0.0.signum() == 1.0`): the thermal net
        // drops such an edge outright (`build_thermal_net`'s
        // `sep < 1e-18` skip), and this pass takes the matching
        // fallback — a joint load still needs an axis, so it falls
        // back to the joint→subtree-CoM direction exactly like the
        // no-skin case below, one convention for both degeneracies.
        let fallback_axis = |d: DVec3| {
            if d.norm() > 1e-9 {
                d / d.norm()
            } else {
                DVec3::Z
            }
        };
        let axis = match (aabbs.get(id), aabbs.get(&parent)) {
            (Some(&(cl, ch)), Some(&(pl, ph))) => {
                let sep = ((cl + ch) - (pl + ph)) * 0.5;
                if sep.norm_squared() < 1e-18 {
                    fallback_axis(c_s - p_j)
                } else if sep.x.abs() >= sep.y.abs() && sep.x.abs() >= sep.z.abs() {
                    DVec3::new(sep.x.signum(), 0.0, 0.0)
                } else if sep.y.abs() >= sep.z.abs() {
                    DVec3::new(0.0, sep.y.signum(), 0.0)
                } else {
                    DVec3::new(0.0, 0.0, sep.z.signum())
                }
            }
            _ => {
                // Degenerate attachment (no skin on one side): fall
                // back to the joint→subtree-CoM direction, then +z.
                fallback_axis(c_s - p_j)
            }
        };
        let fa = f_j.dot(axis);
        loads.push(JointLoad {
            node: *id,
            axial_n: fa.abs(),
            shear_n: (f_j - axis * fa).norm(),
            torque_nm: tau_j.norm(),
        });
    }
    Ok(loads)
}

/// The attachments whose load exceeds the CHILD part's authored
/// `structural` limits, in `loads` order (the [`joint_loads`] DFS
/// preorder — the split driver re-sorts by depth before consuming).
/// A zero limit fails only on a strictly-positive load (`max = 0` is
/// authored data for "no tolerance", not "always broken").
///
/// Errors `InternalInvariant` rather than panicking when a load names
/// a node outside `graph` or a node whose `effective_index` is out of
/// `parts` — contract violations like the other typed paths here.
pub fn failed_joints(
    loads: &[JointLoad],
    graph: &PartGraph,
    parts: &[WeldPart],
) -> Result<Vec<PartNodeId>, WeldError> {
    let mut failed = Vec::new();
    for l in loads {
        let node = graph
            .nodes
            .get(&l.node)
            .ok_or(WeldError::InternalInvariant(
                "structural: joint load names a node outside the graph",
            ))?;
        let s = &parts
            .get(node.effective_index as usize)
            .ok_or(WeldError::InternalInvariant(
                "structural: effective index out of range in joint check",
            ))?
            .structural;
        if (l.axial_n > 0.0 && l.axial_n > s.max_axial_n)
            || (l.shear_n > 0.0 && l.shear_n > s.max_shear_n)
            || (l.torque_nm > 0.0 && l.torque_nm > s.max_torque_nm)
        {
            failed.push(l.node);
        }
    }
    Ok(failed)
}

/// The closed subtree rooted at `root`, ascending id.
pub fn subtree_set(graph: &PartGraph, root: PartNodeId) -> std::collections::BTreeSet<PartNodeId> {
    let mut set = std::collections::BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if set.insert(id) {
            stack.extend(
                graph
                    .nodes
                    .iter()
                    .filter(|(_, n)| n.parent == Some(id))
                    .map(|(c, _)| *c),
            );
        }
    }
    set
}

/// Extract the subtree rooted at `root` as a free-standing graph:
/// `root` becomes the fragment root with `transforms[root]` — its
/// composed *body-frame* transform — so the fragment weld's
/// `com_local` is the subtree CoM expressed in the parent body
/// frame. `transforms` comes from [`crate::weld::body_transforms`]
/// on the parent graph.
pub fn fragment_graph(
    graph: &PartGraph,
    transforms: &BTreeMap<PartNodeId, PartTransform>,
    root: PartNodeId,
) -> Result<PartGraph, WeldError> {
    fragment_graph_of(graph, transforms, root, &subtree_set(graph, root))
}

/// [`fragment_graph`] over an explicit member set. The sim's breakup
/// path subtracts already-claimed nested subtrees from
/// `subtree_set(root)` before calling this — a fragment must never
/// carry nodes another fragment owns (the alternative is fabricated
/// mass: the same subtree welded into two debris bodies).
///
/// `members` must contain `root` and be upward-closed inside the
/// subtree — a member's ancestors up to `root` are themselves
/// members — which is exactly what removing whole closed subtrees
/// from a subtree produces.
///
/// Errors `InternalInvariant` rather than panicking on a contract
/// violation: a `members` id absent from `graph`, or `root` missing
/// from `transforms` (the caller's `body_transforms` output covers
/// every graph node, so a miss means the arguments disagree).
pub fn fragment_graph_of(
    graph: &PartGraph,
    transforms: &BTreeMap<PartNodeId, PartTransform>,
    root: PartNodeId,
    members: &std::collections::BTreeSet<PartNodeId>,
) -> Result<PartGraph, WeldError> {
    let mut frag = PartGraph::new(root);
    for &id in members {
        let node = graph.nodes.get(&id).ok_or(WeldError::InternalInvariant(
            "structural: fragment member missing from the parent graph",
        ))?;
        let transform = if id == root {
            *transforms.get(&id).ok_or(WeldError::InternalInvariant(
                "structural: fragment root missing from body transforms",
            ))?
        } else {
            node.transform
        };
        frag.insert(
            id,
            crate::graph::PartNode::new(
                if id == root { None } else { node.parent },
                transform,
                node.effective_index,
            )
            // The joint record rides its edge into the fragment — a
            // non-root member keeps both its parent and the snapped
            // point's parent-frame meaning. The fragment root's edge
            // is severed, so its record is dropped (a `Some` would
            // dangle: the point is expressed in a frame this graph no
            // longer contains).
            .with_joint(if id == root { None } else { node.joint }),
        );
    }
    Ok(frag)
}

/// The fragment CoM velocity inherited from the parent rigid body —
/// `v + ω_world×(c_frag − c_parent)`, the momentum-conserving stamp
/// of §2.4. `frag_com_body` is the fragment weld's `com_local`
/// (the subtree CoM in parent body frame, per [`fragment_graph`]).
pub fn fragment_velocity(parent: &WeldedBody, frag_com_body: DVec3) -> (DVec3, DVec3) {
    let omega_world = parent.rot.rotate(parent.ang_vel);
    let vel = parent.vel + omega_world.cross(parent.rot.rotate(frag_com_body - parent.com_local));
    (vel, parent.ang_vel)
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::ColliderShape;
    use crate::graph::{PartNode, PartNodeId};
    use csp_core::DQuat;

    fn part(m: f64, half: f64) -> WeldPart {
        WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            parachute: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            mass_kg: m,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(m, DVec3::new(half, half, half)),
            collider: ColliderShape::Box {
                half_extents: DVec3::new(half, half, half),
            },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(0.8, 0.0, 1.0),
            thermal: csp_data::ThermalDef {
                heat_capacity_j_per_k: 400.0 * m,
                max_temp_k: 1_200.0,
                emissivity: 0.8,
                conduction_w_per_k: 50.0,
            },
            structural: csp_data::StructuralDef::unlimited(),
        }
    }

    /// A 3-part stack along +z — engine root, adapter, pod.
    fn stack() -> (PartGraph, Vec<WeldPart>) {
        let (e, a, p) = (PartNodeId(1), PartNodeId(2), PartNodeId(3));
        let mut g = PartGraph::new(e);
        g.insert(e, PartNode::new(None, PartTransform::IDENTITY, 0));
        g.insert(
            a,
            PartNode::new(
                Some(e),
                PartTransform::new(DVec3::new(0.0, 0.0, 1.0), DQuat::IDENTITY),
                1,
            ),
        );
        g.insert(
            p,
            PartNode::new(
                Some(a),
                PartTransform::new(DVec3::new(0.0, 0.0, 1.0), DQuat::IDENTITY),
                2,
            ),
        );
        (
            g,
            vec![part(500.0, 0.5), part(100.0, 0.3), part(200.0, 0.4)],
        )
    }

    fn total_pl(bodies: &[&WeldedBody]) -> (DVec3, DVec3) {
        let mut p = DVec3::ZERO;
        let mut l = DVec3::ZERO;
        for b in bodies {
            p = p + b.vel * b.mass_kg;
            l = l
                + b.rot.rotate(b.inertia_local.mul_vec(b.ang_vel))
                + b.com_world().cross(b.vel) * b.mass_kg;
        }
        (p, l)
    }

    /// §2.4's tolerances: a structural split conserves linear and
    /// angular momentum exactly — the fragments stamp the parent's
    /// rigid-body point velocity at their CoM.
    #[test]
    fn fragment_split_conserves_momentum_to_tolerance() {
        let (graph, parts) = stack();
        let mut body = crate::weld(&graph, &parts).unwrap();
        body.pos = DVec3::new(0.0, 0.0, 600_010.0);
        body.rot = DQuat::from_axis_angle(DVec3::new(0.3, 0.9, 0.1), 0.7);
        body.vel = DVec3::new(-42.0, 3.0, 150.0);
        body.ang_vel = DVec3::new(0.4, -0.2, 0.1);

        let (p0, l0) = total_pl(&[&body]);
        let transforms = crate::weld::body_transforms(&graph, &parts).unwrap();
        // Split at the adapter: {adapter,pod} fragment, {engine} retained.
        let frag = fragment_graph(&graph, &transforms, PartNodeId(2)).unwrap();
        let mut fb = crate::weld(&frag, &parts).unwrap();
        let mut retained_graph = graph.clone();
        for n in subtree_set(&graph, PartNodeId(2)) {
            retained_graph.nodes.remove(&n);
        }
        let mut rb = crate::weld(&retained_graph, &parts).unwrap();

        // The §2.4 stamp: fragment gets `v + ω×(c_S − c)`; retained
        // keeps the jettison correction `v + ω×(com_new − com_old)`.
        let (fv, fav) = fragment_velocity(&body, fb.com_local);
        let omega_w = body.rot.rotate(body.ang_vel);
        fb.pos = body.pos;
        fb.rot = body.rot;
        fb.vel = fv;
        fb.ang_vel = fav;
        rb.pos = body.pos;
        rb.rot = body.rot;
        rb.vel = body.vel + omega_w.cross(body.rot.rotate(rb.com_local - body.com_local));
        rb.ang_vel = body.ang_vel;

        let (p1, l1) = total_pl(&[&fb, &rb]);
        let m = body.mass_kg;
        let dp = (p1 - p0).norm() / p0.norm().max(m * 1.0);
        let i_max = body.inertia_local.trace() / 2.0; // largest principal bound
        let dl = (l1 - l0).norm() / l0.norm().max(i_max * 1e-3);
        assert!(dp < 1e-9, "linear momentum error {dp:e}");
        assert!(dl < 1e-9, "angular momentum error {dl:e}");
    }

    /// The joint-load evaluator flags a weak attachment under a
    /// measured acceleration and spares an unlimited one.
    #[test]
    fn joint_load_eval_flags_authored_limit() {
        let (mut graph, mut parts) = stack();
        // Weak adapter: any positive axial load fails it.
        parts[1].structural = csp_data::StructuralDef {
            max_axial_n: 1.0,
            max_shear_n: 1e30,
            max_torque_nm: 1e30,
        };
        let body = crate::weld(&graph, &parts).unwrap();
        // Uniform 10 m/s² body acceleration — the {adapter,pod}
        // subtree needs 300 N along the joint axis.
        let ext: BTreeMap<PartNodeId, Vec<PartForce>> = BTreeMap::new();
        let loads = joint_loads(
            &body,
            &graph,
            &parts,
            &ext,
            DVec3::new(0.0, 0.0, 10.0),
            DVec3::ZERO,
            &crate::weld::body_transforms(&graph, &parts).unwrap(),
        )
        .unwrap();
        let adapter = loads.iter().find(|l| l.node == PartNodeId(2)).unwrap();
        assert!(adapter.axial_n > 100.0, "expected a real axial load");
        let failed = failed_joints(&loads, &graph, &parts).unwrap();
        assert_eq!(failed, vec![PartNodeId(2)]);
        // Raise the limit: nothing fails.
        parts[1].structural = csp_data::StructuralDef::unlimited();
        let failed2 = failed_joints(&loads, &graph, &parts).unwrap();
        assert!(failed2.is_empty());
        // The pod's own joint is unlimited too.
        graph.nodes.remove(&PartNodeId(2));
        let _ = graph;
    }

    /// C14-F4: the recorded `PartNode::joint` — the snapped attach
    /// point in the parent's frame — is the torque arm's `p_J`, not
    /// the child's origin. The `{adapter,pod}` subtree (300 kg, CoM
    /// at z = 5/3 in the body frame) under a lateral 10 m/s² body
    /// acceleration reacts `M_S·(c_S−p_J)×a`: measured about the
    /// adapter's origin (z = 1) that is 300·(2/3)·10 = 2000 N·m;
    /// measured about a joint stamped at z = 0.5 on the parent face
    /// it is 300·(7/6)·10 = 3500 N·m.
    #[test]
    fn recorded_joint_moves_the_torque_arm() {
        let (graph, parts) = stack();
        let body = crate::weld(&graph, &parts).unwrap();
        let ext: BTreeMap<PartNodeId, Vec<PartForce>> = BTreeMap::new();
        let a = DVec3::new(10.0, 0.0, 0.0);
        let origin_arm = joint_loads(
            &body,
            &graph,
            &parts,
            &ext,
            a,
            DVec3::ZERO,
            &crate::weld::body_transforms(&graph, &parts).unwrap(),
        )
        .unwrap()
        .iter()
        .find(|l| l.node == PartNodeId(2))
        .unwrap()
        .torque_nm;
        // Same craft with the edge's snapped attach point recorded at
        // z = 0.5 on the root's face — the physical mating surface.
        let mut graph2 = graph.clone();
        let adapter = graph2.nodes.get_mut(&PartNodeId(2)).unwrap();
        adapter.joint = Some(DVec3::new(0.0, 0.0, 0.5));
        let joint_arm = joint_loads(
            &body,
            &graph2,
            &parts,
            &ext,
            a,
            DVec3::ZERO,
            &crate::weld::body_transforms(&graph2, &parts).unwrap(),
        )
        .unwrap()
        .iter()
        .find(|l| l.node == PartNodeId(2))
        .unwrap()
        .torque_nm;
        assert!(
            (origin_arm - 2_000.0).abs() < 1.0,
            "origin-fallback torque arm: {origin_arm}"
        );
        assert!(
            (joint_arm - 3_500.0).abs() < 1.0,
            "snapped-joint torque arm: {joint_arm}"
        );
    }
}
