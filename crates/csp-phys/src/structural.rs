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
//! node's body-frame attach point
//! ([`crate::graph::PartNode::attach_point`] — the
//! snapped `AttachNode` position, falling back to `PartTransform::offset`
//! when unrecorded), `I_J` the subtree inertia about `p_J`, and
//! `F_ext,i` the external forces applied to part `i` this tick
//! (gravity, per-part aero, thrust sites, ground-contact impulses
//! ÷ dt — the channel [`PartForce::ground`] marks).
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
//! # Touchdown absorption (M4c landing)
//!
//! The solver stops a landing body inside one tick, so the measured
//! `Δv/dt` of a touchdown is the whole impact speed over `1/64` s —
//! ~100 g at 16 m/s, more than any stack's inline joints survive. The
//! body's motion is the solver's and stays untouched; what the
//! evaluation reads is the stop **as if spread over a crush distance
//! `s`** (`balance.ron`'s `touchdown_crush_m`), and a crush stop is the
//! velocity change alone:
//!
//! - each component of the stop keeps `|Δv_c|·dt / (2·s)` of its share
//!   of the measured acceleration — the ratio of the load that would
//!   spread that component's removal over `s`, `m·Δv_c²/(2·s)`, to the
//!   one-tick load `m·Δv_c/dt`. The components are the stop along the
//!   contact normals (the crush) and the friction across it, so an
//!   inline joint above a touchdown carries exactly `m_above·Δv²/(2·s)`;
//! - the solved per-vertex impulses give way to their resultant on the
//!   parts that touched, through the CoM: it turns nothing. A
//!   sequential-impulse solve lands a flat base's whole stop on one rim
//!   vertex and spins the body up doing it — a one-tick artifact, not
//!   a load a crushing base applies — so the spin that solve imparted
//!   is lifted out of the reading whole.
//!
//! A stop that would outrun the crush distance (`|Δv_c|·dt ≥ 2·s`) is
//! not absorbed at all: the tick's own load is read, per-vertex forces
//! and spin change included, and a hot impact still breaks the stack.
//! The backend builds the record from every ground impulse it solved
//! this tick, the speculative near-miss rows included — a speculative
//! row brakes the body without a public [`crate::Contact`], and its
//! `Δv` is as much the touchdown as the penetrating rows'. Water is not
//! this channel: a splashdown's loads are the water drag's own, spread
//! by the water already.
//!
//! # Decelerator pull path
//!
//! A `Parachute` site's drag is one more `F_ext` — applied at the site,
//! off the stack's axis for a side-mounted canister — and it reaches
//! every joint of the craft, not only the chute's own mount: the joints
//! above the chute transmit it outright (`Σ F_ext` of their subtree),
//! and the rest carry the turn it induces (`I_J·α`).
//! [`JointModel::pull_budgets`] reads the model above as a bound on it —
//! every joint's load is the rest of the tick plus `F` times the pull's
//! own share, so `F` is what the tightest channel has room for. With
//! several sites pulling, the shares add, vector by vector: one scale
//! over their own budgets keeps every channel inside its derate.
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
/// at `at` (the point the moment arm is taken from), plus `torque`, a
/// pure couple on the part. Produced by
/// [`crate::simple::SimpleBackend::external_loads`]; consumed by
/// [`joint_loads`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartForce {
    /// Application point, body frame.
    pub at: DVec3,
    /// Force vector, body frame, newtons.
    pub force: DVec3,
    /// Pure couple, body frame, newtons·metres.
    pub torque: DVec3,
    /// The ground-contact channel — a solved ground impulse ÷ dt.
    /// Under the touchdown absorption ([`Touchdown`]) this channel
    /// yields to the spread stop's forces; every other channel is read
    /// as recorded.
    pub ground: bool,
}

/// The no-data touchdown crush distance, metres — half a metre of
/// structure crushing under a lander is what turns a ~15 m/s touchdown
/// (a ~100 g one-tick stop) into a load the shipped inline joints
/// survive. `balance.ron`'s `touchdown_crush_m` is the authored value.
pub const DEFAULT_TOUCHDOWN_CRUSH_M: f64 = 0.5;

/// The §2.4 touchdown absorption — one tick's ground-contact stop read
/// as spread over a crush distance (see the module doc). The backend
/// builds it from the impulses the tick solved; [`joint_loads`]
/// consumes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Touchdown {
    /// The stop's velocity change along the contact normals this tick,
    /// body frame (m/s) — the crush.
    normal_body: DVec3,
    /// Its change across them, body frame — the friction.
    tangent_body: DVec3,
    /// The crush distance's share of each: `|Δv_c|·dt / (2·s)`.
    keep_normal: f64,
    keep_tangent: f64,
    /// The stop's angular-velocity change as the tick solved it, body
    /// frame (rad/s).
    dw_body: DVec3,
    /// The spread stop's force on each touched part, ascending node:
    /// the impulse the solve landed on the part over the tick, each
    /// component scaled to its share per second, acting through the
    /// body's CoM (`WeldedBody::com_local`) — no application point rides
    /// the record, the evaluation takes the moment `com × f`. Empty
    /// when nothing touched.
    forces: Vec<(PartNodeId, DVec3)>,
}

impl Touchdown {
    /// The crush distance's share of each component of a stop —
    /// `|Δv_c|·dt / (2·crush)`. `None` without a crush distance or when
    /// either component would outrun it: at or above the full channel
    /// the one-tick load is already the lesser and is read as is.
    pub(crate) fn shares(
        normal_speed: f64,
        tangent_speed: f64,
        crush_m: f64,
    ) -> Option<(f64, f64)> {
        if crush_m > 0.0 {
            let share = |speed: f64| {
                let keep = speed * crate::PHYSICS_DT / (2.0 * crush_m);
                (keep < 1.0).then_some(keep)
            };
            Some((share(normal_speed)?, share(tangent_speed)?))
        } else {
            None
        }
    }

    /// The record of a stop whose normal and tangential velocity
    /// changes are `normal_body` / `tangent_body` and whose spin change
    /// is `dw_body` (all body frame), at the `shares` the crush
    /// distance keeps of each ([`Self::shares`]), its `forces` as the
    /// field doc describes. `None` when there is no stop at all.
    pub(crate) fn stopped_by(
        normal_body: DVec3,
        tangent_body: DVec3,
        dw_body: DVec3,
        shares: (f64, f64),
        forces: Vec<(PartNodeId, DVec3)>,
    ) -> Option<Self> {
        (normal_body != DVec3::ZERO || tangent_body != DVec3::ZERO).then_some(Self {
            keep_normal: shares.0,
            keep_tangent: shares.1,
            normal_body,
            tangent_body,
            dw_body,
            forces,
        })
    }

    /// The measured body-frame acceleration with each component of the
    /// stop scaled to its share.
    fn absorb_accel(&self, measured: DVec3) -> DVec3 {
        measured
            - self.normal_body * ((1.0 - self.keep_normal) / crate::PHYSICS_DT)
            - self.tangent_body * ((1.0 - self.keep_tangent) / crate::PHYSICS_DT)
    }

    /// The measured body-frame angular acceleration with the solved
    /// stop's spin change lifted out whole.
    fn absorb_alpha(&self, measured: DVec3) -> DVec3 {
        measured - self.dw_body / crate::PHYSICS_DT
    }
}

/// What one stepped tick did to a body — the kinematic half of
/// [`joint_loads`]' input: the measured body-frame acceleration
/// `(v₁−v₀)/dt` conjugated by the post-step `rot`, the measured
/// body-frame angular acceleration `(ω₁−ω₀)/dt`, and the tick's
/// ground-contact stop record when it is read as spread over the crush
/// distance — `None` without a stop and for one that outruns it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TickMotion<'a> {
    /// `(v₁−v₀)/dt`, body frame.
    pub accel_body: DVec3,
    /// `(ω₁−ω₀)/dt`, body frame.
    pub alpha_body: DVec3,
    /// The tick's [`Touchdown`], when there is one.
    pub touchdown: Option<&'a Touchdown>,
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
/// for the model. `ext` is the per-node external force list from the
/// backend; `transforms` the craft's body-frame skeleton; `motion`
/// what the tick did to the body ([`TickMotion`]). Returns loads in
/// **depth-first preorder** from the graph
/// root (a parent ahead of its children — the traversal order the
/// split driver's subtree bookkeeping needs), NOT ascending
/// `PartNodeId`. [`failed_joints`] preserves that order; sim
/// consumers that need id order re-sort by `(depth, id)`.
pub fn joint_loads(
    body: &WeldedBody,
    graph: &PartGraph,
    parts: &[WeldPart],
    ext: &BTreeMap<PartNodeId, Vec<PartForce>>,
    transforms: &BTreeMap<PartNodeId, PartTransform>,
    motion: TickMotion<'_>,
) -> Result<Vec<JointLoad>, WeldError> {
    Ok(
        joint_load_vecs(body, graph, parts, ext, transforms, motion)?
            .into_iter()
            .map(|j| {
                let fa = j.force.dot(j.axis);
                JointLoad {
                    node: j.node,
                    axial_n: fa.abs(),
                    shear_n: (j.force - j.axis * fa).norm(),
                    torque_nm: j.torque.norm(),
                }
            })
            .collect(),
    )
}

/// One joint's load before the channel decomposition — the reaction
/// force and torque the model gives it, and the axis [`joint_loads`]
/// splits the force on. [`JointModel::pull_budgets`] adds a decelerator
/// site's pull to these; the magnitudes cannot be added to.
#[derive(Clone, Copy)]
struct JointVec {
    node: PartNodeId,
    force: DVec3,
    torque: DVec3,
    axis: DVec3,
}

/// [`joint_loads`] before the decomposition — every evaluated joint's
/// [`JointVec`], in the same preorder.
fn joint_load_vecs(
    body: &WeldedBody,
    graph: &PartGraph,
    parts: &[WeldPart],
    ext: &BTreeMap<PartNodeId, Vec<PartForce>>,
    transforms: &BTreeMap<PartNodeId, PartTransform>,
    motion: TickMotion<'_>,
) -> Result<Vec<JointVec>, WeldError> {
    let TickMotion {
        accel_body,
        alpha_body,
        touchdown,
    } = motion;
    // The touchdown absorption rewrites the ground-contact channel
    // wherever the evaluation reads it: the measured accelerations
    // here, the channel's forces in the own-contribution loop below.
    let (accel_body, alpha_body) = match touchdown {
        Some(t) => (t.absorb_accel(accel_body), t.absorb_alpha(alpha_body)),
        None => (accel_body, alpha_body),
    };
    let aabbs = node_aabbs(body);
    let order = preorder(graph);
    let aggs = subtree_aggs(
        graph,
        parts,
        transforms,
        &order,
        ext,
        touchdown,
        body.com_local,
    )?;

    let omega = body.ang_vel;
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
        // `p_j` is the edge's snapped attach point in body frame
        // (C14-F4: the load's torque arm is measured about the point
        // the joint physically mates at, not the child's origin, which
        // can sit a full part-length away).
        let p_j = node.attach_point(*id, transforms);
        let i_pj = i_cs.shift(a.mass, c_s - p_j);
        let f_j = a.force * -1.0 + point_accel(c_s) * a.mass;
        let tau_j = i_pj.mul_vec(alpha_body)
            + omega.cross(i_pj.mul_vec(omega))
            + (c_s - p_j).cross(point_accel(p_j)) * a.mass
            - (a.moment - p_j.cross(a.force));

        let axis = attach_axis(
            aabbs.get(id).copied(),
            aabbs.get(&parent).copied(),
            c_s - p_j,
        );
        loads.push(JointVec {
            node: *id,
            force: f_j,
            torque: tau_j,
            axis,
        });
    }
    Ok(loads)
}

/// Every node's subtree aggregates ([`Agg`]): the own contributions in
/// `order`, then the bottom-up sums over its reverse — children before
/// parents, so a parent reads its subtree whole when its turn comes.
/// `order` is [`preorder`]'s output; `ext` the per-node external
/// forces, the touchdown absorption already applied to them; `com` the
/// body's CoM, body frame — the point every [`Touchdown`] force acts
/// through, so its moment about the origin is `com × f` and about a
/// joint `(com − p_J) × f`: the same arm the inertia term beside it
/// measures from.
fn subtree_aggs(
    graph: &PartGraph,
    parts: &[WeldPart],
    transforms: &BTreeMap<PartNodeId, PartTransform>,
    order: &[PartNodeId],
    ext: &BTreeMap<PartNodeId, Vec<PartForce>>,
    touchdown: Option<&Touchdown>,
    com: DVec3,
) -> Result<BTreeMap<PartNodeId, Agg>, WeldError> {
    // Own contribution per node, then subtree sums bottom-up.
    let mut aggs: BTreeMap<PartNodeId, Agg> = BTreeMap::new();
    for id in order {
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
                // Under absorption the solved ground impulses yield to
                // the spread stop's forces.
                if f.ground && touchdown.is_some() {
                    continue;
                }
                agg.force = agg.force + f.force;
                agg.moment = agg.moment + f.at.cross(f.force) + f.torque;
            }
        }
        if let Some((_, f)) = touchdown.and_then(|t| t.forces.iter().find(|(n, _)| n == id)) {
            agg.force = agg.force + *f;
            agg.moment = agg.moment + com.cross(*f);
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
    Ok(aggs)
}

/// The thermal net's per-node hull bounds — the attach-axis
/// convention's input (same data the conduction edge axis uses).
fn node_aabbs(body: &WeldedBody) -> BTreeMap<PartNodeId, (DVec3, DVec3)> {
    body.thermal
        .nodes
        .iter()
        .map(|n| (n.node, (n.aabb_lo, n.aabb_hi)))
        .collect()
}

/// The graph's depth-first preorder from the root — a node's
/// descendants follow it, so the reversed order is
/// children-before-parent (bottom-up): the traversal [`joint_loads`]
/// evaluates in and [`pull_path`] sums subtrees in.
fn preorder(graph: &PartGraph) -> Vec<PartNodeId> {
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
    order
}

/// The attach axis of a joint — the convention [`joint_loads`] splits
/// its load on and [`pull_path`] hands the reef: the dominant axis of
/// the AABB-centre separation between the child and its parent, signed
/// toward the child (the thermal net's convention). `child`/`parent`
/// are the two nodes' hull bounds ([`node_aabbs`]); `joint_to_com` the
/// joint's subtree CoM less its attach point.
///
/// When the centres coincide the dominant-axis rule would pick an
/// arbitrary +X (`0.0.signum() == 1.0`): the thermal net drops such an
/// edge outright (`build_thermal_net`'s `sep < 1e-18` skip), and the
/// load model takes the matching fallback — a joint load still needs
/// an axis, so it falls back to the joint→subtree-CoM direction
/// exactly like the no-skin case, one convention for both
/// degeneracies.
fn attach_axis(
    child: Option<(DVec3, DVec3)>,
    parent: Option<(DVec3, DVec3)>,
    joint_to_com: DVec3,
) -> DVec3 {
    let fallback_axis = |d: DVec3| {
        if d.norm() > 1e-9 {
            d / d.norm()
        } else {
            DVec3::Z
        }
    };
    match (child, parent) {
        (Some((cl, ch)), Some((pl, ph))) => {
            let sep = ((cl + ch) - (pl + ph)) * 0.5;
            if sep.norm_squared() < 1e-18 {
                fallback_axis(joint_to_com)
            } else if sep.x.abs() >= sep.y.abs() && sep.x.abs() >= sep.z.abs() {
                DVec3::new(sep.x.signum(), 0.0, 0.0)
            } else if sep.y.abs() >= sep.z.abs() {
                DVec3::new(0.0, sep.y.signum(), 0.0)
            } else {
                DVec3::new(0.0, 0.0, sep.z.signum())
            }
        }
        // Degenerate attachment (no skin on one side): fall back to
        // the joint→subtree-CoM direction, then +z.
        _ => fallback_axis(joint_to_com),
    }
}

/// The decelerator site the pull-path reef reads a budget for: the
/// node that carries it, where in the body its drag applies, and which
/// way that drag pushes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PullSite {
    /// The node carrying the decelerator — its own mount is the one
    /// joint the reef leaves out.
    pub node: PartNodeId,
    /// Application point of the drag, body frame.
    pub pos: DVec3,
    /// Unit direction the drag pushes, body frame.
    pub drag: DVec3,
}

/// The joint-load model bound to one craft: its weld, its graph, its
/// effective parts and its body-frame skeleton — the four inputs every
/// evaluation in this module reads. [`joint_loads`] and
/// [`failed_joints`] take them one by one; this is the same bundle,
/// held, for the reef that reads it three times over.
#[derive(Clone, Copy)]
pub struct JointModel<'a> {
    body: &'a WeldedBody,
    graph: &'a PartGraph,
    parts: &'a [WeldPart],
    transforms: &'a BTreeMap<PartNodeId, PartTransform>,
}

impl<'a> JointModel<'a> {
    /// The model of the craft `weld` produced `body` from, in the
    /// skeleton [`crate::weld::body_transforms`] gives for it.
    pub const fn new(
        body: &'a WeldedBody,
        graph: &'a PartGraph,
        parts: &'a [WeldPart],
        transforms: &'a BTreeMap<PartNodeId, PartTransform>,
    ) -> Self {
        Self {
            body,
            graph,
            parts,
            transforms,
        }
    }

    /// The decelerator reef's **pull path** (C14-F1's companion): for
    /// every site of `sites`, in its order, the greatest drag (N) it may
    /// apply this tick, along its `drag`, without loading any joint of
    /// the craft past `margin` times its authored limits — with every
    /// site pulling at once. A site's drag reaches every joint,
    /// whichever way the graph is rooted: the joints from the chute up
    /// to the root have the site in their subtrees and transmit its pull
    /// outright, and every other joint has to bring its own part of the
    /// mass along under it — an off-axis pull turns the whole craft, and
    /// the Stock Hopper's pod-to-canister joint is one it broke. Each
    /// chute's own mount is left to its canopy's reef
    /// ([`crate::weld::DecelSite`]'s `reef_n` and co): a joint is read
    /// under every pull but the sites mounted on it.
    ///
    /// The answer is the joint-load model read as a bound, with nothing
    /// but its own readings: `loads` — the rest of the tick's external
    /// forces (aero, thrust, gravity) as the §2.4 evaluation reads them
    /// — under the rigid response the weld gives `response`, the forces
    /// the step integrates (`ΣF/m`, `I⁻¹(Στ − ω×Iω)`); then, per site,
    /// the same with a unit pull and with its negative, whose difference
    /// is the pull's own share per newton and cancels every term the
    /// pull does not cause. A drag of `F` loads a joint with exactly the
    /// rest plus `F` times that share, so each channel leaves `F` the
    /// room between the rest and its derated limit ([`pull_room`]); the
    /// least over every joint and channel is what the site may pull
    /// alone. `loads` and `response` are one map when the evaluation
    /// reads the forces where they act.
    ///
    /// One site, that is its budget. Several pull together, and what
    /// each may pull alone is not what they may pull together: two
    /// off-axis canopies, each at its own budget, load the one joint
    /// both turn with the sum of both (two Stock Hopper side chutes met
    /// the battery joint's torque rating 3.5 s after their catch). So
    /// every site's budget is its own times the one scale that leaves
    /// every channel inside its derate with all of them pulling it: the
    /// pulls' shares add, vector by vector, each times its budget. The
    /// scale never exceeds 1 — beside another chute a site pulls no more
    /// than alone: two canopies whose turns cancel are level only while
    /// the stack stands exactly between them, and the derate's working
    /// point was measured for one.
    ///
    /// `0` when the rest alone already overloads a channel the pull
    /// loads; `f64::INFINITY` for a site nothing beside its mount feels
    /// the pull of (a pod-alone chute) — then the mount's reef alone
    /// caps the site.
    ///
    /// Errors `InternalInvariant` rather than panicking when a site's
    /// node is not a node of the graph or a node's `effective_index`
    /// falls outside the parts — contract violations like the other
    /// typed paths here.
    pub fn pull_budgets(
        &self,
        loads: &BTreeMap<PartNodeId, Vec<PartForce>>,
        response: &BTreeMap<PartNodeId, Vec<PartForce>>,
        sites: &[PullSite],
        margin: f64,
    ) -> Result<Vec<f64>, WeldError> {
        let Self { graph, parts, .. } = *self;
        for PullSite { node, .. } in sites {
            if !graph.nodes.contains_key(node) {
                return Err(WeldError::InternalInvariant(
                    "structural: decelerator site names a node outside the graph",
                ));
            }
        }
        if graph
            .nodes
            .values()
            .any(|n| n.effective_index as usize >= parts.len())
        {
            return Err(WeldError::InternalInvariant(
                "structural: effective index out of range in pull budget",
            ));
        }
        let PullReadings { rest, shares } = self.pull_readings(loads, response, sites)?;
        let structural =
            |joint: &JointVec| &parts[graph.nodes[&joint.node].effective_index as usize].structural;
        // What each site may pull alone: every joint's every channel but
        // its own mount's.
        let alone: Vec<f64> = (0..sites.len())
            .map(|i| {
                rest.iter()
                    .enumerate()
                    .filter(|(_, joint)| joint.node != sites[i].node)
                    .map(|(k, joint)| {
                        joint_pull_room(joint, shares[i][k], structural(joint), margin)
                    })
                    .fold(f64::INFINITY, f64::min)
            })
            .collect();
        if sites.len() < 2 {
            return Ok(alone);
        }
        // Each site at its own budget; one with none to speak of (nothing
        // bounds it alone) is read at the largest finite budget beside it.
        let largest = alone
            .iter()
            .copied()
            .filter(|b| b.is_finite())
            .fold(0.0, f64::max);
        if alone.iter().all(|b| !b.is_finite()) {
            return Ok(alone);
        }
        let weights: Vec<f64> = alone
            .iter()
            .map(|b| if b.is_finite() { *b } else { largest })
            .collect();
        let mut scale = 1.0_f64;
        for (k, joint) in rest.iter().enumerate() {
            let (mut pull_f, mut pull_t) = (DVec3::ZERO, DVec3::ZERO);
            let mut others = false;
            for (i, site) in sites.iter().enumerate() {
                if site.node == joint.node {
                    continue;
                }
                others = true;
                pull_f = pull_f + shares[i][k].0 * weights[i];
                pull_t = pull_t + shares[i][k].1 * weights[i];
            }
            if others {
                scale = scale.min(joint_pull_room(
                    joint,
                    (pull_f, pull_t),
                    structural(joint),
                    margin,
                ));
            }
        }
        Ok(alone
            .iter()
            .map(|b| if b.is_finite() { b * scale } else { *b })
            .collect())
    }

    /// [`PullReadings`] under `loads` and `response`, one share list
    /// per site of `sites`.
    fn pull_readings(
        &self,
        loads: &BTreeMap<PartNodeId, Vec<PartForce>>,
        response: &BTreeMap<PartNodeId, Vec<PartForce>>,
        sites: &[PullSite],
    ) -> Result<PullReadings, WeldError> {
        let Self {
            body,
            graph,
            parts,
            transforms,
        } = *self;
        // The body's response to the forces the step integrates: its
        // linear and angular acceleration.
        let com = body.com_local;
        let (mut force, mut torque) = (DVec3::ZERO, DVec3::ZERO);
        for fs in response.values() {
            for f in fs {
                force = force + f.force;
                torque = torque + (f.at - com).cross(f.force) + f.torque;
            }
        }
        let omega = body.ang_vel;
        let inv_inertia = body.inertia_local.inverse().unwrap_or(SymMat3::ZERO);
        let gyro = omega.cross(body.inertia_local.mul_vec(omega));
        let motion = |f: DVec3, tau: DVec3| TickMotion {
            accel_body: f / body.mass_kg,
            alpha_body: inv_inertia.mul_vec(tau - gyro),
            touchdown: None,
        };
        let read = |ext: &BTreeMap<PartNodeId, Vec<PartForce>>, m: TickMotion<'_>| {
            joint_load_vecs(body, graph, parts, ext, transforms, m)
        };
        // The rest as the evaluation reads it, under the step's own motion.
        let rest = read(loads, motion(force, torque))?;
        let mut shares = Vec::with_capacity(sites.len());
        for PullSite { node, pos, drag } in sites {
            let pull_ext = |f: DVec3| {
                BTreeMap::from([(
                    *node,
                    vec![PartForce {
                        at: *pos,
                        force: f,
                        torque: DVec3::ZERO,
                        ground: false,
                    }],
                )])
            };
            let lever = *pos - com;
            let plus = read(&pull_ext(*drag), motion(*drag, lever.cross(*drag)))?;
            let minus = read(
                &pull_ext(*drag * -1.0),
                motion(*drag * -1.0, lever.cross(*drag) * -1.0),
            )?;
            shares.push(
                rest.iter()
                    .zip(&plus)
                    .zip(&minus)
                    .map(|((_, p), m)| ((p.force - m.force) * 0.5, (p.torque - m.torque) * 0.5))
                    .collect(),
            );
        }
        Ok(PullReadings { rest, shares })
    }
}

/// One pull's own share per newton at a joint: its `(force, torque)`.
type PullShare = (DVec3, DVec3);

/// [`JointModel::pull_budgets`]' readings: every joint's rest under the
/// tick's other loads, moved by the rigid response the weld gives the
/// forces the step integrates, and — per site, in the order asked — that
/// pull's own [`PullShare`] at every joint: half the difference of a
/// unit pull and its negative, which cancels the spin terms and anything
/// else not the pull's doing.
struct PullReadings {
    rest: Vec<JointVec>,
    shares: Vec<Vec<PullShare>>,
}

/// What a pull leaves a joint: the least room its three channels —
/// torque, shear, and the force along the attach axis — give a pull of
/// `share` per newton beside the joint's own rest load, each channel to
/// `margin` times its limit ([`pull_room`]).
fn joint_pull_room(
    joint: &JointVec,
    share: PullShare,
    limits: &csp_data::StructuralDef,
    margin: f64,
) -> f64 {
    let (pull_f, pull_t) = share;
    let (along, pull_along) = (joint.force.dot(joint.axis), pull_f.dot(joint.axis));
    [
        (joint.torque, pull_t),
        (
            joint.force - joint.axis * along,
            pull_f - joint.axis * pull_along,
        ),
        (
            DVec3::new(along, 0.0, 0.0),
            DVec3::new(pull_along, 0.0, 0.0),
        ),
    ]
    .into_iter()
    .zip([limits.max_torque_nm, limits.max_shear_n, limits.max_axial_n])
    .map(|((rest, pull), limit)| pull_room(rest, pull, limit * margin))
    .fold(f64::INFINITY, f64::min)
}

/// The greatest `F ≥ 0` with `‖rest + F·pull‖ ≤ limit` — what a
/// channel's rest load leaves its pull's share: the far root of the
/// quadratic. `0` when the rest alone is past the limit; unbounded
/// when the pull does not load the channel or the limit is not finite.
fn pull_room(rest: DVec3, pull: DVec3, limit: f64) -> f64 {
    if !limit.is_finite() {
        return f64::INFINITY;
    }
    let pull_sq = pull.norm_squared();
    if pull_sq == 0.0 {
        return f64::INFINITY;
    }
    // `≤ 0` while the rest is inside the limit.
    let beyond = rest.norm_squared() - limit * limit;
    if beyond > 0.0 {
        return 0.0;
    }
    let half = rest.dot(pull);
    let root = libm::sqrt(half * half - pull_sq * beyond);
    if half < 0.0 {
        // The pull first relieves the rest: room until the far side.
        (root - half) / pull_sq
    } else if beyond == 0.0 {
        0.0
    } else {
        // The pull adds to the rest: the root rationalized, so a rest
        // at the limit does not cancel it away.
        -beyond / (half + root)
    }
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
            &crate::weld::body_transforms(&graph, &parts).unwrap(),
            TickMotion {
                accel_body: DVec3::new(0.0, 0.0, 10.0),
                alpha_body: DVec3::ZERO,
                touchdown: None,
            },
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
            &crate::weld::body_transforms(&graph, &parts).unwrap(),
            TickMotion {
                accel_body: a,
                alpha_body: DVec3::ZERO,
                touchdown: None,
            },
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
            &crate::weld::body_transforms(&graph2, &parts).unwrap(),
            TickMotion {
                accel_body: a,
                alpha_body: DVec3::ZERO,
                touchdown: None,
            },
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

    /// The record of a stop, or `None` when the crush distance does not
    /// absorb it — the backend's own construction.
    fn spread(
        normal: DVec3,
        tangent: DVec3,
        dw: DVec3,
        crush_m: f64,
        forces: Vec<(PartNodeId, DVec3)>,
    ) -> Option<Touchdown> {
        Touchdown::shares(normal.norm(), tangent.norm(), crush_m)
            .and_then(|shares| Touchdown::stopped_by(normal, tangent, dw, shares, forces))
    }

    /// The `stack()` craft caught flat on the engine's base by a `dv`
    /// stop the solver landed in one tick, read as spread over
    /// `crush_m`: the solver's own per-vertex force in `ext`, the tick's
    /// measured acceleration, and — with `with_gravity` — gravity on
    /// each part, which is not the channel and must cancel either way.
    fn caught_stack_loads(dv: f64, crush_m: f64, with_gravity: bool) -> Vec<JointLoad> {
        let (graph, parts) = stack();
        let body = crate::weld(&graph, &parts).unwrap();
        let transforms = crate::weld::body_transforms(&graph, &parts).unwrap();
        let dt = crate::PHYSICS_DT;
        let engine = PartNodeId(1);
        let mut ext: BTreeMap<PartNodeId, Vec<PartForce>> = BTreeMap::from([(
            engine,
            vec![PartForce {
                at: DVec3::new(0.0, 0.0, -0.5),
                force: DVec3::new(0.0, 0.0, body.mass_kg * dv / dt),
                torque: DVec3::ZERO,
                ground: true,
            }],
        )]);
        let mut accel = DVec3::new(0.0, 0.0, dv / dt);
        if with_gravity {
            for (i, id) in [PartNodeId(1), PartNodeId(2), PartNodeId(3)]
                .into_iter()
                .enumerate()
            {
                ext.entry(id).or_default().push(PartForce {
                    at: DVec3::ZERO,
                    force: DVec3::new(0.0, 0.0, -9.81 * parts[i].mass_kg),
                    torque: DVec3::ZERO,
                    ground: false,
                });
            }
            accel.z -= 9.81;
        }
        let touchdown = Touchdown::shares(dv, 0.0, crush_m).and_then(|(keep, _)| {
            Touchdown::stopped_by(
                DVec3::new(0.0, 0.0, dv),
                DVec3::ZERO,
                DVec3::ZERO,
                (keep, 0.0),
                vec![(engine, DVec3::new(0.0, 0.0, body.mass_kg * dv * keep / dt))],
            )
        });
        joint_loads(
            &body,
            &graph,
            &parts,
            &ext,
            &transforms,
            TickMotion {
                accel_body: accel,
                alpha_body: DVec3::ZERO,
                touchdown: touchdown.as_ref(),
            },
        )
        .unwrap()
    }

    /// M4c touchdown absorption: a stop the solver lands in one tick is
    /// read as if spread over the crush distance. The stack is caught
    /// on the engine's base — the engine is the root, so the adapter's
    /// joint carries the 300 kg above it. Raw, that is `300·Δv/dt`;
    /// under the absorption it is `m_above·Δv²/(2·s)`: 19 200 N at 8 m/s
    /// over 0.5 m against 153 600 N. Gravity is not the channel — it
    /// cancels identically either way — and a stop that would outrun
    /// the crush distance (`|Δv|·dt ≥ 2·s`) is not absorbed at all:
    /// 64 m/s is exactly the 0.5 m tick, and 100 m/s is the hot impact
    /// that still breaks the stack.
    #[test]
    fn touchdown_reads_the_stop_over_the_crush_distance() {
        let adapter_axial = |dv: f64, crush_m: f64, with_gravity: bool| {
            caught_stack_loads(dv, crush_m, with_gravity)
                .into_iter()
                .find(|l| l.node == PartNodeId(2))
                .unwrap()
                .axial_n
        };
        for with_gravity in [false, true] {
            assert!(
                (adapter_axial(8.0, -1.0, with_gravity) - 153_600.0).abs() < 1e-6,
                "the raw one-tick stop is m_above·Δv/dt"
            );
            assert!(
                (adapter_axial(8.0, 0.5, with_gravity) - 19_200.0).abs() < 1e-6,
                "the absorbed stop is m_above·Δv²/(2·s)"
            );
            assert!(
                (adapter_axial(64.0, 0.5, with_gravity) - 1_228_800.0).abs() < 1e-6,
                "a stop exactly the crush distance long keeps its own load"
            );
            assert!(
                (adapter_axial(100.0, 0.5, with_gravity) - 1_920_000.0).abs() < 1e-6,
                "a hot impact keeps the tick's load"
            );
            assert!(
                (adapter_axial(8.0, 0.0, with_gravity) - 153_600.0).abs() < 1e-6,
                "no crush distance is no absorption"
            );
        }
    }

    /// The friction across the contact is its own stop — the same law
    /// over its own velocity change: a 0.63 m/s sideways kick beside an
    /// 8 m/s crush reads at 0.63²/(2·0.5) = 0.40 m/s², not as a share of
    /// the crush, and the solved stop's own spin leaves the reading
    /// whole. No stop, no crush distance, or a component that outruns
    /// it: nothing to spread.
    #[test]
    fn the_friction_across_a_touchdown_is_its_own_stop() {
        let dt = crate::PHYSICS_DT;
        let (crush, slide) = (DVec3::new(0.0, 0.0, 8.0), DVec3::new(0.0, 0.63, 0.0));
        let record = spread(crush, slide, DVec3::new(0.5, 0.0, 0.0), 0.5, Vec::new()).unwrap();
        assert_eq!(record.keep_normal, 0.125);
        let absorbed = record.absorb_accel(crush / dt + slide / dt);
        assert!((absorbed.z - 64.0).abs() < 1e-9, "{absorbed:?}");
        assert!((absorbed.y - 0.63 * 0.63).abs() < 1e-9, "{absorbed:?}");
        assert_eq!(
            record.absorb_alpha(DVec3::new(32.0, 0.0, 1.0)),
            DVec3::new(0.0, 0.0, 1.0)
        );
        assert!(
            spread(DVec3::ZERO, DVec3::ZERO, DVec3::ZERO, 0.5, Vec::new()).is_none(),
            "no stop"
        );
        assert!(
            spread(crush, DVec3::ZERO, DVec3::ZERO, 0.0, Vec::new()).is_none(),
            "no crush distance"
        );
        assert!(
            spread(
                crush,
                DVec3::new(0.0, 64.0, 0.0),
                DVec3::ZERO,
                0.5,
                Vec::new()
            )
            .is_none(),
            "a component that outruns the crush distance is not absorbed"
        );
    }

    /// The one-tick solve lands a flat base's whole stop on one rim
    /// vertex and spins the body up doing it. Read raw, that is a
    /// bending moment on every joint of the stack; absorbed, the same
    /// impulse is the pure axial load through the CoM — nothing turns,
    /// no joint carries torque or shear, wherever the solve landed it.
    #[test]
    fn touchdown_is_the_stop_alone() {
        let (graph, parts) = stack();
        let body = crate::weld(&graph, &parts).unwrap();
        let transforms = crate::weld::body_transforms(&graph, &parts).unwrap();
        let dt = crate::PHYSICS_DT;
        let (dv, crush_m) = (8.0, 0.5);
        let impulse = DVec3::new(0.0, 0.0, body.mass_kg * dv);
        let loads_at = |point: DVec3, absorbed: bool| {
            // The solve's own reading: the whole impulse at `point`,
            // the body's spin changed by its moment about the CoM.
            let dw = body
                .inertia_local
                .inverse()
                .unwrap()
                .mul_vec((point - body.com_local).cross(impulse));
            let ext = BTreeMap::from([(
                PartNodeId(1),
                vec![PartForce {
                    at: point,
                    force: impulse / dt,
                    torque: DVec3::ZERO,
                    ground: true,
                }],
            )]);
            let touchdown = absorbed
                .then(|| {
                    let (keep, _) = Touchdown::shares(dv, 0.0, crush_m)?;
                    Touchdown::stopped_by(
                        DVec3::new(0.0, 0.0, dv),
                        DVec3::ZERO,
                        dw,
                        (keep, 0.0),
                        vec![(PartNodeId(1), impulse * (keep / dt))],
                    )
                })
                .flatten();
            joint_loads(
                &body,
                &graph,
                &parts,
                &ext,
                &transforms,
                TickMotion {
                    accel_body: DVec3::new(0.0, 0.0, dv / dt),
                    alpha_body: dw / dt,
                    touchdown: touchdown.as_ref(),
                },
            )
            .unwrap()
        };
        let rim = DVec3::new(0.5, 0.0, -0.5);
        let base = DVec3::new(0.0, 0.0, -0.5);
        let raw = loads_at(rim, false);
        assert!(
            raw.iter().all(|l| l.torque_nm > 1.0e3) && raw[0].torque_nm > 1.0e4,
            "the one-tick rim stop bends every joint: {raw:?}"
        );
        for point in [rim, base] {
            let loads = loads_at(point, true);
            assert!(
                loads.iter().all(|l| l.torque_nm < 1e-9 && l.shear_n < 1e-9),
                "an absorbed stop is the axial load alone: {loads:?}"
            );
            let adapter = loads.iter().find(|l| l.node == PartNodeId(2)).unwrap();
            assert!((adapter.axial_n - 19_200.0).abs() < 1e-6);
        }
    }

    /// A 2-part stack along +z rooted at its TOP — the Stock Hopper's
    /// rooting — so the part that meets the ground, the 700 kg cube
    /// under the 300 kg root, is not the root: the one joint is the
    /// lower cube's, and the whole root's inertia is what it carries.
    fn top_rooted() -> (PartGraph, Vec<WeldPart>) {
        let (root, lower) = (PartNodeId(1), PartNodeId(2));
        let mut g = PartGraph::new(root);
        g.insert(root, PartNode::new(None, PartTransform::IDENTITY, 0));
        g.insert(
            lower,
            PartNode::new(
                Some(root),
                PartTransform::new(DVec3::new(0.0, 0.0, -1.0), DQuat::IDENTITY),
                1,
            ),
        );
        (g, vec![part(300.0, 0.5), part(700.0, 0.5)])
    }

    /// M4c: the absorbed stop acts through the body's CoM, as the record
    /// says it does. A lateral stop landed on a NON-root part — the
    /// Stock Hopper's landing wherever the friction across the contact
    /// is a sideways stop — loads that part's joint with the inertia of
    /// what hangs off it: a 1 m/s drift stopped over 0.5 m of crush is
    /// 1 m/s², and the lower cube's joint carries the 300 kg root a
    /// metre above it — 300 N of shear, 300 N·m, no axial load — with
    /// the solved rim impulse's spin lifted out whole, gravity or not.
    /// Read with the stop's force at the body origin instead (a moment
    /// of zero, 0.7 m from the CoM) the same joint carried the whole
    /// craft's stop about that metre: 1000 N·m, 3.3×. On the Stock
    /// Hopper, whose CoM sits above three of its four inline joints, the
    /// same misread put the battery joint at 114% of its torque rating
    /// at a 7 m/s touchdown on a 1.5° slope — 5.6× the 20% it carries
    /// through the CoM — and at 36% on the pad, against 6%.
    #[test]
    fn a_lateral_stop_on_a_non_root_part_acts_through_the_com() {
        let (graph, parts) = top_rooted();
        let body = crate::weld(&graph, &parts).unwrap();
        let transforms = crate::weld::body_transforms(&graph, &parts).unwrap();
        assert!(
            (body.com_local - DVec3::new(0.0, 0.0, -0.7)).norm() < 1e-12,
            "{:?}",
            body.com_local
        );
        let dt = crate::PHYSICS_DT;
        let (drift, crush_m) = (1.0, 0.5);
        let lower = PartNodeId(2);
        // The solver's own tick: the craft's whole drift stopped on one
        // rim vertex of the lower cube's base, the body spun up about it.
        let rim = DVec3::new(0.5, 0.0, -1.5);
        let impulse = DVec3::X * (body.mass_kg * drift);
        let dw = body
            .inertia_local
            .inverse()
            .unwrap()
            .mul_vec((rim - body.com_local).cross(impulse));
        let loads_with = |with_gravity: bool| {
            let mut ext = BTreeMap::from([(
                lower,
                vec![PartForce {
                    at: rim,
                    force: impulse / dt,
                    torque: DVec3::ZERO,
                    ground: true,
                }],
            )]);
            let mut accel = impulse / body.mass_kg / dt;
            if with_gravity {
                for (i, id) in [PartNodeId(1), lower].into_iter().enumerate() {
                    ext.entry(id).or_default().push(PartForce {
                        at: DVec3::new(0.0, 0.0, -(i as f64)),
                        force: DVec3::Z * (-9.81 * parts[i].mass_kg),
                        torque: DVec3::ZERO,
                        ground: false,
                    });
                }
                accel.z -= 9.81;
            }
            let shares = Touchdown::shares(0.0, drift, crush_m).unwrap();
            let touchdown = Touchdown::stopped_by(
                DVec3::ZERO,
                DVec3::X * drift,
                dw,
                shares,
                vec![(lower, impulse * (shares.1 / dt))],
            )
            .unwrap();
            joint_loads(
                &body,
                &graph,
                &parts,
                &ext,
                &transforms,
                TickMotion {
                    accel_body: accel,
                    alpha_body: dw / dt,
                    touchdown: Some(&touchdown),
                },
            )
            .unwrap()
        };
        for with_gravity in [false, true] {
            let loads = loads_with(with_gravity);
            assert_eq!(loads.len(), 1, "{loads:?}");
            let (shear, torque, axial) = (loads[0].shear_n, loads[0].torque_nm, loads[0].axial_n);
            assert!(
                (shear - 300.0).abs() < 1e-9,
                "gravity {with_gravity}: shear {shear}"
            );
            assert!(
                (torque - 300.0).abs() < 1e-9,
                "gravity {with_gravity}: torque {torque}"
            );
            assert!(axial < 1e-9, "gravity {with_gravity}: axial {axial}");
        }
    }

    /// M4c pull-path reef, the arithmetic: how far a pull may go before
    /// its channel's rest load is at its derated limit again.
    #[test]
    fn pull_room_stops_where_the_channel_meets_its_limit() {
        let v = |x: f64, y: f64, z: f64| DVec3::new(x, y, z);
        // Adding to the rest: the room that is left.
        assert!((pull_room(v(0.0, 0.0, 100.0), v(0.0, 0.0, 1.0), 900.0) - 800.0).abs() < 1e-9);
        // Relieving it first: room until the far side of the limit.
        assert!((pull_room(v(0.0, 0.0, 100.0), v(0.0, 0.0, -1.0), 900.0) - 1_000.0).abs() < 1e-9);
        // Across it: the quadratic.
        let across = pull_room(v(300.0, 400.0, 0.0), v(1.0, 0.0, 0.0), 700.0);
        assert!(
            (across - (libm::sqrt(330_000.0) - 300.0)).abs() < 1e-9,
            "{across}"
        );
        // At the limit there is none, and past it the pull is not the
        // one to blame — nor the one to call on.
        assert_eq!(
            pull_room(v(300.0, 400.0, 0.0), v(1.0, 0.0, 0.0), 500.0),
            0.0
        );
        assert_eq!(
            pull_room(v(300.0, 400.0, 0.0), v(-1.0, 0.0, 0.0), 400.0),
            0.0
        );
        // A channel the pull leaves alone, or one without a limit,
        // bounds nothing.
        assert_eq!(pull_room(v(0.0, 0.0, 1.0), DVec3::ZERO, 0.0), f64::INFINITY);
        assert_eq!(
            pull_room(DVec3::ZERO, v(0.0, 0.0, 1.0), f64::INFINITY),
            f64::INFINITY
        );
    }

    /// M4c pull-path reef: the budget is the joint-load model read as a
    /// bound, over every joint but the site's own. A 3-part stack along
    /// +z — engine root, adapter, pod — with the decelerator site on the
    /// pod, pulled along the stack so nothing turns: the adapter's joint
    /// transmits the pull outright, less its own subtree's share of the
    /// acceleration the pull gives the body (300 of 800 kg), 0.625 N per
    /// newton, against a 1 kN axial limit (900 derated); the pod's own
    /// mount, the site's, is given next to no limit and must not
    /// matter — the canopy's reef owns it.
    #[test]
    fn pull_budget_leaves_every_joint_but_the_mount_its_room() {
        let (graph, mut parts) = stack();
        parts[1].structural = csp_data::StructuralDef {
            max_axial_n: 1_000.0,
            max_shear_n: 1.0e30,
            max_torque_nm: 1.0e30,
        };
        parts[2].structural = csp_data::StructuralDef {
            max_axial_n: 1.0,
            max_shear_n: 1.0e30,
            max_torque_nm: 1.0e30,
        };
        let body = crate::weld(&graph, &parts).unwrap();
        let transforms = crate::weld::body_transforms(&graph, &parts).unwrap();
        let (root, pod) = (PartNodeId(1), PartNodeId(3));
        let site_pos = transforms[&pod].apply(parts[2].com);
        let model = JointModel::new(&body, &graph, &parts, &transforms);
        let push = |node: PartNodeId, force: DVec3| {
            BTreeMap::from([(
                node,
                vec![PartForce {
                    at: DVec3::ZERO,
                    force,
                    torque: DVec3::ZERO,
                    ground: false,
                }],
            )])
        };
        let budget = |loads: &BTreeMap<PartNodeId, Vec<PartForce>>,
                      response: &BTreeMap<PartNodeId, Vec<PartForce>>,
                      drag: DVec3| {
            model
                .pull_budgets(
                    loads,
                    response,
                    &[PullSite {
                        node: pod,
                        pos: site_pos,
                        drag,
                    }],
                    0.9,
                )
                .unwrap()[0]
        };
        let none = BTreeMap::new();
        // No other load: the derated limit over the share.
        assert!((budget(&none, &none, -DVec3::Z) - 1_440.0).abs() < 1e-9);

        // An 800 N thrust on the pod — inside the adapter's subtree —
        // reads on the joint as −0.625·T: a pull pushing with it has
        // 400 N of room left, 0.625 N per newton.
        let thrust = push(pod, DVec3::Z * 800.0);
        assert!((budget(&thrust, &thrust, DVec3::Z) - 640.0).abs() < 1e-9);
        // Read as nothing but the motion it gives the body, the same
        // thrust loads the joint with the subtree's inertia alone
        // (+300 N) and the pull relieves it: room to the far side.
        assert!((budget(&none, &thrust, DVec3::Z) - 1_920.0).abs() < 1e-9);

        // A thrust on the root — outside the subtree — is motion only:
        // +300 N, whichever map it is read through.
        let thrust = push(root, DVec3::Z * 800.0);
        assert!((budget(&thrust, &thrust, -DVec3::Z) - 960.0).abs() < 1e-9);
        assert!((budget(&thrust, &thrust, DVec3::Z) - 1_920.0).abs() < 1e-9);
        // The model itself, at the budget: the adapter's channel sits
        // exactly on its derate, and the site's own mount is far past
        // its 1 N — not the pull path's to bound.
        let mut all = thrust.clone();
        all.insert(
            pod,
            vec![PartForce {
                at: site_pos,
                force: DVec3::Z * -960.0,
                torque: DVec3::ZERO,
                ground: false,
            }],
        );
        let loads = joint_loads(
            &body,
            &graph,
            &parts,
            &all,
            &transforms,
            TickMotion {
                accel_body: DVec3::Z * ((800.0 - 960.0) / 800.0),
                alpha_body: DVec3::ZERO,
                touchdown: None,
            },
        )
        .unwrap();
        let adapter = loads.iter().find(|l| l.node == PartNodeId(2)).unwrap();
        assert!((adapter.axial_n - 900.0).abs() < 1e-9);
        assert!(loads.iter().find(|l| l.node == pod).unwrap().axial_n > 1.0);

        // The rest past the limit: nothing to add.
        let thrust = push(root, DVec3::Z * 4_000.0);
        assert_eq!(budget(&thrust, &thrust, -DVec3::Z), 0.0);

        // A site outside the graph is a contract violation.
        assert!(
            model
                .pull_budgets(
                    &none,
                    &none,
                    &[PullSite {
                        node: PartNodeId(9),
                        pos: site_pos,
                        drag: -DVec3::Z,
                    }],
                    0.9,
                )
                .is_err()
        );
    }

    /// M4c pull-path reef with several chutes: one site's budget is what
    /// it may pull ALONE, and sites pulling together share one scale
    /// over their own budgets. On the `stack()` craft, sites on the pod:
    ///
    /// - two pulls along the stack load the adapter's axial channel with
    ///   the sum of both — 1.25 N per newton against 0.625 for one — so
    ///   each gets half its 1 440 N, and the model at both budgets sits
    ///   the adapter exactly on its 900 N derate;
    /// - two pulls that turn the craft about different axes load the
    ///   adapter's torque channel with two orthogonal vectors: each
    ///   alone is 90 N·m of torque, together `√2` times that, so each
    ///   gets its own budget over `√2` and the model sits the channel
    ///   on its 90 N·m derate — where each at its own budget read 127%;
    /// - two pulls whose turns cancel leave the room they would have
    ///   used — and keep their own budgets all the same: beside another
    ///   chute a site never pulls more than alone.
    #[test]
    fn several_sites_share_one_scale_over_their_own_budgets() {
        let (graph, mut parts) = stack();
        parts[1].structural = csp_data::StructuralDef {
            max_axial_n: 1_000.0,
            max_shear_n: 1.0e30,
            max_torque_nm: 100.0,
        };
        let body = crate::weld(&graph, &parts).unwrap();
        let transforms = crate::weld::body_transforms(&graph, &parts).unwrap();
        let pod = PartNodeId(3);
        let top = transforms[&pod].apply(parts[2].com);
        let model = JointModel::new(&body, &graph, &parts, &transforms);
        let none: BTreeMap<PartNodeId, Vec<PartForce>> = BTreeMap::new();
        let site = |pos: DVec3, drag: DVec3| PullSite {
            node: pod,
            pos,
            drag,
        };
        let budgets = |sites: &[PullSite]| model.pull_budgets(&none, &none, sites, 0.9).unwrap();
        let alone = |site: PullSite| budgets(&[site])[0];
        // The model under the pulls themselves, each at `n` newtons.
        let loads_at = |pulls: &[(PullSite, f64)]| {
            let mut ext: BTreeMap<PartNodeId, Vec<PartForce>> = BTreeMap::new();
            let (mut force, mut torque) = (DVec3::ZERO, DVec3::ZERO);
            for (PullSite { node, pos, drag }, n) in pulls {
                ext.entry(*node).or_default().push(PartForce {
                    at: *pos,
                    force: *drag * *n,
                    torque: DVec3::ZERO,
                    ground: false,
                });
                force = force + *drag * *n;
                torque = torque + (*pos - body.com_local).cross(*drag * *n);
            }
            let adapter = joint_loads(
                &body,
                &graph,
                &parts,
                &ext,
                &transforms,
                TickMotion {
                    accel_body: force / body.mass_kg,
                    alpha_body: body.inertia_local.inverse().unwrap().mul_vec(torque),
                    touchdown: None,
                },
            )
            .unwrap()
            .into_iter()
            .find(|l| l.node == PartNodeId(2))
            .unwrap();
            (adapter.axial_n, adapter.torque_nm)
        };

        // Along the stack: the shares add.
        let along = site(top, -DVec3::Z);
        assert!((alone(along) - 1_440.0).abs() < 1e-9);
        let both = budgets(&[along, along]);
        assert_eq!(both.len(), 2);
        for b in &both {
            assert!((b - 720.0).abs() < 1e-9, "{both:?}");
        }
        assert!((loads_at(&[(along, both[0]), (along, both[1])]).0 - 900.0).abs() < 1e-6);

        // Across it, two ways: the torque vectors add at right angles.
        let (east, north) = (
            site(top + DVec3::X * 0.5, DVec3::X),
            site(top + DVec3::Y * 0.5, DVec3::Y),
        );
        let (alone_east, alone_north) = (alone(east), alone(north));
        assert!(
            (alone_east - alone_north).abs() < 1e-9,
            "{alone_east} / {alone_north}"
        );
        assert!((loads_at(&[(east, alone_east)]).1 - 90.0).abs() < 1e-9);
        // Each at its own budget: the two turns read √2 × the derate.
        assert!(
            (loads_at(&[(east, alone_east), (north, alone_north)]).1 - 90.0 * libm::sqrt(2.0))
                .abs()
                < 1e-6
        );
        let both = budgets(&[east, north]);
        let over_sqrt2 = 1.0 / libm::sqrt(2.0);
        for (b, a) in both.iter().zip([alone_east, alone_north]) {
            assert!((b / a - over_sqrt2).abs() < 1e-9, "{b} of {a}");
        }
        assert!((loads_at(&[(east, both[0]), (north, both[1])]).1 - 90.0).abs() < 1e-6);

        // Turns that cancel: the room is there, and stays unused.
        let (port, starboard) = (
            site(top - DVec3::X * 0.5, -DVec3::Z),
            site(top + DVec3::X * 0.5, -DVec3::Z),
        );
        let both = budgets(&[port, starboard]);
        for (b, s) in both.iter().zip([port, starboard]) {
            assert!(
                (b - alone(s)).abs() < 1e-9 * alone(s),
                "{b} against {}",
                alone(s)
            );
        }
        assert!(loads_at(&[(port, both[0]), (starboard, both[1])]).1 < 1e-9);
    }
}
