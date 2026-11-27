//! `weld` — the §2.4 pure function turning a part graph into a
//! [`WeldedBody`]. **Stage 1 only**: mass properties, force sites, and
//! the merged convex-hull compound collider. Stage 2 (aero tables,
//! §2.4.1) is a separate, deferred computation that installs by
//! command — it is not built here.
//!
//! Contract (§2.4):
//! - **Pure**: no threads, no hash maps, no I/O. Every output is a pure
//!   function of `(graph, effective_parts)` and is bit-identical for a
//!   given input pair — the tests below assert invariance under
//!   part-ordering permutation. Bit-identity is **per id-labelling**:
//!   f64 summation order follows ascending `PartNodeId`, so relabelling
//!   the same physical tree reproduces the output only to ordinary
//!   rounding, not bitwise.
//! - **Synchronous and exact**: runs on the sim thread. Mass, CoM, and
//!   the inertia tensor are computed in one fixed summation order
//!   (ascending [`PartNodeId`]) over all parts.
//! - **No time trigger**: the output is a derived cache of
//!   currently-serialized state; callers re-weld when the quantized
//!   resource mass changes a 1/1024 quantum, never "every N ticks".

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use csp_core::{DQuat, DVec3, SimError};

use crate::graph::{PartGraph, PartNodeId, PartTransform};
use crate::part::{ForceSite, WeldPart};
use crate::simple::PHYSICS_DT;
use crate::tensor::SymMat3;
use crate::thermal::{ThermalEdge, ThermalNet, ThermalNode, joint_conductance};

/// One hull of the merged compound collider — the convex vertex set of
/// one part, transformed into body frame. Stored in a canonical order
/// (sorted by `(x, y, z)`, duplicates removed) so the representation
/// is invariant under input ordering; the contact solver derives
/// facet planes from these point sets via `hull::facet_planes`
/// (§2.9) — no parry materialization.
///
/// The set is canonicalized but **not** hull-reduced: interior points
/// a resolver supplied may survive — consumers must not assume
/// minimality (the facet detector tolerates them: an interior vertex
/// simply never forms a facet).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConvexHull {
    pub vertices: Vec<DVec3>,
}

/// The merged convex-hull compound collider of §2.4 Stage 1: one
/// [`ConvexHull`] per welded part, indexed by
/// [`PartWeld::hull_index`]. Kept as plain vertex sets — a serde-able,
/// hashable mirror the physics backend's facet machinery reads
/// directly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompoundCollider {
    pub hulls: Vec<ConvexHull>,
}

/// The per-part half of the "part index map": where one graph node's
/// weld outputs live inside the [`WeldedBody`] vectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartWeld {
    /// Index into [`WeldedBody::force_sites`] where this part's sites
    /// begin; `site_len` consecutive entries.
    pub site_start: u32,
    pub site_len: u32,
    /// Index into [`CompoundCollider::hulls`].
    pub hull_index: u32,
}

/// §2.6.4 surface-rails bookkeeping, serialized with the body. The
/// stepping backend owns the counter: it increments while the body is
/// in contact with `‖v‖ < settle_v` and `‖ω‖ < settle_w`, resets it on
/// any tick that fails either condition, and sets `latched` at
/// `settle_ticks` (default 128 = 2 s). The latch is the *decision*; the
/// World-side consequences of §2.6.4 (leaving the bubble, storing the
/// pose body-fixed as `(lat, lon, alt, orientation)`) are M2-C wiring —
/// this flag is what they key on.
///
/// The counter lives on the body — not inside the backend — because it
/// is sim state: a save taken mid-settle must resume the countdown
/// bit-identically, and backend scratch is re-derived, never
/// serialized (§2.4's "every derived cache is a pure function of
/// currently-serialized state").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettleState {
    /// Consecutive ticks this body has satisfied the settle conditions
    /// while in contact.
    pub settled_ticks: u32,
    /// §2.6.4 latched: the body is at rest on the surface and the
    /// backend does not integrate it.
    pub latched: bool,
}

/// A vessel as one rigid body — §2.4's "one rigid body per vessel; all
/// per-part forces are analytic and summed into (force, torque)".
///
/// Two halves:
/// - **Kinematic state** (`pos`, `rot`, `vel`, `ang_vel`) — owned by
///   the sim, mutated by the `PhysicsBackend` (§2.9), serialized with
///   bubble-vessel state (§7.2). `weld` initializes it to origin /
///   identity / zero; spawn, merge, and split paths then stamp the
///   initial pose and momentum (docking hand-off takes the island's
///   aggregate momentum, §2.9).
/// - **Weld-derived mass properties** (`mass_kg`, `com_local`,
///   `inertia_local`, `force_sites`, `collider`, `part_index`) — the
///   Stage-1 output, an exact derived cache re-created by re-welding
///   on load (§7.2/§7.3 re-derivation).
///
/// Frame conventions: `pos`/`vel` are in the bubble frame (§2.3);
/// `rot` carries body-frame vectors into bubble frame;
/// `ang_vel` is expressed in **body frame** (the frame
/// `inertia_local` is constant in, so `L_body = I·ω` needs no
/// rotation); `com_local`, `inertia_local`, `force_sites`, and the
/// collider are body-frame.
///
/// `throttle` and `settle` are the vessel-control and surface-rails
/// state the §2.9 backend reads and mutates: `throttle` is the
/// vessel-level throttle fraction the `ForceSiteKind::Thrust` force
/// model multiplies `rated_n` by (§2.6.2 forces it to zero on bubble
/// exit; a latched vessel's throttle is zero by the same rule, and a
/// nonzero throttle unlatches — §2.6.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeldedBody {
    /// Position of the body origin, bubble frame, metres.
    pub pos: DVec3,
    /// Orientation: body frame → bubble frame.
    pub rot: DQuat,
    /// Linear velocity, bubble frame, m/s.
    pub vel: DVec3,
    /// Angular velocity, body frame, rad/s.
    pub ang_vel: DVec3,
    /// Vessel throttle fraction `[0, 1]` — sim-owned control state
    /// (written by `SimCommand`s at M2-C; the backend only reads it).
    pub throttle: f64,
    /// §2.6.4 surface-rails bookkeeping — see [`SettleState`].
    pub settle: SettleState,
    /// Total mass, kg — `Σ mᵢ`.
    pub mass_kg: f64,
    /// Centre of mass, body frame.
    pub com_local: DVec3,
    /// Inertia tensor about `com_local`, body frame, kg·m².
    pub inertia_local: SymMat3,
    /// All parts' force sites transformed to body frame, ordered
    /// ascending [`PartNodeId`] then per-part resolver order.
    pub force_sites: Vec<ForceSite>,
    /// Merged convex-hull compound collider, body frame.
    pub collider: CompoundCollider,
    /// The part index map: `PartNodeId` → weld output locations.
    /// `BTreeMap` order is the canonical part order (§2.2).
    pub part_index: BTreeMap<PartNodeId, PartWeld>,
    /// §2.4.1 Stage-2 aero tables for the vessel's *active*
    /// [`crate::aero::ConfigMask`], attached by the world — never
    /// produced by `weld` (Stage 2 is deferred and installs by
    /// command). **Derived cache**: skipped in serialization and in
    /// the state-hash walk (a pure function of `(craft, mask)` —
    /// §2.4.1's "aero tables are never serialized in the save").
    /// `Arc`-shared: the per-tick rollback shadow and the batch
    /// bookkeeping clone `WeldedBody` wholesale, and a thousand-part
    /// table is megabytes — a clone here must be a refcount bump, not
    /// a deep copy (in-place mutation goes through `Arc::make_mut`,
    /// the `rebody` restamp precedent).
    #[serde(skip)]
    pub aero: Option<std::sync::Arc<crate::aero::CraftAeroTables>>,
    /// §3.1 `Parachute` decelerator sites — the additive analytic-drag
    /// terms the §2.9 force model sums after the aero-table
    /// contribution. **Sim-owned state**: `weld` emits an empty list
    /// (deploy state is not weld input); the world refreshes the
    /// sites per tick from the craft's `ParachuteSpec`s and the
    /// vessel's deployed record — a pure function, so skipped in
    /// serialization and the hash walk exactly like `aero`.
    #[serde(skip)]
    pub decel: Vec<DecelSite>,
    /// The M4b conduction/radiation network — per-part lumps plus the
    /// attachment-edge conductances ([`crate::thermal`]). Weld output
    /// like `inertia_local`/`collider`: a pure function of
    /// `(graph, effective_parts)`, serialized with the body and
    /// rebuilt by the re-weld on restore. The live temperatures it is
    /// indexed against are NOT here — they are sim state on the
    /// vessel record (`csp_sim::Vessel::thermal`), hashed per §7.1.
    /// Boxed: the net is cold data (touched once per tick) and
    /// `WeldedBody` lives inside `VesselState`'s enum layout.
    pub thermal: Box<ThermalNet>,
}

impl Default for WeldedBody {
    /// The empty placeholder — **never a sim value**. `csp-sim`'s tick
    /// batch moves each `InBubble` body into the solver's slice with
    /// `mem::take` and swaps the stepped body back after, so the
    /// placeholder occupies the vessel record only for the duration of
    /// `backend.step` (nothing reads an `InBubble` body in that
    /// window), replacing what used to be a full deep clone of the
    /// body — collider, part index, aero tables and all — per vessel
    /// per tick.
    fn default() -> Self {
        Self {
            pos: DVec3::ZERO,
            rot: DQuat::IDENTITY,
            vel: DVec3::ZERO,
            ang_vel: DVec3::ZERO,
            throttle: 0.0,
            settle: SettleState::default(),
            mass_kg: 0.0,
            com_local: DVec3::ZERO,
            inertia_local: SymMat3::ZERO,
            force_sites: Vec::new(),
            collider: CompoundCollider { hulls: Vec::new() },
            part_index: BTreeMap::new(),
            aero: None,
            decel: Vec::new(),
            thermal: Box::default(),
        }
    }
}

/// One decelerator drag site on a [`WeldedBody`] — a §3.1 `Parachute`
/// part's additive analytic-drag channel (`F = −½·ρ·CdA·|v_rel|·v_rel`),
/// in two terms that do not act on the craft alike. The **packed**
/// canister's drag acts at the site point (`local_pos`), where the
/// canister is. The **canopy's** pull acts on the craft's axis, at
/// `axis_pos` (§3.1 rule A) — for the craft's rigid-body motion and
/// for every joint but the chute's own mount, whose reef is what bounds
/// it. The world rebuilds the body's site list each tick;
/// `deployed_at_s` is the sim-time stamp the deploy apply recorded —
/// `None` while packed; `open_at_s` is the first stamp at which BOTH
/// catch gates (`deploy_alt_m`, `deploy_speed_mps`) held under flow —
/// the `deploy_time_s` inflation ramp's anchor, so a deploy-early
/// command cannot pre-complete inflation before the flow can hold the
/// canopy (C14-F1). The reef bounds the canopy at what its own mount
/// takes: `reef_n` on the force. Each term is then capped, on its own,
/// at what every other joint of the craft takes of it where it acts
/// (`packed_budget_n`, `canopy_budget_n`). Gates and reef are evaluated
/// per tick by the backend's force accumulation against the body's own
/// altitude, speed, and density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecelSite {
    /// Owning graph node — bookkeeping for the per-part force
    /// attribution in `external_loads`.
    pub node: PartNodeId,
    /// The canister, body frame — the node's resolved CoM. Its own
    /// packed drag acts here.
    pub local_pos: DVec3,
    /// Where the craft takes the canopy's pull, body frame: the site
    /// projected onto the craft's longitudinal axis (§3.1 rule A) —
    /// the mount point does not lever the stack.
    pub axis_pos: DVec3,
    /// The craft's longitudinal axis, unit, body frame — the direction
    /// a deployed canopy steers onto the flow ([`CanopySteering`]).
    pub axis_dir: DVec3,
    /// Packed drag area Cd·A (m²) — applies whenever the site is in
    /// the flow, deployed or not.
    pub packed_drag_area_m2: f64,
    /// Deployed drag area Cd·A (m²) — the open canopy's catch.
    pub deployed_drag_area_m2: f64,
    /// Altitude ceiling (m) for the canopy catch.
    pub deploy_alt_m: f64,
    /// Air-relative speed ceiling (m/s) for the catch.
    pub deploy_speed_mps: f64,
    /// Inflation ramp duration (s); `0` deploys instantly.
    pub deploy_time_s: f64,
    /// Deploy stamp in sim seconds — `None` = packed.
    pub deployed_at_s: Option<f64>,
    /// First sim second BOTH catch gates held under this deploy —
    /// `None` until the flow can hold the canopy. The ramp opens from
    /// here, not from `deployed_at_s`.
    pub open_at_s: Option<f64>,
    /// Canopy force bound (N) — the weaker of the mount's authored
    /// axial and shear ratings, derated: the ceiling on the reefed
    /// `½·ρ·v²·A_canopy`. `f64::INFINITY`-class values (an unlimited
    /// mount) leave the canopy unreefed.
    pub reef_n: f64,
    /// The packed term's budget: the greatest packed drag (N) the site
    /// may apply this tick without loading any joint of the craft but
    /// its own mount past the working-load derate —
    /// [`crate::structural::JointModel::pull_budgets`], which the world
    /// computes before each step from the tick's other loads, every
    /// term of the body at once (each takes its own budget times the
    /// one scale that keeps the joints inside the derate with all of
    /// them pulling). The canister's own drag acts where it is, so off
    /// the craft's axis it is a pitch moment on everything below it —
    /// at 250 m/s it overloads the Stock Hopper's pod-to-canister joint
    /// before the canopy ever opens. `f64::INFINITY` when nothing
    /// beside the mount feels it — a pod-alone chute.
    pub packed_budget_n: f64,
    /// The canopy term's budget, the same way: its pull acts on the
    /// craft's axis, so what it levers is the hang — a canopy catching
    /// on a craft leaning 18° turns the whole stack about the CoM, and
    /// the joints below carry the turn. Aligned, a canopy is far past
    /// anything the mount's reef lets through and the budget never
    /// binds. `f64::INFINITY` when nothing beside the mount feels it.
    pub canopy_budget_n: f64,
}

impl WeldedBody {
    /// Centre of mass in bubble frame — `pos + rot·com_local`. The
    /// point the §2.9 force model applies CoM forces at and contact
    /// lever arms are measured from.
    pub fn com_world(&self) -> DVec3 {
        self.pos + self.rot.rotate(self.com_local)
    }

    /// Linear momentum `p = m·v` — the §2.4 conservation invariant the
    /// split/merge paths distribute and check (`‖Δp‖`-relative 1e-9).
    pub fn momentum(&self) -> DVec3 {
        self.vel * self.mass_kg
    }

    /// Angular momentum in body frame, `L = I·ω`. The bubble-frame
    /// momentum is `self.rot.rotate(self.angular_momentum_local())`.
    pub fn angular_momentum_local(&self) -> DVec3 {
        self.inertia_local.mul_vec(self.ang_vel)
    }
}

/// Why a part graph cannot weld — the typed boundary between bad craft
/// data and the pure function (§2.13: typed errors, never panics).
/// Variants carry the offending node id for diagnostics; they map onto
/// `SimError::SaveBlocking` — a craft that cannot weld is a data
/// validation failure: unlaunchable, and the save that produced it is
/// not persistable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeldError {
    /// `graph.nodes` is empty.
    EmptyGraph,
    /// `graph.root` is not a node of the graph.
    MissingRoot(PartNodeId),
    /// The root node names a parent — the root must be parentless.
    RootHasParent(PartNodeId),
    /// A non-root node's `parent` is not in the graph.
    MissingParent {
        node: PartNodeId,
        parent: PartNodeId,
    },
    /// A node is unreachable from the root — covers both disconnected
    /// subtrees and parent cycles (a cycle never reaches the root).
    Unreachable(PartNodeId),
    /// `effective_index` is out of bounds for the `effective` slice.
    EffectiveIndexOutOfRange {
        node: PartNodeId,
        index: u32,
        len: usize,
    },
    /// A finite-ness/validity check failed on named input data
    /// (transform, mass sign, CoM, inertia PSD-ness, collider, site).
    InvalidPart {
        node: PartNodeId,
        field: &'static str,
    },
    /// A node's `rotation` is finite but not a unit quaternion within
    /// [`PartTransform::UNIT_ROTATION_TOLERANCE`] — e.g. `(0,0,0,2)`,
    /// which would silently scale every rotated quantity (CoM, site
    /// directions, hull vertices) if composed raw. Rejected, never
    /// normalized-and-accepted: a stored non-unit quat is bad data.
    NonUnitRotation(PartNodeId),
    /// A weld-output index exceeded `u32::MAX` — `PartWeld` fields are
    /// `u32`, so >4G force sites or hulls cannot be indexed. The
    /// carried string names the overflowing counter.
    IndexOverflow(&'static str),
    /// A lookup an earlier pass already proved (presence, bounds)
    /// missed anyway — an internal bug, reported typed per §2.13
    /// rather than silently skipped or panicked on.
    InternalInvariant(&'static str),
    /// `Σ mᵢ ≤ 0` — CoM is undefined.
    ZeroTotalMass,
    /// Defensive: a computed mass property came out non-finite despite
    /// validated inputs.
    NonFiniteResult(&'static str),
    /// The attachment edge's conductance and endpoint heat capacities
    /// violate the thermal tick's explicit-Euler stability bound
    /// `G·dt·(1/C_a + 1/C_b) < 2` at the pinned [`PHYSICS_DT`] — the
    /// two-lump conduction map's multiplier `1 − G·dt·(1/C_a + 1/C_b)`
    /// would leave `[−1, 1]`, so the net cannot be emitted: the
    /// per-field gates on `conduction`/`heat_capacity` can't see the
    /// coupling, which is why the check lives at edge formation.
    /// Carries the edge's canonical `(a < b)` endpoints.
    UnstableThermalCoupling { a: PartNodeId, b: PartNodeId },
}

impl WeldError {
    /// Static reason text for the `SimError` mapping — logs and bug
    /// bundles only, never hashed (§2.13).
    pub fn reason(self) -> &'static str {
        match self {
            Self::EmptyGraph => "weld: empty part graph",
            Self::MissingRoot(_) => "weld: graph root is not a node",
            Self::RootHasParent(_) => "weld: root node has a parent",
            Self::MissingParent { .. } => "weld: node parent missing from graph",
            Self::Unreachable(_) => "weld: node unreachable from root (cycle or orphan)",
            Self::EffectiveIndexOutOfRange { .. } => "weld: effective part index out of range",
            Self::InvalidPart { field, .. } => field,
            Self::NonUnitRotation(_) => "weld: node rotation is not a unit quaternion",
            Self::IndexOverflow(f) | Self::InternalInvariant(f) | Self::NonFiniteResult(f) => f,
            Self::ZeroTotalMass => "weld: total mass is zero",
            Self::UnstableThermalCoupling { .. } => {
                "weld: thermal edge coupling exceeds the explicit-Euler stability bound"
            }
        }
    }
}

impl fmt::Display for WeldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason())
    }
}

impl std::error::Error for WeldError {}

impl From<WeldError> for SimError {
    fn from(e: WeldError) -> Self {
        SimError::SaveBlocking(e.reason())
    }
}

/// Per-node weld inputs resolved to graph (= body) frame: the node's
/// transform, its part's body-frame CoM, and the part itself — the
/// effective-part lookup happens exactly once, in
/// [`solve_transforms`], so [`assemble`] never re-looks-up anything it
/// could silently drop.
struct SolvedNode<'a> {
    /// The node's parent — `None` on the root. Carried so
    /// `assemble` can form the conduction edges without re-reading
    /// the graph.
    parent: Option<PartNodeId>,
    transform: PartTransform,
    com_body: DVec3,
    part: &'a WeldPart,
}

/// `csp_phys::weld(part_graph, &[WeldPart]) -> WeldedBody` — §2.4
/// Stage 1, on the sim thread, always.
///
/// Errors instead of panicking on every malformed input: empty or
/// structurally broken graphs (missing root/parents, cycles, orphans),
/// out-of-range effective indices, non-finite or non-physical part
/// data, non-unit rotations, and zero total mass.
///
/// Determinism: all iteration is ascending `PartNodeId` (the
/// `BTreeMap` arenas' key order); summation is plain `f64` in that
/// fixed order — no `HashMap`, no parallelism, no reassociation. The
/// id labelling is part of the input: a relabelled graph produces the
/// same physical result to rounding, but bit-identity holds only for
/// identical labellings.
pub fn weld(graph: &PartGraph, effective: &[WeldPart]) -> Result<WeldedBody, WeldError> {
    validate_structure(graph, effective)?;
    let solved = solve_transforms(graph, effective)?;
    assemble(&solved)
}

/// Every node's composed body-frame transform — the map the §2.4
/// breakup machinery needs to extract subtrees with their body-frame
/// pose preserved ([`crate::structural::fragment_graph`]). Same
/// traversal and compose order as the weld itself; validates the
/// graph first so a malformed craft errors identically.
pub fn body_transforms(
    graph: &PartGraph,
    effective: &[WeldPart],
) -> Result<BTreeMap<PartNodeId, PartTransform>, WeldError> {
    validate_structure(graph, effective)?;
    Ok(solve_transforms(graph, effective)?
        .into_iter()
        .map(|(id, s)| (id, s.transform))
        .collect())
}

/// Structural pass: root exists and is parentless, non-root parents
/// exist, effective indices are in bounds, node transforms are finite
/// with unit rotations, and per-part data is finite and physical. All
/// checks ascend `PartNodeId`, so the *first reported* error is also
/// deterministic.
fn validate_structure(graph: &PartGraph, effective: &[WeldPart]) -> Result<(), WeldError> {
    if graph.nodes.is_empty() {
        return Err(WeldError::EmptyGraph);
    }
    let root = graph.root;
    if !graph.nodes.contains_key(&root) {
        return Err(WeldError::MissingRoot(root));
    }
    for (id, node) in &graph.nodes {
        if *id == root {
            if node.parent.is_some() {
                return Err(WeldError::RootHasParent(root));
            }
        } else {
            match node.parent {
                None => return Err(WeldError::Unreachable(*id)),
                Some(p) if !graph.nodes.contains_key(&p) => {
                    return Err(WeldError::MissingParent {
                        node: *id,
                        parent: p,
                    });
                }
                Some(_) => {}
            }
        }
        let eff = node.effective_index as usize;
        if eff >= effective.len() {
            return Err(WeldError::EffectiveIndexOutOfRange {
                node: *id,
                index: node.effective_index,
                len: effective.len(),
            });
        }
        if !node.transform.offset.is_finite() || !node.transform.rotation.is_finite() {
            return Err(WeldError::InvalidPart {
                node: *id,
                field: "weld: invalid node transform",
            });
        }
        if !node.transform.has_unit_rotation() {
            return Err(WeldError::NonUnitRotation(*id));
        }
        // In-bounds is proven immediately above; a miss can only be a
        // bug in this pass — typed internal error, never a silent skip
        // of `validate_effective`.
        let Some(ep) = effective.get(eff) else {
            return Err(WeldError::InternalInvariant(
                "weld: effective index proven in bounds missed the slice",
            ));
        };
        validate_effective(*id, ep)?;
    }
    Ok(())
}

/// Per-part physical validity: finite non-negative mass, finite CoM,
/// PSD inertia, finite collider within the vertex cap, finite unit
/// site directions.
fn validate_effective(node: PartNodeId, ep: &WeldPart) -> Result<(), WeldError> {
    let bad = |field: &'static str| WeldError::InvalidPart { node, field };
    if !ep.mass_kg.is_finite() || ep.mass_kg < 0.0 {
        return Err(bad("weld: part mass is negative or non-finite"));
    }
    if !ep.com.is_finite() {
        return Err(bad("weld: part centre of mass is non-finite"));
    }
    if !ep.inertia.is_positive_semidefinite() {
        return Err(bad("weld: part inertia is not positive-semidefinite"));
    }
    if !ep.collider.is_valid() {
        return Err(bad(
            "weld: collider has non-finite vertices or exceeds MAX_COLLIDER_VERTICES",
        ));
    }
    for site in &ep.force_sites {
        // `local_dir` is a unit direction (its doc contract): the
        // force model multiplies `throttle·rated_n` by it verbatim,
        // so a non-unit direction silently scales the force — the
        // `NonUnitRotation` gate above is the precedent. The same
        // `q·q − 1` tolerance applies (`‖d‖²` within 1e-9 of 1).
        if !site.local_pos.is_finite()
            || !site.local_dir.is_finite()
            || (site.local_dir.norm_squared() - 1.0).abs() > PartTransform::UNIT_ROTATION_TOLERANCE
        {
            return Err(bad("weld: force site direction is non-finite or non-unit"));
        }
        if !site.rated_n.is_finite() || site.rated_n < 0.0 {
            return Err(bad(
                "weld: force site rated force is negative or non-finite",
            ));
        }
    }
    // The resolved engine spec — `None` is the unmetered hand-authored
    // convention (`EngineSpec`'s doc); a `Some` must carry a positive
    // finite isp and a non-empty propellant list of unique, positive,
    // finite fractions summing to 1 — the resolver's
    // `check_engine_propellants` bounds, re-checked because a
    // `WeldPart` can arrive unvalidated. A bad spec is bad input, not
    // a free-burning engine.
    if let Some(eng) = &ep.engine {
        if !eng.isp_vac_s.is_finite() || eng.isp_vac_s <= 0.0 {
            return Err(bad("weld: engine isp is not positive and finite"));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut sum = 0.0;
        for (r, f) in &eng.propellants {
            if !seen.insert(r.as_str()) || !f.is_finite() || *f <= 0.0 {
                return Err(bad("weld: engine propellant list malformed"));
            }
            sum += f;
        }
        if eng.propellants.is_empty() || (sum - 1.0).abs() > 1e-6 {
            return Err(bad("weld: engine propellant list malformed"));
        }
    }
    // §3.1 thermal lump — the M4b network needs finite, in-range
    // values (the same bounds `csp-data`'s validator enforces,
    // re-checked here because a `WeldPart` can arrive unvalidated).
    let th = &ep.thermal;
    if !th.heat_capacity_j_per_k.is_finite() || th.heat_capacity_j_per_k <= 0.0 {
        return Err(bad("weld: part heat capacity is not positive-finite"));
    }
    if !th.max_temp_k.is_finite() || th.max_temp_k <= 0.0 {
        return Err(bad("weld: part max_temp_k is not positive-finite"));
    }
    if !th.emissivity.is_finite() || th.emissivity <= 0.0 || th.emissivity > 1.0 {
        return Err(bad("weld: part emissivity is outside (0, 1]"));
    }
    if !th.conduction_w_per_k.is_finite() || th.conduction_w_per_k < 0.0 {
        return Err(bad("weld: part conduction is negative or non-finite"));
    }
    // §3.1 structural limits — the breakup joint thresholds; finite
    // and non-negative like every other authored bound.
    let s = &ep.structural;
    if !s.max_axial_n.is_finite() || s.max_axial_n < 0.0 {
        return Err(bad("weld: part max_axial_n is negative or non-finite"));
    }
    if !s.max_shear_n.is_finite() || s.max_shear_n < 0.0 {
        return Err(bad("weld: part max_shear_n is negative or non-finite"));
    }
    if !s.max_torque_nm.is_finite() || s.max_torque_nm < 0.0 {
        return Err(bad("weld: part max_torque_nm is negative or non-finite"));
    }
    Ok(())
}

/// DFS from the root (children explored in ascending-id order),
/// composing each node's graph-frame transform, then pass 1 of the
/// mass accumulation: `Σ mᵢ` and each part's body-frame CoM. A node
/// the DFS cannot visit is `Unreachable` — disconnected subtrees and
/// parent cycles alike.
fn solve_transforms<'a>(
    graph: &PartGraph,
    effective: &'a [WeldPart],
) -> Result<BTreeMap<PartNodeId, SolvedNode<'a>>, WeldError> {
    let mut children: BTreeMap<PartNodeId, Vec<PartNodeId>> = BTreeMap::new();
    for (id, node) in &graph.nodes {
        if let Some(p) = node.parent {
            children.entry(p).or_default().push(*id);
        }
    }
    let mut solved: BTreeMap<PartNodeId, SolvedNode> = BTreeMap::new();
    // Parent transforms must exist before children compose against
    // them, so the map carries `t` between DFS visit and insertion.
    let mut transforms: BTreeMap<PartNodeId, PartTransform> = BTreeMap::new();
    let mut stack = vec![graph.root];
    while let Some(id) = stack.pop() {
        // Only graph ids are ever pushed (the validated root and
        // children enumerated from `graph.nodes`), so a miss is a bug
        // in this pass — typed internal error, never a silent skip.
        let Some(node) = graph.nodes.get(&id) else {
            return Err(WeldError::InternalInvariant(
                "weld: DFS visited an id that is not a graph node",
            ));
        };
        let t = match node.parent {
            // The root's transform is the one place a stored quat is
            // used raw (compose renormalizes every product), so it is
            // normalized once here: a stored quat at the edge of the
            // unit tolerance cannot seed drift into descendants.
            None => node.transform.normalized(),
            Some(p) => match transforms.get(&p) {
                Some(pt) => pt.compose(node.transform),
                // A node reaches the stack only through its parent's
                // visit, which inserts the parent's transform first —
                // a miss is a bug, not `Unreachable` (the sweep below
                // owns that error).
                None => {
                    return Err(WeldError::InternalInvariant(
                        "weld: parent transform missing during DFS",
                    ));
                }
            },
        };
        transforms.insert(id, t);
        // Bounds proven by `validate_structure` for every node. This is
        // the one effective-part lookup per node: the resolved part is
        // stored in `SolvedNode` so no later pass can drop it.
        let Some(ep) = effective.get(node.effective_index as usize) else {
            return Err(WeldError::InternalInvariant(
                "weld: validated effective index missed the slice",
            ));
        };
        solved.insert(
            id,
            SolvedNode {
                parent: node.parent,
                transform: t,
                com_body: t.apply(ep.com),
                part: ep,
            },
        );
        if let Some(cs) = children.get(&id) {
            // Reversed so the stack pops children in ascending-id order.
            stack.extend(cs.iter().rev().copied());
        }
    }
    for id in graph.nodes.keys() {
        if !solved.contains_key(id) {
            return Err(WeldError::Unreachable(*id));
        }
    }
    Ok(solved)
}

/// Pass 2 — the assembly sweep in ascending `PartNodeId`: `Σ mᵢ`, the
/// body CoM, then `Σᵢ [Rᵢ·Iᵢ·Rᵢᵀ + mᵢ·(‖dᵢ‖²E − dᵢ⊗dᵢ)]` with
/// `dᵢ = cᵢ − CoM` (each part's tensor rotated to body frame and
/// shifted by parallel axis from its own CoM to the body's). Force
/// sites, collider hulls, and the part index map accumulate in the
/// same fixed order.
#[allow(clippy::too_many_lines)] // the sequential mass → sites → hulls → thermal pipeline
fn assemble(solved: &BTreeMap<PartNodeId, SolvedNode<'_>>) -> Result<WeldedBody, WeldError> {
    let mut total = 0.0;
    let mut moment = DVec3::ZERO;
    for s in solved.values() {
        total += s.part.mass_kg;
        moment = moment + s.com_body * s.part.mass_kg;
    }
    // `<= 0` plus the finiteness gate rejects zero-sum and NaN alike.
    if !total.is_finite() || total <= 0.0 {
        return Err(WeldError::ZeroTotalMass);
    }
    let com = moment / total;
    if !com.is_finite() {
        return Err(WeldError::NonFiniteResult(
            "weld: centre of mass is non-finite",
        ));
    }

    let mut inertia = SymMat3::ZERO;
    let mut force_sites = Vec::new();
    let mut hulls = Vec::new();
    let mut part_index = BTreeMap::new();
    // Per-node body-frame hull AABBs for the thermal net — collected
    // in the same pass the collider hulls are canonicalized in.
    let mut aabbs: BTreeMap<PartNodeId, (DVec3, DVec3)> = BTreeMap::new();
    let mut thermal_nodes = Vec::new();
    for (id, s) in solved {
        let ep = s.part;
        inertia = inertia
            + ep.inertia
                .rotate(s.transform.rotation)
                .shift(ep.mass_kg, s.com_body - com);

        // `u32` index space is a real bound — `PartWeld` fields are
        // `u32`. Overflow is a typed error, never a `u32::MAX` sentinel
        // that would silently misindex every later part's outputs.
        let site_start = u32::try_from(force_sites.len())
            .map_err(|_| WeldError::IndexOverflow("weld: force site start exceeds u32::MAX"))?;
        for site in &ep.force_sites {
            force_sites.push(ForceSite {
                local_pos: s.transform.apply(site.local_pos),
                local_dir: s.transform.apply_dir(site.local_dir),
                kind: site.kind,
                rated_n: site.rated_n,
            });
        }
        let site_len = u32::try_from(ep.force_sites.len()).map_err(|_| {
            WeldError::IndexOverflow("weld: per-part force site count exceeds u32::MAX")
        })?;

        let hull_index = u32::try_from(hulls.len())
            .map_err(|_| WeldError::IndexOverflow("weld: hull index exceeds u32::MAX"))?;
        let mut vertices = ep.collider.vertices();
        for v in &mut vertices {
            *v = s.transform.apply(*v);
        }
        canonicalize(&mut vertices);
        // The part's body-frame hull AABB — the thermal net's
        // contact/radiative geometry. An empty vertex set (a
        // point-collider part) produces a degenerate AABB at the
        // part's CoM: zero contact and zero radiative area, both
        // honest readings of "no skin".
        let mut lo = DVec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut hi = DVec3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for v in &vertices {
            lo = DVec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
            hi = DVec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
        }
        if vertices.is_empty() {
            lo = s.com_body;
            hi = s.com_body;
        }
        aabbs.insert(*id, (lo, hi));
        let ext = hi - lo;
        thermal_nodes.push(ThermalNode {
            node: *id,
            heat_capacity_j_per_k: ep.thermal.heat_capacity_j_per_k,
            max_temp_k: ep.thermal.max_temp_k,
            emissivity: ep.thermal.emissivity,
            radiative_area_m2: 2.0 * (ext.x * ext.y + ext.y * ext.z + ext.x * ext.z),
            aabb_lo: lo,
            aabb_hi: hi,
        });
        hulls.push(ConvexHull { vertices });

        part_index.insert(
            *id,
            PartWeld {
                site_start,
                site_len,
                hull_index,
            },
        );
    }
    if !inertia.is_finite() {
        return Err(WeldError::NonFiniteResult(
            "weld: inertia tensor is non-finite",
        ));
    }
    let thermal = Box::new(build_thermal_net(solved, &aabbs, thermal_nodes)?);
    Ok(WeldedBody {
        pos: DVec3::ZERO,
        rot: DQuat::IDENTITY,
        vel: DVec3::ZERO,
        ang_vel: DVec3::ZERO,
        throttle: 0.0,
        settle: SettleState::default(),
        mass_kg: total,
        com_local: com,
        inertia_local: inertia,
        force_sites,
        collider: CompoundCollider { hulls },
        part_index,
        aero: None,
        decel: Vec::new(),
        thermal,
    })
}

/// The thermal net half of `assemble` — one [`ThermalEdge`] per
/// parent→child attachment with the joint conductance of
/// `crate::thermal`'s module doc. The attach axis is the dominant
/// component of the AABB-centre separation; coincident centres (a
/// part fully inside its parent) produce no edge — a degenerate
/// attachment has no defined contact face.
fn build_thermal_net(
    solved: &BTreeMap<PartNodeId, SolvedNode<'_>>,
    aabbs: &BTreeMap<PartNodeId, (DVec3, DVec3)>,
    nodes: Vec<ThermalNode>,
) -> Result<ThermalNet, WeldError> {
    let mut edges = Vec::new();
    for (id, s) in solved {
        let Some(parent) = s.parent else { continue };
        let (Some(&(cl, ch)), Some(&(pl, ph))) = (aabbs.get(id), aabbs.get(&parent)) else {
            continue;
        };
        let sep = ((cl + ch) - (pl + ph)) * 0.5;
        // Dominant axis of the separation → the contact plane.
        let ax = if sep.x.abs() >= sep.y.abs() && sep.x.abs() >= sep.z.abs() {
            0usize
        } else if sep.y.abs() >= sep.z.abs() {
            1usize
        } else {
            2usize
        };
        let (u, v) = ([1, 2, 0][ax], [2, 0, 1][ax]);
        let comp = |p: DVec3, i: usize| [p.x, p.y, p.z][i];
        let face_c = (comp(ch, u) - comp(cl, u)).max(0.0) * (comp(ch, v) - comp(cl, v)).max(0.0);
        let face_p = (comp(ph, u) - comp(pl, u)).max(0.0) * (comp(ph, v) - comp(pl, v)).max(0.0);
        let ov_u = (comp(ch, u).min(comp(ph, u)) - comp(cl, u).max(comp(pl, u))).max(0.0);
        let ov_v = (comp(ch, v).min(comp(ph, v)) - comp(cl, v).max(comp(pl, v))).max(0.0);
        let shared = ov_u * ov_v;
        if sep.norm_squared() < 1e-18 {
            continue;
        }
        // The parent's part is reachable through `solved` — the DFS
        // guarantees the parent was inserted first.
        let Some(ps) = solved.get(&parent) else {
            continue;
        };
        let g = joint_conductance(
            s.part.thermal.conduction_w_per_k,
            ps.part.thermal.conduction_w_per_k,
            shared,
            face_c.min(face_p),
        );
        if g > 0.0 {
            // The joint is undirected — store canonically (lesser id
            // first) so a re-rooted graph produces the identical net.
            let (a, b) = if parent < *id {
                (parent, *id)
            } else {
                (*id, parent)
            };
            // The thermal tick is explicit Euler — the edge's
            // two-lump conduction map has multiplier
            // `1 − G·dt·(1/C_a + 1/C_b)`, which leaves [−1, 1] (and
            // diverges, alternating) once the product reaches 2.
            // `validate_effective`'s per-field gates can't see the
            // coupling, so the coupled bound is enforced at edge
            // formation: a net the pinned-dt tick cannot integrate
            // is not emitted (both capacities are proven
            // positive-finite upstream).
            if g * PHYSICS_DT
                * (1.0 / s.part.thermal.heat_capacity_j_per_k
                    + 1.0 / ps.part.thermal.heat_capacity_j_per_k)
                >= 2.0
            {
                return Err(WeldError::UnstableThermalCoupling { a, b });
            }
            edges.push(ThermalEdge {
                a,
                b,
                conductance_w_per_k: g,
            });
        }
    }
    // Canonical order — emission followed the child-side traversal,
    // which a re-root permutes; the joint set itself is unchanged.
    edges.sort_by_key(|e| (e.a, e.b));
    Ok(ThermalNet { nodes, edges })
}

/// Sort vertices by `(x, y, z)` total order and drop duplicates — the
/// hull's canonical, input-order-invariant form. `f64::total_cmp` is a
/// bit-level order: deterministic even on `-0.0`.
///
/// This is **not** a hull reduction: interior (non-extreme) points of
/// the input survive — canonicalization dedups and sorts, it does not
/// compute the convex hull. Consumers must not assume the vertex set
/// is reduced (the facet detector tolerates interior points).
fn canonicalize(vertices: &mut Vec<DVec3>) {
    vertices.sort_by(|a, b| {
        a.x.total_cmp(&b.x)
            .then_with(|| a.y.total_cmp(&b.y))
            .then_with(|| a.z.total_cmp(&b.z))
    });
    vertices.dedup();
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::graph::PartNode;
    use crate::part::{ColliderShape, ForceSiteKind};
    use core::f64::consts::FRAC_PI_2;

    /// A legal `ThermalDef` for the weld's input gate — tests that
    /// care about specific values override fields.
    fn test_thermal() -> csp_data::ThermalDef {
        csp_data::ThermalDef {
            heat_capacity_j_per_k: 100_000.0,
            max_temp_k: 1_200.0,
            emissivity: 0.8,
            conduction_w_per_k: 50.0,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-13 * b.abs().max(1.0)
    }

    fn assert_vec_close(a: DVec3, b: DVec3) {
        assert!(
            close(a.x, b.x) && close(a.y, b.y) && close(a.z, b.z),
            "expected {b:?}, got {a:?}"
        );
    }

    fn point_part(mass_kg: f64) -> WeldPart {
        WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            parachute: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg,
            com: DVec3::ZERO,
            inertia: SymMat3::ZERO,
            collider: ColliderShape::Points(vec![DVec3::ZERO]),
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: test_thermal(),
        }
    }

    fn box_part(mass_kg: f64, half: DVec3) -> WeldPart {
        WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            parachute: None,
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

    /// Two 10 kg point masses at x = ±2 m — the classic dumbbell.
    /// Hand-computed: M = 20, CoM = 0, Ixx = 0, Iyy = Izz = 80.
    #[test]
    fn dumbbell_rod_hand_computed() {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(
                None,
                PartTransform::new(DVec3::new(-2.0, 0.0, 0.0), DQuat::IDENTITY),
                0,
            ),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(4.0, 0.0, 0.0), DQuat::IDENTITY),
                1,
            ),
        );
        let parts = [point_part(10.0), point_part(10.0)];
        let w = weld(&g, &parts).unwrap();

        assert_eq!(w.mass_kg, 20.0);
        assert_eq!(w.com_local, DVec3::ZERO);
        assert_eq!(w.inertia_local.xx, 0.0);
        assert_eq!(w.inertia_local.yy, 80.0);
        assert_eq!(w.inertia_local.zz, 80.0);
        assert_eq!(w.inertia_local.xy, 0.0);
        // PSD on a degenerate axis: a point-mass rod has a zero
        // principal moment — symmetric by construction, semidefinite.
        assert!(w.inertia_local.is_positive_semidefinite());
    }

    /// Two stacked solid cuboids. Hand-computed:
    /// A: m = 8, h = (1,1,1) at origin → local I = 16/3 each diagonal.
    /// B: m = 8, h = (0.5,0.5,0.5) at (0,2,0) → local I = 4/3.
    /// M = 16, CoM = (0,1,0).
    /// Ixx = Izz = 16/3 + 8·1 + 4/3 + 8·1 = 68/3; Iyy = 20/3.
    #[test]
    fn box_stack_hand_computed() {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.0, 2.0, 0.0), DQuat::IDENTITY),
                1,
            ),
        );
        let parts = [
            box_part(8.0, DVec3::new(1.0, 1.0, 1.0)),
            box_part(8.0, DVec3::new(0.5, 0.5, 0.5)),
        ];
        let w = weld(&g, &parts).unwrap();

        assert_eq!(w.mass_kg, 16.0);
        assert_vec_close(w.com_local, DVec3::new(0.0, 1.0, 0.0));
        assert!(close(w.inertia_local.xx, 68.0 / 3.0));
        assert!(close(w.inertia_local.yy, 20.0 / 3.0));
        assert!(close(w.inertia_local.zz, 68.0 / 3.0));
        assert_eq!(w.inertia_local.xy, 0.0);
        assert_eq!(w.inertia_local.xz, 0.0);
        assert_eq!(w.inertia_local.yz, 0.0);
        assert!(w.inertia_local.is_positive_definite());
    }

    /// Asymmetric assembly with hand-computed products of inertia.
    /// A: solid box m = 12, h = (0.5,0.5,0.5) at origin (I = 2 diag).
    /// B: point mass m = 4 at (0.75, 1.5, −0.5).
    /// M = 16, CoM = (0.1875, 0.375, −0.125) — all dyadic, exact.
    /// Tensor (about CoM): xx = 9.5, yy = 4.4375, zz = 10.4375,
    /// xy = −3.375, xz = 1.125, yz = 2.25.
    #[test]
    fn asymmetric_hand_computed_with_products() {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.75, 1.5, -0.5), DQuat::IDENTITY),
                1,
            ),
        );
        let parts = [box_part(12.0, DVec3::new(0.5, 0.5, 0.5)), point_part(4.0)];
        let w = weld(&g, &parts).unwrap();

        assert_eq!(w.mass_kg, 16.0);
        assert_eq!(w.com_local, DVec3::new(0.1875, 0.375, -0.125));
        let i = w.inertia_local;
        assert!(close(i.xx, 9.5), "xx = {}", i.xx);
        assert!(close(i.yy, 4.4375), "yy = {}", i.yy);
        assert!(close(i.zz, 10.4375), "zz = {}", i.zz);
        assert!(close(i.xy, -3.375), "xy = {}", i.xy);
        assert!(close(i.xz, 1.125), "xz = {}", i.xz);
        assert!(close(i.yz, 2.25), "yz = {}", i.yz);
        assert!(i.is_positive_definite());
    }

    /// A child rotated 90° about z: its long-x box becomes long-y in
    /// body frame; collider vertices and site directions rotate too.
    #[test]
    fn rotated_child_transforms_everything() {
        let rot = DQuat::from_axis_angle(DVec3::Z, FRAC_PI_2);
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(1.0, 0.0, 0.0), rot),
                1,
            ),
        );
        let mut child = box_part(6.0, DVec3::new(1.0, 0.25, 0.25));
        child.force_sites = vec![ForceSite::new(
            DVec3::new(0.0, -1.0, 0.0),
            DVec3::new(0.0, -1.0, 0.0),
            ForceSiteKind::Thrust,
            1_000.0,
        )];
        let parts = [point_part(2.0), child];
        let w = weld(&g, &parts).unwrap();

        assert_eq!(w.mass_kg, 8.0);
        // CoM: 6 kg at (1,0,0), 2 kg at origin → (0.75, 0, 0).
        assert_vec_close(w.com_local, DVec3::new(0.75, 0.0, 0.0));
        // Child tensor after RIRᵀ: xx=2.125, yy=0.25, zz=2.125; shifted
        // by d=(0.25,0,0): +6·(0, 0.0625, 0.0625). Root point at
        // d=(−0.75,0,0): +2·(0, 0.5625, 0.5625).
        let i = w.inertia_local;
        assert!(close(i.xx, 2.125), "xx = {}", i.xx);
        assert!(
            close(i.yy, 0.25 + 6.0 * 0.0625 + 2.0 * 0.5625),
            "yy = {}",
            i.yy
        );
        assert!(
            close(i.zz, 2.125 + 6.0 * 0.0625 + 2.0 * 0.5625),
            "zz = {}",
            i.zz
        );
        // Site: part-local pos (0,-1,0) → rotated (−(−1))= +1 x? R
        // maps (0,-1,0)→(1,0,0); +offset → body pos (2,0,0); dir → +x.
        assert_eq!(w.force_sites.len(), 1);
        let s = w.force_sites[0];
        assert_vec_close(s.local_pos, DVec3::new(2.0, 0.0, 0.0));
        assert_vec_close(s.local_dir, DVec3::new(1.0, 0.0, 0.0));
        assert_eq!(s.kind, ForceSiteKind::Thrust);
        // Hull vertex (1, 0.25, 0.25) local → (−0.25, 1, 0.25) rot
        // → (0.75, 1, 0.25) body.
        let hull = &w.collider.hulls[w.part_index[&PartNodeId(2)].hull_index as usize];
        assert!(
            hull.vertices
                .iter()
                .any(|v| close(v.x, 0.75) && close(v.y, 1.0) && close(v.z, 0.25))
        );
        assert_eq!(hull.vertices.len(), 8);
    }

    /// Sites order ascending by node id and `part_index` records each
    /// node's site range — the provenance the flat spec triple needs.
    #[test]
    fn part_index_maps_sites_and_hulls() {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.0, 2.0, 0.0), DQuat::IDENTITY),
                1,
            ),
        );
        let mut a = box_part(10.0, DVec3::new(0.5, 0.5, 0.5));
        a.force_sites = vec![ForceSite::new(
            DVec3::new(0.0, -0.5, 0.0),
            DVec3::new(0.0, -1.0, 0.0),
            ForceSiteKind::Thrust,
            500.0,
        )];
        let mut b = point_part(4.0);
        b.force_sites = vec![
            ForceSite::new(DVec3::ZERO, DVec3::Z, ForceSiteKind::Rcs, 25.0),
            ForceSite::new(DVec3::ZERO, -DVec3::Z, ForceSiteKind::Rcs, 25.0),
        ];
        let parts = [a, b];
        let w = weld(&g, &parts).unwrap();

        let wa = w.part_index[&PartNodeId(1)];
        let wb = w.part_index[&PartNodeId(2)];
        assert_eq!((wa.site_start, wa.site_len), (0, 1));
        assert_eq!((wb.site_start, wb.site_len), (1, 2));
        assert_eq!(w.force_sites.len(), 3);
        assert_eq!(w.force_sites[0].kind, ForceSiteKind::Thrust);
        // `rated_n` welds through unmodified (the backend's thrust
        // magnitude channel — only geometry is transformed).
        assert_eq!(w.force_sites[0].rated_n, 500.0);
        assert_eq!(w.force_sites[1].rated_n, 25.0);
        assert_eq!((wa.hull_index, wb.hull_index), (0, 1));
        assert_eq!(w.collider.hulls.len(), 2);
        // Child RCS site translated into body frame at (0,2,0).
        assert_eq!(w.force_sites[1].local_pos, DVec3::new(0.0, 2.0, 0.0));
    }

    /// §2.4: "bit-identical for a given (graph, effective_parts); a
    /// property test asserts invariance under part-ordering
    /// permutation." BTreeMap construction is order-free, so the test
    /// also permutes the `effective` slice with remapped indices —
    /// the only permutation channel left.
    #[test]
    fn bit_identical_under_input_permutation() {
        let mk = |insert_order: &[u8], eff: &[WeldPart], idx: &[u32]| {
            let mut g = PartGraph::new(PartNodeId(1));
            let nodes = [
                (
                    PartNodeId(1),
                    PartNode::new(None, PartTransform::IDENTITY, idx[0]),
                ),
                (
                    PartNodeId(2),
                    PartNode::new(
                        Some(PartNodeId(1)),
                        PartTransform::new(DVec3::new(0.75, 1.5, -0.5), DQuat::IDENTITY),
                        idx[1],
                    ),
                ),
                (
                    PartNodeId(3),
                    PartNode::new(
                        Some(PartNodeId(2)),
                        PartTransform::new(
                            DVec3::new(0.25, 0.5, 0.125),
                            DQuat::from_axis_angle(DVec3::Y, 0.3),
                        ),
                        idx[2],
                    ),
                ),
            ];
            for &i in insert_order {
                let (id, node) = &nodes[i as usize];
                g.insert(*id, node.clone());
            }
            weld(&g, eff).unwrap()
        };
        let parts = [
            box_part(12.0, DVec3::new(0.5, 0.5, 0.5)),
            point_part(4.0),
            box_part(2.0, DVec3::new(0.25, 0.125, 0.375)),
        ];
        let a = mk(&[0, 1, 2], &parts, &[0, 1, 2]);
        // Reverse insertion order, reversed effective slice, remapped.
        let rev: Vec<WeldPart> = parts.iter().rev().cloned().collect();
        let b = mk(&[2, 1, 0], &rev, &[2, 1, 0]);
        let shuffled: Vec<WeldPart> = vec![parts[1].clone(), parts[2].clone(), parts[0].clone()];
        let c = mk(&[1, 0, 2], &shuffled, &[2, 0, 1]);
        assert_bit_identical(&a, &b);
        assert_bit_identical(&a, &c);
    }

    /// `f64::to_bits` comparison of the mass-property fields — `==`
    /// cannot distinguish `+0.0` from `-0.0` or compare NaN payloads,
    /// so bit-identity is asserted at the bit level. The remaining
    /// fields (`force_sites`, `collider`, `part_index`, kinematics)
    /// get `PartialEq` plus per-component `to_bits` on their `f64`s.
    fn assert_bit_identical(a: &WeldedBody, b: &WeldedBody) {
        assert_eq!(a.mass_kg.to_bits(), b.mass_kg.to_bits(), "mass_kg");
        for (x, y) in [
            (a.com_local.x, b.com_local.x),
            (a.com_local.y, b.com_local.y),
            (a.com_local.z, b.com_local.z),
            (a.pos.x, b.pos.x),
            (a.pos.y, b.pos.y),
            (a.pos.z, b.pos.z),
            (a.vel.x, b.vel.x),
            (a.vel.y, b.vel.y),
            (a.vel.z, b.vel.z),
            (a.ang_vel.x, b.ang_vel.x),
            (a.ang_vel.y, b.ang_vel.y),
            (a.ang_vel.z, b.ang_vel.z),
        ] {
            assert_eq!(x.to_bits(), y.to_bits());
        }
        let (ia, ib) = (a.inertia_local, b.inertia_local);
        for (x, y) in [
            (ia.xx, ib.xx),
            (ia.yy, ib.yy),
            (ia.zz, ib.zz),
            (ia.xy, ib.xy),
            (ia.xz, ib.xz),
            (ia.yz, ib.yz),
        ] {
            assert_eq!(x.to_bits(), y.to_bits());
        }
        for (qa, qb) in [
            (a.rot.x, b.rot.x),
            (a.rot.y, b.rot.y),
            (a.rot.z, b.rot.z),
            (a.rot.w, b.rot.w),
        ] {
            assert_eq!(qa.to_bits(), qb.to_bits());
        }
        assert_eq!(a.force_sites, b.force_sites);
        assert_eq!(a.collider, b.collider);
        assert_eq!(a.part_index, b.part_index);
    }

    /// The same physical assembly under a *permuted node-id
    /// labelling*: tree structure preserved (edges mapped through the
    /// relabel), transforms and effective parts attached to the same
    /// physical nodes. Bit-identity holds only for identical
    /// labellings — `f64` summation follows ascending `PartNodeId`, so
    /// a relabelled weld sums in a different order and can differ in
    /// the last ulp. Mass, CoM, and inertia must agree to the stated
    /// `1e-13` relative tolerance; `close` is that tolerance.
    #[test]
    fn permuted_node_ids_agree_to_rounding() {
        // Physical nodes: r (root) with children a and b; c under a.
        // Labelling A: r=1 a=2 b=3 c=4 — sum order r,a,b,c.
        // Labelling B: r=40 a=10 b=30 c=20 — sum order a,c,b,r.
        let mk = |r: u64, a: u64, b: u64, c: u64| {
            let (r, a, b, c) = (PartNodeId(r), PartNodeId(a), PartNodeId(b), PartNodeId(c));
            let mut g = PartGraph::new(r);
            g.insert(r, PartNode::new(None, PartTransform::IDENTITY, 0));
            g.insert(
                a,
                PartNode::new(
                    Some(r),
                    PartTransform::new(DVec3::new(0.75, 1.5, -0.5), DQuat::IDENTITY),
                    1,
                ),
            );
            g.insert(
                b,
                PartNode::new(
                    Some(r),
                    PartTransform::new(
                        DVec3::new(-0.5, 0.25, 1.0),
                        DQuat::from_axis_angle(DVec3::new(0.0, 1.0, 1.0), -0.7),
                    ),
                    2,
                ),
            );
            g.insert(
                c,
                PartNode::new(
                    Some(a),
                    PartTransform::new(
                        DVec3::new(0.125, -0.375, 0.5),
                        DQuat::from_axis_angle(DVec3::Y, 0.3),
                    ),
                    3,
                ),
            );
            g
        };
        let mut parts = [
            box_part(12.0, DVec3::new(0.5, 0.5, 0.5)),
            point_part(4.0),
            box_part(2.0, DVec3::new(0.25, 0.125, 0.375)),
            point_part(1.5),
        ];
        parts[2].force_sites = vec![
            ForceSite::new(DVec3::ZERO, DVec3::Z, ForceSiteKind::Thrust, 10.0),
            ForceSite::new(DVec3::ZERO, -DVec3::Z, ForceSiteKind::Thrust, 10.0),
        ];
        let x = weld(&mk(1, 2, 3, 4), &parts).unwrap();
        let y = weld(&mk(40, 10, 30, 20), &parts).unwrap();
        assert!(close(x.mass_kg, y.mass_kg), "mass {x:?} {y:?}");
        assert_vec_close(x.com_local, y.com_local);
        let (i, j) = (x.inertia_local, y.inertia_local);
        assert!(close(i.xx, j.xx) && close(i.yy, j.yy) && close(i.zz, j.zz));
        assert!(close(i.xy, j.xy) && close(i.xz, j.xz) && close(i.yz, j.yz));
        // Force sites and hulls are ordered by ascending id, so the
        // relabelled weld lists them in a different sequence — only
        // the aggregate mass properties are order-invariant.
        assert_eq!(x.force_sites.len(), y.force_sites.len());
        assert_eq!(x.collider.hulls.len(), y.collider.hulls.len());
    }

    /// Two welds of the same inputs are byte-for-byte equal — cheap
    /// re-run determinism check distinct from the permutation one.
    #[test]
    fn reweld_is_bit_identical() {
        let mut g = PartGraph::new(PartNodeId(7));
        g.insert(
            PartNodeId(7),
            PartNode::new(
                None,
                PartTransform::new(DVec3::new(1.0, 2.0, 3.0), DQuat::IDENTITY),
                0,
            ),
        );
        let parts = [box_part(5.0, DVec3::new(1.0, 0.5, 0.25))];
        assert_bit_identical(&weld(&g, &parts).unwrap(), &weld(&g, &parts).unwrap());
    }

    #[test]
    fn empty_and_broken_graphs_are_typed_errors() {
        let parts = [point_part(1.0)];

        let g = PartGraph::new(PartNodeId(0));
        assert_eq!(weld(&g, &parts), Err(WeldError::EmptyGraph));

        let mut g = PartGraph::new(PartNodeId(9)); // root not a node
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        assert_eq!(weld(&g, &parts), Err(WeldError::MissingRoot(PartNodeId(9))));

        let mut g = PartGraph::new(PartNodeId(1)); // root with a parent
        g.insert(
            PartNodeId(1),
            PartNode::new(Some(PartNodeId(2)), PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        assert_eq!(
            weld(&g, &parts),
            Err(WeldError::RootHasParent(PartNodeId(1)))
        );

        let mut g = PartGraph::new(PartNodeId(1)); // dangling parent
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(Some(PartNodeId(77)), PartTransform::IDENTITY, 0),
        );
        assert_eq!(
            weld(&g, &parts),
            Err(WeldError::MissingParent {
                node: PartNodeId(2),
                parent: PartNodeId(77),
            })
        );

        // A two-node parent cycle detached from the root: both are
        // unreachable; lowest id reports first (deterministic error).
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(3),
            PartNode::new(Some(PartNodeId(4)), PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(4),
            PartNode::new(Some(PartNodeId(3)), PartTransform::IDENTITY, 0),
        );
        assert_eq!(weld(&g, &parts), Err(WeldError::Unreachable(PartNodeId(3))));

        // Orphan (parent = None) away from root → Unreachable.
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        assert_eq!(weld(&g, &parts), Err(WeldError::Unreachable(PartNodeId(2))));
    }

    #[test]
    fn bad_part_data_is_a_typed_error() {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(
                None,
                PartTransform::IDENTITY,
                3, // out of range
            ),
        );
        let parts = [point_part(1.0)];
        assert_eq!(
            weld(&g, &parts),
            Err(WeldError::EffectiveIndexOutOfRange {
                node: PartNodeId(1),
                index: 3,
                len: 1,
            })
        );

        // Negative mass.
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        let mut bad = point_part(1.0);
        bad.mass_kg = -2.0;
        assert!(matches!(
            weld(&g, &[bad]),
            Err(WeldError::InvalidPart {
                node: PartNodeId(1),
                ..
            })
        ));

        // Non-PSD inertia.
        let mut bad = point_part(1.0);
        bad.inertia = SymMat3::new(-1.0, 1.0, 1.0, 0.0, 0.0, 0.0);
        assert!(matches!(
            weld(&g, &[bad]),
            Err(WeldError::InvalidPart {
                node: PartNodeId(1),
                ..
            })
        ));

        // NaN transform offset.
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(
                None,
                PartTransform::new(DVec3::new(f64::NAN, 0.0, 0.0), DQuat::IDENTITY),
                0,
            ),
        );
        assert!(matches!(
            weld(&g, &parts),
            Err(WeldError::InvalidPart {
                node: PartNodeId(1),
                ..
            })
        ));

        // Non-unit force-site direction — would silently scale
        // `throttle·rated_n`; the `NonUnitRotation` precedent.
        let mut g_ok = PartGraph::new(PartNodeId(1));
        g_ok.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        let mut bad = point_part(1.0);
        bad.force_sites = vec![ForceSite::new(
            DVec3::ZERO,
            DVec3::new(2.0, 0.0, 0.0),
            ForceSiteKind::Thrust,
            10.0,
        )];
        assert!(matches!(
            weld(&g_ok, &[bad]),
            Err(WeldError::InvalidPart {
                node: PartNodeId(1),
                ..
            })
        ));

        // Over-cap collider vertex set — the `O(n⁴)` per-tick
        // facet sweep bound.
        let mut bad = point_part(1.0);
        bad.collider =
            ColliderShape::Points(vec![DVec3::ZERO; crate::part::MAX_COLLIDER_VERTICES + 1]);
        assert!(matches!(
            weld(&g_ok, &[bad]),
            Err(WeldError::InvalidPart {
                node: PartNodeId(1),
                ..
            })
        ));

        // Zero total mass → CoM undefined.
        let zero = [point_part(0.0), point_part(0.0)];
        assert_eq!(
            weld(&g_valid_two_zeros(), &zero),
            Err(WeldError::ZeroTotalMass)
        );

        // WeldError maps onto SimError::SaveBlocking (§2.13 data
        // validation failure) rather than panicking.
        assert_eq!(
            SimError::from(WeldError::EmptyGraph),
            SimError::SaveBlocking("weld: empty part graph"),
        );
    }

    /// F1: a non-unit rotation is a typed `NonUnitRotation` error on
    /// any node — `(0,0,0,2)` must not produce plausible-but-wrong
    /// output (it would double every rotated quantity), and the zero
    /// quat is not a rotation either.
    #[test]
    fn non_unit_rotation_is_a_typed_error() {
        // Scaled quat on the root — the transform is used raw.
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(
                None,
                PartTransform::new(DVec3::ZERO, DQuat::new(0.0, 0.0, 0.0, 2.0)),
                0,
            ),
        );
        let parts = [point_part(1.0)];
        assert_eq!(
            weld(&g, &parts),
            Err(WeldError::NonUnitRotation(PartNodeId(1)))
        );

        // Zero quat on a child — compose would normalize-and-propagate
        // NaN; rejected at validation instead.
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(1.0, 0.0, 0.0), DQuat::new(0.0, 0.0, 0.0, 0.0)),
                1,
            ),
        );
        let parts = [point_part(1.0), point_part(1.0)];
        assert_eq!(
            weld(&g, &parts),
            Err(WeldError::NonUnitRotation(PartNodeId(2)))
        );

        // A quat inside UNIT_ROTATION_TOLERANCE still welds — it is
        // renormalized at the root / per compose step.
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(
                None,
                PartTransform::new(DVec3::ZERO, DQuat::new(0.0, 0.0, 0.0, 1.0 + 1e-11)),
                0,
            ),
        );
        let w = weld(&g, &parts[..1]).unwrap();
        assert_eq!(w.mass_kg, 1.0);
    }

    fn g_valid_two_zeros() -> PartGraph {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(1.0, 0.0, 0.0), DQuat::IDENTITY),
                1,
            ),
        );
        g
    }

    /// A single part: the weld is a frame change — mass preserved,
    /// CoM translated, tensor rotated+shifted to the body CoM.
    #[test]
    fn single_part_passthrough() {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(
                None,
                PartTransform::new(DVec3::new(3.0, -1.0, 2.0), DQuat::IDENTITY),
                0,
            ),
        );
        let parts = [box_part(6.0, DVec3::new(1.0, 0.5, 0.25))];
        let w = weld(&g, &parts).unwrap();
        assert_eq!(w.mass_kg, 6.0);
        assert_eq!(w.com_local, DVec3::new(3.0, -1.0, 2.0));
        // Part com sits at body CoM → no parallel-axis addition.
        assert_eq!(w.inertia_local, parts[0].inertia);
        assert!(w.inertia_local.is_positive_definite());
        // Fresh kinematics: weld stamps rest-at-origin (§2.4 spawn/
        // merge paths set pose + momentum afterwards).
        assert_eq!(w.pos, DVec3::ZERO);
        assert_eq!(w.rot, DQuat::IDENTITY);
        assert_eq!(w.vel, DVec3::ZERO);
        assert_eq!(w.ang_vel, DVec3::ZERO);
        assert_eq!(w.momentum(), DVec3::ZERO);
    }
}
