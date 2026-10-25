#![forbid(unsafe_code)]

//! Weld (pure fn), collision + rigid-body integration behind `PhysicsBackend`
//! (PLAN §2.4, §2.9). `parry3d-f64`/`rapier3d-f64` stay workspace-declared
//! for the M6 jointed-island work; the M6-A body-vs-body contact model uses
//! the in-crate facet-plane machinery in [`hull`].
//!
//! M2 sub-phase A landed the §2.4 vessel model and **weld Stage 1** —
//! `csp_phys::weld(part_graph, &[WeldPart]) -> WeldedBody`, the pure,
//! bit-deterministic function computing total mass, exact CoM, the full
//! inertia tensor (per-part `R·I·Rᵀ` + parallel-axis composition), body-frame
//! `ForceSite`s, and the merged convex-hull compound collider.
//!
//! M2 sub-phase B lands the §2.9 **`SimpleBackend`**: the pinned
//! semi-implicit Euler integrator (`dt = 1/64` always), the analytic force
//! model (gravity `μ/r²`, pre-voxel analytic drag on the §3.0 closed-form
//! atmosphere, `ForceSite` thrust `throttle·rated_n`), the projected
//! Gauss–Seidel contact solve against the planet sphere (8 iterations,
//! fixed ascending `(body_id, contact_index)` order, Baumgarte β = 0.2 at
//! the position level, restitution 0, two-direction Coulomb friction), and
//! the §2.6.4 settle latch as its terminal state. Determinism rules per
//! §2.2/§2.7: `BTreeMap`/ordered `Vec` iteration only, plain `f64` behind
//! `csp_core`'s libm-only primitives, serde derives on everything that
//! enters `state_hash`.
//!
//! M3 sub-phase B lands the §3.1 **effective-stat pipeline**:
//! `resolve(BuiltPart) ∘ wear(PartUsage)` — the serialized
//! craft-instance record → the resolved [`EffectivePart`], with the
//! weld-consumed view (`WeldPart`, its `weld_input` field) embedded.
//! Wear is plumbed but stubbed pending the §3.4 accumulation loop.
//!
//! M6 sub-phase A lands **vessel-vessel contact**: every body in one
//! bubble batch sees every other — collider vertices penetrating a
//! peer hull's facet volume (and vice versa) form a pair manifold the
//! same PGS machinery solves with equal-and-opposite impulses, plus
//! the §7.4/§2.6.2 geometric `vessel_contact` predicate. Latched
//! bodies act as static colliders.
//!
//! M6 sub-phase B lands the §2.4 **transient joint** machinery:
//! [`JointDef`]/[`JointKind`] (`SoftCapture`) and a bilateral
//! three-row joint constraint solved inside `SimpleBackend`'s PGS
//! velocity pass — a compliant spherical joint that reels its anchors
//! together under a capped closing speed. The sim's capture detection,
//! cap-16 bookkeeping, and the latch merge live in `csp-sim`/M6-C.
//!
//! M6 sub-phase C lands the §2.4 **latch merge** graph edit:
//! [`merge::merge_graphs`] folds a settled soft-capture's secondary
//! graph into the primary's — the secondary root becomes a child of
//! the primary-side port node at the recorded seam transform, its
//! ids remapped above the primary's — so the merged graph welds
//! bit-identically to a from-scratch graph of the same parts. Undock
//! splits through the [`structural`] fragment path; the `DockSeam`
//! record and latch bookkeeping live in `csp-sim`.
//!
//! Deliberately absent here: a `RapierBackend` jointed-island solver —
//! §2.9 names it, and `rapier3d-f64` remains workspace-pinned for the
//! §2.9 oracle cross-check and later robotics work; [`JointDef`]'s
//! backend note documents why v1 soft-capture runs inside
//! `SimpleBackend` instead — plus transfer/crew plumbing (M6-D).
//!
//! - [`graph`] — `PartGraph`/`PartNode`/`PartTransform`, the weld's
//!   topology input.
//! - [`part`] — `WeldPart`, `ForceSite`, `ColliderShape`: the
//!   resolved per-part data the weld consumes.
//! - [`resolve`] — `BuiltPart`, `PartUsage`, `EffectivePart`,
//!   `resolve_part`/`resolve_parts`: the §3.1 resolver.
//! - [`session`] — `CraftSession`: the M3 editor session model (part
//!   snap graph, staging, craft save/load with stable `PartNodeId`s).
//!   The type lives here — not in `csp-ui`, which owns the working
//!   instance — so headless consumers and the weld boundary stay
//!   egui-free; see the module doc for the placement decision.
//! - [`readout`] — `analyze` → `CraftAnalysis`/`StageStats`: the VAB's
//!   live per-stage Δv/TWR/burn-time over a session + resolved parts.
//! - [`profile`] — `MissionProfile`/`BurnSegment`/`Regime`: the §3.4
//!   mission-profile stub the readout compares the craft against.
//! - [`tensor`] — `SymMat3` symmetric tensor + primitive inertia
//!   constructors.
//! - [`weld`] — the pure function itself, `WeldedBody`, `WeldError`.
//! - [`backend`] — the `PhysicsBackend` trait and its value types
//!   (`Ray`/`Hit`/`Contact`/`BodyHandle`/`JointDef`).
//! - [`simple`] — `SimpleBackend`, the §2.9 default backend, and the
//!   §3.0 closed-form atmosphere evaluators.

pub mod aero;
pub mod backend;
pub mod graph;
mod hull;
pub mod merge;
pub mod part;
pub mod profile;
pub mod readout;
pub mod resolve;
pub mod session;
pub mod simple;
pub mod structural;
pub mod tensor;
pub mod thermal;
pub mod weld;

pub use aero::{
    AERO_AOA_KNOTS_RAD, AERO_DIRECTION_COUNT, AERO_MACH_KNOTS, AIR_VISCOSITY_PA_S, AeroError,
    ConfigMask, CraftAeroTables, DirTri, DirectionSet, MAX_CONFIG_MASKS, PartAeroTable,
    VOXEL_BUDGET, VOXEL_TARGET_EDGE_M, VOXELIZER_VERSION, build_aero_tables, direction_set,
    eval_aero_curve, flow_dir_at_aoa, skin_friction_cd,
};
pub use backend::{BodyHandle, Contact, Hit, JointDef, JointKind, PhysicsBackend, Ray};
pub use graph::{PartGraph, PartNode, PartNodeId, PartTransform};
pub use merge::{MergeError, MergedGraph, merge_graphs};
pub use part::{
    ColliderShape, DockPortSpec, EngineSpec, ExperimentSpec, ForceSite, ForceSiteKind, LabSpec,
    TankSpec, WeldPart,
};
pub use profile::{
    BurnSegment, HazardReport, MissionCheck, MissionProfile, Regime, confidence, expected_hazard,
    expected_hazard_report,
};
pub use readout::{CraftAnalysis, StageStats, analyze};
pub use resolve::{
    BuiltPart, EffectivePart, EngineReliability, FamilyQualification, FamilyQualifications,
    PartUsage, RegimeMetrics, ReliabilitySpec, ResolveError, ResolvedReliability, TrackLevels,
    WEAR_HAZARD_STUB, apply_wear, compose_upgrade_levels, qual_factor, resolve_built_part,
    resolve_part, resolve_parts, stress_breakdown, stress_product, wear_multiplier,
};
pub use session::{
    ATTACH_DIAMETER_REL_TOLERANCE, AttachEdge, CRAFT_FILE_MAGIC, CRAFT_SCHEMA_VERSION,
    CraftFileError, CraftSession, SessionError, SessionNode, SessionWarning, Validation,
    snap_transform,
};
pub use simple::{
    BAUMGARTE_BETA, DEFAULT_DRAG_CD, DEFAULT_FRICTION_MU, DEFAULT_SETTLE_TICKS, DEFAULT_SETTLE_V,
    DEFAULT_SETTLE_W, Environment, PHYSICS_DT, SOLVER_ITERATIONS, SimpleBackend, TerrainSurface,
    atmosphere_density_kg_m3, atmosphere_pressure_pa, atmosphere_sound_speed_mps,
    atmosphere_temperature_k,
};
pub use structural::{
    JointLoad, PartForce, failed_joints, fragment_graph, fragment_graph_of, fragment_velocity,
    joint_loads, subtree_set,
};
pub use tensor::{Axis, SymMat3};
pub use thermal::step as thermal_step;
pub use thermal::{
    CONVECTIVE_HEAT_FRACTION, SPACE_SINK_K, STEFAN_BOLTZMANN_W_M2_K4, ThermalEdge, ThermalFlow,
    ThermalNet, ThermalNode, ambient_k, flow_sample, joint_conductance, steady_state_temp_k,
};
pub use weld::{CompoundCollider, ConvexHull, PartWeld, SettleState, WeldError, WeldedBody, weld};
