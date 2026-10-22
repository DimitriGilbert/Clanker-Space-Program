#![forbid(unsafe_code)]

//! Weld (pure fn), collision + rigid-body integration behind `PhysicsBackend`
//! (PLAN §2.4, §2.9). parry3d-f64 for future hull materialization; rapier for
//! jointed islands (M6).
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
//! Deliberately absent here: Stage-2 aero tables (§2.4.1 — deferred,
//! command-installed, M4a), the `RapierBackend` jointed-island solver and
//! body-vs-body/hull-facet contact (M6 — `parry3d-f64`/`rapier3d-f64` are
//! workspace-declared, not yet depended on: the sphere-vs-vertex contact
//! model above needs neither), `DockSeam` (M6).
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
pub mod part;
pub mod profile;
pub mod readout;
pub mod resolve;
pub mod session;
pub mod simple;
pub mod tensor;
pub mod weld;

pub use aero::{
    AERO_AOA_KNOTS_RAD, AERO_DIRECTION_COUNT, AERO_MACH_KNOTS, AIR_VISCOSITY_PA_S, AeroError,
    ConfigMask, CraftAeroTables, DirTri, DirectionSet, MAX_CONFIG_MASKS, PartAeroTable,
    VOXEL_BUDGET, VOXEL_TARGET_EDGE_M, VOXELIZER_VERSION, build_aero_tables, direction_set,
    eval_aero_curve, flow_dir_at_aoa, skin_friction_cd,
};
pub use backend::{BodyHandle, Contact, Hit, JointDef, PhysicsBackend, Ray};
pub use graph::{PartGraph, PartNode, PartNodeId, PartTransform};
pub use part::{ColliderShape, ForceSite, ForceSiteKind, WeldPart};
pub use profile::{BurnSegment, MissionCheck, MissionProfile, Regime};
pub use readout::{CraftAnalysis, StageStats, analyze};
pub use resolve::{
    BuiltPart, EffectivePart, FamilyQualification, FamilyQualifications, PartUsage, ResolveError,
    ResolvedReliability, TrackLevels, WEAR_HAZARD_STUB, apply_wear, resolve_built_part,
    resolve_part, resolve_parts, wear_multiplier,
};
pub use session::{
    ATTACH_DIAMETER_REL_TOLERANCE, AttachEdge, CRAFT_FILE_MAGIC, CRAFT_SCHEMA_VERSION,
    CraftFileError, CraftSession, SessionError, SessionNode, SessionWarning, Validation,
    snap_transform,
};
pub use simple::{
    BAUMGARTE_BETA, DEFAULT_DRAG_CD, DEFAULT_FRICTION_MU, DEFAULT_SETTLE_TICKS, DEFAULT_SETTLE_V,
    DEFAULT_SETTLE_W, Environment, PHYSICS_DT, SOLVER_ITERATIONS, SimpleBackend,
    atmosphere_density_kg_m3, atmosphere_pressure_pa, atmosphere_sound_speed_mps,
    atmosphere_temperature_k,
};
pub use tensor::{Axis, SymMat3};
pub use weld::{CompoundCollider, ConvexHull, PartWeld, SettleState, WeldError, WeldedBody, weld};
