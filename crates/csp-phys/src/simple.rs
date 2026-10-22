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
//! The environment's ground is a **sphere of `radius_m` centred at
//! `body_center`** (§2.3: at 600 km the surface is locally flat to
//! µm and the exact sphere keeps lat/long meaningful). Generation:
//! every collider vertex whose world position falls below the surface
//! (`‖p − c‖ < R`) produces one manifold point `{pos, normal = up,
//! depth}` — vertex-only contact per the M2 scope; face/edge contacts
//! on a curved surface differ from the vertex manifold by ≪ µm at
//! home-planet radius. Body-vs-body contact is out of scope for
//! `SimpleBackend` (vessels in one bubble are rare; hull-vs-hull needs
//! the parry materialization anyway).
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
//! 2 s) the body is marked `latched`, its velocities are zeroed, and
//! its contacts are released — the backend holds no integrating state
//! for it. A `throttle > 0` unlatches (§2.6.4's "any throttle command
//! unlatches it first"); the other unlatch triggers (`SimCommand::
//! Unlatch`, activation, nearby impact) are M2-C World wiring.

use serde::{Deserialize, Serialize};

use csp_core::{DQuat, DVec3, TICK_HZ};
use csp_data::{AtmosphereDef, BodyDef};

use crate::backend::{BodyHandle, Contact, Hit, JointDef, PhysicsBackend, Ray};
use crate::part::ForceSiteKind;
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

/// The world a `SimpleBackend` steps inside: the dominant gravitating
/// body (its sphere is also the contact surface and altimetry datum),
/// the §3.0 atmosphere, and the solver/latch tunables.
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
    /// system, §3.0), metres — the contact sphere.
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
    /// Coulomb μ for ground contact (see [`DEFAULT_FRICTION_MU`]).
    pub friction_mu: f64,
    /// Pre-voxel drag coefficient (see [`DEFAULT_DRAG_CD`]).
    pub drag_cd: f64,
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
        }
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
    fn new(
        body_index: usize,
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
        // Fixed-order two-direction friction basis, deterministic from
        // the normal: reference axis = least-aligned of X/Y.
        let reference = if normal_world.x.abs() <= normal_world.y.abs() {
            DVec3::X
        } else {
            DVec3::Y
        };
        let t1_raw = normal_world.cross(reference);
        let t1_world = t1_raw / t1_raw.norm();
        let t2_world = normal_world.cross(t1_world);
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
        };
        (constraint, contact)
    }
}

/// Apply impulse `dλ·dir_b` to `b` (dir in body frame): updates `vel`
/// (world), `vel_b` (its cached body-frame twin — a pure vector add,
/// no quaternion rotate) and `ang_vel` (body) — the Gauss–Seidel
/// write-back.
fn apply_impulse(
    b: &mut WeldedBody,
    vel_b: &mut DVec3,
    c: &Constraint,
    dir_body: DVec3,
    d_lambda: f64,
) {
    let j_body = dir_body * d_lambda;
    *vel_b = *vel_b + j_body / b.mass_kg;
    b.vel = b.vel + b.rot.rotate(j_body) / b.mass_kg;
    b.ang_vel = b.ang_vel + c.inv_i.mul_vec(c.r_body.cross(j_body));
}

/// Point velocity at the contact *relative to the ground surface*,
/// body frame: `v_rel = v_body + ω × r − v_surf`. `v_body` is the
/// caller's cached body-frame linear velocity (the solve caches it
/// per body rather than quaternion-rotating `vel` on every
/// evaluation); `v_surf` is `ZERO` on a non-rotating world.
fn rel_point_velocity_body(vel_b: DVec3, b: &WeldedBody, c: &Constraint) -> DVec3 {
    vel_b + b.ang_vel.cross(c.r_body) - c.surface_vel_body
}

/// §2.9's `SimpleBackend` — see the module docs for the full spec
/// reading. Owns its [`Environment`] and the last step's public
/// contact record (a derived observation of the solve, re-created each
/// tick — never serialized, per §2.4's derived-cache rule).
#[derive(Clone, Debug)]
pub struct SimpleBackend {
    env: Environment,
    /// Per-body contact manifold from the most recent `step`, indexed
    /// by slice position ([`BodyHandle`]). Cleared for latched bodies.
    contacts: Vec<Vec<Contact>>,
}

impl SimpleBackend {
    pub fn new(env: Environment) -> Self {
        Self {
            env,
            contacts: Vec::new(),
        }
    }

    /// The environment the backend was built with.
    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// Geometric ground-contact predicate — the same rule
    /// [`Self::generate_contacts`] applies (any collider vertex with
    /// `depth > 0` against the environment sphere), evaluated
    /// on-demand. The `contacts()` record is only a view of the most
    /// recent `step` call; the sim's warp-refusal and save-boundary
    /// predicates need the per-body truth *between* steps, which is
    /// geometry, not solver state.
    pub fn ground_contact(&self, b: &WeldedBody) -> bool {
        for hull in &b.collider.hulls {
            for v in &hull.vertices {
                let p = b.pos + b.rot.rotate(*v);
                let dist = (p - self.env.body_center).norm();
                if self.env.radius_m - dist > 0.0 && dist > 0.0 {
                    return true;
                }
            }
        }
        false
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
    /// Every collider vertex below the environment sphere's surface
    /// yields one manifold point, in fixed ascending `(body, hull,
    /// vertex)` order. `constraints` is the solver working set,
    /// `self.contacts` the public record — same points, two views.
    fn generate_contacts(&mut self, bodies: &[WeldedBody], constraints: &mut Vec<Constraint>) {
        for (i, b) in bodies.iter().enumerate() {
            if b.settle.latched {
                continue;
            }
            for hull in &b.collider.hulls {
                for v in &hull.vertices {
                    let p = b.pos + b.rot.rotate(*v);
                    let rel = p - self.env.body_center;
                    let dist = rel.norm();
                    let depth = self.env.radius_m - dist;
                    // Strict `> 0`: a vertex exactly grazing the
                    // surface carries no penetration and no impulse
                    // authority; it is caught next tick at depth
                    // ≤ v·dt (module docs).
                    if depth > 0.0 && dist > 0.0 {
                        let (c, record) =
                            Constraint::new(i, *v, p, rel / dist, depth, b, &self.env);
                        constraints.push(c);
                        self.contacts[i].push(record);
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
    /// `v_surf = body_spin × rel` is nonzero on a rotating world.
    fn solve_velocity(&self, bodies: &mut [WeldedBody], constraints: &mut [Constraint]) {
        // Cache each body's body-frame linear velocity once for the
        // whole solve; impulses update it by pure vector addition —
        // `rot` does not change inside this pass, so the cache stays
        // exact and saves a quaternion rotate per velocity evaluation.
        let mut vel_body: Vec<DVec3> = bodies
            .iter()
            .map(|b| b.rot.conjugate().rotate(b.vel))
            .collect();
        for _ in 0..SOLVER_ITERATIONS {
            for c in constraints.iter_mut() {
                let b = &mut bodies[c.body as usize];
                let vel_b = &mut vel_body[c.body as usize];

                // Normal: target vn ≥ v_surf·n (restitution 0; the
                // surface velocity is tangential on the sphere, so in
                // practice vn ≥ 0 — the relative form is the honest
                // statement either way).
                let vn = rel_point_velocity_body(*vel_b, b, c).dot(c.n_body);
                let old = c.lambda_n;
                c.lambda_n = (c.lambda_n - vn * c.kn).max(0.0);
                let dl = c.lambda_n - old;
                if dl != 0.0 {
                    apply_impulse(b, vel_b, c, c.n_body, dl);
                }

                // Friction, fixed order t1 then t2.
                let limit = self.env.friction_mu * c.lambda_n;
                let vt1 = rel_point_velocity_body(*vel_b, b, c).dot(c.t1_body);
                let old = c.lambda_t1;
                c.lambda_t1 = (c.lambda_t1 - vt1 * c.kt1).clamp(-limit, limit);
                let dl = c.lambda_t1 - old;
                if dl != 0.0 {
                    apply_impulse(b, vel_b, c, c.t1_body, dl);
                }

                let vt2 = rel_point_velocity_body(*vel_b, b, c).dot(c.t2_body);
                let old = c.lambda_t2;
                c.lambda_t2 = (c.lambda_t2 - vt2 * c.kt2).clamp(-limit, limit);
                let dl = c.lambda_t2 - old;
                if dl != 0.0 {
                    apply_impulse(b, vel_b, c, c.t2_body, dl);
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
                        apply_impulse(b, vel_b, c, c.t1_body, dl1);
                    }
                    if dl2 != 0.0 {
                        apply_impulse(b, vel_b, c, c.t2_body, dl2);
                    }
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
    fn solve_positions(&self, bodies: &mut [WeldedBody], constraints: &[Constraint]) {
        // Per-body budget of CoM translation this pass may apply this
        // tick — spent in fixed ascending constraint order.
        let mut spent = vec![0.0; bodies.len()];
        for _ in 0..SOLVER_ITERATIONS {
            for c in constraints {
                let b = &mut bodies[c.body as usize];
                let p = b.pos + b.rot.rotate(c.local_vertex);
                let rel = p - self.env.body_center;
                let dist = rel.norm();
                let depth = self.env.radius_m - dist;
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
                    b.settle.latched = true;
                    b.vel = DVec3::ZERO;
                    b.ang_vel = DVec3::ZERO;
                    self.contacts[i].clear();
                }
            } else {
                b.settle.settled_ticks = 0;
            }
        }
    }
}

impl PhysicsBackend for SimpleBackend {
    /// One pinned `1/64` s tick. `joints` is ignored: §2.9 assigns
    /// jointed islands to `RapierBackend` — `SimpleBackend` has no
    /// joint solver by design (the §2.4 cap-16 transient joints
    /// degrade to clean separation upstream).
    fn step(&mut self, bodies: &mut [WeldedBody], joints: &[JointDef]) {
        let _ = joints;
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
                // §2.6.4: a latched vessel's throttle is zero, and any
                // throttle command unlatches it first.
                if b.throttle > 0.0 {
                    b.settle.latched = false;
                    b.settle.settled_ticks = 0;
                } else {
                    continue;
                }
            }
            self.integrate_free(b);
        }

        // Phase 4 — contact solve: generation, velocity PGS, position
        // Baumgarte, all in fixed ascending order.
        let mut constraints = Vec::new();
        self.generate_contacts(bodies, &mut constraints);
        self.solve_velocity(bodies, &mut constraints);
        self.solve_positions(bodies, &constraints);

        // Phase 5 — the §2.6.4 terminal state.
        self.update_settle(bodies, &pose0);
    }

    /// Ray vs the environment sphere (nearest positive root, `toi ≤
    /// max_toi`). Stepped bodies are not raycast targets yet — hull-vs-
    /// ray needs the parry `ConvexPolyhedron` materialization, the same
    /// deferred work body-vs-body contact waits on.
    fn cast_ray(&self, ray: &Ray, max_toi: f64) -> Option<Hit> {
        let oc = ray.origin - self.env.body_center;
        let b = oc.dot(ray.dir);
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
        Some(Hit {
            toi,
            point,
            normal: rel / rel.norm(),
            body: None,
        })
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
}
