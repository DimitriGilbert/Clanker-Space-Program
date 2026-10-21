//! The §2.9 `PhysicsBackend` seam — the trait shape the sim's physics
//! bubble steps through, plus the value types its signature speaks in.
//!
//! The trait is verbatim from PLAN §2.9:
//!
//! ```text
//! pub trait PhysicsBackend {
//!     fn step(&mut self, bodies: &mut [WeldedBody], joints: &[JointDef]);   // dt is always 1/64
//!     fn cast_ray(&self, ray: &Ray, max_toi: f64) -> Option<Hit>;
//!     fn contacts(&self, body: BodyHandle) -> &[Contact];
//! }
//! ```
//!
//! `dt` is never a parameter: the sim tick is immutably `1/64` s
//! (§2.3/§2.6 — warp never changes it, and the immutable-dt rule is why
//! contact substepping is illegal and §2.9's impulse solve must be
//! unconditionally stable at that step).
//!
//! Two backends are planned: [`crate::SimpleBackend`] (default v1 — the
//! pinned semi-implicit Euler + PGS contact solver) and a `RapierBackend`
//! for jointed islands (M6; `rapier3d-f64` is workspace-declared but no
//! crate depends on it yet).

use serde::{Deserialize, Serialize};

use csp_core::DVec3;

use crate::WeldedBody;

/// Index of a body inside the `&mut [WeldedBody]` slice `step` is
/// handed. The handle is positional — it is valid only against the
/// slice layout of the `step` call that produced it, and is how the
/// solver's fixed ascending `(body_id, contact_index)` iteration order
/// (§2.9) refers to bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BodyHandle(pub u32);

/// A transient live joint — the `joints` argument of
/// [`PhysicsBackend::step`].
///
/// **Stub for M6.** §2.4 makes live joints transients only (docking
/// soft-capture ≤ a few seconds, robotic hinges/pistons, decoupler
/// failures; hard cap 16 per bubble) and §2.9 assigns jointed islands
/// to `RapierBackend` — [`crate::SimpleBackend`] ignores `joints`
/// entirely. The type exists now so the trait signature is stable; the
/// fields are the minimal anchor description every joint kind shares.
/// `JointKind` variants (soft-capture, hinge, piston) land with the
/// M6 solver.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct JointDef {
    /// First body (index into the `bodies` slice).
    pub body_a: BodyHandle,
    /// Second body.
    pub body_b: BodyHandle,
    /// Attachment point in `body_a`'s local (body) frame.
    pub local_anchor_a: DVec3,
    /// Attachment point in `body_b`'s local frame.
    pub local_anchor_b: DVec3,
}

/// A ray for [`PhysicsBackend::cast_ray`]. `dir` is a unit vector;
/// callers validate finiteness at the same boundary where they reject
/// non-finite geometry. `toi` results are measured in metres along
/// `dir` — i.e. `hit.point = ray.origin + ray.dir * hit.toi`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ray {
    pub origin: DVec3,
    /// Unit direction.
    pub dir: DVec3,
}

/// What a cast ray hit. `body` is `None` when the hit is the terrain
/// body (the environment's planet sphere) rather than a stepped body.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Time-of-impact along the ray direction, metres.
    pub toi: f64,
    /// World(-bubble)-frame hit point.
    pub point: DVec3,
    /// Surface normal at the hit point, pointing back toward the ray
    /// origin side (outward for the terrain sphere).
    pub normal: DVec3,
    /// The stepped body that was hit, or `None` for the terrain body.
    pub body: Option<BodyHandle>,
}

/// One point of a contact manifold — the per-penetrating-vertex record
/// the solver works on. `normal` points from the environment surface
/// toward the body ("up"); `depth` is the vertex's penetration below
/// the surface at generation time, metres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    /// World(-bubble)-frame position of the penetrating collider
    /// vertex at generation time.
    pub pos: DVec3,
    /// Contact normal — the sphere surface's outward direction at the
    /// nearest surface point (`(pos − centre).normalized`).
    pub normal: DVec3,
    /// Penetration depth in metres at generation time (`> 0`).
    pub depth: f64,
}

/// The §2.9 physics backend interface — what `csp-sim`'s bubble step
/// drives. Implementations: [`crate::SimpleBackend`] (v1 default) and
/// the M6 `RapierBackend` for jointed islands.
///
/// `step` mutates the bodies' kinematic state in place; weld-derived
/// mass properties (`mass_kg`, `inertia_local`, `force_sites`,
/// `collider`) are inputs the backend must not modify. Solver state a
/// backend carries between ticks (contact caches, warm starts) is a
/// derived cache of serialized body state — never itself authoritative.
pub trait PhysicsBackend {
    /// Advance every body by exactly one tick — `dt` is always `1/64`
    /// (§2.3/§2.9; it is deliberately not a parameter). `joints` are
    /// the bubble's live transient joints (§2.4, cap 16); a backend
    /// with no joint solver ignores them.
    fn step(&mut self, bodies: &mut [WeldedBody], joints: &[JointDef]);
    /// Cast `ray` up to `max_toi` metres; nearest hit wins.
    fn cast_ray(&self, ray: &Ray, max_toi: f64) -> Option<Hit>;
    /// The contact manifold recorded for `body` during the most recent
    /// `step` — empty when the body touched nothing (or was latched).
    /// A derived observation, not authoritative state.
    fn contacts(&self, body: BodyHandle) -> &[Contact];
}
