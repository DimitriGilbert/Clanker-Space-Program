#![forbid(unsafe_code)]

//! Weld (pure fn), collision + rigid-body integration behind `PhysicsBackend`
//! (PLAN §2.4, §2.9). parry3d-f64 for future hull materialization; rapier for
//! jointed islands (M6).
//!
//! M2 sub-phase A landed the §2.4 vessel model and **weld Stage 1** —
//! `csp_phys::weld(part_graph, &[EffectivePart]) -> WeldedBody`, the pure,
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
//! Deliberately absent here: Stage-2 aero tables (§2.4.1 — deferred,
//! command-installed, M4a), the `RapierBackend` jointed-island solver and
//! body-vs-body/hull-facet contact (M6 — `parry3d-f64`/`rapier3d-f64` are
//! workspace-declared, not yet depended on: the sphere-vs-vertex contact
//! model above needs neither), `DockSeam` (M6), and the `BuiltPart ∘
//! wear` resolver that produces `EffectivePart`s (M3, §3.1).
//!
//! - [`graph`] — `PartGraph`/`PartNode`/`PartTransform`, the weld's
//!   topology input.
//! - [`part`] — `EffectivePart`, `ForceSite`, `ColliderShape`: the
//!   resolved per-part data the weld consumes.
//! - [`tensor`] — `SymMat3` symmetric tensor + primitive inertia
//!   constructors.
//! - [`weld`] — the pure function itself, `WeldedBody`, `WeldError`.
//! - [`backend`] — the `PhysicsBackend` trait and its value types
//!   (`Ray`/`Hit`/`Contact`/`BodyHandle`/`JointDef`).
//! - [`simple`] — `SimpleBackend`, the §2.9 default backend, and the
//!   §3.0 closed-form atmosphere evaluators.

pub mod backend;
pub mod graph;
pub mod part;
pub mod simple;
pub mod tensor;
pub mod weld;

pub use backend::{BodyHandle, Contact, Hit, JointDef, PhysicsBackend, Ray};
pub use graph::{PartGraph, PartNode, PartNodeId, PartTransform};
pub use part::{ColliderShape, EffectivePart, ForceSite, ForceSiteKind};
pub use simple::{
    BAUMGARTE_BETA, DEFAULT_DRAG_CD, DEFAULT_FRICTION_MU, DEFAULT_SETTLE_TICKS, DEFAULT_SETTLE_V,
    DEFAULT_SETTLE_W, Environment, PHYSICS_DT, SOLVER_ITERATIONS, SimpleBackend,
    atmosphere_density_kg_m3, atmosphere_pressure_pa, atmosphere_temperature_k,
};
pub use tensor::{Axis, SymMat3};
pub use weld::{CompoundCollider, ConvexHull, PartWeld, SettleState, WeldError, WeldedBody, weld};
