//! `SimpleBackend` — the §2.9 default physics backend: pinned
//! semi-implicit (symplectic) Euler integrator, analytic force model,
//! and a projected Gauss–Seidel contact solver with the §2.6.4 settle
//! latch as its terminal state.
//!
//! # Phase order (§2.9)
//!
//! One `step` is, per body, in a fixed order: **force accumulation →
//! velocity integrate → position integrate → contact solve**. Bodies
//! carry no inter-body coupling in the force phase (all forces are
//! environment- or site-generated), so the first three phases are fused
//! into one ascending-index pass — semantically identical to three
//! separate passes and cheaper. Contact generation uses the
//! post-integration pose; the impulse solve then runs over the whole
//! manifold in fixed ascending `(body_id, contact_index)` order.
//!
//! # Integrator
//!
//! Semi-implicit Euler, pinned by §2.9 (RK4 explicitly rejected — it
//! costs 4× the force loop and is wrong anyway with discontinuous
//! contact forces):
//!
//! ```text
//! v  += (F/m)·dt                              ω_b += I⁻¹·(τ_b − ω_b×(I·ω_b))·dt
//! x̄ += v·dt  (x̄ = CoM)                       q   += ½·(ω_world)·q·dt, normalized
//! pos = x̄ − q·com_local
//! ```
//!
//! The position phase advances the **centre of mass**, not the body
//! origin: `vel` is the CoM velocity (`momentum() = m·v`, and every
//! force model applies at `com_world()`), so the tick computes
//! `x̄ += v·dt`, integrates `q`, then re-derives `pos = x̄ − q·com_local`.
//! Advancing `pos` directly while `q` rotates would orbit the CoM
//! around the origin — fabricating a `ω×(q·com_local)` CoM velocity
//! for any body with `com_local ≠ 0`.
//!
//! The gyroscopic term `ω×(Iω)` is included so torque-free precession
//! is represented (it is one of §2.9's oracle-checkable scenarios).
//! `ω` is integrated in **body frame**, where `inertia_local` is
//! constant — matching `WeldedBody`'s storage convention.
//!
//! # Force model (§2.4/§2.9 — "thrust, pre-voxel analytic drag, gravity")
//!
//! - **Gravity**: `F = −μ·m·r̂/r²` toward [`Environment::body_center`],
//!   applied at the CoM (uniform-field approximation — no gravity-
//!   gradient torque in v1).
//! - **Pre-voxel analytic drag**: `F = −½·ρ·Cd·A_ref·|v_rel|·v_rel`,
//!   applied at the CoM (no aero moment — per-part application points
//!   and stability derivatives arrive with the §2.4.1 voxel tables at
//!   M4a). `ρ` is the §3.0 closed form `p(h) = p0·exp(−h/H)` combined
//!   with the ideal-gas law and the authored temperature table.
//!   `A_ref` is the cross-sectional area of the velocity-aligned AABB
//!   of the compound collider — a deterministic, over-estimating
//!   placeholder while no voxel table exists. `v_rel` is the velocity
//!   relative to the atmosphere co-rotating with `body_spin`.
//! - **Thrust**: each `ForceSiteKind::Thrust` site emits
//!   `throttle·rated_n` along `rot·local_dir`; `throttle` is the
//!   vessel-level control field on [`WeldedBody`], clamped to `[0,1]`
//!   here (per-engine `throttle_range` enforcement is the M3+ control
//!   layer's job). `Rcs`/`Aero` sites are inert until their control
//!   channels exist. Torque is `Σ r×F` about the world-frame CoM.
//!
//! # Contact model
//!
//! The environment's ground is the **contact surface
//! [`Environment::surface_radius`]`(dir)` — the sphere of `radius_m`
//! centred at `body_center` for `terrain: None`, and the displaced
//! seeded heightfield `radius_m + h(dir)·amplitude` (the exact
//! surface `csp-render` draws) when the environment carries a
//! [`TerrainSurface`] — `Environment::for_body` always attaches one,
//! so authored worlds contact the rendered ground (§2.3: at 600 km
//! the surface is locally flat to µm and the sphere keeps lat/long
//! meaningful). Generation: every collider vertex whose world
//! position falls below the surface (`‖p − c‖ < surface_radius(p̂)`)
//! produces one manifold point `{pos, normal = up, depth}` —
//! vertex-only contact per the M2 scope with a radial "up" normal;
//! face/edge contacts on a curved surface differ from the vertex
//! manifold by ≪ µm at home-planet radius.
//!
//! **Vessel-vessel contact** (M6 sub-phase A): bodies in one `step`
//! slice see each other through a symmetric vertex-vs-hull rule — a
//! collider vertex of `a` strictly inside a facet volume of `b` (and
//! vice versa) yields one manifold point with the penetrated facet's
//! outward normal. Facet planes are re-detected from each hull's
//! canonicalized vertex set per call by [`crate::hull::facet_planes`]
//! (a pure derived cache, never stored — the same `O(n⁴)` detector
//! the §2.4.1 voxelizer uses; `n` is a hull's vertex count, 8 for the
//! ubiquitous box), behind per-body and per-hull AABB broadphase.
//! Each pair contact is a **two-body constraint**: the impulse is
//! applied `+λ·n` to the penetrator and `−λ·n` to the penetrated body,
//! so linear momentum is conserved by construction and the velocity
//! solve reads the *relative* point velocity `v_a − v_b` — the moving-
//! surface rule generalized from the spinning-ground term (the peer
//! body's point velocity *is* the surface velocity). A latched body
//! participates as a static collider (`inv_m = inv_i = 0`, impulses
//! on it skipped): a resting vessel can be landed on — and pushed
//! against — without the latched body ever moving.
//!
//! Pair constraints live in their own list solved in fixed ascending
//! `(i, j, hull_i, hull_j, direction, vertex)` generation order —
//! the §2.9 `(body_id, contact_index)` reading applied pairwise; each
//! solver iteration runs the whole ground manifold first, then the
//! whole pair manifold, a fixed order like everything else here. When
//! `bodies` has one element (or no pair overlaps) the pair list is
//! empty and every ground path is **bit-identical** to the pre-M6
//! single-body behaviour.
//!
//! **Transient joints** (M6 sub-phase B, §2.4): each [`JointDef`] in
//! `step`'s `joints` argument becomes a bilateral three-row
//! [`JointConstraint`] solved in the same PGS velocity pass, after the
//! pair manifold, in the re-sorted `(body_a, body_b, anchors)`
//! generation order. Where a contact drives relative point velocity
//! to `≥ 0`, a joint row drives it to a **target**: for
//! [`JointKind::SoftCapture`], `−pull·separation` clamped to
//! `±max_correction` per axis — an exponential reel-in with a hard
//! closing-speed cap (the "soft" in soft-capture). Rows are unclamped
//! (a joint pulls as well as pushes) and there is deliberately **no
//! position-level row** — the velocity target is the whole correction
//! channel, which is what "compliant" means: a Baumgarte pass would
//! weld the pose while leaving velocity untouched, the opposite of a
//! soft joint. §2.9 assigns jointed islands to `RapierBackend`; the
//! documented v1 reading is that a compliant spherical joint is a
//! bilateral three-row constraint this solver already expresses —
//! see [`JointDef`]'s backend note.
//!
//! The model is deliberately **vertex-only**, matching the ground
//! manifold's scope: an edge-edge crossing with no vertex inside
//! either hull generates nothing that tick and is caught when a
//! vertex crosses. Face/edge contacts between hulls are therefore an
//! approximation of the same order as the M2 ground rule — docking
//! approaches, resting stacks, and side-by-side contact are
//! face-dominated configurations where it is exact to generation
//! precision.
//!
//! Generation requires `depth > 0` *strictly*: a vertex exactly
//! grazing the surface (`depth == 0`) produces no manifold point. A
//! zero-depth point has no penetration to correct and, under the
//! restitution-0 velocity target `vn ≥ 0`, zero impulse authority —
//! it would only spend solver iterations. A vertex falling through
//! the grazing position is caught the following tick having sunk at
//! most `v·dt`, which the Baumgarte pass then recovers.
//!
//! The contact surface **moves** when `body_spin ≠ 0`: the ground
//! under the body travels at `v_surf = body_spin × (p − body_center)`,
//! the same co-rotating frame the atmosphere's `v_rel` uses. The
//! velocity solve therefore targets the *relative* velocity
//! `v_point − v_surf` — a landed body brakes against the moving
//! surface and is dragged toward co-rotation, rather than toward
//! inertial rest on a spinning world. (The settle latch still reads
//! the stored inertial-frame `‖v‖`, so on a spinning world a
//! co-rotating body above `settle_v` legitimately never latches —
//! rotating-frame settle is M2-C World wiring.)
//!
//! # Solver (§2.9 spec, verbatim)
//!
//! **Projected Gauss–Seidel impulse solve**, [`SOLVER_ITERATIONS`] = 8
//! iterations in fixed ascending `(body_id, contact_index)` order,
//! restitution 0, Coulomb friction with a fixed-order two-direction
//! basis (`t1 = normalize(n × ref)`, `t2 = n × t1`, `ref` chosen
//! deterministically from the least-aligned world axis). Accumulated
//! impulses are clamped: `λ_n ≥ 0`, and the tangential pair is
//! projected onto the **Coulomb disc** `‖(λ_t1, λ_t2)‖ ≤ μ·λ_n` —
//! the two per-direction clamps alone would admit `√2·μ·λ_n` on
//! diagonal slides, over-braking by 41%.
//!
//! **Baumgarte `β = 0.2` is applied at the position level**
//! (pseudo-impulse position correction after the velocity solve),
//! *not* as velocity bias. Under the pinned phase order (positions are
//! already integrated when contacts solve), a velocity-space bias
//! leaves a residual `+g·dt` bias velocity in storage at the rest
//! fixed point — the body would hold `‖v‖ ≈ 0.15 m/s` forever and the
//! §2.6.4 settle latch (`settle_v = 0.05`) could never fire. The
//! position-level reading of "Baumgarte position stabilization" is
//! what the spec's own terminal state requires.
//!
//! The position correction is applied **about the CoM**, same frame
//! discipline as the integrator: the linear pseudo-impulse advances
//! `x̄`, the angular one rotates `q`, and `pos` is re-derived. Rotating
//! `q` about the origin would sweep `q·com_local` tangentially —
//! injecting lateral slide (or even deepening penetration) for
//! `com_local ≠ 0` bodies.
//!
//! The total CoM translation the position pass may apply to one body
//! in one tick is capped at [`MAX_TICK_POS_CORRECTION`] — without a
//! cap a deeply spawned body (or pathological geometry) could teleport
//! `0.83·depth` metres in a single tick. The cap is ~70× the rest
//! equilibrium's per-tick correction (`g·dt² ≈ 2.4 mm`), so resting
//! behaviour is untouched; deep penetrations resolve over a handful
//! of ticks instead of one.
//!
//! No warm starting: each tick solves from `λ = 0`, which keeps the
//! solver a pure function of the tick's inputs — no extra serialized
//! solver state.
//!
//! # Settle latch (§2.6.4)
//!
//! After the solve, a body in contact with `‖v‖ < settle_v`,
//! `‖ω‖ < settle_w`, **and per-tick pose deltas below the same
//! limits** (`‖Δpos‖ < settle_v·dt`, `‖Δrot‖ < settle_w·dt` in
//! radians — i.e. the pose itself is at rest to the same tolerance
//! the velocities certify) increments [`SettleState::settled_ticks`];
//! any failing tick resets it to zero. The pose-delta clause is not
//! redundant: the Baumgarte pass moves `pos`/`rot` without touching
//! `vel`/`ang_vel`, so a velocity-only gate could latch a body the
//! position pass was still dragging (offset-CoM corrections, emerging
//! penetration). `‖Δrot‖` is measured as `‖q₁ − q₀‖` — for the small
//! per-tick angles at issue that equals the rotation angle to second
//! order (`2·sin(θ/2) ≈ θ`). At `settle_ticks` (default 128 =
//! 2 s) the body is marked `latched`, its velocities and throttle are
//! zeroed, and its contacts are released — the backend holds no
//! integrating state for it. A `throttle > 0` unlatches (§2.6.4's "any throttle command
//! unlatches it first"); the other unlatch triggers (`SimCommand::
//! Unlatch`, activation, nearby impact) are M2-C World wiring.

use std::cell::{Cell, OnceCell};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use csp_core::terrain::{AMPLITUDE_FRAC, BAKE_HEIGHT, BAKE_WIDTH, HeightField};
use csp_core::{DQuat, DVec3, SimTime, TICK_HZ};
use csp_data::{AtmosphereDef, BodyDef};

use crate::backend::{BodyHandle, Contact, Hit, JointDef, JointKind, PhysicsBackend, Ray};
use crate::part::{ForceSiteKind, WeldPart};
use crate::tensor::SymMat3;
use crate::weld::WeldedBody;

/// The immutable physics timestep — `1/64` s exactly (§2.3/§2.6; a
/// power of two, so this constant is exact in `f64`).
pub const PHYSICS_DT: f64 = 1.0 / TICK_HZ as f64;

/// §2.9's pinned iteration count for the projected Gauss–Seidel
/// impulse solve. The Baumgarte position pass reuses the same fixed
/// count — fixed iteration counts are a determinism rule, and a second
/// constant would be unprincipled tuning.
pub const SOLVER_ITERATIONS: usize = 8;

/// §2.9's Baumgarte factor — fraction of remaining penetration removed
/// per position-solve iteration.
pub const BAUMGARTE_BETA: f64 = 0.2;

/// Per-body, per-tick cap on the total CoM translation the Baumgarte
/// position pass may apply, metres. Uncapped, `β = 0.2` over 8
/// iterations removes `1 − (1−β)⁸ ≈ 83%` of any depth in a single
/// tick — a 20 m teleport for a badly spawned body. The cap bounds
/// worst-case correction speed to `0.2·64 = 12.8 m/s` while leaving
/// the rest equilibrium (`g·dt² ≈ 2.4 mm` per tick) untouched.
pub const MAX_TICK_POS_CORRECTION: f64 = 0.2;

/// §2.6.4's named default: `‖v‖ < settle_v` with `settle_v = 0.05` m/s.
pub const DEFAULT_SETTLE_V: f64 = 0.05;
/// §2.6.4 names `settle_w` but gives no number; `0.05` rad/s matches
/// the linear default's order (a vessel rotating slower than ~3°/s).
/// Balance-data promotion can tune it; it is an [`Environment`] field,
/// not a hard-coded constant, for exactly that reason.
pub const DEFAULT_SETTLE_W: f64 = 0.05;
/// §2.6.4's named default: `settle_ticks = 128` = 2 s at 64 Hz.
pub const DEFAULT_SETTLE_TICKS: u32 = 128;
/// Coulomb friction coefficient for ground contact — a v1 constant
/// (rock/regolith order of magnitude). §2.9 pins the friction *model*
/// (two-direction Coulomb cone) but not the coefficient; per-material
/// or per-biome μ is a later-milestone data concern.
pub const DEFAULT_FRICTION_MU: f64 = 0.5;
/// Pre-voxel drag coefficient — flat-plate order of magnitude. The
/// §2.4.1 Mach/Reynolds Cd/Cl curves replace it at M4a.
pub const DEFAULT_DRAG_CD: f64 = 1.0;

/// Universal gas constant, J/(mol·K) — CODATA exact value; §3.0's
/// ideal-gas density `ρ = pM/(RT)` needs it.
const R_GAS: f64 = 8.314_462_618_153_24;

/// The seeded heightfield contact surface — the SIM half of the R2
/// terrain. `csp-render` displaces the drawn surface to
/// `radius + h(dir)·amplitude` where `h` is the baked equirect field
/// from `csp_core::terrain`; this type samples the *same* bake so the
/// ground the solver contacts is bit-identical to the ground the
/// renderer draws. (The pre-terrain contact surface was the bare
/// datum sphere — the "sank below the visible ground before the sim
/// noticed" defect this type exists to close.)
///
/// The baked field is a **pure derived cache** of `seed`: it is
/// skipped by serde (a save/replay carries the seed, not the ~2 MB of
/// texels) and lazily re-baked on first sample — bit-identical every
/// time, per `HeightField`'s determinism contract. The `OnceCell`
/// keeps the `&self` predicate paths (`ground_contact`, `cast_ray`)
/// honest: evaluation warms the cache without mutating state.
#[derive(Clone, Serialize, Deserialize)]
pub struct TerrainSurface {
    /// `TerrainDef::noise_seed` — the height function's identity.
    pub seed: u64,
    /// Height amplitude, metres — `radius_m · AMPLITUDE_FRAC`, the
    /// same scale `csp-view`'s `TerrainBody::amplitude_m` computes.
    pub amplitude_m: f64,
    /// The baked equirect field — derived cache of `seed` (see the
    /// type doc); `Arc`-shared so an `Environment` clone stays cheap.
    #[serde(skip)]
    field: OnceCell<Arc<HeightField>>,
}

impl TerrainSurface {
    /// A terrain surface for `seed` — the field bakes lazily on first
    /// sample (or on [`Self::prewarm`]).
    pub fn new(seed: u64, amplitude_m: f64) -> Self {
        Self {
            seed,
            amplitude_m,
            field: OnceCell::new(),
        }
    }

    /// A surface over an already-baked field — the test seam: a small
    /// bake keeps the contact-surface tests fast while exercising the
    /// identical sample path the full-size field drives.
    #[cfg(test)]
    pub(crate) fn with_field(field: HeightField, seed: u64, amplitude_m: f64) -> Self {
        let s = Self::new(seed, amplitude_m);
        // New cell — `set` cannot fail on an empty OnceCell.
        let _ = s.field.set(Arc::new(field));
        s
    }

    /// Bake the field now — a construction-time warm-up so the first
    /// contact tick doesn't eat the ~0.5 M-texel bake mid-frame. Pure
    /// function of `seed`; safe to call repeatedly.
    pub fn prewarm(&self) {
        let _ = self.field();
    }

    /// The baked field — `get_or_init` behind `OnceCell`, the lazy
    /// half of [`Self::prewarm`].
    fn field(&self) -> &Arc<HeightField> {
        self.field
            .get_or_init(|| Arc::new(HeightField::bake(self.seed, BAKE_WIDTH, BAKE_HEIGHT)))
    }

    /// Terrain height above the datum radius at `dir_fixed` — a
    /// BODY-FIXED unit direction (the inertial→fixed conjugate is the
    /// caller's; see [`Environment::surface_radius`]). Same value the
    /// shader's `height_at` computes at the same texel coordinates,
    /// scaled to metres.
    pub fn height_m(&self, dir_fixed: DVec3) -> f64 {
        self.field().sample_dir(dir_fixed) * self.amplitude_m
    }
}

impl PartialEq for TerrainSurface {
    /// Identity is the seeded descriptor — the baked field is a
    /// derived cache and never participates.
    fn eq(&self, other: &Self) -> bool {
        self.seed == other.seed && self.amplitude_m == other.amplitude_m
    }
}

impl core::fmt::Debug for TerrainSurface {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TerrainSurface")
            .field("seed", &self.seed)
            .field("amplitude_m", &self.amplitude_m)
            .field("field_baked", &self.field.get().is_some())
            .finish()
    }
}

/// The world a `SimpleBackend` steps inside: the dominant gravitating
/// body (its sphere is the altimetry datum and, via [`Self::terrain`],
/// carries the displaced contact surface), the §3.0 atmosphere, and
/// the solver/latch tunables.
///
/// Everything is expressed in the **bubble frame** — `body_center` is
/// where the planet centre sits relative to the bubble origin (§2.3's
/// recentered local frame; at f64 a 600 km offset carries sub-nm
/// precision, so the sphere contact needs no local trickery).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    /// Centre of the dominant body, bubble frame, metres.
    pub body_center: DVec3,
    /// Surface radius (equatorial; axial tilt is 0 across the v1 home
    /// system, §3.0), metres — the altimetry datum and, for
    /// `terrain: None`, the contact sphere (with terrain the datum
    /// stays `radius_m` and the contact surface is displaced — see
    /// [`Self::surface_radius`]).
    pub radius_m: f64,
    /// Gravitational parameter μ, m³/s².
    pub mu_m3_per_s2: f64,
    /// The body's angular velocity vector, bubble frame, rad/s — used
    /// for the co-rotating atmosphere's `v_rel` and for the ground
    /// contact surface velocity `body_spin × (p − body_center)` the
    /// contact solve brakes against. `DVec3::ZERO` for a non-rotating
    /// frame (then the surface is inertially static).
    pub body_spin: DVec3,
    /// §3.0 atmosphere; `None` = vacuum body.
    pub atmosphere: Option<AtmosphereDef>,
    /// `‖v‖` settle threshold (§2.6.4 default 0.05 m/s).
    pub settle_v: f64,
    /// `‖ω‖` settle threshold (see [`DEFAULT_SETTLE_W`]).
    pub settle_w: f64,
    /// Consecutive settled ticks required to latch (§2.6.4: 128 = 2 s).
    pub settle_ticks: u32,
    /// Coulomb μ for contact friction — authored as the *ground*
    /// coefficient (see [`DEFAULT_FRICTION_MU`]) and reused verbatim
    /// for the vessel-vessel pair manifold (see `solve_velocity`):
    /// a deliberate single-coefficient approximation — there is no
    /// per-material pair table in v1.
    pub friction_mu: f64,
    /// Pre-voxel drag coefficient (see [`DEFAULT_DRAG_CD`]).
    pub drag_cd: f64,
    /// The seeded heightfield contact surface (see [`TerrainSurface`]):
    /// `Some` ⇒ ground contact evaluates against the displaced surface
    /// `radius_m + h(dir_fixed)` the renderer draws; `None` ⇒ the bare
    /// datum sphere (test environments, pre-terrain construction
    /// configs). `serde(default)` keeps pre-terrain save/replay
    /// artifacts decodable — they replay on the sphere they recorded.
    #[serde(default)]
    pub terrain: Option<TerrainSurface>,
}

impl Environment {
    /// Build an environment from an authored `BodyDef`. `center` is the
    /// body's position in the bubble frame; `spin_period_s` is the
    /// sidereal rotation period the *caller* resolved (`None` →
    /// non-rotating; `BodyDef::rotation_period_s == None` means tidally
    /// locked, whose period is the orbital period — resolving that is
    /// the orbit layer's business, M2-C). Tilt is honoured in the spin
    /// axis even though the v1 system is tilt-free.
    pub fn for_body(body: &BodyDef, center: DVec3, spin_period_s: Option<f64>) -> Self {
        let spin = match spin_period_s {
            Some(period) if period > 0.0 => {
                let axis = DVec3::new(0.0, -libm::sin(body.tilt_rad), libm::cos(body.tilt_rad));
                axis * (2.0 * core::f64::consts::PI / period)
            }
            _ => DVec3::ZERO,
        };
        Self {
            body_center: center,
            radius_m: body.radius_m,
            mu_m3_per_s2: body.mu_m3_per_s2,
            body_spin: spin,
            atmosphere: body.atmosphere.clone(),
            settle_v: DEFAULT_SETTLE_V,
            settle_w: DEFAULT_SETTLE_W,
            settle_ticks: DEFAULT_SETTLE_TICKS,
            friction_mu: DEFAULT_FRICTION_MU,
            drag_cd: DEFAULT_DRAG_CD,
            // The seeded surface the renderer draws — `noise_seed` is
            // the authored `TerrainDef` identity and the amplitude the
            // shared `AMPLITUDE_FRAC · radius` rule `csp-view`'s
            // `TerrainBody` computes; sim and render displace to the
            // same surface by construction.
            terrain: Some(TerrainSurface::new(
                body.terrain.noise_seed,
                body.radius_m * AMPLITUDE_FRAC,
            )),
        }
    }

    /// The contact-surface radius along `dir` at sim-second `seconds`
    /// — `dir` a UNIT body-center-relative direction in the inertial
    /// bubble frame. `radius_m` for a bare-sphere environment
    /// (`terrain: None`); with terrain, `radius_m + h·amplitude` where
    /// `h` is the baked field's bilinear sample.
    ///
    /// The heightfield is **body-fixed**: the inertial sample
    /// direction conjugates by `R(axis, −ω·seconds)` — the same
    /// `unspin` the vertex shader applies in `vs_terrain`, and the
    /// same phase convention `bubble.rs`'s surface records use —
    /// so sim, renderer, and surface rails name the same surface. On
    /// a non-rotating body (`body_spin == 0`) the conjugate is
    /// identity and `seconds` is inert.
    pub fn surface_radius(&self, dir: DVec3, seconds: f64) -> f64 {
        let Some(t) = &self.terrain else {
            return self.radius_m;
        };
        let omega = self.body_spin.norm();
        let dir_fixed = if omega > 0.0 {
            DQuat::from_axis_angle(self.body_spin / omega, -omega * seconds).rotate(dir)
        } else {
            dir
        };
        self.radius_m + t.height_m(dir_fixed)
    }
}

/// §3.0's closed form: `p(h) = p0·exp(−h/H)` up to `top_m`, then a
/// documented hard cut to zero. Below the datum (`h < 0`) the pressure
/// is clamped to `p0` — the model has no below-surface branch.
pub fn atmosphere_pressure_pa(atm: &AtmosphereDef, altitude_m: f64) -> f64 {
    if altitude_m >= atm.top_m {
        return 0.0;
    }
    let h = altitude_m.max(0.0);
    atm.sea_level_pressure_pa * libm::exp(-h / atm.scale_height_m)
}

/// §3.0 temperature: piecewise-linear over the authored knots, clamped
/// to the table ends outside its range.
pub fn atmosphere_temperature_k(atm: &AtmosphereDef, altitude_m: f64) -> f64 {
    let knots = &atm.temperature_knots;
    // The validator guarantees ≥2 strictly-increasing knots (§3.0); a
    // degenerate table yields the first temperature rather than NaN.
    let Some(first) = knots.first() else {
        return 0.0;
    };
    if altitude_m <= first.altitude_m {
        return first.temperature_k;
    }
    for w in knots.windows(2) {
        let (a, b) = (w[0], w[1]);
        if altitude_m <= b.altitude_m {
            // Guard the interpolation denominator: the validator
            // promises strictly-increasing knots, but a degenerate or
            // repeated pair must yield an endpoint, never NaN.
            let span = b.altitude_m - a.altitude_m;
            if span <= 0.0 {
                return a.temperature_k;
            }
            let t = (altitude_m - a.altitude_m) / span;
            return a.temperature_k + t * (b.temperature_k - a.temperature_k);
        }
    }
    // Above the last knot — `windows` leaves `last` unreachable only if
    // the table had <2 knots, covered by the same fallback.
    knots
        .last()
        .map_or(first.temperature_k, |k| k.temperature_k)
}

/// §3.0 density from the ideal gas law: `ρ = p·M/(R·T)`. Zero above
/// `top_m` (pressure is zero) or when the temperature table is
/// degenerate.
pub fn atmosphere_density_kg_m3(atm: &AtmosphereDef, altitude_m: f64) -> f64 {
    let p = atmosphere_pressure_pa(atm, altitude_m);
    let t = atmosphere_temperature_k(atm, altitude_m);
    if p <= 0.0 || t <= 0.0 {
        return 0.0;
    }
    p * atm.molar_mass_kg_per_mol / (R_GAS * t)
}

/// §3.0 speed of sound `a = √(γRT/M)` — the Mach denominator the
/// voxel-table force model needs. `0.0` for a degenerate temperature.
pub fn atmosphere_sound_speed_mps(atm: &AtmosphereDef, altitude_m: f64) -> f64 {
    let t = atmosphere_temperature_k(atm, altitude_m);
    if t <= 0.0 {
        return 0.0;
    }
    libm::sqrt(atm.heat_capacity_ratio * R_GAS * t / atm.molar_mass_kg_per_mol)
}

/// One contact constraint in the solver's fixed working set — the
/// public [`Contact`] record plus the solver bookkeeping (effective
/// masses, accumulated impulses, lever arm). `contact_index` is the
/// constraint's position within its body's generation sequence, which
/// *is* the ascending order §2.9 pins.
struct Constraint {
    /// Index into the `bodies` slice (the `body_id` of the ordering).
    body: u32,
    /// Index into `self.contacts[body]` of this constraint's public
    /// record — the impulse writeback's channel.
    contact_index: u32,
    /// The penetrating vertex, body frame (a material point — its
    /// world position is recomputed as the pose evolves).
    local_vertex: DVec3,
    /// Lever arm from the body CoM to the contact point, **body
    /// frame**, at generation pose (`vertex − com_local`).
    r_body: DVec3,
    /// Contact normal and the fixed two-direction friction basis, body
    /// frame.
    n_body: DVec3,
    t1_body: DVec3,
    t2_body: DVec3,
    /// Effective masses along `n`, `t1`, `t2` — `1/K`, where
    /// `K = m⁻¹ + n·[(I⁻¹(r×n))×r]` is the constraint's *inverse*
    /// effective mass along that axis. PGS applies `Δλ = −v_rel/K`,
    /// so the stored reciprocal is what multiplies the velocity error.
    kn: f64,
    kt1: f64,
    kt2: f64,
    /// Velocity of the ground surface at the generation point, body
    /// frame: `body_spin × (pos − body_center)` for a rotating world
    /// (`ZERO` when `body_spin` is zero). The velocity solve drives
    /// the *relative* velocity `v_point − v_surf` to zero, so landed
    /// bodies brake against the moving surface (see module docs).
    surface_vel_body: DVec3,
    /// The body's inverse inertia, body frame (`ZERO` for a singular
    /// tensor — no angular response).
    inv_i: SymMat3,
    /// Accumulated impulses this tick (Gauss–Seidel, cold-started).
    lambda_n: f64,
    lambda_t1: f64,
    lambda_t2: f64,
}

impl Constraint {
    /// Build the constraint for one penetrating vertex of `body`.
    #[allow(clippy::too_many_arguments)]
    fn new(
        body_index: usize,
        contact_index: usize,
        hull_index: u32,
        vertex_body: DVec3,
        pos_world: DVec3,
        normal_world: DVec3,
        depth: f64,
        body: &WeldedBody,
        env: &Environment,
    ) -> (Self, Contact) {
        let conj = body.rot.conjugate();
        let r_body = vertex_body - body.com_local;
        let n_body = conj.rotate(normal_world);
        // Fixed-order two-direction friction basis — shared with the
        // pair constraints so both manifolds pick tangents identically.
        let (t1_world, t2_world) = friction_basis(normal_world);
        let t1_body = conj.rotate(t1_world);
        let t2_body = conj.rotate(t2_world);
        let inv_i = body.inertia_local.inverse().unwrap_or(SymMat3::ZERO);
        let inv_m = 1.0 / body.mass_kg;
        let effective = |dir_b: DVec3| {
            let k = inv_m + inv_i.mul_vec(r_body.cross(dir_b)).cross(r_body).dot(dir_b);
            if k > 0.0 { 1.0 / k } else { 0.0 }
        };
        let constraint = Self {
            body: body_index as u32,
            contact_index: contact_index as u32,
            local_vertex: vertex_body,
            r_body,
            n_body,
            t1_body,
            t2_body,
            kn: effective(n_body),
            kt1: effective(t1_body),
            kt2: effective(t2_body),
            surface_vel_body: conj.rotate(env.body_spin.cross(pos_world - env.body_center)),
            inv_i,
            lambda_n: 0.0,
            lambda_t1: 0.0,
            lambda_t2: 0.0,
        };
        let contact = Contact {
            pos: pos_world,
            normal: normal_world,
            depth,
            hull: hull_index,
            impulse: DVec3::ZERO,
            peer: None,
        };
        (constraint, contact)
    }
}

/// Fixed-order two-direction friction basis, deterministic from the
/// world-frame normal: reference axis = least-aligned of X/Y, then
/// `t1 = normalize(n × ref)`, `t2 = n × t1` (§2.9's pinned basis).
fn friction_basis(n_world: DVec3) -> (DVec3, DVec3) {
    let reference = if n_world.x.abs() <= n_world.y.abs() {
        DVec3::X
    } else {
        DVec3::Y
    };
    let t1_raw = n_world.cross(reference);
    let t1 = t1_raw / t1_raw.norm();
    (t1, n_world.cross(t1))
}

/// Apply impulse `dλ·dir_b` to `b` (dir in body frame): updates `vel`
/// (world), `vel_b` (its cached body-frame twin — a pure vector add,
/// no quaternion rotate), `ang_vel` (body), and `omega_w` (the solve's
/// cached world-frame `rot·ang_vel` — kept in sync because the pair
/// constraints evaluate relative point velocities in world frame).
/// The Gauss–Seidel write-back; `rot` does not change inside the
/// velocity pass so every cached frame stays exact.
fn apply_impulse(
    b: &mut WeldedBody,
    vel_b: &mut DVec3,
    omega_w: &mut DVec3,
    c: &Constraint,
    dir_body: DVec3,
    d_lambda: f64,
) {
    let j_body = dir_body * d_lambda;
    *vel_b = *vel_b + j_body / b.mass_kg;
    b.vel = b.vel + b.rot.rotate(j_body) / b.mass_kg;
    let dw_body = c.inv_i.mul_vec(c.r_body.cross(j_body));
    b.ang_vel = b.ang_vel + dw_body;
    *omega_w = *omega_w + b.rot.rotate(dw_body);
}

/// Point velocity at the contact *relative to the ground surface*,
/// body frame: `v_rel = v_body + ω × r − v_surf`. `v_body` is the
/// caller's cached body-frame linear velocity (the solve caches it
/// per body rather than quaternion-rotating `vel` on every
/// evaluation); `v_surf` is `ZERO` on a non-rotating world.
fn rel_point_velocity_body(vel_b: DVec3, b: &WeldedBody, c: &Constraint) -> DVec3 {
    vel_b + b.ang_vel.cross(c.r_body) - c.surface_vel_body
}

/// One vessel-vessel contact constraint — a vertex of `a`'s compound
/// collider strictly inside `b`'s hull, solved as a **two-body** PGS
/// constraint in the **world frame** (the bodies' frames differ, so
/// unlike the ground manifold no single body frame hosts both lever
/// arms). `n` points penetrated → penetrator: `a` receives `+λ·n`,
/// `b` receives `−λ·n` — equal and opposite, so the constraint
/// conserves linear momentum by construction and solves the relative
/// point velocity `v_a − v_b` (the peer body's velocity is the
/// contact's "surface velocity", generalizing the spinning-ground
/// `v_surf` term).
///
/// Generation order is fixed ascending `(i, j, hull_i, hull_j,
/// direction, vertex)` — the §2.9 `(body_id, contact_index)` reading
/// applied pairwise; the velocity and position passes iterate the
/// pair list after the ground manifold each iteration.
struct PairConstraint {
    /// Penetrator — index into the `bodies` slice of the body whose
    /// collider vertex is inside; it receives `+λ·n`.
    a: u32,
    /// Penetrated — the body owning the facet; receives `−λ·n`.
    b: u32,
    /// Index into `self.contacts[a]` of `a`'s public record — the
    /// impulse write-back's channel.
    contact_index_a: u32,
    /// Same for `b`'s record (it records the `−n`/`−J` view).
    contact_index_b: u32,
    /// The penetrating vertex, `a`'s body frame — a material point;
    /// the position pass re-poses it as `a` moves.
    local_vertex: DVec3,
    /// The penetrated facet's plane, `b`'s body frame — the persistent
    /// feature the position pass re-measures depth against each
    /// iteration (the face chosen at generation stays the feature,
    /// the standard persistent-manifold reading).
    n_face_b: DVec3,
    d_face_b: f64,
    /// Contact normal (the facet's outward normal, penetrated →
    /// penetrator) and the fixed two-direction friction basis —
    /// world frame, the pair solve's shared frame.
    n: DVec3,
    t1: DVec3,
    t2: DVec3,
    /// Lever arms from each body's CoM to the contact point, world
    /// frame, at generation pose — rotation is frozen across the
    /// velocity solve, so they stay exact through it; the position
    /// pass recomputes them per iteration like the ground pass does.
    r_a: DVec3,
    r_b: DVec3,
    /// Two-body effective masses along `n`/`t1`/`t2` — `1/K` with
    /// `K = inv_m_a + inv_m_b +` both angular terms (a latched side
    /// contributes `inv_m = 0`, `inv_i = ZERO` — a static collider).
    kn: f64,
    kt1: f64,
    kt2: f64,
    /// Each body's inverse inertia, its own body frame.
    inv_i_a: SymMat3,
    inv_i_b: SymMat3,
    /// `settle.latched` at generation: a latched body is static for
    /// this tick — impulses on it are skipped and the position pass
    /// never moves it (its side's share of the constraint still
    /// solves correctly: `inv_m = inv_i = 0` makes it immovable).
    a_latched: bool,
    b_latched: bool,
    /// Accumulated impulses this tick (Gauss–Seidel, cold-started).
    lambda_n: f64,
    lambda_t1: f64,
    lambda_t2: f64,
}

impl PairConstraint {
    /// Build the pair constraint + the two public [`Contact`] records
    /// (the penetrator's `+n` view and the penetrated body's `−n`
    /// view — equal and opposite, like the impulses they will carry).
    #[allow(clippy::too_many_arguments)]
    fn new(
        a_index: usize,
        b_index: usize,
        contact_index_a: usize,
        contact_index_b: usize,
        vertex_a: DVec3,
        face_normal_b: DVec3,
        face_d_b: f64,
        pos_world: DVec3,
        normal_world: DVec3,
        depth: f64,
        a: &WeldedBody,
        b: &WeldedBody,
        hull_a: u32,
        hull_b: u32,
    ) -> (Self, Contact, Contact) {
        let (t1, t2) = friction_basis(normal_world);
        let r_a = pos_world - a.com_world();
        let r_b = pos_world - b.com_world();
        let a_latched = a.settle.latched;
        let b_latched = b.settle.latched;
        let inv_m_a = if a_latched { 0.0 } else { 1.0 / a.mass_kg };
        let inv_m_b = if b_latched { 0.0 } else { 1.0 / b.mass_kg };
        let inv_i_a = if a_latched {
            SymMat3::ZERO
        } else {
            a.inertia_local.inverse().unwrap_or(SymMat3::ZERO)
        };
        let inv_i_b = if b_latched {
            SymMat3::ZERO
        } else {
            b.inertia_local.inverse().unwrap_or(SymMat3::ZERO)
        };
        // Angular term of the inverse effective mass along `dir`,
        // world frame: `dir · (I_w⁻¹(r×dir) × r)` with
        // `I_w⁻¹ x = rot·(inv_i·(rot̄·x))` — the same `K` the ground
        // constraint computes in body frame, evaluated where the pair
        // shares a frame.
        let ang = |rot: DQuat, inv_i: SymMat3, r: DVec3, dir: DVec3| {
            let rd = r.cross(dir);
            rot.rotate(inv_i.mul_vec(rot.conjugate().rotate(rd)))
                .cross(r)
                .dot(dir)
        };
        let effective = |dir: DVec3| {
            let k =
                inv_m_a + inv_m_b + ang(a.rot, inv_i_a, r_a, dir) + ang(b.rot, inv_i_b, r_b, dir);
            if k > 0.0 { 1.0 / k } else { 0.0 }
        };
        let constraint = Self {
            a: a_index as u32,
            b: b_index as u32,
            contact_index_a: contact_index_a as u32,
            contact_index_b: contact_index_b as u32,
            local_vertex: vertex_a,
            n_face_b: face_normal_b,
            d_face_b: face_d_b,
            n: normal_world,
            t1,
            t2,
            r_a,
            r_b,
            kn: effective(normal_world),
            kt1: effective(t1),
            kt2: effective(t2),
            inv_i_a,
            inv_i_b,
            a_latched,
            b_latched,
            lambda_n: 0.0,
            lambda_t1: 0.0,
            lambda_t2: 0.0,
        };
        let record_a = Contact {
            pos: pos_world,
            normal: normal_world,
            depth,
            hull: hull_a,
            impulse: DVec3::ZERO,
            peer: Some(BodyHandle(b_index as u32)),
        };
        let record_b = Contact {
            pos: pos_world,
            normal: -normal_world,
            depth,
            hull: hull_b,
            impulse: DVec3::ZERO,
            peer: Some(BodyHandle(a_index as u32)),
        };
        (constraint, record_a, record_b)
    }
}

/// The world-frame impulse `j` of a two-body constraint (contact pair
/// or joint): `+j` on `a`, `−j` on `b` — the equal-and-opposite
/// write-back that conserves linear momentum by construction. Updates
/// `vel` (world) directly, plus the solve's `vel_body`/`omega_world`
/// caches (the ground constraints read the former; the pair/joint
/// constraints the latter — both must reflect every write, latched
/// bodies excepted).
#[allow(clippy::too_many_arguments)]
fn apply_two_body_impulse(
    bodies: &mut [WeldedBody],
    vel_body: &mut [DVec3],
    omega_world: &mut [DVec3],
    ia: usize,
    ib: usize,
    r_a: DVec3,
    r_b: DVec3,
    inv_i_a: SymMat3,
    inv_i_b: SymMat3,
    a_latched: bool,
    b_latched: bool,
    j: DVec3,
) {
    if !a_latched {
        let b = &mut bodies[ia];
        b.vel = b.vel + j / b.mass_kg;
        vel_body[ia] = vel_body[ia] + b.rot.conjugate().rotate(j) / b.mass_kg;
        let dw = inv_i_a.mul_vec(b.rot.conjugate().rotate(r_a.cross(j)));
        b.ang_vel = b.ang_vel + dw;
        omega_world[ia] = omega_world[ia] + b.rot.rotate(dw);
    }
    if !b_latched {
        let b = &mut bodies[ib];
        let j = -j;
        b.vel = b.vel + j / b.mass_kg;
        vel_body[ib] = vel_body[ib] + b.rot.conjugate().rotate(j) / b.mass_kg;
        let dw = inv_i_b.mul_vec(b.rot.conjugate().rotate(r_b.cross(j)));
        b.ang_vel = b.ang_vel + dw;
        omega_world[ib] = omega_world[ib] + b.rot.rotate(dw);
    }
}

/// The world-frame impulse `j` of a [`PairConstraint`]: `+j` on `a`,
/// `−j` on `b` — the equal-and-opposite two-body write-back. Updates
/// `vel` (world) directly, plus the solve's `vel_body`/`omega_world`
/// caches (the ground constraints read the former; the pair
/// constraints the latter — both must reflect every write, latched
/// bodies excepted).
fn apply_pair_impulse(
    bodies: &mut [WeldedBody],
    vel_body: &mut [DVec3],
    omega_world: &mut [DVec3],
    c: &PairConstraint,
    j: DVec3,
) {
    apply_two_body_impulse(
        bodies,
        vel_body,
        omega_world,
        c.a as usize,
        c.b as usize,
        c.r_a,
        c.r_b,
        c.inv_i_a,
        c.inv_i_b,
        c.a_latched,
        c.b_latched,
        j,
    );
}

/// One §2.4 transient joint constraint — a **bilateral** three-row PGS
/// constraint in the fixed canonical basis (X, Y, Z), world frame like
/// [`PairConstraint`]. It drives the anchors' relative point velocity
/// `v_a − v_b` to a per-axis **target** rather than to zero: the target
/// is the compliance. For [`JointKind::SoftCapture`] the target is
/// `−pull·(p_a − p_b)` component-clamped to `±max_correction` — an
/// exponential reel-in with a hard closing-speed cap, so a capture
/// damps approach energy instead of welding the pose (and coincident
/// anchors get a zero target, which is a hard hold).
///
/// Unlike contacts the rows are unclamped — a joint pulls as well as
/// pushes — and there is **no position-level row**: the velocity target
/// itself does the closing (a Baumgarte row would be a second,
/// redundant correction channel and would move the pose without moving
/// stored velocity, which is exactly wrong for a compliant joint).
/// Cold-started per tick like the rest of the solve — no warm-start
/// state, so the joint carries nothing to serialize.
struct JointConstraint {
    /// Index into `bodies` of the joint's first/second body.
    a: u32,
    b: u32,
    /// Lever arms from each body's CoM to its anchor, world frame, at
    /// generation pose — frozen across the velocity solve (`rot` does
    /// not change inside it), same discipline as `PairConstraint`.
    r_a: DVec3,
    r_b: DVec3,
    /// Per-axis two-body inverse effective masses `1/K` along X/Y/Z —
    /// the pair solve's `K` applied to the canonical basis; a latched
    /// side contributes `inv_m = 0`, `inv_i = ZERO`.
    k: [f64; 3],
    /// Each body's inverse inertia, its own body frame.
    inv_i_a: SymMat3,
    inv_i_b: SymMat3,
    /// `settle.latched` at generation — a latched body is static for
    /// the tick (its side of the constraint still solves: it is the
    /// immovable anchor the free side reels toward).
    a_latched: bool,
    b_latched: bool,
    /// Target relative point velocity `v_a − v_b` per axis.
    target: DVec3,
}

impl JointConstraint {
    /// Build the constraint for one [`JointDef`] at the
    /// post-integration pose. Returns `None` when both bodies are
    /// latched — a joint between two static bodies has nothing to do.
    fn new(
        joint: &JointDef,
        a_index: usize,
        b_index: usize,
        bodies: &[WeldedBody],
    ) -> Option<Self> {
        let a = &bodies[a_index];
        let b = &bodies[b_index];
        let a_latched = a.settle.latched;
        let b_latched = b.settle.latched;
        if a_latched && b_latched {
            return None;
        }
        let p_a = a.pos + a.rot.rotate(joint.local_anchor_a);
        let p_b = b.pos + b.rot.rotate(joint.local_anchor_b);
        let r_a = p_a - a.com_world();
        let r_b = p_b - b.com_world();
        let inv_m_a = if a_latched { 0.0 } else { 1.0 / a.mass_kg };
        let inv_m_b = if b_latched { 0.0 } else { 1.0 / b.mass_kg };
        let inv_i_a = if a_latched {
            SymMat3::ZERO
        } else {
            a.inertia_local.inverse().unwrap_or(SymMat3::ZERO)
        };
        let inv_i_b = if b_latched {
            SymMat3::ZERO
        } else {
            b.inertia_local.inverse().unwrap_or(SymMat3::ZERO)
        };
        // Angular term of the inverse effective mass along `dir`,
        // world frame — identical to `PairConstraint::new`'s `ang`.
        let ang = |rot: DQuat, inv_i: SymMat3, r: DVec3, dir: DVec3| {
            let rd = r.cross(dir);
            rot.rotate(inv_i.mul_vec(rot.conjugate().rotate(rd)))
                .cross(r)
                .dot(dir)
        };
        let effective = |dir: DVec3| {
            let k =
                inv_m_a + inv_m_b + ang(a.rot, inv_i_a, r_a, dir) + ang(b.rot, inv_i_b, r_b, dir);
            if k > 0.0 { 1.0 / k } else { 0.0 }
        };
        let c = p_a - p_b;
        let target = match joint.kind {
            // `v_a − v_b` target per axis: `−pull·C` reels the
            // separation down exponentially; the component clamp is
            // the "soft" — closing speed can never exceed
            // `max_correction` no matter how far the anchors sit.
            JointKind::SoftCapture {
                pull_per_s,
                max_correction_mps,
            } => DVec3::new(
                (-pull_per_s * c.x).clamp(-max_correction_mps, max_correction_mps),
                (-pull_per_s * c.y).clamp(-max_correction_mps, max_correction_mps),
                (-pull_per_s * c.z).clamp(-max_correction_mps, max_correction_mps),
            ),
        };
        Some(Self {
            a: a_index as u32,
            b: b_index as u32,
            r_a,
            r_b,
            k: [
                effective(DVec3::X),
                effective(DVec3::Y),
                effective(DVec3::Z),
            ],
            inv_i_a,
            inv_i_b,
            a_latched,
            b_latched,
            target,
        })
    }
}

/// Phase 4a″ — joint constraint generation. The caller's `JointDef`
/// list is re-sorted into the solver's fixed order — ascending
/// `(body_a, body_b, anchors)` — so the iteration order is a property
/// of the constraint set, not of the caller's enumeration (§2.9's
/// fixed-order rule applied to the cap-16 transient list). Joints
/// naming a handle outside `bodies` are skipped: a handle is only
/// valid against one `step` call's slice and the sim never emits a
/// stale one, but a solver must not index blindly.
fn generate_joint_constraints(bodies: &[WeldedBody], joints: &[JointDef]) -> Vec<JointConstraint> {
    let mut order: Vec<&JointDef> = joints
        .iter()
        // Self-joints are meaningless input the caller should never
        // emit: `body_a == body_b` would halve the constraint's
        // impulse authority (both write-backs land on the same body)
        // and inject a spurious torque. Filter them alongside the
        // stale handles rather than solve them wrong.
        .filter(|j| {
            j.body_a != j.body_b
                && (j.body_a.0 as usize) < bodies.len()
                && (j.body_b.0 as usize) < bodies.len()
        })
        .collect();
    order.sort_by_key(|j| {
        (
            j.body_a.0,
            j.body_b.0,
            j.local_anchor_a.x.to_bits(),
            j.local_anchor_a.y.to_bits(),
            j.local_anchor_a.z.to_bits(),
            j.local_anchor_b.x.to_bits(),
            j.local_anchor_b.y.to_bits(),
            j.local_anchor_b.z.to_bits(),
        )
    });
    let mut out = Vec::with_capacity(order.len());
    for j in order {
        if let Some(c) = JointConstraint::new(j, j.body_a.0 as usize, j.body_b.0 as usize, bodies) {
            out.push(c);
        }
    }
    out
}

/// The position pass's pair write-back — pseudo-impulse `j` (world
/// frame, `σ·n` signed) applied to `b` about its CoM: `x̄ += j/m`,
/// quaternion advance by `I⁻¹(r×j)`, `pos` re-derived — the same
/// CoM-frame discipline `integrate_free` and the ground correction
/// apply. `rot` may change here, so `r` is recomputed from the live
/// pose, not the constraint's generation arm.
fn pair_position_correction(b: &mut WeldedBody, inv_i: SymMat3, p_world: DVec3, j: DVec3) {
    let com = b.com_world() + j / b.mass_kg;
    let conj = b.rot.conjugate();
    let r_body = conj.rotate(p_world - b.com_world());
    let j_body = conj.rotate(j);
    let dtheta_body = inv_i.mul_vec(r_body.cross(j_body));
    let dtheta_world = b.rot.rotate(dtheta_body);
    let wq = DQuat::new(dtheta_world.x, dtheta_world.y, dtheta_world.z, 0.0);
    let dq = wq * b.rot;
    b.rot = DQuat::new(
        b.rot.x + 0.5 * dq.x,
        b.rot.y + 0.5 * dq.y,
        b.rot.z + 0.5 * dq.z,
        b.rot.w + 0.5 * dq.w,
    )
    .normalize();
    b.pos = com - b.rot.rotate(b.com_local);
}

/// World-frame AABB of one hull of `b` — `None` on an empty vertex
/// set (a hull with no points cannot overlap anything).
fn hull_world_aabb(b: &WeldedBody, hull: &crate::weld::ConvexHull) -> Option<(DVec3, DVec3)> {
    let (first, rest) = hull.vertices.split_first()?;
    let p0 = b.pos + b.rot.rotate(*first);
    let (mut lo, mut hi) = (p0, p0);
    for v in rest {
        let p = b.pos + b.rot.rotate(*v);
        lo = DVec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = DVec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    Some((lo, hi))
}

/// World-frame AABB of `b`'s whole compound collider — `None` when
/// every hull is empty.
fn body_world_aabb(b: &WeldedBody) -> Option<(DVec3, DVec3)> {
    let mut out: Option<(DVec3, DVec3)> = None;
    for hull in &b.collider.hulls {
        if let Some((lo, hi)) = hull_world_aabb(b, hull) {
            out = Some(match out {
                Some((l, h)) => (
                    DVec3::new(l.x.min(lo.x), l.y.min(lo.y), l.z.min(lo.z)),
                    DVec3::new(h.x.max(hi.x), h.y.max(hi.y), h.z.max(hi.z)),
                ),
                None => (lo, hi),
            });
        }
    }
    out
}

/// Strict AABB overlap — touching boxes do not overlap (matching the
/// strict `depth > 0` generation rule).
fn aabbs_overlap(a: (DVec3, DVec3), b: (DVec3, DVec3)) -> bool {
    a.0.x < b.1.x
        && b.0.x < a.1.x
        && a.0.y < b.1.y
        && b.0.y < a.1.y
        && a.0.z < b.1.z
        && b.0.z < a.1.z
}

/// Inclusive AABB overlap — face-touching counts (the spawn-occupancy
/// rule: a box sharing a boundary claims the spawn volume's edge).
fn aabbs_touch(a: (DVec3, DVec3), b: (DVec3, DVec3)) -> bool {
    a.0.x <= b.1.x
        && b.0.x <= a.1.x
        && a.0.y <= b.1.y
        && b.0.y <= a.1.y
        && a.0.z <= b.1.z
        && b.0.z <= a.1.z
}

/// Does any vertex of `p`'s collider lie strictly inside a facet
/// volume of `q`'s? One direction of the pair-generation narrowphase,
/// shared with the `vessel_contact` predicate (the rules must be the
/// same test — a predicate that disagreed with generation would
/// refuse warp/save on contacts the solver never produced).
fn any_vertex_penetrates(p: &WeldedBody, q: &WeldedBody) -> bool {
    for hp in &p.collider.hulls {
        let Some(ap) = hull_world_aabb(p, hp) else {
            continue;
        };
        for hq in &q.collider.hulls {
            let Some(aq) = hull_world_aabb(q, hq) else {
                continue;
            };
            if !aabbs_overlap(ap, aq) {
                continue;
            }
            let Some(planes) =
                crate::hull::facet_planes(&hq.vertices, crate::hull::plane_epsilon(&hq.vertices))
            else {
                continue;
            };
            for v in &hp.vertices {
                let p_w = p.pos + p.rot.rotate(*v);
                let p_q = q.rot.conjugate().rotate(p_w - q.pos);
                if !crate::hull::point_penetration(
                    &planes,
                    p_q,
                    crate::hull::vertex_margin(&hq.vertices),
                )
                .is_empty()
                {
                    return true;
                }
            }
        }
    }
    false
}

/// The pair-geometry predicate both `vessel_contact` and generation
/// share: a vertex of either body strictly inside a facet volume of
/// the other, behind body- and hull-AABB broadphase.
fn hulls_overlap(a: &WeldedBody, b: &WeldedBody) -> bool {
    let (Some(aa), Some(bb)) = (body_world_aabb(a), body_world_aabb(b)) else {
        return false;
    };
    if !aabbs_overlap(aa, bb) {
        return false;
    }
    any_vertex_penetrates(a, b) || any_vertex_penetrates(b, a)
}

/// §2.9's `SimpleBackend` — see the module docs for the full spec
/// reading. Owns its [`Environment`] and the last step's public
/// contact record (a derived observation of the solve, re-created each
/// tick — never serialized, per §2.4's derived-cache rule).
#[derive(Clone, Debug)]
pub struct SimpleBackend {
    env: Environment,
    /// The sim-second the stepped/predicated state is stamped at —
    /// `set_sim_time` input. The body-fixed heightfield's inertial
    /// direction is a function of the spin phase `ω·t`, so terrain
    /// contact evaluation on a rotating world needs the tick (the
    /// same instant `bubble.rs`'s pose records and the renderer's
    /// `spin_phase` use). `Cell` because the `&self` predicates
    /// (`ground_contact`, `cast_ray`) share the stamp. Inert on a
    /// non-rotating environment (the phase conjugate is identity);
    /// per-tick construction scratch, never sim state.
    time_s: Cell<f64>,
    /// Per-body contact manifold from the most recent `step`, indexed
    /// by slice position ([`BodyHandle`]). Cleared for latched bodies.
    contacts: Vec<Vec<Contact>>,
}

impl SimpleBackend {
    pub fn new(env: Environment) -> Self {
        // Warm the terrain bake at construction rather than on the
        // first contact tick — the ~0.5 M-texel bake is a pure
        // function of the seed, so when it runs is unobservable, but
        // a startup stall beats a mid-step one.
        if let Some(t) = &env.terrain {
            t.prewarm();
        }
        Self {
            env,
            time_s: Cell::new(0.0),
            contacts: Vec::new(),
        }
    }

    /// Stamp the sim time the backend's state is indexed at. On a
    /// rotating world the heightfield rotates with the body, so the
    /// ground the solver should contact at a given instant is the
    /// field sampled through `R(axis, −ω·t)` — the caller's `t` is
    /// the *state's* tick (the convention `surface_pose_capture`
    /// stamps post-step poses with); `step` evaluates the ground at
    /// `t + dt`, the instant the post-integration pose lands.
    /// Inert when the environment has no terrain or does not rotate.
    pub fn set_sim_time(&self, t: SimTime) {
        self.time_s.set(t.seconds());
    }

    /// The environment the backend was built with.
    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// The per-part external-force budget for the §2.4 breakup load
    /// evaluation (see `crate::structural` for the model): every
    /// force the environment and the vessel's own sites apply to each
    /// graph node this tick — gravity at each part's CoM, per-part
    /// aero (table path, or the placeholder distributed by mass
    /// share), thrust sites, and the solved contact impulses (ground
    /// and vessel-vessel pair) attributed by hull — expressed in
    /// **body frame** as [`PartForce`]s.
    ///
    /// `contacts` is the manifold record from the last `step` for
    /// this body (`self.contacts(handle)`); passing it explicitly
    /// keeps the borrow discipline honest.
    pub fn external_loads(
        &self,
        body: &WeldedBody,
        graph: &crate::graph::PartGraph,
        parts: &[WeldPart],
        contacts: &[Contact],
    ) -> Result<
        std::collections::BTreeMap<crate::graph::PartNodeId, Vec<crate::structural::PartForce>>,
        crate::weld::WeldError,
    > {
        use crate::structural::PartForce;
        let transforms = crate::weld::body_transforms(graph, parts)?;
        let mut out: std::collections::BTreeMap<crate::graph::PartNodeId, Vec<PartForce>> =
            std::collections::BTreeMap::new();
        let conj = body.rot.conjugate();
        let mut push = |id: crate::graph::PartNodeId, at: DVec3, force: DVec3| {
            out.entry(id).or_default().push(PartForce { at, force });
        };

        // Gravity at each part's body-frame CoM — the same analytic
        // −μ·r̂/r² as `accumulate_forces`, attributed per part.
        for (id, node) in &graph.nodes {
            let ep = &parts[node.effective_index as usize];
            let c_b = transforms[id].apply(ep.com);
            let rel = (body.pos + body.rot.rotate(c_b)) - self.env.body_center;
            let r = rel.norm();
            if r > 0.0 && ep.mass_kg > 0.0 {
                push(
                    *id,
                    c_b,
                    conj.rotate(rel * (-self.env.mu_m3_per_s2 * ep.mass_kg / (r * r * r))),
                );
            }
        }

        // Aero — the §2.4.1 per-part table breakdown when installed,
        // else the placeholder total distributed by mass share (a
        // documented approximation — the placeholder has no
        // application-point data to do better).
        if let Some(atm) = &self.env.atmosphere {
            let rel = body.com_world() - self.env.body_center;
            let alt = rel.norm() - self.env.radius_m;
            let rho = atmosphere_density_kg_m3(atm, alt);
            if rho > 0.0 {
                let v_rel = body.vel - self.env.body_spin.cross(rel);
                let speed = v_rel.norm();
                if speed > 0.0 {
                    match &body.aero {
                        Some(tables) => {
                            let a = atmosphere_sound_speed_mps(atm, alt);
                            let mach = if a > 0.0 { speed / a } else { 0.0 };
                            let re_per_len = rho * speed / crate::aero::AIR_VISCOSITY_PA_S;
                            let w_body = body.rot.conjugate().rotate(v_rel / -speed);
                            for (node, f, cop) in tables.part_forces_body(
                                w_body,
                                0.5 * rho * speed * speed,
                                mach,
                                re_per_len,
                            ) {
                                push(node, cop, f);
                            }
                        }
                        None => {
                            let area = cross_section_area(body, v_rel / speed);
                            let f_b =
                                conj.rotate(v_rel * (-0.5 * rho * self.env.drag_cd * area * speed));
                            if body.mass_kg > 0.0 {
                                for (id, node) in &graph.nodes {
                                    let ep = &parts[node.effective_index as usize];
                                    if ep.mass_kg > 0.0 {
                                        push(
                                            *id,
                                            transforms[id].apply(ep.com),
                                            f_b * (ep.mass_kg / body.mass_kg),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Thrust sites — same channel as `accumulate_forces`.
        let throttle = body.throttle.clamp(0.0, 1.0);
        if throttle > 0.0 {
            for (id, pw) in &body.part_index {
                let sites = &body.force_sites
                    [pw.site_start as usize..(pw.site_start + pw.site_len) as usize];
                for site in sites {
                    if site.kind == ForceSiteKind::Thrust {
                        push(
                            *id,
                            site.local_pos,
                            site.local_dir * (throttle * site.rated_n),
                        );
                    }
                }
            }
        }

        // Contact impulses ÷ dt — ground and vessel-vessel pair —
        // attributed by hull → part.
        let mut hull_owner: std::collections::BTreeMap<u32, crate::graph::PartNodeId> =
            std::collections::BTreeMap::new();
        for (id, pw) in &body.part_index {
            hull_owner.insert(pw.hull_index, *id);
        }
        for c in contacts {
            if c.impulse.norm_squared() > 0.0
                && let Some(id) = hull_owner.get(&c.hull)
            {
                push(
                    *id,
                    conj.rotate(c.pos - body.pos),
                    conj.rotate(c.impulse) * (1.0 / PHYSICS_DT),
                );
            }
        }
        Ok(out)
    }

    /// Geometric ground-contact predicate — the same rule
    /// [`Self::generate_contacts`] applies (any collider vertex with
    /// `depth > 0` against the environment's surface — the displaced
    /// heightfield when `env.terrain` is set), evaluated
    /// on-demand. The `contacts()` record is only a view of the most
    /// recent `step` call; the sim's warp-refusal and save-boundary
    /// predicates need the per-body truth *between* steps, which is
    /// geometry, not solver state. Evaluated at the stamped instant
    /// (`set_sim_time`): the surface under a vertex at tick `t` is
    /// the field at spin phase `ω·t`.
    pub fn ground_contact(&self, b: &WeldedBody) -> bool {
        for hull in &b.collider.hulls {
            for v in &hull.vertices {
                let p = b.pos + b.rot.rotate(*v);
                let rel = p - self.env.body_center;
                let dist = rel.norm();
                if dist > 0.0 && self.env.surface_radius(rel / dist, self.time_s.get()) - dist > 0.0
                {
                    return true;
                }
            }
        }
        false
    }

    /// Geometric vessel-vessel contact predicate — the same rule
    /// [`Self::generate_pair_contacts`] applies (a collider vertex of
    /// either body strictly inside a facet volume of the other,
    /// behind the body/hull AABB broadphase), evaluated on demand.
    /// The world's §7.4 save-boundary, §2.6.2 warp-refusal, and
    /// to-rails predicates need the pairwise truth *between* steps,
    /// which is geometry, not solver state.
    pub fn vessel_contact(&self, a: &WeldedBody, b: &WeldedBody) -> bool {
        hulls_overlap(a, b)
    }

    /// Spawn-site occupancy predicate — do the two bodies' collider
    /// volumes claim any of the same space? Deliberately broader than
    /// [`Self::vessel_contact`]: that rule asks for a vertex *strictly
    /// inside* a facet volume, which two hulls materialized in the
    /// same pose can never produce (every vertex lies *on* the shared
    /// facets — the pair would ghost through each other forever). The
    /// inclusive world-AABB test here catches the coincident and
    /// merely-touching cases the vertex rule misses; a false positive
    /// only refuses a spawn into a crowded volume, it never invents
    /// a contact the solver must then resolve.
    pub fn volumes_overlap(&self, a: &WeldedBody, b: &WeldedBody) -> bool {
        let (Some(aa), Some(bb)) = (body_world_aabb(a), body_world_aabb(b)) else {
            return false;
        };
        aabbs_touch(aa, bb)
    }

    /// Force and torque (about the world-frame CoM) on `b`, in the
    /// fixed accumulation order gravity → drag → thrust.
    fn accumulate_forces(&self, b: &WeldedBody) -> (DVec3, DVec3) {
        let rel = b.com_world() - self.env.body_center;
        let r = rel.norm();
        let mut force = DVec3::ZERO;
        let mut torque = DVec3::ZERO;

        // Gravity: −μ·m·r̂/r², at the CoM (no torque).
        if r > 0.0 {
            force = force + rel * (-self.env.mu_m3_per_s2 * b.mass_kg / (r * r * r));
        }

        // Aerodynamics: the §2.4.1 table-driven model when Stage-2
        // tables are attached (`b.aero`), else the M2 pre-voxel
        // placeholder — both apply at the CoM-relative points the
        // force model documents. Table path: per-part directional
        // exposure × authored Cd/Cl(Mach) + Cf(Re), forces applied at
        // each part's centre of pressure → drag, lift, and the pitch
        // moment all emerge (the placeholder produced no aero torque).
        if let Some(atm) = &self.env.atmosphere {
            let alt = r - self.env.radius_m;
            let rho = atmosphere_density_kg_m3(atm, alt);
            if rho > 0.0 {
                let v_rel = b.vel - self.env.body_spin.cross(rel);
                let speed = v_rel.norm();
                if speed > 0.0 {
                    match &b.aero {
                        Some(tables) => {
                            let a = atmosphere_sound_speed_mps(atm, alt);
                            let mach = if a > 0.0 { speed / a } else { 0.0 };
                            let re_per_len = rho * speed / crate::aero::AIR_VISCOSITY_PA_S;
                            let w_body = b.rot.conjugate().rotate(v_rel / -speed);
                            let (fb, taub) = tables.force_torque_body(
                                w_body,
                                0.5 * rho * speed * speed,
                                mach,
                                re_per_len,
                            );
                            force = force + b.rot.rotate(fb);
                            torque = torque + b.rot.rotate(taub);
                        }
                        None => {
                            let area = cross_section_area(b, v_rel / speed);
                            force = force + v_rel * (-0.5 * rho * self.env.drag_cd * area * speed);
                        }
                    }
                }
            }
        }

        // Thrust sites: throttle·rated_n along rot·local_dir, torque
        // r×F about the CoM. Clamping is the backend's validity gate;
        // per-engine throttle_range enforcement is the control layer.
        let throttle = b.throttle.clamp(0.0, 1.0);
        if throttle > 0.0 {
            for site in &b.force_sites {
                if site.kind == ForceSiteKind::Thrust {
                    let f_world = b.rot.rotate(site.local_dir) * (throttle * site.rated_n);
                    let r_world = b.rot.rotate(site.local_pos - b.com_local);
                    force = force + f_world;
                    torque = torque + r_world.cross(f_world);
                }
            }
        }
        (force, torque)
    }

    /// Phases 1–3 for one body: force accumulation → velocity
    /// integrate → position integrate (semi-implicit Euler; angular
    /// part includes the gyroscopic `ω×(Iω)` term, body frame).
    fn integrate_free(&self, b: &mut WeldedBody) {
        let (force, torque) = self.accumulate_forces(b);
        b.vel = b.vel + force * (PHYSICS_DT / b.mass_kg);
        let inv_i = b.inertia_local.inverse().unwrap_or(SymMat3::ZERO);
        let tau_body = b.rot.conjugate().rotate(torque);
        let gyro = b.ang_vel.cross(b.inertia_local.mul_vec(b.ang_vel));
        b.ang_vel = b.ang_vel + inv_i.mul_vec(tau_body - gyro) * PHYSICS_DT;

        // The position phase advances the CoM, then re-derives the
        // origin from the new rotation — `vel` is the CoM velocity, so
        // integrating `pos` directly would orbit the CoM about the
        // origin for `com_local ≠ 0` (module docs).
        let com = b.com_world() + b.vel * PHYSICS_DT;
        let omega_world = b.rot.rotate(b.ang_vel);
        let wq = DQuat::new(omega_world.x, omega_world.y, omega_world.z, 0.0);
        let dq = wq * b.rot;
        b.rot = DQuat::new(
            b.rot.x + 0.5 * PHYSICS_DT * dq.x,
            b.rot.y + 0.5 * PHYSICS_DT * dq.y,
            b.rot.z + 0.5 * PHYSICS_DT * dq.z,
            b.rot.w + 0.5 * PHYSICS_DT * dq.w,
        )
        .normalize();
        b.pos = com - b.rot.rotate(b.com_local);
    }

    /// Phase 4a — contact generation at the post-integration pose.
    /// Every collider vertex below the environment's surface —
    /// `env.surface_radius`, the displaced heightfield when
    /// `env.terrain` is set — yields one manifold point, in fixed
    /// ascending `(body, hull, vertex)` order. `constraints` is the
    /// solver working set, `self.contacts` the public record — same
    /// points, two views.
    ///
    /// The surface is evaluated at `time_s + dt`: the pose the step
    /// produces is stamped with the NEXT tick (the convention
    /// `transition_latched` applies when it captures the pose), so
    /// the ground the vertex rests against is the field at that
    /// instant's spin phase — the same surface the renderer draws
    /// for that tick.
    fn generate_contacts(&mut self, bodies: &[WeldedBody], constraints: &mut Vec<Constraint>) {
        let eval_s = self.time_s.get() + PHYSICS_DT;
        for (i, b) in bodies.iter().enumerate() {
            if b.settle.latched {
                continue;
            }
            for (hull_index, hull) in b.collider.hulls.iter().enumerate() {
                for v in &hull.vertices {
                    let p = b.pos + b.rot.rotate(*v);
                    let rel = p - self.env.body_center;
                    let dist = rel.norm();
                    let depth = if dist > 0.0 {
                        self.env.surface_radius(rel / dist, eval_s) - dist
                    } else {
                        self.env.radius_m - dist
                    };
                    // Strict `> 0`: a vertex exactly grazing the
                    // surface carries no penetration and no impulse
                    // authority; it is caught next tick at depth
                    // ≤ v·dt (module docs).
                    if depth > 0.0 && dist > 0.0 {
                        let ci = self.contacts[i].len();
                        let (c, record) = Constraint::new(
                            i,
                            ci,
                            hull_index as u32,
                            *v,
                            p,
                            rel / dist,
                            depth,
                            b,
                            &self.env,
                        );
                        constraints.push(c);
                        self.contacts[i].push(record);
                    }
                }
            }
        }
    }

    /// Phase 4a′ — vessel-vessel contact generation at the
    /// post-integration pose: for each ordered body pair `(i, j)`
    /// (`i < j`, both not simultaneously latched, body AABBs
    /// overlapping), every collider vertex of `i` strictly inside a
    /// facet volume of `j` — and vice versa — yields one
    /// [`PairConstraint`] plus one [`Contact`] record on **each**
    /// body: the penetrator's `+n` record and the penetrated body's
    /// `−n` record (hull-indexed to its own collider for the §2.4
    /// breakup load attribution). Fixed ascending
    /// `(i, j, hull_i, hull_j, direction, vertex)` order.
    ///
    /// Facet planes are re-detected per call from the penetrated
    /// hull's canonicalized vertex set — a pure derived cache with no
    /// stored state — and vertex-only like the ground manifold (the
    /// module doc's documented limitation).
    #[allow(clippy::too_many_lines)]
    fn generate_pair_contacts(&mut self, bodies: &[WeldedBody], pairs: &mut Vec<PairConstraint>) {
        if bodies.len() < 2 {
            return;
        }
        // Per-body world AABBs — the broadphase reject before any
        // facet-plane work happens (the common case: vessels sharing
        // a bubble but kilometres apart, pays only O(total verts)).
        let body_aabbs: Vec<Option<(DVec3, DVec3)>> = bodies.iter().map(body_world_aabb).collect();
        for i in 0..bodies.len() {
            for j in (i + 1)..bodies.len() {
                let (Some(bi), Some(bj)) = (body_aabbs[i], body_aabbs[j]) else {
                    continue;
                };
                if !aabbs_overlap(bi, bj) {
                    continue;
                }
                let (a, b) = (&bodies[i], &bodies[j]);
                if a.settle.latched && b.settle.latched {
                    // Two static bodies cannot resolve a mutual
                    // penetration — the constraint would carry zero
                    // impulse authority either way.
                    continue;
                }
                // Per-hull broadphase + facet planes for this pair.
                // Planes are computed once per hull (lazily per pair —
                // a hull may pair with several others) and `None`
                // marks a degenerate hull that can never be a
                // penetrated target.
                let aabbs_a: Vec<Option<(DVec3, DVec3)>> = a
                    .collider
                    .hulls
                    .iter()
                    .map(|h| hull_world_aabb(a, h))
                    .collect();
                let aabbs_b: Vec<Option<(DVec3, DVec3)>> = b
                    .collider
                    .hulls
                    .iter()
                    .map(|h| hull_world_aabb(b, h))
                    .collect();
                let planes_a: Vec<Option<Vec<crate::hull::Plane>>> = a
                    .collider
                    .hulls
                    .iter()
                    .map(|h| {
                        crate::hull::facet_planes(
                            &h.vertices,
                            crate::hull::plane_epsilon(&h.vertices),
                        )
                    })
                    .collect();
                let planes_b: Vec<Option<Vec<crate::hull::Plane>>> = b
                    .collider
                    .hulls
                    .iter()
                    .map(|h| {
                        crate::hull::facet_planes(
                            &h.vertices,
                            crate::hull::plane_epsilon(&h.vertices),
                        )
                    })
                    .collect();
                for (hi, ha) in a.collider.hulls.iter().enumerate() {
                    for (hb_i, hb) in b.collider.hulls.iter().enumerate() {
                        let (Some(aa), Some(bb)) = (aabbs_a[hi], aabbs_b[hb_i]) else {
                            continue;
                        };
                        if !aabbs_overlap(aa, bb) {
                            continue;
                        }
                        // Direction i→j: a's vertices inside hull hb_i.
                        if let Some(pb) = &planes_b[hb_i] {
                            for v in &ha.vertices {
                                let p_w = a.pos + a.rot.rotate(*v);
                                let p_b = b.rot.conjugate().rotate(p_w - b.pos);
                                // One manifold point per facet the
                                // vertex entered — ascending facet
                                // index, a fixed order like every
                                // other loop here.
                                for (face, depth) in crate::hull::point_penetration(
                                    pb,
                                    p_b,
                                    crate::hull::vertex_margin(&hb.vertices),
                                ) {
                                    let n_w = b.rot.rotate(pb[face].n);
                                    let (cia, cib) =
                                        (self.contacts[i].len(), self.contacts[j].len());
                                    let (c, ra, rb) = PairConstraint::new(
                                        i,
                                        j,
                                        cia,
                                        cib,
                                        *v,
                                        pb[face].n,
                                        pb[face].d,
                                        p_w,
                                        n_w,
                                        depth,
                                        a,
                                        b,
                                        hi as u32,
                                        hb_i as u32,
                                    );
                                    pairs.push(c);
                                    self.contacts[i].push(ra);
                                    self.contacts[j].push(rb);
                                }
                            }
                        }
                        // Direction j→i: b's vertices inside hull hi.
                        if let Some(pa) = &planes_a[hi] {
                            for v in &hb.vertices {
                                let p_w = b.pos + b.rot.rotate(*v);
                                let p_a = a.rot.conjugate().rotate(p_w - a.pos);
                                for (face, depth) in crate::hull::point_penetration(
                                    pa,
                                    p_a,
                                    crate::hull::vertex_margin(&ha.vertices),
                                ) {
                                    let n_w = a.rot.rotate(pa[face].n);
                                    let (cja, cib) =
                                        (self.contacts[j].len(), self.contacts[i].len());
                                    let (c, rj, ri) = PairConstraint::new(
                                        j,
                                        i,
                                        cja,
                                        cib,
                                        *v,
                                        pa[face].n,
                                        pa[face].d,
                                        p_w,
                                        n_w,
                                        depth,
                                        b,
                                        a,
                                        hb_i as u32,
                                        hi as u32,
                                    );
                                    pairs.push(c);
                                    self.contacts[j].push(rj);
                                    self.contacts[i].push(ri);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Phase 4b — the §2.9 projected Gauss–Seidel velocity solve: 8
    /// iterations, ascending `(body_id, contact_index)` (the
    /// `constraints` vector is built in exactly that order), normal
    /// impulse `≥ 0`, friction inside the Coulomb disc `μ·λ_n`.
    /// Velocities are solved *relative to the ground surface* —
    /// `v_surf = body_spin × rel` is nonzero on a rotating world —
    /// and, for `pairs`, *relative to the peer body*: the pair
    /// constraint drives `v_a − v_b` to `≥ 0` along `n` with
    /// equal-and-opposite impulses (the generalization of the
    /// moving-surface rule). Each iteration runs the whole ground
    /// manifold first, then the whole pair manifold — a fixed order
    /// like every other pass here. Joint rows run last in each
    /// iteration, in generation order (already the fixed
    /// `(body_a, body_b, anchors)` sort), each joint solving its three
    /// bilateral rows X → Y → Z.
    fn solve_velocity(
        &self,
        bodies: &mut [WeldedBody],
        constraints: &mut [Constraint],
        pairs: &mut [PairConstraint],
        joints: &[JointConstraint],
    ) {
        // Cache each body's body-frame linear velocity once for the
        // whole solve; impulses update it by pure vector addition —
        // `rot` does not change inside this pass, so the cache stays
        // exact and saves a quaternion rotate per velocity evaluation.
        let mut vel_body: Vec<DVec3> = bodies
            .iter()
            .map(|b| b.rot.conjugate().rotate(b.vel))
            .collect();
        // Same trick, world frame, for the pair constraints' relative
        // point velocities — `rot·ang_vel`, kept in sync by both
        // impulse write-backs.
        let mut omega_world: Vec<DVec3> = bodies.iter().map(|b| b.rot.rotate(b.ang_vel)).collect();
        for _ in 0..SOLVER_ITERATIONS {
            for c in constraints.iter_mut() {
                let b = &mut bodies[c.body as usize];
                let vel_b = &mut vel_body[c.body as usize];
                let omega_w = &mut omega_world[c.body as usize];

                // Normal: target vn ≥ v_surf·n (restitution 0; the
                // surface velocity is tangential on the sphere, so in
                // practice vn ≥ 0 — the relative form is the honest
                // statement either way).
                let vn = rel_point_velocity_body(*vel_b, b, c).dot(c.n_body);
                let old = c.lambda_n;
                c.lambda_n = (c.lambda_n - vn * c.kn).max(0.0);
                let dl = c.lambda_n - old;
                if dl != 0.0 {
                    apply_impulse(b, vel_b, omega_w, c, c.n_body, dl);
                }

                // Friction, fixed order t1 then t2.
                let limit = self.env.friction_mu * c.lambda_n;
                let vt1 = rel_point_velocity_body(*vel_b, b, c).dot(c.t1_body);
                let old = c.lambda_t1;
                c.lambda_t1 = (c.lambda_t1 - vt1 * c.kt1).clamp(-limit, limit);
                let dl = c.lambda_t1 - old;
                if dl != 0.0 {
                    apply_impulse(b, vel_b, omega_w, c, c.t1_body, dl);
                }

                let vt2 = rel_point_velocity_body(*vel_b, b, c).dot(c.t2_body);
                let old = c.lambda_t2;
                c.lambda_t2 = (c.lambda_t2 - vt2 * c.kt2).clamp(-limit, limit);
                let dl = c.lambda_t2 - old;
                if dl != 0.0 {
                    apply_impulse(b, vel_b, omega_w, c, c.t2_body, dl);
                }

                // Coulomb disc: the per-direction clamps alone admit
                // ‖λ_t‖ ≤ √2·μ·λ_n on diagonal slides. Project the
                // accumulated pair onto the disc (same fixed t1-then-
                // t2 write-back order) so the true bound is μ·λ_n.
                let tnorm = libm::sqrt(c.lambda_t1 * c.lambda_t1 + c.lambda_t2 * c.lambda_t2);
                if tnorm > limit && tnorm > 0.0 {
                    let s = limit / tnorm;
                    let dl1 = c.lambda_t1 * s - c.lambda_t1;
                    let dl2 = c.lambda_t2 * s - c.lambda_t2;
                    c.lambda_t1 *= s;
                    c.lambda_t2 *= s;
                    if dl1 != 0.0 {
                        apply_impulse(b, vel_b, omega_w, c, c.t1_body, dl1);
                    }
                    if dl2 != 0.0 {
                        apply_impulse(b, vel_b, omega_w, c, c.t2_body, dl2);
                    }
                }
            }
            for c in pairs.iter_mut() {
                let (ia, ib) = (c.a as usize, c.b as usize);
                // Relative point velocity along n — the peer body's
                // point velocity is the "surface" velocity here, so
                // the restitution-0 target is `vn ≥ 0` on `v_a − v_b`.
                let vn = (bodies[ia].vel + omega_world[ia].cross(c.r_a)
                    - (bodies[ib].vel + omega_world[ib].cross(c.r_b)))
                .dot(c.n);
                let old = c.lambda_n;
                c.lambda_n = (c.lambda_n - vn * c.kn).max(0.0);
                let dl = c.lambda_n - old;
                if dl != 0.0 {
                    apply_pair_impulse(bodies, &mut vel_body, &mut omega_world, c, c.n * dl);
                }

                // Friction, same fixed t1-then-t2 order and Coulomb
                // disc as the ground manifold, on relative velocity.
                let limit = self.env.friction_mu * c.lambda_n;
                let vt1 = (bodies[ia].vel + omega_world[ia].cross(c.r_a)
                    - (bodies[ib].vel + omega_world[ib].cross(c.r_b)))
                .dot(c.t1);
                let old = c.lambda_t1;
                c.lambda_t1 = (c.lambda_t1 - vt1 * c.kt1).clamp(-limit, limit);
                let dl = c.lambda_t1 - old;
                if dl != 0.0 {
                    apply_pair_impulse(bodies, &mut vel_body, &mut omega_world, c, c.t1 * dl);
                }

                let vt2 = (bodies[ia].vel + omega_world[ia].cross(c.r_a)
                    - (bodies[ib].vel + omega_world[ib].cross(c.r_b)))
                .dot(c.t2);
                let old = c.lambda_t2;
                c.lambda_t2 = (c.lambda_t2 - vt2 * c.kt2).clamp(-limit, limit);
                let dl = c.lambda_t2 - old;
                if dl != 0.0 {
                    apply_pair_impulse(bodies, &mut vel_body, &mut omega_world, c, c.t2 * dl);
                }

                let tnorm = libm::sqrt(c.lambda_t1 * c.lambda_t1 + c.lambda_t2 * c.lambda_t2);
                if tnorm > limit && tnorm > 0.0 {
                    let s = limit / tnorm;
                    let dl1 = c.lambda_t1 * s - c.lambda_t1;
                    let dl2 = c.lambda_t2 * s - c.lambda_t2;
                    c.lambda_t1 *= s;
                    c.lambda_t2 *= s;
                    if dl1 != 0.0 {
                        apply_pair_impulse(bodies, &mut vel_body, &mut omega_world, c, c.t1 * dl1);
                    }
                    if dl2 != 0.0 {
                        apply_pair_impulse(bodies, &mut vel_body, &mut omega_world, c, c.t2 * dl2);
                    }
                }
            }
            Self::solve_joint_rows(bodies, &mut vel_body, &mut omega_world, joints);
        }
    }

    /// One sweep of the joint rows inside a `solve_velocity`
    /// iteration — X → Y → Z, bilateral: the applied impulse is
    /// unclamped — a joint pulls as well as pushes. `vrel` is
    /// `v_a − v_b` at the anchors, so `Δλ = (target − vrel)·k`
    /// applied `+dir` on `a` / `−dir` on `b` closes the row's error.
    /// Each `Δλ` is applied directly (no accumulated λ is kept — the
    /// solve is cold-started per tick, so there is no warm-start
    /// record to maintain).
    fn solve_joint_rows(
        bodies: &mut [WeldedBody],
        vel_body: &mut [DVec3],
        omega_world: &mut [DVec3],
        joints: &[JointConstraint],
    ) {
        const AXES: [DVec3; 3] = [DVec3::X, DVec3::Y, DVec3::Z];
        for c in joints.iter() {
            let (ia, ib) = (c.a as usize, c.b as usize);
            let target = [c.target.x, c.target.y, c.target.z];
            for (row, dir) in AXES.iter().enumerate() {
                // Re-evaluated per row — each row's write is the
                // next row's input (strict Gauss–Seidel, same as
                // the contact manifold's n→t1→t2 within a point).
                let vrel = bodies[ia].vel + omega_world[ia].cross(c.r_a)
                    - (bodies[ib].vel + omega_world[ib].cross(c.r_b));
                let dl = (target[row] - vrel.dot(*dir)) * c.k[row];
                if dl != 0.0 {
                    apply_two_body_impulse(
                        bodies,
                        vel_body,
                        omega_world,
                        ia,
                        ib,
                        c.r_a,
                        c.r_b,
                        c.inv_i_a,
                        c.inv_i_b,
                        c.a_latched,
                        c.b_latched,
                        *dir * dl,
                    );
                }
            }
        }
    }

    /// Phase 4c — Baumgarte position stabilization (β = 0.2) as a
    /// position-level pseudo-impulse solve: each iteration recomputes
    /// penetration at the *current* pose and applies `β·depth` of
    /// correction through the same effective-mass geometry, moving
    /// `pos` and `rot` directly (never `vel` — the bias must not enter
    /// stored velocity, see the module docs). Iterations share the
    /// fixed ascending order with the velocity solve.
    ///
    /// The correction is applied **about the CoM**, exactly as the
    /// integrator moves the pose: the linear pseudo-impulse advances
    /// `x̄`, the angular one rotates `q`, and `pos` is re-derived as
    /// `x̄ − q·com_local`. Rotating `q` while leaving `pos` fixed would
    /// swing the CoM sideways (wrong magnitude *and* direction for
    /// `com_local ≠ 0`, possibly deepening penetration), and a cap of
    /// [`MAX_TICK_POS_CORRECTION`] metres of CoM travel per body per
    /// tick bounds the total correction.
    ///
    /// Pair constraints share the pass (and the per-body budget):
    /// depth is re-measured each iteration against the generation
    /// facet in the penetrated body's live frame, and `σ·n` is applied
    /// `+` to the penetrator / `−` to the penetrated — the latched
    /// side of a pair takes no correction and spends no budget.
    fn solve_positions(
        &self,
        bodies: &mut [WeldedBody],
        constraints: &[Constraint],
        pairs: &[PairConstraint],
    ) {
        // Per-body budget of CoM translation this pass may apply this
        // tick — spent in fixed ascending constraint order.
        let mut spent = vec![0.0; bodies.len()];
        for _ in 0..SOLVER_ITERATIONS {
            for c in constraints {
                let b = &mut bodies[c.body as usize];
                let p = b.pos + b.rot.rotate(c.local_vertex);
                let rel = p - self.env.body_center;
                let dist = rel.norm();
                // Re-measure penetration against the SAME surface the
                // generator used — the heightfield at the step's
                // eval instant (`time_s + dt`), not the bare datum.
                let depth = if dist > 0.0 {
                    self.env
                        .surface_radius(rel / dist, self.time_s.get() + PHYSICS_DT)
                        - dist
                } else {
                    self.env.radius_m - dist
                };
                if depth <= 0.0 || dist <= 0.0 {
                    continue;
                }
                // σ is the pseudo-impulse magnitude; the CoM travels
                // σ·n/m. Scale σ down to the remaining per-tick budget.
                let remaining = (MAX_TICK_POS_CORRECTION - spent[c.body as usize]).max(0.0);
                let com_travel = |sigma: f64| sigma / b.mass_kg;
                let mut sigma = BAUMGARTE_BETA * depth * c.kn;
                if com_travel(sigma) > remaining {
                    sigma *= remaining / com_travel(sigma);
                }
                if sigma <= 0.0 {
                    continue;
                }
                spent[c.body as usize] += com_travel(sigma);

                let normal = rel / dist;
                let conj = b.rot.conjugate();
                let n_body = conj.rotate(normal);
                // CoM-consistent write-back: advance x̄ by the linear
                // pseudo-impulse, rotate q by the angular one, then
                // re-derive pos — same com/pose relation as
                // `integrate_free`.
                let com = b.com_world() + normal * com_travel(sigma);
                let r_body = conj.rotate(p - b.com_world());
                let dtheta_body = c.inv_i.mul_vec(r_body.cross(n_body * sigma));
                let dtheta_world = b.rot.rotate(dtheta_body);
                let wq = DQuat::new(dtheta_world.x, dtheta_world.y, dtheta_world.z, 0.0);
                let dq = wq * b.rot;
                b.rot = DQuat::new(
                    b.rot.x + 0.5 * dq.x,
                    b.rot.y + 0.5 * dq.y,
                    b.rot.z + 0.5 * dq.z,
                    b.rot.w + 0.5 * dq.w,
                )
                .normalize();
                b.pos = com - b.rot.rotate(b.com_local);
            }
            for c in pairs {
                let (ia, ib) = (c.a as usize, c.b as usize);
                // Re-measure depth at the CURRENT pose against the
                // persistent generation facet, in `b`'s live frame —
                // the same re-evaluation the ground pass does against
                // the sphere.
                let p_w = bodies[ia].pos + bodies[ia].rot.rotate(c.local_vertex);
                let p_b = bodies[ib].rot.conjugate().rotate(p_w - bodies[ib].pos);
                let depth = c.d_face_b - c.n_face_b.dot(p_b);
                if depth <= 0.0 {
                    continue;
                }
                let n_w = bodies[ib].rot.rotate(c.n_face_b);
                // σ is the pseudo-impulse magnitude; each moving side's
                // CoM travels σ/m. The per-body budget caps both —
                // a latched side moves nothing and spends nothing.
                let mut sigma = BAUMGARTE_BETA * depth * c.kn;
                if !c.a_latched {
                    let remaining = (MAX_TICK_POS_CORRECTION - spent[ia]).max(0.0);
                    let travel = sigma / bodies[ia].mass_kg;
                    if travel > remaining && travel > 0.0 {
                        sigma *= remaining / travel;
                    }
                }
                if !c.b_latched {
                    let remaining = (MAX_TICK_POS_CORRECTION - spent[ib]).max(0.0);
                    let travel = sigma / bodies[ib].mass_kg;
                    if travel > remaining && travel > 0.0 {
                        sigma *= remaining / travel;
                    }
                }
                if sigma <= 0.0 {
                    continue;
                }
                if !c.a_latched {
                    spent[ia] += sigma / bodies[ia].mass_kg;
                    pair_position_correction(&mut bodies[ia], c.inv_i_a, p_w, n_w * sigma);
                }
                if !c.b_latched {
                    spent[ib] += sigma / bodies[ib].mass_kg;
                    pair_position_correction(&mut bodies[ib], c.inv_i_b, p_w, n_w * -sigma);
                }
            }
        }
    }

    /// Phase 5 — §2.6.4 settle bookkeeping (the latch decision; the
    /// World-side body-fixed storage it enables is M2-C).
    ///
    /// Beyond the spec's velocity gates, a qualifying tick also
    /// requires the pose to have been at rest *to the same
    /// tolerance*: `‖Δpos‖ < settle_v·dt` and `‖Δrot‖ < settle_w·dt`
    /// over this step (`pose0` is the pre-integration pose, so the
    /// delta covers everything the tick did — free integration and
    /// the position pass, which moves the pose without touching
    /// velocities). Without it, a body whose position solve is still
    /// correcting (emerging penetration, offset-CoM geometry) could
    /// accumulate settled ticks — and latch — mid-slide with `v ≈ ω
    /// ≈ 0`. `‖Δrot‖` is `‖q₁ − q₀‖ ≈ θ` for the small angles at
    /// issue (module docs).
    ///
    /// **Rotating-frame settle** (`body_spin ≠ 0`, M2-C): the §2.6.4
    /// rest condition is rest *relative to the surface*, so on a
    /// spinning world every gate is measured in the co-rotating frame —
    /// `v_rel = v − ω×(x̄ − c)`, `ω_rel = ω_b − ω_world` (body frame),
    /// and the pose deltas are residuals after the tick's expected
    /// co-rotation displacement `ω×(x̄₀ − c)·dt` and quaternion advance
    /// `q₀ + ½·(ω⊗q₀)·dt`. A body parked on the surface of a spinning
    /// world then legitimately latches; the stored inertial `‖v‖` alone
    /// could never satisfy `settle_v` at ~174 m/s of surface speed.
    /// For `body_spin == ZERO` every correction term vanishes and the
    /// gates read exactly as before — that path is guarded so the
    /// non-rotating case stays bitwise identical.
    fn update_settle(&mut self, bodies: &mut [WeldedBody], pose0: &[(DVec3, DQuat)]) {
        let dp_limit = self.env.settle_v * PHYSICS_DT;
        let dq_limit = self.env.settle_w * PHYSICS_DT;
        let spinning = self.env.body_spin != DVec3::ZERO;
        for (i, b) in bodies.iter_mut().enumerate() {
            if b.settle.latched {
                continue;
            }
            let in_contact = !self.contacts[i].is_empty();
            // Surface-relative gates: identity when `body_spin` is zero.
            let v_rel = if spinning {
                b.vel
                    - self
                        .env
                        .body_spin
                        .cross(b.com_world() - self.env.body_center)
            } else {
                b.vel
            };
            let w_rel = if spinning {
                b.ang_vel - b.rot.conjugate().rotate(self.env.body_spin)
            } else {
                b.ang_vel
            };
            // Pose deltas as residuals after the expected co-rotation
            // displacement — zero expectation on a static world.
            let dpos = if spinning {
                let v_surf0 = self
                    .env
                    .body_spin
                    .cross(pose0[i].0 + pose0[i].1.rotate(b.com_local) - self.env.body_center);
                ((b.pos - pose0[i].0) - v_surf0 * PHYSICS_DT).norm()
            } else {
                (b.pos - pose0[i].0).norm()
            };
            // ‖q₁ − q_expected‖ for unit quats ≈ the residual rotation
            // angle; on a static world q_expected = q₀ verbatim.
            let q0 = pose0[i].1;
            let q_expected = if spinning {
                let wq = DQuat::new(
                    self.env.body_spin.x,
                    self.env.body_spin.y,
                    self.env.body_spin.z,
                    0.0,
                ) * q0;
                DQuat::new(
                    q0.x + 0.5 * PHYSICS_DT * wq.x,
                    q0.y + 0.5 * PHYSICS_DT * wq.y,
                    q0.z + 0.5 * PHYSICS_DT * wq.z,
                    q0.w + 0.5 * PHYSICS_DT * wq.w,
                )
                .normalize()
            } else {
                q0
            };
            let drot = libm::sqrt(
                (b.rot.x - q_expected.x) * (b.rot.x - q_expected.x)
                    + (b.rot.y - q_expected.y) * (b.rot.y - q_expected.y)
                    + (b.rot.z - q_expected.z) * (b.rot.z - q_expected.z)
                    + (b.rot.w - q_expected.w) * (b.rot.w - q_expected.w),
            );
            if in_contact
                && v_rel.norm() < self.env.settle_v
                && w_rel.norm() < self.env.settle_w
                && dpos < dp_limit
                && drot < dq_limit
            {
                b.settle.settled_ticks = b.settle.settled_ticks.saturating_add(1);
                if b.settle.settled_ticks >= self.env.settle_ticks {
                    // Latch: contacts released, velocities zeroed — the
                    // backend holds no integrating state for the body.
                    // `throttle` is zeroed too — §2.6.4's "a latched
                    // vessel's throttle is zero" is enforced, not
                    // assumed: leaving a residual throttle would
                    // unlatch the body next tick and start a
                    // ~settle_ticks-period latch/unlatch flicker.
                    b.settle.latched = true;
                    b.vel = DVec3::ZERO;
                    b.ang_vel = DVec3::ZERO;
                    b.throttle = 0.0;
                    self.contacts[i].clear();
                }
            } else {
                b.settle.settled_ticks = 0;
            }
        }
    }
}

impl PhysicsBackend for SimpleBackend {
    /// One pinned `1/64` s tick. `joints` are the bubble's live
    /// transient joints (§2.4, cap 16): each becomes a bilateral
    /// three-row [`JointConstraint`] solved inside the same PGS
    /// velocity pass as the contacts — the documented v1 joint-island
    /// path (see [`JointDef`]'s backend note for why this is
    /// `SimpleBackend` and not a `RapierBackend` island).
    fn step(&mut self, bodies: &mut [WeldedBody], joints: &[JointDef]) {
        self.contacts.clear();
        self.contacts.resize_with(bodies.len(), Vec::new);

        // Pre-tick pose snapshot — the settle gate's pose-delta clause
        // (phase 5) measures everything this step moved, including the
        // Baumgarte pass, which never touches stored velocities.
        let pose0: Vec<(DVec3, DQuat)> = bodies.iter().map(|b| (b.pos, b.rot)).collect();

        // Phases 1–3 fused per body in ascending index order — bodies
        // do not couple before the contact solve, so the fusion is
        // semantically identical to three passes.
        for b in bodies.iter_mut() {
            if b.settle.latched {
                // §2.6.4: a latched vessel's throttle is zero (the
                // latch itself zeroes it), so a nonzero value here is
                // a fresh throttle command — unlatch first.
                if b.throttle > 0.0 {
                    b.settle.latched = false;
                    b.settle.settled_ticks = 0;
                } else {
                    continue;
                }
            }
            self.integrate_free(b);
        }

        // Phase 4 — constraint solve: generation (ground contacts,
        // then vessel-vessel pairs, then the transient joints),
        // velocity PGS, position Baumgarte, all in fixed order.
        let mut constraints = Vec::new();
        let mut pair_constraints = Vec::new();
        let joint_constraints = generate_joint_constraints(bodies, joints);
        self.generate_contacts(bodies, &mut constraints);
        self.generate_pair_contacts(bodies, &mut pair_constraints);
        self.solve_velocity(
            bodies,
            &mut constraints,
            &mut pair_constraints,
            &joint_constraints,
        );
        // Write each constraint's accumulated impulse into its public
        // contact record — the §2.4 breakup load evaluation's
        // collision data. Position-level Baumgarte corrections are
        // pseudo-impulses, not forces, and stay out.
        for c in &constraints {
            let b = &bodies[c.body as usize];
            let j = b
                .rot
                .rotate(c.n_body * c.lambda_n + c.t1_body * c.lambda_t1 + c.t2_body * c.lambda_t2);
            self.contacts[c.body as usize][c.contact_index as usize].impulse = j;
        }
        // Pair impulses are already world-frame: `+J` on the
        // penetrator's record, `−J` on the penetrated body's.
        for c in &pair_constraints {
            let j = c.n * c.lambda_n + c.t1 * c.lambda_t1 + c.t2 * c.lambda_t2;
            self.contacts[c.a as usize][c.contact_index_a as usize].impulse = j;
            self.contacts[c.b as usize][c.contact_index_b as usize].impulse = -j;
        }
        self.solve_positions(bodies, &constraints, &pair_constraints);

        // Phase 5 — the §2.6.4 terminal state.
        self.update_settle(bodies, &pose0);
    }

    /// Ray vs the environment surface. Bare-sphere environments take
    /// the analytic nearest positive root (`toi ≤ max_toi`); with
    /// terrain the same question is answered against the displaced
    /// surface — the ray's entry into the `radius ± amplitude` shell
    /// brackets a fixed 96-step scan for a sign change of
    /// `‖p(t)‖ − surface_radius(dir)`, refined by a fixed 40-step
    /// bisection (deterministic counts, like every other loop here).
    /// Stepped bodies are not raycast targets yet — hull-vs-
    /// ray still waits on the parry `ConvexPolyhedron` materialization
    /// (the M6-A vessel-vessel contact path is analytic and never
    /// needed it).
    fn cast_ray(&self, ray: &Ray, max_toi: f64) -> Option<Hit> {
        let oc = ray.origin - self.env.body_center;
        let b = oc.dot(ray.dir);
        if self.env.terrain.is_none() {
            let c = oc.dot(oc) - self.env.radius_m * self.env.radius_m;
            let disc = b * b - c;
            if disc < 0.0 {
                return None;
            }
            let sq = libm::sqrt(disc);
            let toi = if -b - sq >= 0.0 { -b - sq } else { -b + sq };
            if toi < 0.0 || toi > max_toi {
                return None;
            }
            let point = ray.origin + ray.dir * toi;
            let rel = point - self.env.body_center;
            return Some(Hit {
                toi,
                point,
                normal: rel / rel.norm(),
                body: None,
            });
        }
        // Terrain: `f(t) = ‖p(t) − c‖ − surface_radius(p̂(t))`, signed
        // distance to the displaced surface along the ray. March the
        // shell window for the first zero crossing (entry from
        // outside, or exit for an origin already under the surface).
        let amp = self
            .env
            .terrain
            .as_ref()
            .map_or(0.0, |t| t.amplitude_m)
            .abs();
        let r_out = self.env.radius_m + amp;
        let c_out = oc.dot(oc) - r_out * r_out;
        let (w0, mut w1) = if c_out > 0.0 {
            // Outside the outer bound — the entry/exit roots bracket
            // every possible surface crossing.
            let disc = b * b - c_out;
            if disc < 0.0 {
                return None;
            }
            let sq = libm::sqrt(disc);
            ((-b - sq).max(0.0), -b + sq)
        } else {
            // Already inside the shell — the far root bounds the scan.
            (0.0, -b + libm::sqrt((b * b - c_out).max(0.0)))
        };
        w1 = w1.min(max_toi);
        if !(w1 >= w0 && w1 > 0.0) {
            return None;
        }
        let eval = |toi: f64| -> f64 {
            let rel = ray.origin + ray.dir * toi - self.env.body_center;
            let d = rel.norm();
            if d <= 0.0 {
                return -1.0;
            }
            d - self.env.surface_radius(rel / d, self.time_s.get())
        };
        const STEPS: usize = 96;
        const BISECT: usize = 40;
        let mut ta = w0;
        let mut fa = eval(ta);
        for k in 1..=STEPS {
            let tb = w0 + (w1 - w0) * k as f64 / STEPS as f64;
            let fb = eval(tb);
            if (fa > 0.0) != (fb > 0.0) || fb == 0.0 {
                // Sign change in (ta, tb] — bisect a fixed count.
                let (mut lo, mut hi) = (ta, tb);
                let mut flo = fa;
                for _ in 0..BISECT {
                    let m = 0.5 * (lo + hi);
                    let fm = eval(m);
                    if (fm > 0.0) == (flo > 0.0) {
                        lo = m;
                        flo = fm;
                    } else {
                        hi = m;
                    }
                }
                let toi = 0.5 * (lo + hi);
                if toi > max_toi {
                    return None;
                }
                let point = ray.origin + ray.dir * toi;
                let rel = point - self.env.body_center;
                return Some(Hit {
                    toi,
                    point,
                    normal: rel / rel.norm(),
                    body: None,
                });
            }
            ta = tb;
            fa = fb;
        }
        None
    }

    fn contacts(&self, body: BodyHandle) -> &[Contact] {
        self.contacts
            .get(body.0 as usize)
            .map_or(&[], Vec::as_slice)
    }
}

/// The pre-voxel reference area: cross-section of the compound
/// collider's **velocity-aligned AABB** — project every hull vertex
/// (world frame) onto the plane ⊥ `w` and take the extent rectangle.
/// Deterministic (min/max are order-invariant), conservative for
/// non-convex unions, and cheap. The §2.4.1 voxel tables' per-
/// direction areas replace it at M4a.
fn cross_section_area(b: &WeldedBody, w: DVec3) -> f64 {
    let reference = if w.x.abs() <= w.y.abs() {
        DVec3::X
    } else {
        DVec3::Y
    };
    let t1_raw = w.cross(reference);
    let t1 = t1_raw / t1_raw.norm();
    let t2 = w.cross(t1);
    let (mut lo1, mut hi1, mut lo2, mut hi2) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for hull in &b.collider.hulls {
        for v in &hull.vertices {
            let p = b.pos + b.rot.rotate(*v);
            let s1 = p.dot(t1);
            let s2 = p.dot(t2);
            lo1 = lo1.min(s1);
            hi1 = hi1.max(s1);
            lo2 = lo2.min(s2);
            hi2 = hi2.max(s2);
        }
    }
    if hi1 < lo1 || hi2 < lo2 {
        return 0.0;
    }
    (hi1 - lo1) * (hi2 - lo2)
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::graph::{PartGraph, PartNode, PartNodeId, PartTransform};
    use crate::part::{ColliderShape, ForceSite, WeldPart};
    use crate::weld::weld;
    use csp_data::TemperatureKnot;

    fn test_thermal() -> csp_data::ThermalDef {
        csp_data::ThermalDef {
            heat_capacity_j_per_k: 100_000.0,
            max_temp_k: 1_200.0,
            emissivity: 0.8,
            conduction_w_per_k: 50.0,
        }
    }

    const G: f64 = 9.81;
    /// Terrin (§3.0): R = 600 km, μ = 3.5316e12 → surface g ≈ 9.81.
    const TERRIN_R: f64 = 600_000.0;
    const TERRIN_MU: f64 = 3.5316e12;

    fn terrin_env() -> Environment {
        Environment {
            body_center: DVec3::ZERO,
            radius_m: TERRIN_R,
            mu_m3_per_s2: TERRIN_MU,
            body_spin: DVec3::ZERO,
            atmosphere: None,
            terrain: None, // bare-sphere ground: the fixture's pinned model
            settle_v: DEFAULT_SETTLE_V,
            settle_w: DEFAULT_SETTLE_W,
            settle_ticks: DEFAULT_SETTLE_TICKS,
            friction_mu: DEFAULT_FRICTION_MU,
            drag_cd: DEFAULT_DRAG_CD,
        }
    }

    /// One-node graph welding to a box collider of half-extents `half`.
    fn box_body(mass_kg: f64, half: DVec3) -> WeldedBody {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        let parts = [WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
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
        }];
        weld(&g, &parts).unwrap()
    }

    fn step_n(backend: &mut SimpleBackend, bodies: &mut [WeldedBody], n: usize) {
        for _ in 0..n {
            backend.step(bodies, &[]);
        }
    }

    /// §2.9 oracle scenario 1 — free fall. Gravity is `μ/r²` toward the
    /// body centre; over a 1 s, ~5 m drop at 600 km radius the field
    /// varies by ~8e-6 relative, so the constant-g closed forms hold to
    /// far below the semi-implicit integrator's own O(dt) error, which
    /// is asserted separately (position lags the parabola by
    /// `≈ g·t·dt/2`, the known first-order deficit).
    #[test]
    fn free_fall_matches_analytic_gravity() {
        let mut backend = SimpleBackend::new(terrin_env());
        let mut b = box_body(100.0, DVec3::new(0.5, 0.5, 0.5));
        let r0 = TERRIN_R + 1_000.0;
        b.pos = DVec3::new(0.0, 0.0, r0);
        let mut bodies = vec![b];

        // First tick: acceleration is exactly −μ/r₀² (within f64 noise).
        backend.step(&mut bodies, &[]);
        let g0 = TERRIN_MU / (r0 * r0);
        let v1 = bodies[0].vel.z;
        assert!((v1 + g0 * PHYSICS_DT).abs() < 1e-12, "v1 = {v1}");

        // 1 s of fall. Closed form under constant g: v = −g·t,
        // x = x₀ − ½g·t²; semi-implicit Euler's position lags by
        // g·t·dt/2 ≈ 7.7 cm — assert the window, not false exactness.
        step_n(&mut backend, &mut bodies, 63);
        let t = 1.0;
        let v_expected = -g0 * t;
        assert!(
            (bodies[0].vel.z - v_expected).abs() < 1e-3,
            "v = {} vs {v_expected}",
            bodies[0].vel.z
        );
        let x_expected = r0 - 0.5 * g0 * t * t;
        let x_err = bodies[0].pos.z - x_expected;
        assert!(
            x_err.abs() < 0.5 * g0 * t * PHYSICS_DT * 1.5,
            "x lag {x_err}"
        );
    }

    /// §2.9 oracle scenario 2 — terminal velocity. Constant-density
    /// atmosphere (huge scale height) → quadratic drag; the discrete
    /// fixed point satisfies `½ρCdA·v² = mg` exactly (the force-
    /// balance equation is timestep-independent), so the asserted
    /// tolerance is solver convergence, not integrator error.
    #[test]
    fn drag_converges_to_terminal_velocity() {
        let atm = AtmosphereDef {
            sea_level_pressure_pa: 101_325.0,
            scale_height_m: 1.0e12, // ρ constant to ~1e-9 over the drop
            top_m: 100_000.0,
            molar_mass_kg_per_mol: 0.028_964_7,
            heat_capacity_ratio: 1.4,
            temperature_knots: vec![
                TemperatureKnot {
                    altitude_m: 0.0,
                    temperature_k: 288.15,
                },
                TemperatureKnot {
                    altitude_m: 100_000.0,
                    temperature_k: 288.15,
                },
            ],
        };
        let mut env = terrin_env();
        env.atmosphere = Some(atm);
        let mut backend = SimpleBackend::new(env);

        // m = 100, box half (2,2,0.5) falling −z → A = 16 m²,
        // v_t = sqrt(2mg/(ρCdA)) ≈ 10 m/s — slow enough that the body
        // converges (~7.5 s) before covering the 100 m drop.
        let m = 100.0;
        let area = 16.0;
        let mut b = box_body(m, DVec3::new(2.0, 2.0, 0.5));
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R + 100.5);
        let mut bodies = vec![b];

        step_n(&mut backend, &mut bodies, 480); // 7.5 s ≈ 7.3 τ
        // Still airborne — the measurement must precede touchdown.
        assert!(backend.contacts(BodyHandle(0)).is_empty());
        let rho = 101_325.0 * 0.028_964_7 / (8.314_462_618_153_24 * 288.15);
        let v_t = libm::sqrt(2.0 * m * G / (rho * 1.0 * area));
        let speed = -bodies[0].vel.z;
        assert!(
            (speed - v_t).abs() / v_t < 2e-3,
            "terminal {speed} vs closed form {v_t}"
        );
        // The real invariant: drag force balances weight at rest state.
        let drag = 0.5 * rho * 1.0 * area * speed * speed;
        assert!((drag - m * G).abs() / (m * G) < 2e-3);
        assert!(bodies[0].pos.z < TERRIN_R + 100.5); // it fell
        assert!(bodies[0].pos.z > TERRIN_R); // and is still falling free
    }

    /// §2.4's "all per-part forces are summed into (force, torque)":
    /// an off-axis thrust site changes momentum by `F·dt` and spins the
    /// body by `I⁻¹(r×F)·dt`; a site through the CoM produces none.
    #[test]
    fn thrust_produces_force_and_torque() {
        let mut env = terrin_env();
        env.mu_m3_per_s2 = 0.0; // force-model isolation: no gravity
        let mut backend = SimpleBackend::new(env);

        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        let half = DVec3::new(0.5, 0.5, 0.5);
        let parts = [WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: 10.0,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(10.0, half),
            collider: ColliderShape::Box { half_extents: half },
            force_sites: vec![ForceSite::new(
                DVec3::new(1.0, 0.0, 0.0),
                DVec3::Z,
                ForceSiteKind::Thrust,
                100.0,
            )],
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: test_thermal(),
        }];
        let mut b = weld(&g, &parts).unwrap();
        b.pos = DVec3::new(0.0, 0.0, 1.0e9); // far from the sphere
        b.throttle = 1.0;
        let mut bodies = vec![b];

        backend.step(&mut bodies, &[]);
        // Δv = F·dt/m = 100·(1/64)/10 = 0.15625, exactly representable.
        assert!((bodies[0].vel.z - 0.156_25).abs() < 1e-15);
        // τ = r×F = (1,0,0)×(0,0,100) = (0,−100,0);
        // ω_y = Iyy⁻¹·τ·dt, Iyy = m/3·(hx²+hz²) = 10/3·0.5.
        let iyy = 10.0 / 3.0 * 0.5;
        let expected_wy = -100.0 * PHYSICS_DT / iyy;
        assert!((bodies[0].ang_vel.y - expected_wy).abs() < 1e-15);
        assert!(bodies[0].ang_vel.x.abs() < 1e-20 && bodies[0].ang_vel.z.abs() < 1e-20);

        // Momentum accumulates linearly under sustained thrust.
        step_n(&mut backend, &mut bodies, 63);
        let p = bodies[0].momentum();
        // The body has rotated, so thrust is no longer purely +z; the
        // magnitude bound still holds: ‖Δp‖ ≤ F·t and > 0.
        assert!(p.norm() > 0.0 && p.norm() <= 100.0 * 1.0 + 1e-9);
    }

    /// **The M2 contact proof**: a rigid box dropped on the sphere
    /// surface must converge to rest with no jitter — the precursor to
    /// §2.9's 20 t-gear exit criterion. Asserts the velocity decays to
    /// solver noise, penetration stays bounded (≈ the Baumgarte
    /// equilibrium `g·dt²/β`-scale, millimetres), the pose reaches a
    /// *bitwise* fixed point before latch, and the §2.6.4 latch fires.
    #[test]
    fn box_dropped_on_sphere_converges_to_rest_and_latches() {
        let mut backend = SimpleBackend::new(terrin_env());
        let half = DVec3::new(1.0, 1.0, 1.0);
        let mut b = box_body(20_000.0, half); // the §2.9 20 t scale
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R + 1.5); // 0.5 m drop
        let mut bodies = vec![b];

        let mut latched_at = None;
        let mut last_positions = Vec::new();
        for tick in 0..4000 {
            backend.step(&mut bodies, &[]);
            last_positions.push(bodies[0].pos);
            if bodies[0].settle.latched {
                latched_at = Some(tick);
                break;
            }
        }
        let latched_at = latched_at.expect("box must latch within 4000 ticks");
        let b = &bodies[0];

        // Latched means exactly zero velocity by construction.
        assert_eq!(b.vel, DVec3::ZERO);
        assert_eq!(b.ang_vel, DVec3::ZERO);
        assert!(backend.contacts(BodyHandle(0)).is_empty());

        // Bounded penetration: the Baumgarte equilibrium is
        // ~g·dt²/(1−(1−β)⁸) ≈ 3 mm; assert < 5 cm, far inside the box.
        let bottom = b.pos.z - half.z;
        let penetration = TERRIN_R - bottom;
        assert!(
            penetration > 0.0 && penetration < 0.05,
            "resting penetration {penetration}"
        );

        // No jitter: over the last 32 ticks before latch the pose moves
        // at most micrometres per tick — the residual is monotonic
        // secular drift as the manifold equilibrates on the *curved*
        // sphere (the surface normal is radial, so off-axis vertices
        // have slight tangential components), not oscillatory noise.
        // Post-latch bitwise constancy is asserted below.
        let tail = &last_positions[latched_at - 32..latched_at];
        let max_step = tail
            .windows(2)
            .map(|w| (w[1] - w[0]).norm())
            .fold(0.0, f64::max);
        assert!(
            max_step < 2e-5,
            "pose jitter before latch: {max_step} m/tick"
        );

        // Post-latch: bitwise constant state — integration is skipped.
        let frozen = bodies[0].clone();
        step_n(&mut backend, &mut bodies, 200);
        assert_eq!(bodies[0], frozen, "latched body must not drift");
    }

    /// A tumbling box (initial ω about x) must still settle: contact
    /// friction dissipates rotation, the box lands flat, latches.
    #[test]
    fn tumbling_box_settles() {
        let mut backend = SimpleBackend::new(terrin_env());
        let half = DVec3::new(1.0, 0.5, 1.0);
        let mut b = box_body(5_000.0, half);
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R + 2.0);
        b.ang_vel = DVec3::new(0.5, 0.0, 0.0); // tumble about x
        let mut bodies = vec![b];

        let mut latched = false;
        for _ in 0..20_000 {
            backend.step(&mut bodies, &[]);
            if bodies[0].settle.latched {
                latched = true;
                break;
            }
        }
        assert!(latched, "tumbling box never latched");
    }

    /// §2.6.4: latch holds with zero throttle; a nonzero throttle
    /// unlatches and the body resumes integrating (here, falling back
    /// under gravity — there is no thrust site, so only the flag and
    /// the resumed dynamics are checked).
    #[test]
    fn latch_holds_and_throttle_unlatches() {
        let mut backend = SimpleBackend::new(terrin_env());
        let mut b = box_body(1_000.0, DVec3::new(1.0, 1.0, 1.0));
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R + 1.0);
        let mut bodies = vec![b];

        let mut latched = false;
        for _ in 0..4000 {
            backend.step(&mut bodies, &[]);
            if bodies[0].settle.latched {
                latched = true;
                break;
            }
        }
        assert!(latched);
        let pose = (bodies[0].pos, bodies[0].rot);
        step_n(&mut backend, &mut bodies, 64);
        assert_eq!((bodies[0].pos, bodies[0].rot), pose);

        // Throttle command → unlatch. With no thrust sites the body
        // simply resumes free dynamics; still resting in contact, it
        // immediately begins re-counting settled ticks and would
        // re-latch 128 ticks later — the latch flag clearing is the
        // unlatch guarantee (§2.6.4's "any throttle command unlatches
        // it first"), not a promise of motion.
        bodies[0].throttle = 0.5;
        backend.step(&mut bodies, &[]);
        assert!(!bodies[0].settle.latched);
    }

    /// The settle countdown requires *sustained* stillness: a body
    /// still falling must not accumulate settled ticks, and one that
    /// latches does so at exactly `settle_ticks` consecutive
    /// qualifying ticks.
    #[test]
    fn settle_counter_requires_sustained_contact() {
        let mut env = terrin_env();
        env.settle_ticks = DEFAULT_SETTLE_TICKS;
        let mut backend = SimpleBackend::new(env);
        let mut b = box_body(1_000.0, DVec3::new(1.0, 1.0, 1.0));
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R + 1.5);
        let mut bodies = vec![b];

        // While falling, no contact → counter stays 0.
        backend.step(&mut bodies, &[]);
        assert_eq!(bodies[0].settle.settled_ticks, 0);

        // Run to latch; the counter must reach `settle_ticks − 1`
        // while unlatched (the 128th qualifying tick flips the flag).
        let mut saw_full_count = false;
        for _ in 0..4000 {
            backend.step(&mut bodies, &[]);
            if bodies[0].settle.latched {
                break;
            }
            if bodies[0].settle.settled_ticks == DEFAULT_SETTLE_TICKS - 1 {
                saw_full_count = true;
            }
        }
        assert!(bodies[0].settle.latched);
        assert!(saw_full_count);
    }

    /// Fixed solver ordering: contacts enumerate in ascending
    /// `(body_id, contact_index)`; each body's manifold follows hull
    /// then vertex order. Two bodies at different longitudes get
    /// independent manifolds.
    #[test]
    fn contact_generation_order_is_fixed() {
        let mut backend = SimpleBackend::new(terrin_env());
        // Two hulls in one body → contacts in hull order.
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.0, 0.0, 3.0), DQuat::IDENTITY),
                1,
            ),
        );
        let mk = |m| WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: m,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(m, DVec3::new(0.5, 0.5, 0.5)),
            collider: ColliderShape::Box {
                half_extents: DVec3::new(0.5, 0.5, 0.5),
            },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: test_thermal(),
        };
        let parts = [mk(4.0), mk(4.0)];
        let mut b0 = weld(&g, &parts).unwrap();
        // Bottom hull at −0.5 dips 0.25 below the surface.
        b0.pos = DVec3::new(0.0, 0.0, TERRIN_R + 0.25);
        let mut b1 = box_body(4.0, DVec3::new(0.5, 0.5, 0.5));
        b1.pos = DVec3::new(100.0, 0.0, TERRIN_R + 0.25);
        let mut bodies = vec![b0, b1];

        backend.step(&mut bodies, &[]);
        let c0 = backend.contacts(BodyHandle(0));
        let c1 = backend.contacts(BodyHandle(1));
        // Lower hull's 4 bottom vertices penetrate; upper hull is clear.
        assert_eq!(c0.len(), 4);
        assert_eq!(c1.len(), 4);
        // All normals are radial ("up"), depths positive, ascending
        // within a manifold (equal-depth vertices keep vertex order).
        for c in c0.iter().chain(c1.iter()) {
            assert!(c.depth > 0.0);
            let up = (c.pos - backend.environment().body_center)
                / (c.pos - backend.environment().body_center).norm();
            assert!((c.normal - up).norm() < 1e-12);
        }
        // Body 1's contacts sit at its own longitude.
        assert!(c1.iter().all(|c| c.pos.x > 90.0));
    }

    /// Bitwise determinism: two identical runs produce identical body
    /// state — the §7.1 hash-pipeline property, asserted on the types
    /// themselves (`PartialEq` on `f64` is bitwise).
    #[test]
    fn two_runs_are_bitwise_identical() {
        let run = || {
            let mut backend = SimpleBackend::new(terrin_env());
            let mut b = box_body(2_000.0, DVec3::new(1.0, 0.5, 0.75));
            b.pos = DVec3::new(0.0, 0.0, TERRIN_R + 3.0);
            b.ang_vel = DVec3::new(0.2, -0.1, 0.05);
            let mut bodies = vec![b];
            step_n(&mut backend, &mut bodies, 3_000);
            bodies
        };
        assert_eq!(run(), run());
    }

    /// `cast_ray` against the environment sphere: straight-down hit at
    /// the computed toi with an outward normal; a miss returns `None`.
    #[test]
    fn cast_ray_hits_ground_sphere() {
        let backend = SimpleBackend::new(terrin_env());
        let hit = backend
            .cast_ray(
                &Ray {
                    origin: DVec3::new(0.0, 0.0, TERRIN_R + 100.0),
                    dir: -DVec3::Z,
                },
                1_000.0,
            )
            .unwrap();
        assert!((hit.toi - 100.0).abs() < 1e-9);
        assert!((hit.point.z - TERRIN_R).abs() < 1e-9);
        assert!((hit.normal - DVec3::Z).norm() < 1e-12);
        assert!(hit.body.is_none());

        // Pointing away from the surface: no hit within range.
        assert!(
            backend
                .cast_ray(
                    &Ray {
                        origin: DVec3::new(0.0, 0.0, TERRIN_R + 100.0),
                        dir: DVec3::X,
                    },
                    1_000.0,
                )
                .is_none()
        );
        // max_toi bound is honoured.
        assert!(
            backend
                .cast_ray(
                    &Ray {
                        origin: DVec3::new(0.0, 0.0, TERRIN_R + 100.0),
                        dir: -DVec3::Z,
                    },
                    50.0,
                )
                .is_none()
        );
    }

    /// The §3.0 closed forms: exponential pressure with the hard cut
    /// at `top_m`, clamped below the datum, piecewise-linear
    /// temperature, ideal-gas density.
    #[test]
    fn atmosphere_closed_forms() {
        let atm = AtmosphereDef {
            sea_level_pressure_pa: 101_325.0,
            scale_height_m: 5_600.0,
            top_m: 70_000.0,
            molar_mass_kg_per_mol: 0.028_964_7,
            heat_capacity_ratio: 1.4,
            temperature_knots: vec![
                TemperatureKnot {
                    altitude_m: 0.0,
                    temperature_k: 288.0,
                },
                TemperatureKnot {
                    altitude_m: 10_000.0,
                    temperature_k: 220.0,
                },
                TemperatureKnot {
                    altitude_m: 70_000.0,
                    temperature_k: 250.0,
                },
            ],
        };
        // p(H·ln2) = p0/2.
        let h_half = 5_600.0 * libm::log(2.0);
        let p = atmosphere_pressure_pa(&atm, h_half);
        assert!((p / 101_325.0 - 0.5).abs() < 1e-12);
        assert_eq!(atmosphere_pressure_pa(&atm, 70_000.0), 0.0);
        assert_eq!(atmosphere_pressure_pa(&atm, -50.0), 101_325.0);
        // Temperature: midpoint of the first segment is linear.
        assert_eq!(atmosphere_temperature_k(&atm, 5_000.0), 254.0);
        assert_eq!(atmosphere_temperature_k(&atm, -1.0), 288.0);
        assert_eq!(atmosphere_temperature_k(&atm, 80_000.0), 250.0);
        // ρ = pM/(RT) at sea level ≈ 1.225.
        let rho = atmosphere_density_kg_m3(&atm, 0.0);
        assert!((rho - 1.225).abs() < 0.01);
        assert_eq!(atmosphere_density_kg_m3(&atm, 70_000.0), 0.0);
    }

    /// Two equal boxes welded 3 m apart on z: `com_local = (0,0,1.5)`
    /// — the `com_local ≠ 0` configuration the origin-vs-CoM
    /// integration bug needs to be observable.
    fn offset_com_body() -> WeldedBody {
        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(2),
            PartNode::new(
                Some(PartNodeId(1)),
                PartTransform::new(DVec3::new(0.0, 0.0, 3.0), DQuat::IDENTITY),
                1,
            ),
        );
        let mk = |m| WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: m,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(m, DVec3::new(0.5, 0.5, 0.5)),
            collider: ColliderShape::Box {
                half_extents: DVec3::new(0.5, 0.5, 0.5),
            },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: test_thermal(),
        };
        let parts = [mk(4.0), mk(4.0)];
        let b = weld(&g, &parts).unwrap();
        assert!((b.com_local - DVec3::new(0.0, 0.0, 1.5)).norm() < 1e-12);
        b
    }

    /// **Origin-vs-CoM blocker, ballistic half.** A spinning body whose
    /// CoM is offset from its origin must still trace the analytic
    /// free-fall parabola in `com_world` — integrating `pos` directly
    /// would orbit the CoM around the origin at radius
    /// `‖com_local‖ = 1.5 m` (this assertion fails pre-fix by ~1.4 m).
    #[test]
    fn offset_com_com_follows_free_fall_parabola() {
        let mut backend = SimpleBackend::new(terrin_env());
        let mut b = offset_com_body();
        let r0 = TERRIN_R + 1_000.0;
        b.pos = DVec3::new(0.0, 0.0, r0);
        // Spin about x — perpendicular to the z-offset CoM, so the
        // buggy path swings com_world in the y–z plane.
        b.ang_vel = DVec3::new(0.5, 0.0, 0.0);
        let mut bodies = vec![b];
        let com0 = bodies[0].com_world();

        step_n(&mut backend, &mut bodies, 64);
        let t = 1.0;
        let g0 = TERRIN_MU / (r0 * r0);
        let com = bodies[0].com_world();

        // No horizontal force exists — the CoM must not wander. The
        // pre-fix orbit gives com.y = ±1.5·sin(ωt) ≈ 0.7 m here.
        assert!(
            com.x.abs() < 1e-9 && com.y.abs() < 1e-9,
            "CoM wandered off the parabola: {com:?}"
        );
        // Vertical: same semi-implicit window as the com_local = 0
        // free-fall oracle (lag ≈ g·t·dt/2).
        let z_expected = com0.z - 0.5 * g0 * t * t;
        let z_err = com.z - z_expected;
        assert!(
            z_err.abs() < 0.5 * g0 * t * PHYSICS_DT * 1.5,
            "z lag {z_err}"
        );
    }

    /// **Origin-vs-CoM blocker, kinematic half.** Torque-free spin
    /// about a principal axis (the body's z-offset masses make x a
    /// principal axis of the symmetric-top inertia) with a constant
    /// CoM velocity: the CoM must ride a straight line and `‖L_world‖`
    /// must be exactly conserved — the buggy integrator circles the
    /// CoM at radius 1.5 m around that line instead.
    #[test]
    fn offset_com_torque_free_spin_keeps_com_on_straight_line() {
        let mut env = terrin_env();
        env.mu_m3_per_s2 = 0.0; // no forces at all
        env.radius_m = 0.0; // degenerate ground sphere: nothing can contact it
        let mut backend = SimpleBackend::new(env);
        let mut b = offset_com_body();
        let v0 = DVec3::new(1.0, -0.5, 2.0);
        b.vel = v0;
        b.ang_vel = DVec3::new(0.5, 0.0, 0.0);
        let mut bodies = vec![b];
        let com0 = bodies[0].com_world();
        let l0 = bodies[0]
            .rot
            .rotate(bodies[0].angular_momentum_local())
            .norm();

        step_n(&mut backend, &mut bodies, 128); // 2 s
        let expected = com0 + v0 * 2.0;
        let err = (bodies[0].com_world() - expected).norm();
        // Constant-v integration is exact up to f64 rounding; ~1e-12
        // relative is the honest bound (pre-fix error is ~1.5 m).
        let rel = err / (1.0 + (expected - com0).norm());
        assert!(rel < 1e-12, "CoM straight-line rel error {rel}");
        let l1 = bodies[0]
            .rot
            .rotate(bodies[0].angular_momentum_local())
            .norm();
        assert!(
            (l1 - l0).abs() / l0 < 1e-12,
            "‖L_world‖ drift {}",
            (l1 - l0) / l0
        );
    }

    /// Coulomb friction, kinetic half: a box sliding *diagonally* (so
    /// both friction basis directions engage — the per-direction box
    /// clamp would admit √2·μ·g there) decelerates at `μ·g` within
    /// solver tolerance, along its slide direction.
    #[test]
    fn sliding_box_decelerates_at_mu_g() {
        let mut backend = SimpleBackend::new(terrin_env());
        let half = DVec3::new(1.0, 1.0, 0.5);
        let mut b = box_body(2_000.0, half);
        // Resting bite: the bottom face starts 1 mm inside so contacts
        // exist on tick one.
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R + half.z - 0.001);
        b.vel = DVec3::new(1.5, 1.5, 0.0); // |v0| ≈ 2.12 m/s at 45°
        let mut bodies = vec![b];
        let g0 = TERRIN_MU / (TERRIN_R * TERRIN_R);

        // 16 ticks while clearly sliding: v ≈ v0 − μg·t ≈ 0.89.
        step_n(&mut backend, &mut bodies, 16);
        let v = bodies[0].vel;
        let expected_speed = 1.5 * libm::sqrt(2.0) - DEFAULT_FRICTION_MU * g0 * 16.0 * PHYSICS_DT;
        assert!(
            (v.norm() - expected_speed).abs() / expected_speed < 0.15,
            "deceleration: |v| = {} vs μg model {expected_speed}",
            v.norm()
        );
        // Disc projection brakes along the slide axis, not the box
        // clamp's axes — direction stays the 45° diagonal.
        let dir = v / v.norm();
        let want = DVec3::new(1.0, 1.0, 0.0) / libm::sqrt(2.0);
        assert!(
            (dir - want).norm() < 0.1,
            "slide veered off diagonal: {dir:?}"
        );
    }

    /// Coulomb friction, static half. The contact sphere's normal is
    /// always parallel to gravity, so a literal inclined plane does not
    /// exist in this environment — the exact equivalent of "incline,
    /// tanθ < μ ⇒ no slide" is a sustained tangential driving force
    /// `F = m·g·sinθ < μ·N` applied at the CoM of a resting box (here
    /// by a thrust site). The box must never gain sustained velocity.
    ///
    /// Note the pinned phase order leaves a first-order *creep* even at
    /// rest: each tick integrates `pos += v·dt` with the just-applied
    /// force before friction re-zeroes `v`, so the pose ratchets
    /// `F·dt²/m` per tick — identical in kind to the normal-direction
    /// `g·dt²` sag the Baumgarte pass then removes. "No slide" is
    /// therefore asserted as *drift ≈ the creep bound and no
    /// accumulated velocity*, vs the `~½·a·t²` of a slipping body.
    #[test]
    fn incline_equivalent_below_friction_cone_does_not_slide() {
        let mut backend = SimpleBackend::new(terrin_env());
        let theta = 0.35; // ≈ 20° — tan ≈ 0.365 < μ = 0.5
        let m = 2_000.0;
        let half = DVec3::new(1.0, 1.0, 0.5);
        let g0 = TERRIN_MU / (TERRIN_R * TERRIN_R);
        let drive = m * g0 * libm::sin(theta); // m·g·sinθ at the CoM

        let mut g = PartGraph::new(PartNodeId(1));
        g.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        let parts = [WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: m,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(m, half),
            collider: ColliderShape::Box { half_extents: half },
            force_sites: vec![ForceSite::new(
                DVec3::ZERO,
                DVec3::X,
                ForceSiteKind::Thrust,
                drive,
            )],
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: test_thermal(),
        }];
        let mut b = weld(&g, &parts).unwrap();
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R + half.z - 0.001);
        b.throttle = 1.0;
        let mut bodies = vec![b];
        let com0 = bodies[0].com_world();

        let mut max_speed = 0.0f64;
        for _ in 0..1000 {
            backend.step(&mut bodies, &[]);
            max_speed = max_speed.max(bodies[0].vel.norm());
        }
        // Never gains sustained velocity: sub-cone drive is absorbed by
        // static friction every tick. (A slipping body would hold
        // ~(F/m − μ·g) ≈ bounded too — the drift bound is the real
        // discriminator; the speed bound kills sustained sliding.)
        assert!(max_speed < 0.2, "box slid: max speed {max_speed}");
        // Creep bound: N·F·dt²/m ≈ 0.82 m over 1000 ticks; a slipping
        // body covers ~½·(F/m)·t²·(1−tanθ/μ) — metres to hundreds of
        // metres. Factor-3 headroom on the creep model.
        let creep = 1000.0 * drive * PHYSICS_DT * PHYSICS_DT / m;
        let drift = (bodies[0].com_world() - com0).norm();
        assert!(drift < 3.0 * creep, "drift {drift} vs creep bound {creep}");
    }

    /// Torque-free precession: fully asymmetric inertia with `ω₀` off
    /// every principal axis exercises the gyroscopic `ω×(Iω)` term —
    /// `ω` must visibly precess while `‖L_world‖ = ‖I·ω‖` stays
    /// constant to first-order-integrator accuracy (the discrete
    /// update grows `‖L‖` by `O((ω·dt)²)` per step; at this `ω` scale
    /// the 2 s drift is ~1e-6, inside the 1e-5 bound).
    #[test]
    fn torque_free_precession_conserves_world_angular_momentum() {
        let mut env = terrin_env();
        env.mu_m3_per_s2 = 0.0;
        env.radius_m = 0.0; // no contacts possible
        let mut backend = SimpleBackend::new(env);
        // hx ≠ hy ≠ hz ⇒ Ixx ≠ Iyy ≠ Izz.
        let half = DVec3::new(0.4, 0.7, 1.1);
        let mut b = box_body(80.0, half);
        let w0 = DVec3::new(0.025, 0.045, -0.03);
        b.ang_vel = w0;
        let mut bodies = vec![b];
        let l0 = bodies[0]
            .rot
            .rotate(bodies[0].angular_momentum_local())
            .norm();

        step_n(&mut backend, &mut bodies, 128); // 2 s
        let l1 = bodies[0]
            .rot
            .rotate(bodies[0].angular_momentum_local())
            .norm();
        let drift = (l1 - l0).abs() / l0;
        assert!(drift < 1e-5, "‖L_world‖ relative drift {drift}");
        // The gyro term did work — ω genuinely precessed.
        assert!(
            (bodies[0].ang_vel - w0).norm() > 1e-4,
            "no precession: ω unchanged"
        );
    }

    /// `body_spin ≠ 0`: the contact solve brakes against the *moving*
    /// surface `v_surf = body_spin × rel`, not inertial rest. A body
    /// dropped inertial-still on the equator is dragged toward
    /// co-rotation; a body spawned co-rotating is *not* braked — the
    /// pre-fix static-surface solve would have stopped it.
    #[test]
    fn contact_friction_brakes_against_moving_surface() {
        let mut env = terrin_env();
        env.body_spin = DVec3::new(0.0, 0.0, 1.0e-5); // ~6 m/s surface at this radius
        // Keep the settle latch out of the picture — a latched body has
        // vel ≡ 0 and the relative-speed check below could not tell
        // "latched" from the pre-fix "braked to inertial rest" bug.
        env.settle_ticks = u32::MAX;
        let mut backend = SimpleBackend::new(env);
        let half = DVec3::new(1.0, 1.0, 0.5);
        // Equator of the z-spin: v_surf at (0, R, 0) = ω·R·(ẑ×ŷ) = −6·x̂.
        let v_surf = DVec3::new(-1.0e-5 * TERRIN_R, 0.0, 0.0);
        let mut still = box_body(2_000.0, half);
        still.pos = DVec3::new(0.0, TERRIN_R + half.z - 0.001, 0.0);
        let mut moving = box_body(2_000.0, half);
        moving.pos = DVec3::new(50.0, TERRIN_R + half.z - 0.001, 0.0);
        moving.vel = v_surf; // already co-rotating
        let mut bodies = vec![still, moving];

        step_n(&mut backend, &mut bodies, 192); // 3 s
        // Inertial-still body: surface friction dragged it toward −6
        // x̂ (μ·g ≈ 4.9 m/s² covers the gap in ~1.3 s).
        let rel0 = (bodies[0].vel - v_surf).norm();
        assert!(
            rel0 < 1.0,
            "still body not dragged to co-rotation: rel {rel0}, v {:?}",
            bodies[0].vel
        );
        assert!(bodies[0].vel.x < -4.0);
        // Co-rotating body: no relative slide → no brake. Pre-fix it
        // would be braked to inertial rest (rel → ‖v_surf‖ = 6).
        let rel1 = (bodies[1].vel - v_surf).norm();
        assert!(rel1 < 0.5, "co-rotating body was braked: rel {rel1}");
    }

    /// The `settle_w` gate alone: with `‖v‖ ≈ 0` already satisfied, a
    /// body still spinning above `settle_w` must accumulate **zero**
    /// settled ticks — the two gates are independent. Low μ slows the
    /// friction damping so the blocked window spans many ticks.
    #[test]
    fn settle_w_gate_blocks_latch_while_spinning() {
        let mut env = terrin_env();
        env.settle_ticks = 8;
        env.friction_mu = 0.002; // damp the 0.2 rad/s spin over ~seconds
        let settle_w = env.settle_w;
        let mut backend = SimpleBackend::new(env);
        let mut b = box_body(2_000.0, DVec3::new(1.0, 1.0, 0.5));
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R + 0.5 - 0.001);
        b.ang_vel = DVec3::new(0.0, 0.0, 0.2); // spin about vertical, > settle_w
        let mut bodies = vec![b];

        // While ‖ω‖ > settle_w the counter must stay 0 — ‖v‖ ≈ 0 is
        // already true, so only the w-gate can be blocking.
        for _ in 0..200 {
            backend.step(&mut bodies, &[]);
            if bodies[0].ang_vel.norm() > settle_w {
                assert_eq!(
                    bodies[0].settle.settled_ticks,
                    0,
                    "settle counted at ω = {}",
                    bodies[0].ang_vel.norm()
                );
            }
        }
        // Friction eventually damps ω below settle_w; then the latch
        // fires within settle_ticks + slack.
        let mut latched = false;
        for _ in 0..2000 {
            backend.step(&mut bodies, &[]);
            if bodies[0].settle.latched {
                latched = true;
                break;
            }
        }
        assert!(latched, "damped spinner never latched");
    }

    /// **M6-A: two vessels side by side.** Two boxes spawned on the
    /// sphere with a deliberate lateral overlap must not
    /// interpenetrate: the pair manifold pushes them apart along the
    /// penetrated face normal, they come to rest in ground contact,
    /// and both latch — after which the state is bitwise frozen.
    /// `vessel_contact` is the geometric predicate of the same rule
    /// generation applies, so it must agree with the recorded
    /// manifolds.
    #[test]
    fn two_boxes_pushed_apart_do_not_interpenetrate() {
        let mut backend = SimpleBackend::new(terrin_env());
        let half = DVec3::new(0.5, 0.5, 0.5);
        // a at x = −0.2 spans [−0.7, 0.3]; b at +0.7 spans [0.2, 1.2]
        // → 0.1 m of lateral overlap over the shared face area.
        let mut a = box_body(1_000.0, half);
        a.pos = DVec3::new(-0.2, 0.0, TERRIN_R + half.z - 0.001);
        let mut b = box_body(1_000.0, half);
        b.pos = DVec3::new(0.7, 0.0, TERRIN_R + half.z - 0.001);
        let mut bodies = vec![a, b];

        assert!(backend.vessel_contact(&bodies[0], &bodies[1]));
        backend.step(&mut bodies, &[]);
        // The pair generated contacts on BOTH records, peer-tagged.
        assert!(
            backend
                .contacts(BodyHandle(0))
                .iter()
                .any(|c| c.peer == Some(BodyHandle(1)))
        );
        assert!(
            backend
                .contacts(BodyHandle(1))
                .iter()
                .any(|c| c.peer == Some(BodyHandle(0)))
        );

        let mut both_latched = false;
        for _ in 0..8_000 {
            backend.step(&mut bodies, &[]);
            // Never deep-interpenetrated: the hull AABB overlap stays
            // bounded to resting equilibrium (≪ the 0.1 spawn bite).
            assert!(
                !deep_overlap(&bodies[0], &bodies[1]),
                "vessels interpenetrated past tolerance"
            );
            if bodies[0].settle.latched && bodies[1].settle.latched {
                both_latched = true;
                break;
            }
        }
        assert!(both_latched, "contacting pair never settled");
        // The 0.1 m spawn bite resolved to resting equilibrium: the
        // boxes may still be touching (leaning contact is a real
        // resting state — resting equilibrium keeps vertices ~mm
        // inside, like the ground) but nothing deep remains.
        assert!(
            max_vertex_penetration(&bodies[0], &bodies[1]) < 0.02,
            "residual interpenetration too deep"
        );
        // Frozen once latched.
        let frozen = (bodies[0].clone(), bodies[1].clone());
        step_n(&mut backend, &mut bodies, 128);
        assert_eq!((bodies[0].clone(), bodies[1].clone()), frozen);
    }

    /// Deepest vertex penetration between the two bodies' hulls, in
    /// either direction (0.0 = none) — the same facet-penetration
    /// measurement generation uses.
    fn max_vertex_penetration(a: &WeldedBody, b: &WeldedBody) -> f64 {
        let into = |p: &WeldedBody, q: &WeldedBody| {
            let mut worst: f64 = 0.0;
            for hq in &q.collider.hulls {
                let Some(planes) = crate::hull::facet_planes(
                    &hq.vertices,
                    crate::hull::plane_epsilon(&hq.vertices),
                ) else {
                    continue;
                };
                for hp in &p.collider.hulls {
                    for v in &hp.vertices {
                        let p_w = p.pos + p.rot.rotate(*v);
                        let p_q = q.rot.conjugate().rotate(p_w - q.pos);
                        for (_, d) in crate::hull::point_penetration(
                            &planes,
                            p_q,
                            crate::hull::plane_epsilon(&hq.vertices),
                        ) {
                            worst = worst.max(d);
                        }
                    }
                }
            }
            worst
        };
        into(a, b).max(into(b, a))
    }

    /// Is `b`'s geometry buried deeper than `tol` inside `a`'s (or
    /// vice versa)? The interpenetration-depth assertion several
    /// tests need.
    fn deep_overlap(a: &WeldedBody, b: &WeldedBody) -> bool {
        const TOL: f64 = 0.05; // 5 cm — resting equilibrium is ~mm scale
        max_vertex_penetration(a, b) > TOL
    }

    /// **M6-A: approach resolves correctly.** A free-space box
    /// moving along +x into a resting box transfers momentum through
    /// the pair manifold: the striker loses speed, the target gains
    /// +x velocity, and total linear momentum is conserved to solver
    /// tolerance — impulses are applied equal-and-opposite by
    /// construction.
    #[test]
    fn moving_box_pushes_resting_box_and_conserves_momentum() {
        let mut env = terrin_env();
        env.mu_m3_per_s2 = 0.0;
        env.radius_m = 0.0; // no ground sphere: pure pair dynamics
        let mut backend = SimpleBackend::new(env);
        let half = DVec3::new(0.5, 0.5, 0.5);
        let mut a = box_body(1_000.0, half);
        a.pos = DVec3::new(-3.0, 0.0, 0.0);
        a.vel = DVec3::new(2.0, 0.0, 0.0);
        let mut b = box_body(4_000.0, half);
        b.pos = DVec3::new(0.0, 0.0, 0.0);
        let mut bodies = vec![a, b];
        let p0 = bodies[0].momentum() + bodies[1].momentum();

        step_n(&mut backend, &mut bodies, 192); // 3 s
        // The striker hit the target's −x face (n = −x̂ on a's
        // record): a decelerated (may even recoil), b moved +x.
        assert!(
            bodies[1].vel.x > 0.05,
            "target not pushed: v_b {:?}",
            bodies[1].vel
        );
        assert!(bodies[1].pos.x > 0.05, "target not displaced");
        assert!(bodies[0].vel.x < 2.0, "striker did not lose speed");
        // Momentum: equal-and-opposite impulses conserve the total.
        let p1 = bodies[0].momentum() + bodies[1].momentum();
        let drift = (p1 - p0).norm() / p0.norm().max(1.0);
        assert!(drift < 1e-9, "linear momentum drift {drift}");
        // No deep interpenetration at any point of the collision:
        // re-run and assert per tick this time.
        let mut a = box_body(1_000.0, half);
        a.pos = DVec3::new(-3.0, 0.0, 0.0);
        a.vel = DVec3::new(2.0, 0.0, 0.0);
        let mut b = box_body(4_000.0, half);
        b.pos = DVec3::new(0.0, 0.0, 0.0);
        let mut bodies2 = vec![a, b];
        for _ in 0..192 {
            backend.step(&mut bodies2, &[]);
            assert!(!deep_overlap(&bodies2[0], &bodies2[1]));
        }
    }

    /// **M6-A: stacking.** A vessel resting ON another vessel is
    /// stable: the upper body's vertices inside the lower's hull
    /// generate the support manifold; both come to rest (the upper
    /// counts settled ticks on its pair contacts like it would on
    /// ground contacts) and both latch.
    #[test]
    fn vessel_resting_on_vessel_is_stable() {
        let mut backend = SimpleBackend::new(terrin_env());
        let half = DVec3::new(1.0, 1.0, 0.5);
        // Lower: bottom 1 mm inside the ground.
        let mut lower = box_body(5_000.0, half);
        lower.pos = DVec3::new(0.0, 0.0, TERRIN_R + half.z - 0.001);
        // Upper: bottom face 2 mm inside the lower's top face
        // (lower top ≈ R + 0.999, so upper centre ≈ R + 1.497).
        let mut upper = box_body(500.0, half);
        upper.pos = DVec3::new(0.0, 0.0, TERRIN_R + 3.0 * half.z - 0.002);
        let mut bodies = vec![lower, upper];

        let mut both_latched = false;
        for _ in 0..12_000 {
            backend.step(&mut bodies, &[]);
            assert!(
                !deep_overlap(&bodies[0], &bodies[1]),
                "stack interpenetrated"
            );
            if bodies[0].settle.latched && bodies[1].settle.latched {
                both_latched = true;
                break;
            }
        }
        assert!(both_latched, "stack never settled");
        // The upper did not fall through: its bottom face is still
        // above the lower's mid-plane.
        assert!(bodies[1].pos.z > TERRIN_R + 2.0 * half.z);
        let frozen = (bodies[0].clone(), bodies[1].clone());
        step_n(&mut backend, &mut bodies, 64);
        assert_eq!((bodies[0].clone(), bodies[1].clone()), frozen);
    }

    /// **M6-A determinism**: a two-vessel bubble run is bitwise
    /// reproducible — the same property the single-body run asserts,
    /// now covering the pair manifold's fixed ordering.
    #[test]
    fn two_vessel_run_is_bitwise_deterministic() {
        let run = || {
            let mut backend = SimpleBackend::new(terrin_env());
            let half = DVec3::new(0.5, 0.5, 0.5);
            let mut a = box_body(1_000.0, half);
            a.pos = DVec3::new(-0.2, 0.0, TERRIN_R + half.z - 0.001);
            let mut b = box_body(2_000.0, half);
            b.pos = DVec3::new(0.7, 0.0, TERRIN_R + half.z - 0.001);
            let mut upper = box_body(300.0, half);
            upper.pos = DVec3::new(0.7, 0.0, TERRIN_R + 1.6);
            let mut bodies = vec![a, b, upper];
            step_n(&mut backend, &mut bodies, 2_000);
            bodies
        };
        assert_eq!(run(), run());
    }

    /// The Baumgarte cap and the pose-delta settle gate together: a
    /// body spawned buried 1.5 m deep is raised at ≤
    /// [`MAX_TICK_POS_CORRECTION`] per tick, and while it is being
    /// dragged upward with `v ≈ ω ≈ 0` the settle counter stays at
    /// zero — the velocity-only gate would have latched it mid-drag.
    #[test]
    fn position_correction_is_capped_and_blocks_settle() {
        let mut env = terrin_env();
        env.settle_ticks = 8;
        let mut backend = SimpleBackend::new(env);
        let half = DVec3::new(1.0, 1.0, 1.0);
        let mut b = box_body(20_000.0, half);
        b.pos = DVec3::new(0.0, 0.0, TERRIN_R - 0.5); // bottom 1.5 m deep
        let mut bodies = vec![b];

        let mut prev = bodies[0].pos;
        let mut saw_capped_tick = false;
        for _ in 0..4 {
            backend.step(&mut bodies, &[]);
            let rise = bodies[0].pos.z - prev.z;
            assert!(
                rise <= MAX_TICK_POS_CORRECTION + 1e-9,
                "uncapped correction: {rise} m in one tick"
            );
            saw_capped_tick |= rise > 0.05;
            prev = bodies[0].pos;
            assert!(
                bodies[0].vel.norm() < DEFAULT_SETTLE_V,
                "test premise: v should read still, got {:?}",
                bodies[0].vel
            );
            assert_eq!(
                bodies[0].settle.settled_ticks, 0,
                "pose gate failed: counted while rising {rise} m/tick"
            );
        }
        assert!(saw_capped_tick, "cap never engaged");

        // It still climbs out and latches — the cap only rates the
        // correction, it does not prevent rest.
        let mut latched = false;
        for _ in 0..400 {
            backend.step(&mut bodies, &[]);
            if bodies[0].settle.latched {
                latched = true;
                break;
            }
        }
        assert!(latched, "buried box never reached rest/latch");
    }

    // ---- R2: the seeded heightfield IS the contact surface ----

    /// The ground the solver contacts is the same displaced field the
    /// renderer draws: a box dropped above a hill latches and rests
    /// with its lowest vertex at the terrain radius
    /// `radius_m + h·amplitude` — NOT the bare datum sphere (the
    /// pre-terrain model let the box sink ~amplitude metres below the
    /// drawn ground wherever `h > 0` — the "penetrate the ground
    /// before the sim notices" defect).
    #[test]
    fn dropped_box_rests_on_terrain_not_datum() {
        // A small bake keeps the test fast — `with_field` drives the
        // identical `sample_dir` path the full-size field uses.
        let seed = 601u64;
        let amp = TERRIN_R * AMPLITUDE_FRAC; // 2.4 km at Terrin
        let field = HeightField::bake(seed, 128, 64);
        // Drop on a hill: the strongest upward deviation in a 64-dir
        // scan, so "rests at terrain, not at datum" discriminates by
        // ~kilometres rather than millimetres.
        let mut dir = DVec3::X;
        let mut h_best = f64::NEG_INFINITY;
        for k in 0..64 {
            let t = 2.0 * core::f64::consts::PI * k as f64 / 64.0;
            let d = DVec3::new(libm::cos(t), libm::sin(t), 0.35);
            let d = d / d.norm();
            let h = field.sample_dir(d);
            if h > h_best {
                dir = d;
                h_best = h;
            }
        }
        let mut env = terrin_env();
        env.terrain = Some(TerrainSurface::with_field(field, seed, amp));
        env.settle_ticks = 32;
        let surf_r = env.surface_radius(dir, 0.0);
        assert!(
            surf_r - env.radius_m > 0.2 * amp,
            "test premise: no real hill in the scan (best h {h_best})"
        );
        let mut backend = SimpleBackend::new(env.clone());
        let mut b = box_body(1_000.0, DVec3::new(0.5, 0.5, 0.5));
        b.pos = dir * (surf_r + 3.0); // belly ~2.5 m over the surface
        let mut bodies = vec![b];
        let mut latched = false;
        for _ in 0..4_096 {
            backend.step(&mut bodies, &[]);
            if bodies[0].settle.latched {
                latched = true;
                break;
            }
        }
        assert!(latched, "dropped box never latched on terrain");
        let b = &bodies[0];
        // Clearance of each collider vertex above the terrain surface
        // under its own direction — the same `sample_dir` the shader
        // displaces to. The lowest must sit ON the surface, not below.
        let lo_terrain = b
            .collider
            .hulls
            .iter()
            .flat_map(|hull| hull.vertices.iter().copied())
            .map(|v| {
                let rel = b.pos + b.rot.rotate(v) - env.body_center;
                let d = rel.norm();
                d - env.surface_radius(rel / d, 0.0)
            })
            .fold(f64::INFINITY, f64::min);
        assert!(
            lo_terrain.abs() < 0.5,
            "resting box floats/sinks {lo_terrain} m vs the terrain surface"
        );
        // And the discriminator: the lowest vertex is ~amplitude-scale
        // OFF the bare datum — a datum-contact solve would have sunk it.
        let lo_datum = b
            .collider
            .hulls
            .iter()
            .flat_map(|hull| hull.vertices.iter().copied())
            .map(|v| (b.pos + b.rot.rotate(v) - env.body_center).norm() - env.radius_m)
            .fold(f64::INFINITY, f64::min);
        assert!(
            lo_datum > 0.1 * amp,
            "box rests only {lo_datum} m above datum — sank to the sphere"
        );
    }

    /// `ground_contact`/`cast_ray` predicate parity: a vertex just
    /// above the local terrain height is clear, just below is contact
    /// — and a downward ray reports its hit AT the displaced surface.
    #[test]
    fn terrain_predicates_see_the_displaced_surface() {
        let seed = 7u64;
        let amp = 100.0; // exaggerated, keeps the test readable
        let field = HeightField::bake(seed, 64, 32);
        let mut env = terrin_env();
        env.terrain = Some(TerrainSurface::with_field(field, seed, amp));
        let dir = DVec3::X;
        let surf_r = env.surface_radius(dir, 0.0);
        let backend = SimpleBackend::new(env.clone());
        let mut b = box_body(100.0, DVec3::new(0.5, 0.5, 0.5));
        b.pos = dir * (surf_r + 1.0); // bottom vertex 0.5 m clear
        assert!(!backend.ground_contact(&b));
        b.pos = dir * (surf_r + 0.4); // bottom vertex 0.1 m buried
        assert!(backend.ground_contact(&b));
        let hit = backend
            .cast_ray(
                &Ray {
                    origin: dir * (surf_r + 50.0),
                    dir: -dir,
                },
                200.0,
            )
            .expect("downward ray must hit the terrain");
        assert!(
            (hit.toi - 50.0).abs() < 0.5,
            "ray hit {hit:?}, surface at toi 50"
        );
    }

    // ---- M6-B: §2.4 transient joints (soft-capture) ----

    /// Zero-gravity environment — joint dynamics isolated from the
    /// (equal-on-both) gravity term.
    fn free_space_env() -> Environment {
        Environment {
            mu_m3_per_s2: 0.0,
            ..terrin_env()
        }
    }

    fn soft_capture() -> JointKind {
        JointKind::SoftCapture {
            pull_per_s: 8.0,
            max_correction_mps: 1.0,
        }
    }

    /// The world-frame anchor separation of a joint between
    /// `bodies[0]` (anchor `la`) and `bodies[1]` (anchor `lb`).
    fn anchor_sep(bodies: &[WeldedBody], la: DVec3, lb: DVec3) -> f64 {
        let pa = bodies[0].pos + bodies[0].rot.rotate(la);
        let pb = bodies[1].pos + bodies[1].rot.rotate(lb);
        (pa - pb).norm()
    }

    /// A soft-capture joint reels its anchors together and the
    /// relative velocity damps out — the compliant joint converges
    /// instead of welding.
    #[test]
    fn soft_capture_reels_anchors_together() {
        let mut backend = SimpleBackend::new(free_space_env());
        let half = DVec3::new(0.5, 0.5, 0.5);
        let mut bodies = vec![box_body(1_000.0, half), box_body(1_000.0, half)];
        bodies[0].pos = DVec3::new(TERRIN_R + 1_000.0, 0.0, 0.0);
        bodies[1].pos = DVec3::new(TERRIN_R + 1_000.0, 0.5, 0.0);
        let joint = JointDef {
            body_a: BodyHandle(0),
            body_b: BodyHandle(1),
            local_anchor_a: DVec3::ZERO,
            local_anchor_b: DVec3::ZERO,
            kind: soft_capture(),
        };
        let sep0 = anchor_sep(&bodies, DVec3::ZERO, DVec3::ZERO);
        for _ in 0..192 {
            backend.step(&mut bodies, &[joint]);
        }
        let sep = anchor_sep(&bodies, DVec3::ZERO, DVec3::ZERO);
        assert!(
            sep < 0.01,
            "anchors should converge within the capture window: {sep}"
        );
        let vrel = (bodies[0].vel - bodies[1].vel).norm();
        assert!(vrel < 0.01, "relative velocity should damp out: {vrel}");
        // Exponential reel-in: separation decayed far below its start.
        assert!(sep < sep0 * 0.05);
    }

    /// The closing-speed cap is the "soft" — a far-separated pair
    /// closes at ≤ max_correction, never faster.
    #[test]
    fn soft_capture_caps_closing_speed() {
        let mut backend = SimpleBackend::new(free_space_env());
        let half = DVec3::new(0.5, 0.5, 0.5);
        let mut bodies = vec![box_body(1_000.0, half), box_body(1_000.0, half)];
        bodies[0].pos = DVec3::new(TERRIN_R + 1_000.0, 0.0, 0.0);
        bodies[1].pos = DVec3::new(TERRIN_R + 1_000.0, 8.0, 0.0);
        let joint = JointDef {
            body_a: BodyHandle(0),
            body_b: BodyHandle(1),
            local_anchor_a: DVec3::ZERO,
            local_anchor_b: DVec3::ZERO,
            kind: soft_capture(),
        };
        let mut prev = anchor_sep(&bodies, DVec3::ZERO, DVec3::ZERO);
        let mut saw_capped = false;
        for _ in 0..16 {
            backend.step(&mut bodies, &[joint]);
            let sep = anchor_sep(&bodies, DVec3::ZERO, DVec3::ZERO);
            let closing = (prev - sep) / PHYSICS_DT;
            assert!(
                closing <= 1.0 + 1e-9,
                "closing speed {closing} exceeded the 1 m/s cap"
            );
            saw_capped |= closing > 0.9;
            prev = sep;
        }
        assert!(saw_capped, "cap never engaged on a far separation");
        // The pair is still being pulled together (not a dead joint).
        assert!(prev < 8.0);
    }

    /// Equal-and-opposite impulses conserve linear momentum exactly —
    /// gravity-free, so the joint is the only interaction.
    #[test]
    fn joint_impulses_conserve_momentum() {
        let mut backend = SimpleBackend::new(free_space_env());
        let half = DVec3::new(0.5, 0.5, 0.5);
        let mut bodies = vec![box_body(1_000.0, half), box_body(2_000.0, half)];
        bodies[0].pos = DVec3::new(TERRIN_R + 1_000.0, 0.0, 0.0);
        bodies[1].pos = DVec3::new(TERRIN_R + 1_000.0, 2.0, 1.0);
        bodies[0].vel = DVec3::new(0.0, 1.0, 0.0);
        bodies[1].vel = DVec3::new(0.0, -0.5, 0.5);
        let joint = JointDef {
            body_a: BodyHandle(0),
            body_b: BodyHandle(1),
            // Off-CoM anchors, so the joint also torques — momentum
            // conservation must hold through the angular channel too.
            local_anchor_a: DVec3::new(0.2, 0.3, 0.0),
            local_anchor_b: DVec3::new(-0.1, 0.0, 0.4),
            kind: soft_capture(),
        };
        let p0 = bodies[0].vel * bodies[0].mass_kg + bodies[1].vel * bodies[1].mass_kg;
        for _ in 0..64 {
            backend.step(&mut bodies, &[joint]);
        }
        let p1 = bodies[0].vel * bodies[0].mass_kg + bodies[1].vel * bodies[1].mass_kg;
        assert!(
            (p1 - p0).norm() < 1e-6,
            "joint impulses must conserve linear momentum: {p0:?} -> {p1:?}"
        );
    }

    /// A latched side is the island's static anchor — the free body
    /// reels toward it and it never moves.
    #[test]
    fn joint_latched_side_stays_put() {
        let mut backend = SimpleBackend::new(free_space_env());
        let half = DVec3::new(0.5, 0.5, 0.5);
        let mut bodies = vec![box_body(1_000.0, half), box_body(1_000.0, half)];
        bodies[0].pos = DVec3::new(TERRIN_R + 1_000.0, 0.0, 0.0);
        bodies[1].pos = DVec3::new(TERRIN_R + 1_000.0, 2.0, 0.0);
        bodies[0].settle.latched = true;
        let joint = JointDef {
            body_a: BodyHandle(0),
            body_b: BodyHandle(1),
            local_anchor_a: DVec3::ZERO,
            local_anchor_b: DVec3::ZERO,
            kind: soft_capture(),
        };
        let pos0 = bodies[0].pos;
        for _ in 0..192 {
            backend.step(&mut bodies, &[joint]);
        }
        assert_eq!(bodies[0].pos, pos0, "latched body must not move");
        let sep = anchor_sep(&bodies, DVec3::ZERO, DVec3::ZERO);
        assert!(
            sep < 2.0,
            "free body reeled toward the latched anchor: {sep}"
        );
    }

    /// The solver re-sorts the joint list — enumeration order cannot
    /// change the tick.
    #[test]
    fn joint_order_does_not_change_the_solve() {
        let run = |order: [usize; 2]| {
            let mut backend = SimpleBackend::new(free_space_env());
            let half = DVec3::new(0.5, 0.5, 0.5);
            let mut bodies = vec![
                box_body(1_000.0, half),
                box_body(1_000.0, half),
                box_body(1_000.0, half),
            ];
            bodies[0].pos = DVec3::new(TERRIN_R + 1_000.0, 0.0, 0.0);
            bodies[1].pos = DVec3::new(TERRIN_R + 1_000.0, 1.0, 0.0);
            bodies[2].pos = DVec3::new(TERRIN_R + 1_000.0, 0.0, 1.0);
            let js = [
                JointDef {
                    body_a: BodyHandle(0),
                    body_b: BodyHandle(1),
                    local_anchor_a: DVec3::ZERO,
                    local_anchor_b: DVec3::ZERO,
                    kind: soft_capture(),
                },
                JointDef {
                    body_a: BodyHandle(0),
                    body_b: BodyHandle(2),
                    local_anchor_a: DVec3::new(0.1, 0.0, 0.0),
                    local_anchor_b: DVec3::new(0.0, -0.1, 0.0),
                    kind: soft_capture(),
                },
            ];
            let list = [js[order[0]], js[order[1]]];
            for _ in 0..64 {
                backend.step(&mut bodies, &list);
            }
            bodies
        };
        let (a, b) = (run([0, 1]), run([1, 0]));
        for i in 0..3 {
            assert_eq!(a[i].pos, b[i].pos);
            assert_eq!(a[i].vel, b[i].vel);
            assert_eq!(a[i].ang_vel, b[i].ang_vel);
        }
    }

    /// Bitwise determinism: two identical runs of a jointed island
    /// agree bit-for-bit.
    #[test]
    fn joint_solve_is_bit_deterministic() {
        let run = || {
            let mut backend = SimpleBackend::new(free_space_env());
            let half = DVec3::new(0.5, 0.5, 0.5);
            let mut bodies = vec![box_body(1_000.0, half), box_body(750.0, half)];
            bodies[0].pos = DVec3::new(TERRIN_R + 1_000.0, 0.0, 0.0);
            bodies[1].pos = DVec3::new(TERRIN_R + 1_000.0, 1.5, 0.5);
            bodies[1].vel = DVec3::new(0.0, -0.5, 0.25);
            bodies[1].ang_vel = DVec3::new(0.01, 0.0, 0.02);
            let joint = JointDef {
                body_a: BodyHandle(0),
                body_b: BodyHandle(1),
                local_anchor_a: DVec3::new(0.1, 0.2, 0.0),
                local_anchor_b: DVec3::ZERO,
                kind: soft_capture(),
            };
            for _ in 0..96 {
                backend.step(&mut bodies, &[joint]);
            }
            bodies
        };
        assert_eq!(run(), run());
    }
}
