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
/// §2.4 makes live joints transients only (docking soft-capture ≤ a
/// few seconds, robotic hinges/pistons, decoupler failures; hard cap
/// 16 per bubble). The joint is a compliant anchor-to-anchor
/// constraint: the backend pulls `local_anchor_a` on `body_a` toward
/// `local_anchor_b` on `body_b` under [`JointKind`]'s law — **velocity
/// level only** (it damps and reels, it never welds a position).
///
/// **Backend note (documented v1 deviation):** §2.9 names
/// `RapierBackend` as the jointed-island solver. v1's soft-capture
/// islands are instead solved inside [`crate::SimpleBackend`]'s PGS
/// velocity pass — the same fixed-order, cold-started, unconditionally
/// stable machinery §2.9 pins for contacts — because a compliant
/// spherical joint is exactly a bilateral three-row constraint there,
/// and routing the island through a second backend would add a
/// collider/pipeline lifecycle the ≤3 s transient does not need.
/// `rapier3d-f64` stays workspace-pinned for the §2.9 oracle
/// cross-check and a later `RapierBackend` (robotics).
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
    /// The constraint law + its parameters.
    pub kind: JointKind,
}

/// What the joint enforces. Discriminants are append-only `u16`s —
/// they are the §7.1 rule-7 hash encoding once live joints enter
/// `state_hash` (the sim's `LiveJoint` carries this value).
///
/// Robotic hinge/piston variants land with that milestone; §2.4's
/// decoupler-failure joint reuses this enum too.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum JointKind {
    /// Docking soft-capture (§2.4): a compliant spherical (ball)
    /// constraint at the anchors. The velocity solve drives the
    /// anchors' relative point velocity to a per-axis target of
    /// `−pull_per_s · separation`, component-clamped to
    /// `±max_correction_mps` — an exponential reel-in with a hard
    /// closing-speed cap (the "soft": approach energy damps instead
    /// of welding the pose). Coincident anchors get a zero target —
    /// the constraint then simply holds them together.
    SoftCapture {
        /// Exponential reel-in rate (1/s): the anchor separation's
        /// decay rate — `sep(t) ≈ sep₀·e^{−pull·t}` under the solve.
        pull_per_s: f64,
        /// Anchor-closing speed cap (m/s) — the compliance knob.
        max_correction_mps: f64,
    } = 0,
}

impl csp_core::StableDiscriminant for JointKind {
    fn discriminant(&self) -> u16 {
        match self {
            Self::SoftCapture { .. } => 0,
        }
    }
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
/// the solver works on. `normal` points from the surface that was
/// penetrated toward the body ("up" for the environment sphere; the
/// direction the impulse pushes the recording body for a
/// vessel-vessel pair); `depth` is the vertex's penetration at
/// generation time, metres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    /// World(-bubble)-frame position of the penetrating collider
    /// vertex at generation time.
    pub pos: DVec3,
    /// Contact normal — the penetrated surface's outward direction:
    /// the sphere surface's outward direction at the nearest surface
    /// point (`(pos − centre).normalized`) for a ground contact, the
    /// peer hull's facet normal for a vessel-vessel contact (negated
    /// on the penetrated body's own record).
    pub normal: DVec3,
    /// Penetration depth in metres at generation time (`> 0`).
    pub depth: f64,
    /// Index into the *recording* body's `CompoundCollider::hulls` —
    /// the penetrating vertex's hull on the penetrator's record, the
    /// penetrated hull on the penetrated body's own record. The part
    /// attribution the §2.4 breakup load evaluation needs
    /// (`PartWeld::hull_index`).
    pub hull: u32,
    /// The velocity solve's total accumulated impulse on this
    /// constraint, world frame, N·s (`λ_n·n + λ_t1·t1 + λ_t2·t2`).
    /// Written back after `solve_velocity`; the Baumgarte position
    /// pass's pseudo-impulses are deliberately excluded — they are
    /// position corrections, not forces. Zero on a contact that
    /// carried no impulse authority this tick.
    pub impulse: DVec3,
    /// The other stepped body this contact is with — `Some` for a
    /// vessel-vessel pair contact (the peer's slice position in the
    /// `step` call's `bodies`), `None` for the environment sphere.
    /// Distinguishes "touched the ground" (debris removal, §7.4 save
    /// refusal's contact clause, rails-warp refusal) from "touched
    /// another vessel"; positional like every `BodyHandle`.
    pub peer: Option<BodyHandle>,
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
    /// `step` — empty when the body touched nothing. Latching clears a
    /// body's records, but a latched body held in the batch as a
    /// static collider (a still-moving body resting on it) keeps
    /// collecting vessel-vessel pair records — `peer.is_some()` — on
    /// later ticks; it just generates no new ground contacts. A
    /// derived observation, not authoritative state.
    fn contacts(&self, body: BodyHandle) -> &[Contact];
}
