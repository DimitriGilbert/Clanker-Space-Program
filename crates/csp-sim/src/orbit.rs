//! The orbit layer (PLAN §2.8): a patched-conics [`OrbitModel`] with the
//! universal-variable Kepler propagator and the §2.6.6 SOI-transition scan.
//!
//! Scope note: `World` consumes this module through [`PatchedConics`] —
//! vessels live on rails in `crate::World`'s arena and their §2.6.6
//! transitions re-base elements through [`cartesian_to_elements`].
//! Everything here is self-contained pure math over [`DVec3`]/`f64`,
//! `libm` transcendentals only (§2.7).
//!
//! # Element conventions (documented because they ARE the contract)
//!
//! [`OrbitState`] reuses `csp_data`'s [`KeplerElements`]; `epoch` carries the
//! sim-time the elements' anomaly refers to (data bodies use epoch 0).
//!
//! - `mean_anomaly_epoch_rad` is `M = n·(t − τ)` for **every** conic class,
//!   where `n = sqrt(μ/|a|³)` and `τ` is periapsis time: for an ellipse `M`
//!   is the usual `E − e·sinE`; for a hyperbola `e·sinhF − F`; for the
//!   parabolic encoding below it equals `(D + D³/3)/2` (Barker's
//!   `D = tan(ν/2)`).
//! - Open orbits: hyperbolic elements store the standard `a < 0`. A
//!   **parabolic** (`e ≈ 1`) orbit has no finite `a`, so the schema field
//!   carries `a := −p` (negative semi-latus rectum `p = h²/μ`); then
//!   `r_p = |a|/2` and `n = sqrt(μ/|a|³)` — the same `n·(t−τ)` convention
//!   as the hyperbola. `period_s` is `+∞` for all open orbits.
//! - `|e − 1| ≤ `[`E_PARABOLIC_TOL`] classifies an orbit as parabolic. The
//!   local trajectory difference between `e = 1` and `e = 1 ± 1e-9` is far
//!   below the §2.8 1e-6 round-trip budget.
//! - Degenerate-angle conventions (the standard set): circular orbits
//!   (`e < `[`E_CIRCULAR_TOL`]) store `ω = 0` and carry the anomaly as the
//!   argument of latitude — the true longitude when also equatorial;
//!   equatorial orbits (`|n̂| < `[`NODE_TOL`]) store `Ω = 0` and `ω`
//!   becomes the longitude of periapsis measured from +x **about the
//!   orbit's own `ŵ = ĥ` axis** — so a retrograde-equatorial (`i ≈ π`)
//!   `ω` reads as the negation of the prograde convention, which is the
//!   value `R1(π)·R3(ω)` needs to reproduce the state.
//!
//! # Solver (§2.8, implemented literally)
//!
//! 1. Elliptic `Δt` is reduced mod the period **before** solving — under
//!    rails warp `Δt` can be enormous.
//! 2. Stumpff `C(z)`/`S(z)`: closed form for `|z| ≥ `[`STUMPFF_EPS`],
//!    series expansion below it (no catastrophic cancellation near `z ≈ 0`).
//! 3. Laguerre–Conway iteration, `n = 5`, with a maintained sign bracket on
//!    `χ` — the time equation `F(χ)` is monotone in `χ` (`F′ = r > 0`), so a
//!    bracket, once found, is always valid; a step that leaves the bracket
//!    (or goes non-finite) falls back to bisection, and before a bracket
//!    exists the search expands geometrically.
//! 4. Cap [`SOLVER_MAX_ITERS`] = 64. Non-convergence is
//!    `SimError::Recoverable` — the caller keeps the previous valid state
//!    and logs the failing [`OrbitState`]'s elements ([`SimError`] payloads
//!    are `&'static str` by design, so the elements are logged at the call
//!    site rather than baked into the error).
//!
//! `state_at` evaluates an orbit by propagating its **periapsis state** —
//! derived in the perifocal frame from `(a, e)` — by `M(t)/n`. One code
//! path covers every conic class; the elliptic period reduction is
//! equivalent to wrapping `M` to `(−π, π]`.
//!
//! # SOI transitions (§2.6.6)
//!
//! `next_soi_event` scans `d(t) = |r_vessel − r_body| − r_soi` on the
//! integer-tick lattice (the only domain `state_at` is defined on — time is
//! integral, §2.3) at a stride `h = 0.1·r_soi / v_rel_max` ceiled to whole
//! ticks (min 1), brackets sign changes, and Brent-refines each bracket on
//! the real-tick axis to `≤ 1 tick` (cap 64) — the reported event tick may
//! sit on *either* side of the crossing inside that bound; the claim is
//! distance, not direction. The honest guarantee:
//! **a residence shorter than `h` may be missed** — `h` is exposed per
//! candidate via [`PatchedConics::scan_strides`] for the debug overlay,
//! computed by the *same* candidate-enumeration and stride helpers the
//! scan itself uses, so the overlay can never drift from the scan.
//!
//! The coarse scan's sample count is bounded per call by
//! [`SOI_SCAN_MAX_SAMPLES`] — §2.6.5's back-pressure philosophy applied
//! to the scan loop: `(horizon − now)/h` is caller-controlled and an
//! unbounded grid could run tens of millions of Kepler solves inside a
//! single warp step. On exhaustion the call fails `Recoverable` — never
//! `Ok(None)`, which would silently hide a crossing in the unscanned
//! suffix — and the caller's fallback is the §2.6.5 re-issue pattern:
//! clamp the horizon and retry.
//!
//! `v_rel_max` is an upper bound on relative speed: the vessel's periapsis
//! speed `sqrt(μ(1+e)/r_p)` (its global speed bound on the conic) plus, for
//! a moving target body, that body's own speed bound — `|v_v − v_b| ≤
//! |v_v| + |v_b|` by triangle inequality. Because nested SOIs make the only
//! possible transitions "exit the parent's SOI" or "enter a child's SOI",
//! the candidate set is exactly `{parent} ∪ children(parent)` (the star's
//! infinite SOI contributes no exit candidate).

use std::collections::BTreeMap;

use csp_core::{DVec3, SimError, SimTime, TICK_HZ};
use csp_data::{BodyDef, BodyId, KeplerElements};
use serde::{Deserialize, Serialize};

/// `2π` — period arithmetic and mean-anomaly wrapping.
const TAU: f64 = core::f64::consts::TAU;
/// `π`.
const PI: f64 = core::f64::consts::PI;

/// Stumpff series/closed-form boundary `|z|` (§2.8 item 2).
const STUMPFF_EPS: f64 = 1e-6;
/// Kepler-solver iteration cap (§2.8 item 4).
const SOLVER_MAX_ITERS: u32 = 64;
/// Laguerre–Conway polynomial order (§2.8 item 3).
const LAGUERRE_N: f64 = 5.0;
/// Brent refinement cap (§2.6.6 item 3).
const BRENT_MAX_ITERS: u32 = 64;
/// §2.6.5's back-pressure philosophy applied to the §2.6.6 coarse scan:
/// the total number of stride-grid samples one [`OrbitModel::next_soi_event`]
/// call may evaluate across all candidates. Each sample is ≥1 Kepler
/// solve, so the caller-controlled `(horizon − now)/h` grid needs an
/// explicit ceiling — without one a far-horizon call would run tens of
/// millions of solves inside a single warp step.
///
/// 2²⁰ ≈ 15× [`crate::MAX_TICKS_PER_FRAME`]: a full-frame advance window
/// fits even at the pathological 1-tick minimum stride with several
/// candidates in play. A call whose grid needs more fails
/// `SimError::Recoverable` — the defined result, *not* `Ok(None)`, which
/// would silently hide crossings in the unscanned suffix. The documented
/// caller fallback is the §2.6.5 re-issue pattern: clamp the horizon and
/// retry. (Brent refinement is already bounded separately by
/// `BRENT_MAX_ITERS` per bracket.)
pub const SOI_SCAN_MAX_SAMPLES: u64 = 1_048_576;
/// `|e − 1|` at/below which an orbit is treated as parabolic (module docs).
/// Shared with `crate::transfer` — the parabolic `a := −p` encoding must be
/// classified identically wherever elements are read.
pub(crate) const E_PARABOLIC_TOL: f64 = 1e-9;
/// `e` below which the circular-element conventions apply (module docs).
const E_CIRCULAR_TOL: f64 = 1e-10;
/// Node-vector length threshold for the equatorial conventions (module docs).
const NODE_TOL: f64 = 1e-10;

/// Solve tolerance on the time equation, in seconds: relative to `|Δt|`
/// with an absolute floor so `Δt → 0` still converges tightly. Sized so the
/// converged-state error `v·tol` stays orders of magnitude under the §2.8
/// 1e-6 round-trip budget for any physical orbit.
fn solve_tol_seconds(dt: f64) -> f64 {
    (dt.abs() * 1e-14).max(1e-12)
}

/// One body's orbit contribution to a [`PatchedConics`] system model.
///
/// Mirrors `csp_data::BodyDef`'s orbit-relevant fields; `elements` is
/// `None` for the star only. Every body's elements are expressed relative
/// to its `parent` with sim-time 0 as their epoch (the `body.ron`
/// convention).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BodyOrbit {
    /// μ (m³/s²).
    pub mu_m3_per_s2: f64,
    /// SOI radius (m); `None` = infinite (the star).
    pub soi_m: Option<f64>,
    /// This body's parent; `None` for the root.
    pub parent: Option<BodyId>,
    /// Keplerian elements relative to `parent`, epoch = sim-time 0.
    pub elements: Option<KeplerElements>,
}

/// A scheduled sphere-of-influence transition (§2.6.6). `tick` is a whole
/// sim tick within 1 tick of the refined crossing — on either side of it;
/// the §2.6.6 "≤ 1 tick" claim is a *distance* bound, not a side claim.
/// Events are always in `(now, horizon]` when returned by
/// [`OrbitModel::next_soi_event`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoiEvent {
    /// The body whose SOI is crossed.
    pub body: BodyId,
    /// `Enter` = vessel enters `body`'s SOI; `Exit` = vessel leaves it.
    pub direction: SoiDirection,
    /// Whole tick within 1 tick of the crossing (either side of it).
    pub tick: u64,
}

/// Direction of a [`SoiEvent`]. The explicit `u16` discriminants future-proof
/// the §7.1 rule-7 hash encoding for when the event heap lands.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SoiDirection {
    /// Vessel enters `body`'s SOI.
    Enter = 0,
    /// Vessel leaves `body`'s SOI.
    Exit = 1,
}

impl csp_core::StableDiscriminant for SoiDirection {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// One SOI-boundary candidate's scan stride — the debug surface §2.6.6
/// item 4 requires ("`h` is reported in the debug overlay").
#[derive(Clone, Debug, PartialEq)]
pub struct SoiScan {
    /// The body whose SOI boundary is scanned.
    pub body: BodyId,
    /// `h` in whole ticks: `ceil(0.1·r_soi / v_rel_max · TICK_HZ)`, min 1.
    pub stride_ticks: u64,
}

/// §2.8 verbatim: a conic is elements plus the sim-time they are stamped
/// at. `parent` names the frame's central body; positions returned by
/// [`OrbitModel::state_at`] are **parent-centered inertial** — composing
/// the parent chain into system-frame positions is the caller's job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrbitState {
    /// The central body these elements are relative to.
    pub parent: BodyId,
    /// Elements at `epoch` (conventions in the module docs).
    pub elements: KeplerElements,
    /// The sim-time `elements.mean_anomaly_epoch_rad` refers to.
    pub epoch: SimTime,
}

/// One SOI boundary under scan: whose SOI, its radius, and whether it is
/// the vessel-parent's own boundary (`is_parent` ⇒ the relative-distance
/// frame origin) or a child's.
struct SoiCandidate {
    body: BodyId,
    r_soi: f64,
    is_parent: bool,
}

/// The §2.8 orbit-model trait — the seam the post-v1 `NBody` model also
/// implements (which is why `next_soi_event` exists rather than a
/// step-and-check loop).
///
/// The plan's sketch returns bare tuples; every method here returns
/// `Result` instead — spec item 4's "non-convergence → `SimError::
/// Recoverable`" needs an error channel, and a pure `state_at` has nowhere
/// else to put it. Callers implement "fall back to the previous valid
/// state" by keeping their last good propagated state on `Err`.
/// Note on the plan's sketch: §2.8's `advance(s, to)` epoch re-stamp is
/// deliberately **absent**. Re-stamping `M += n·Δt` per call is not
/// associative under float partitioning —
/// `fl(fl(M + n·Δt₁) + n·Δt₂) ≠ fl(M + n·(Δt₁+Δt₂))` — so any re-stamp on
/// the rails path breaks §2.6.1 partition independence (the ×1-vs-×10⁵
/// equivalence test diverges by last-ulp drift). Epoch-anchored elements
/// make the method unnecessary: `state_at` evaluates the closed form at
/// any tick from the fixed epoch, and stored state mutates ONLY at
/// absolute-tick events (the §2.6.6 re-base) and commands (spawn). No
/// code may re-add a per-tick/per-interval anomaly re-stamp.
pub trait OrbitModel {
    /// Cheap, pure: same `(s, t)` ⇒ bitwise-identical output. `t` may be
    /// before `epoch` (backward propagation is supported).
    fn state_at(&self, s: &OrbitState, t: SimTime) -> Result<(DVec3, DVec3), SimError>;

    /// The earliest SOI change in `(now, horizon]` per §2.6.6 — see the
    /// module docs for the scan/`h`/Brent contract and the per-call
    /// sample budget ([`SOI_SCAN_MAX_SAMPLES`]).
    fn next_soi_event(
        &self,
        s: &OrbitState,
        now: SimTime,
        horizon: SimTime,
    ) -> Result<Option<SoiEvent>, SimError>;
}

// ---------------------------------------------------------------------------
// Stumpff functions
// ---------------------------------------------------------------------------

/// Stumpff `C(z)`, `S(z)` (§2.8 item 2): closed form for `|z| ≥
/// [`STUMPFF_EPS`], series below it.
///
/// ```text
/// z > 0:  C = (1 − cos√z)/z            S = (√z − sin√z)/√z³
/// z < 0:  C = (cosh√(−z) − 1)/(−z)     S = (sinh√(−z) − √−z)/(−z)^1.5
/// ```
///
/// `pub(crate)` for the Lambert solver (`crate::transfer`) — the same
/// Stumpff evaluation serves the universal-variable Lambert form.
pub(crate) fn stumpff(z: f64) -> (f64, f64) {
    if z > STUMPFF_EPS {
        let s = libm::sqrt(z);
        ((1.0 - libm::cos(s)) / z, (s - libm::sin(s)) / (s * s * s))
    } else if z < -STUMPFF_EPS {
        let s = libm::sqrt(-z);
        (
            (libm::cosh(s) - 1.0) / (-z),
            (libm::sinh(s) - s) / (s * s * s),
        )
    } else {
        // Series: C = Σ(−z)^k/(2k+2)!, S = Σ(−z)^k/(2k+3)! — terms past z⁴
        // are below f64 resolution for |z| < 1e-6.
        let z2 = z * z;
        let z3 = z2 * z;
        let z4 = z3 * z;
        (
            0.5 - z / 24.0 + z2 / 720.0 - z3 / 40320.0 + z4 / 3628800.0,
            1.0 / 6.0 - z / 120.0 + z2 / 5040.0 - z3 / 362880.0 + z4 / 39916800.0,
        )
    }
}

// ---------------------------------------------------------------------------
// Universal-variable Kepler propagation
// ---------------------------------------------------------------------------

/// Propagate a two-body state `(r0, v0)` (parent-centered inertial, m and
/// m/s) forward `dt` seconds by the §2.8 universal-variable solver.
///
/// Errors are `Recoverable`: non-finite/ill-posed input, or the
/// [`SOLVER_MAX_ITERS`] cap hit — the caller keeps its previous valid
/// state (§2.8 item 4).
pub fn propagate(r0: DVec3, v0: DVec3, mu: f64, dt: f64) -> Result<(DVec3, DVec3), SimError> {
    if !(mu.is_finite() && mu > 0.0) {
        return Err(SimError::Recoverable("kepler propagate: non-positive mu"));
    }
    if !(r0.is_finite() && v0.is_finite() && dt.is_finite()) {
        return Err(SimError::Recoverable("kepler propagate: non-finite state"));
    }
    let r0n = r0.norm();
    if r0n <= 0.0 {
        return Err(SimError::Recoverable("kepler propagate: zero radius"));
    }
    if dt == 0.0 {
        return Ok((r0, v0));
    }
    let sqrt_mu = libm::sqrt(mu);
    let alpha = 2.0 / r0n - v0.norm_squared() / mu; // 1/a — sign selects conic

    // §2.8 item 1: reduce Δt mod the period for elliptic orbits BEFORE
    // solving. rem_euclid folds a negative Δt onto the equivalent forward
    // offset — the same conic state, but a bounded solve.
    let mut dt_eff = dt;
    if alpha > 0.0 {
        let a = 1.0 / alpha;
        let period = TAU * libm::sqrt(a * a * a / mu);
        // Skip the reduction when the period isn't representable: a
        // near-parabolic alpha → 0⁺ overflows `a³` to +∞ (a degenerate
        // `a` underflows it to 0), and `rem_euclid` against ±inf/0
        // yields NaN — which used to turn an ordinary backward step
        // into a spurious `Recoverable`. An unreduced Δt is legal here;
        // the bracketed solver either bounds it or honestly reports
        // non-convergence.
        if period.is_finite() && period > 0.0 {
            dt_eff = dt.rem_euclid(period);
            if dt_eff == 0.0 {
                return Ok((r0, v0));
            }
        }
    }
    let sigma = r0.dot(v0) / sqrt_mu; // r0·v0/√μ
    let chi = solve_universal(r0n, sigma, alpha, mu, sqrt_mu, dt_eff)?;

    // Lagrange f&g coefficients, universal form (f = 1 − χ²C/r0,
    // g = Δt − χ³S/√μ, ḟ = √μ·χ(zS−1)/(r·r0), ġ = 1 − χ²C/r).
    let z = alpha * chi * chi;
    let (c2, c3) = stumpff(z);
    let chi2 = chi * chi;
    let f = 1.0 - chi2 * c2 / r0n;
    let g = dt_eff - chi2 * chi * c3 / sqrt_mu;
    let r_new = chi2 * c2 + sigma * chi * (1.0 - z * c3) + r0n * (1.0 - z * c2);
    let fdot = sqrt_mu * chi * (z * c3 - 1.0) / (r_new * r0n);
    let gdot = 1.0 - chi2 * c2 / r_new;
    let r = f * r0 + g * v0;
    let v = fdot * r0 + gdot * v0;
    if !(r.is_finite() && v.is_finite()) {
        return Err(SimError::Recoverable("kepler propagate: non-finite result"));
    }
    Ok((r, v))
}

/// The universal time equation and its derivatives, with `z = α·χ²`,
/// `σ = r0·v0/√μ`:
///
/// ```text
/// F(χ)  = σ·χ²·C + (1 − α·r0)·χ³·S + r0·χ − √μ·Δt
/// F′(χ) = r(χ) = χ²·C + σ·χ·(1 − z·S) + r0·(1 − z·C)
/// F″(χ) = (1 − α·r0)·χ·(1 − z·S) + σ·(1 − z·C)
/// ```
///
/// `F′ = r` is what makes the equation *monotone* in `χ` — the §2.8
/// bracketing requirement rests on it.
fn universal_f(r0n: f64, sigma: f64, alpha: f64, target: f64, chi: f64) -> (f64, f64, f64) {
    let z = alpha * chi * chi;
    let (c2, c3) = stumpff(z);
    let one_zs = 1.0 - z * c3;
    let one_zc = 1.0 - z * c2;
    let f =
        sigma * chi * chi * c2 + (1.0 - alpha * r0n) * chi * chi * chi * c3 + r0n * chi - target;
    let fp = chi * chi * c2 + sigma * chi * one_zs + r0n * one_zc;
    let fpp = (1.0 - alpha * r0n) * chi * one_zs + sigma * one_zc;
    (f, fp, fpp)
}

/// Initial `χ` guess (Vallado's set): `√μ·α·Δt` for elliptic (one orbit
/// advances `χ` by `2π√a`), the logarithmic hyperbolic guess, and the
/// linear `√μ·Δt/r0` near the parabolic branch. Bracketing makes a rough
/// guess merely slower, never wrong.
fn initial_chi(r0n: f64, sigma: f64, alpha: f64, mu: f64, sqrt_mu: f64, dt: f64) -> f64 {
    if alpha > 1e-11 {
        sqrt_mu * alpha * dt
    } else if alpha < -1e-11 {
        let a = 1.0 / alpha; // < 0
        let num = -2.0 * mu * alpha * dt;
        let den = sigma * sqrt_mu + dt.signum() * libm::sqrt(-mu * a) * (1.0 - r0n * alpha);
        let arg = num / den;
        if arg > 0.0 {
            dt.signum() * libm::sqrt(-a) * libm::log(arg)
        } else {
            sqrt_mu * dt / r0n
        }
    } else {
        sqrt_mu * dt / r0n
    }
}

/// Laguerre–Conway iteration `n = 5` with bracketing (§2.8 items 3–4).
/// `target = √μ·Δt`; convergence is checked in seconds against
/// [`solve_tol_seconds`].
fn solve_universal(
    r0n: f64,
    sigma: f64,
    alpha: f64,
    mu: f64,
    sqrt_mu: f64,
    dt: f64,
) -> Result<f64, SimError> {
    let target = sqrt_mu * dt;
    let tol_t = solve_tol_seconds(dt);
    let mut chi = initial_chi(r0n, sigma, alpha, mu, sqrt_mu, dt);
    if !chi.is_finite() {
        chi = sqrt_mu * dt / r0n;
    }
    // Bracket on χ: F < 0 ⇒ χ too small, F > 0 ⇒ too large (F′ = r > 0).
    let mut lo = f64::NEG_INFINITY;
    let mut hi = f64::INFINITY;
    // Geometric expansion step used until a bracket exists; seeded at the
    // linear-timescale χ estimate so it is scale-aware.
    let mut expand_step = chi
        .abs()
        .max(sqrt_mu * dt.abs() / r0n)
        .max(libm::sqrt(r0n) * 1e-3);
    for _ in 0..SOLVER_MAX_ITERS {
        let (f, fp, fpp) = universal_f(r0n, sigma, alpha, target, chi);
        if f.is_nan() {
            return Err(SimError::Recoverable("kepler solve: non-finite residual"));
        }
        if f.abs() / sqrt_mu <= tol_t {
            return Ok(chi);
        }
        if f < 0.0 {
            lo = lo.max(chi);
        } else {
            hi = hi.min(chi);
        }
        // Laguerre–Conway:
        //   Δχ = −n·F / (F′ + sign(F′)·√((n−1)²·F′² − n(n−1)·F·F″)).
        // The discriminant can go negative where the cubic model is a
        // poor local fit (exactly where conditioning is worst).
        // `|disc|` keeps the damping term's magnitude; clamping to zero
        // instead degenerates the step to `n·F/F′` — a 5× Newton
        // overshoot — precisely in that worst-conditioned regime.
        let disc = (LAGUERRE_N - 1.0) * (LAGUERRE_N - 1.0) * fp * fp
            - LAGUERRE_N * (LAGUERRE_N - 1.0) * f * fpp;
        let root = libm::sqrt(disc.abs());
        let denom = if fp >= 0.0 { fp + root } else { fp - root };
        let mut next = if denom.is_finite() && denom != 0.0 {
            chi - LAGUERRE_N * f / denom
        } else {
            f64::NAN // falls through to the bracket logic below
        };
        if !(next.is_finite() && next > lo && next < hi) {
            next = if lo.is_finite() && hi.is_finite() {
                0.5 * (lo + hi) // bracketed: bisect
            } else {
                // Unbracketed: expand geometrically in the direction the
                // residual demands.
                expand_step *= 4.0;
                chi + (if f < 0.0 { expand_step } else { -expand_step })
            };
        }
        if !next.is_finite() {
            return Err(SimError::Recoverable("kepler solve: diverged"));
        }
        // Secondary convergence: the bracket collapsed to χ's own
        // resolution — further iteration is noise.
        if lo.is_finite()
            && hi.is_finite()
            && (hi - lo) <= next.abs().mul_add(1e-15, f64::MIN_POSITIVE)
        {
            return Ok(0.5 * (lo + hi));
        }
        chi = next;
    }
    Err(SimError::Recoverable(
        "kepler solve: iteration cap (64) exceeded",
    ))
}

// ---------------------------------------------------------------------------
// Elements <-> cartesian
// ---------------------------------------------------------------------------

/// Periapsis state of an elements set in the **perifocal** frame —
/// `(r_p·x̂, v_p·ŷ)` — plus mean motion `n`. The single conic-decoding
/// path: every public conversion funnels through it, so the element
/// conventions live in exactly one place.
///
/// `v_p = sqrt(μ(1+e)/r_p)` is also the orbit's global speed bound.
fn periapsis_state(elements: &KeplerElements, mu: f64) -> Result<(DVec3, DVec3, f64), SimError> {
    if !(mu.is_finite() && mu > 0.0) {
        return Err(SimError::Recoverable("elements: non-positive mu"));
    }
    let e = elements.eccentricity;
    let a = elements.semi_major_axis_m;
    if !(e.is_finite() && e >= 0.0 && a.is_finite()) {
        return Err(SimError::Recoverable("elements: invalid a or e"));
    }
    // The three orientation angles feed `perifocal_to_inertial` on every
    // decode path. A NaN/±inf angle would otherwise sail through as an
    // Ok((NaN, NaN, NaN)) state and silently blind the SOI solver — the
    // funnel rejects it here, up front.
    if !(elements.inclination_rad.is_finite()
        && elements.ascending_node_rad.is_finite()
        && elements.arg_periapsis_rad.is_finite())
    {
        return Err(SimError::Recoverable("elements: non-finite angle"));
    }
    let (r_p, v_p, n) = if e < 1.0 - E_PARABOLIC_TOL {
        // Elliptic: a > 0 required.
        if a <= 0.0 {
            return Err(SimError::Recoverable("elements: elliptic a <= 0"));
        }
        let r_p = a * (1.0 - e);
        (
            r_p,
            libm::sqrt(mu * (1.0 + e) / r_p),
            libm::sqrt(mu / (a * a * a)),
        )
    } else if e <= 1.0 + E_PARABOLIC_TOL {
        // Parabolic encoding: a := −p (module docs).
        if a >= 0.0 {
            return Err(SimError::Recoverable(
                "elements: parabolic encoding requires a = -p < 0",
            ));
        }
        let p = -a;
        let r_p = 0.5 * p;
        (
            r_p,
            libm::sqrt(2.0 * mu / r_p),
            libm::sqrt(mu / (p * p * p)),
        )
    } else {
        // Hyperbolic: a < 0 required (standard convention).
        if a >= 0.0 {
            return Err(SimError::Recoverable("elements: hyperbolic a >= 0"));
        }
        let r_p = a * (1.0 - e); // = |a|·(e−1) > 0
        (
            r_p,
            libm::sqrt(mu * (1.0 + e) / r_p),
            libm::sqrt(mu / -(a * a * a)),
        )
    };
    if !(r_p.is_finite() && r_p > 0.0 && v_p.is_finite() && n.is_finite() && n > 0.0) {
        return Err(SimError::Recoverable("elements: degenerate periapsis"));
    }
    Ok((DVec3::new(r_p, 0.0, 0.0), DVec3::new(0.0, v_p, 0.0), n))
}

/// Mean motion `n = sqrt(μ/|a|³)` — the `M = n·(t − τ)` clock rate shared
/// by every conic class (module docs).
fn mean_motion(elements: &KeplerElements, mu: f64) -> Result<f64, SimError> {
    periapsis_state(elements, mu).map(|(_, _, n)| n)
}

/// Wrap an angle to `(−π, π]` — the elliptic `M` normalization that
/// implements §2.8 item 1's mod-period reduction in anomaly space.
fn wrap_pi(m: f64) -> f64 {
    let w = m - TAU * libm::floor(m / TAU + 0.5);
    // floor edge: m/TAU + 0.5 exactly on an integer yields −π; the
    // documented range is (−π, π], so push it to +π.
    if w <= -PI { w + TAU } else { w }
}

/// §2.8 item 1's anomaly-space period reduction, the one place the
/// "wrap `M` iff elliptic" rule lives: closed orbits wrap to `(−π, π]`;
/// an open orbit's `M` is unbounded and passes through untouched.
fn wrap_mean_anomaly(elements: &KeplerElements, m: f64) -> f64 {
    if elements.eccentricity < 1.0 - E_PARABOLIC_TOL {
        wrap_pi(m)
    } else {
        m
    }
}

/// Perifocal-frame vector → inertial via `R3(Ω)·R1(i)·R3(ω)`. The full
/// rotation: every caller passes a planar (z = 0) perifocal vector today,
/// but `v.z` is carried honestly so the helper is a true frame rotation,
/// not a 2-vector projection in disguise.
/// `pub(crate)` for `crate::transfer`'s ejection geometry — the parking
/// orbit's inertial periapsis/normal basis comes through this rotation.
pub(crate) fn perifocal_to_inertial(v: DVec3, node: f64, incl: f64, argp: f64) -> DVec3 {
    let (cw, sw) = (libm::cos(argp), libm::sin(argp));
    let (ci, si) = (libm::cos(incl), libm::sin(incl));
    let (co, so) = (libm::cos(node), libm::sin(node));
    DVec3::new(
        (co * cw - so * ci * sw) * v.x + (-co * sw - so * ci * cw) * v.y + so * si * v.z,
        (so * cw + co * ci * sw) * v.x + (-so * sw + co * ci * cw) * v.y - co * si * v.z,
        (si * sw) * v.x + (si * cw) * v.y + ci * v.z,
    )
}

/// State of an elements set `dt` seconds after its own epoch: the epoch
/// anomaly plus `n·dt` (wrapped for elliptic), decoded through the
/// periapsis state and one universal propagation.
fn elements_state_at(
    elements: &KeplerElements,
    mu: f64,
    dt: f64,
) -> Result<(DVec3, DVec3), SimError> {
    if !dt.is_finite() {
        return Err(SimError::Recoverable("elements: non-finite dt"));
    }
    let (peri_r, peri_v, n) = periapsis_state(elements, mu)?;
    let m = wrap_mean_anomaly(elements, elements.mean_anomaly_epoch_rad + n * dt);
    let (r_pf, v_pf) = propagate(peri_r, peri_v, mu, m / n)?;
    Ok((
        perifocal_to_inertial(
            r_pf,
            elements.ascending_node_rad,
            elements.inclination_rad,
            elements.arg_periapsis_rad,
        ),
        perifocal_to_inertial(
            v_pf,
            elements.ascending_node_rad,
            elements.inclination_rad,
            elements.arg_periapsis_rad,
        ),
    ))
}

/// Elements → cartesian state at the elements' own epoch.
pub fn elements_to_cartesian(
    elements: &KeplerElements,
    mu: f64,
) -> Result<(DVec3, DVec3), SimError> {
    elements_state_at(elements, mu, 0.0)
}

/// Cartesian state → classical elements (the §2.8 round-trip path).
///
/// Handles the full conic set: elliptic `a > 0`, hyperbolic `a < 0`, and
/// the parabolic `a := −p` encoding for `|e − 1| ≤ `[`E_PARABOLIC_TOL`].
/// `a` comes from `p/(1 − e²)` rather than the energy integral — the sign
/// falls out of `e` alone and the expression never divides by a near-zero
/// energy. Degenerate-angle conventions are in the module docs; a radial
/// trajectory (`h = 0`) has no plane to hang angles on → `Recoverable`.
pub fn cartesian_to_elements(
    r_vec: DVec3,
    v_vec: DVec3,
    mu: f64,
) -> Result<KeplerElements, SimError> {
    if !(mu.is_finite() && mu > 0.0) {
        return Err(SimError::Recoverable("cart2kep: non-positive mu"));
    }
    if !(r_vec.is_finite() && v_vec.is_finite()) {
        return Err(SimError::Recoverable("cart2kep: non-finite state"));
    }
    let r = r_vec.norm();
    if r <= 0.0 {
        return Err(SimError::Recoverable("cart2kep: zero radius"));
    }
    let h_vec = r_vec.cross(v_vec);
    let h = h_vec.norm();
    if h <= 0.0 {
        // Rectilinear orbit: no orbital plane exists to hang Ω, i, ω on.
        return Err(SimError::Recoverable("cart2kep: radial trajectory"));
    }
    let w_hat = h_vec / h;
    // Ascending-node vector ẑ×h — zero for equatorial orbits.
    let node_vec = DVec3::new(-h_vec.y, h_vec.x, 0.0);
    let node_n = node_vec.norm();
    let equatorial = node_n < NODE_TOL * h;
    let node_hat = if equatorial {
        DVec3::X
    } else {
        node_vec / node_n
    };
    // Eccentricity vector in the algebraically-stable Laplace form:
    // e_vec = ((v² − μ/r)·r_vec − (r·v)·v_vec)/μ.
    let e_vec = ((v_vec.norm_squared() - mu / r) * r_vec - r_vec.dot(v_vec) * v_vec) / mu;
    let e = e_vec.norm();
    let p = h * h / mu;
    let parabolic = (e - 1.0).abs() <= E_PARABOLIC_TOL;
    let a = if parabolic { -p } else { p / (1.0 - e * e) };
    let ascending_node = if equatorial {
        0.0
    } else {
        libm::atan2(node_vec.y, node_vec.x)
    };
    // In-plane direction 90° ahead of the node (or of +x when equatorial).
    let quad_hat = w_hat.cross(node_hat);
    let circular = e < E_CIRCULAR_TOL;
    let (arg_periapsis, true_anomaly) = if circular {
        // ω := 0; the anomaly is the argument of latitude — the true
        // longitude when also equatorial (node_hat = +x ⇒ atan2(r_y, r_x)).
        let u = libm::atan2(r_vec.dot(quad_hat), r_vec.dot(node_hat));
        (0.0, u)
    } else {
        let e_hat = e_vec / e;
        // ω measured in the orbital plane: from the node direction
        // toward quad_hat = ŵ×n̂. When equatorial (node_hat = +x̂) this
        // is the longitude of periapsis — and quad_hat still carries
        // ŵ's direction, so retrograde equatorial (i ≈ π) resolves
        // correctly with no special case. (The deleted `atan2(e_y,
        // e_x)` shortcut measured about +ẑ and was off by 2ω at i = π.)
        let argp = libm::atan2(e_vec.dot(quad_hat), e_vec.dot(node_hat));
        let nu = libm::atan2(r_vec.dot(w_hat.cross(e_hat)), r_vec.dot(e_hat));
        (argp, nu)
    };
    let sin_nu = libm::sin(true_anomaly);
    let cos_nu = libm::cos(true_anomaly);
    let (mean_anomaly, period_s) = if parabolic {
        // Barker: M_par := n(t−τ) = (D + D³/3)/2 with D = tan(ν/2), written
        // in the cancellation-free form sinν/(1 + cosν).
        let d = sin_nu / (1.0 + cos_nu);
        (0.5 * (d + d * d * d / 3.0), f64::INFINITY)
    } else if e < 1.0 {
        let ecc = libm::atan2(libm::sqrt(1.0 - e * e) * sin_nu, e + cos_nu);
        (
            wrap_pi(ecc - e * libm::sin(ecc)),
            TAU * libm::sqrt(a * a * a / mu),
        )
    } else {
        // sinhF = √(e²−1)·sinν/(1 + e·cosν); asinh preserves ν's sign.
        let f_anom = libm::asinh(libm::sqrt(e * e - 1.0) * sin_nu / (1.0 + e * cos_nu));
        (e * libm::sinh(f_anom) - f_anom, f64::INFINITY)
    };
    if !(mean_anomaly.is_finite() && arg_periapsis.is_finite() && ascending_node.is_finite()) {
        return Err(SimError::Recoverable("cart2kep: non-finite angles"));
    }
    Ok(KeplerElements {
        semi_major_axis_m: a,
        eccentricity: e,
        inclination_rad: libm::atan2(libm::sqrt(h_vec.x * h_vec.x + h_vec.y * h_vec.y), h_vec.z),
        ascending_node_rad: ascending_node,
        arg_periapsis_rad: arg_periapsis,
        mean_anomaly_epoch_rad: mean_anomaly,
        period_s,
    })
}

/// A state's maximum speed bound on its conic — the `v_rel_max` ingredient
/// of §2.6.6's `h = 0.1·r_soi / v_rel_max`. For both closed and open orbits
/// this is the periapsis speed (speed decreases monotonically away from
/// periapsis).
pub fn speed_bound(elements: &KeplerElements, mu: f64) -> Result<f64, SimError> {
    let (_, peri_v, _) = periapsis_state(elements, mu)?;
    Ok(peri_v.norm())
}

/// `(t − epoch)` in seconds, differenced on the integer tick lattice and
/// converted once — `t.seconds() − epoch.seconds()` subtracts two large
/// `f64`s and can round a small tick difference entirely away at large
/// tick counts (backward propagation is supported, so the sign must
/// survive too). The `i128` intermediate holds any `u64` pair exactly.
fn tick_delta_seconds(t: SimTime, epoch: SimTime) -> f64 {
    (t.ticks() as i128 - epoch.ticks() as i128) as f64 / TICK_HZ as f64
}

/// §2.6.6 item 1's scan stride: `h = 0.1·r_soi / v_rel_max`, ceiled to
/// whole ticks, min 1 — the single formula, shared by
/// `next_soi_event`'s scan and [`PatchedConics::scan_strides`]'s report
/// so the overlay can never disagree with the scan.
fn stride_ticks(r_soi: f64, v_rel_max: f64) -> u64 {
    (0.1 * r_soi / v_rel_max * TICK_HZ as f64).ceil().max(1.0) as u64
}

/// Spend one coarse-scan sample against the call's shared budget
/// ([`SOI_SCAN_MAX_SAMPLES`]); exhaustion is `Recoverable` — the defined
/// §2.6.5-style back-pressure result, never a silent truncation.
fn spend_sample(budget: &mut u64) -> Result<(), SimError> {
    *budget = budget
        .checked_sub(1)
        .ok_or(SimError::Recoverable("soi scan: sample budget exhausted"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// PatchedConics
// ---------------------------------------------------------------------------

/// The v1 orbit model (§2.8): bodies and on-rails vessels on Keplerian
/// rails inside their parent's SOI. Holds the system body set it resolves
/// elements against — built once from the loaded `BodyDef`s (or directly
/// from [`BodyOrbit`]s in tests).
#[derive(Clone, Debug)]
pub struct PatchedConics {
    /// The system model, keyed by `BodyId` — `BTreeMap` order is the
    /// deterministic body order everywhere it matters (§2.2).
    bodies: BTreeMap<BodyId, BodyOrbit>,
    /// Physical radii (m) per body — construction-side metadata for
    /// queries that need the body's extent (`transfer`'s encounter
    /// prediction answers "does it hit the body" against these).
    /// Deliberately NOT a `BodyOrbit` field: `BodyOrbit` is serialized
    /// inside `WorldConfig` in the §7.6 replay artifact, and adding a
    /// field there invalidates every recorded golden. This table is
    /// rebuilt at construction like every other §7.3 derived view —
    /// populated by [`Self::from_body_defs`] or [`Self::set_body_radius`];
    /// `PatchedConics::new` leaves it empty (an unknown radius reads as
    /// `None`, never a wrong answer).
    radii: BTreeMap<BodyId, f64>,
}

impl PatchedConics {
    /// Build a system model directly from per-body specs. No physical
    /// radii are known on this path — [`Self::set_body_radius`] adds them.
    pub fn new(bodies: BTreeMap<BodyId, BodyOrbit>) -> Self {
        Self {
            bodies,
            radii: BTreeMap::new(),
        }
    }

    /// Build from the loaded `body.ron` set — `parent`/`orbit` carried
    /// through verbatim; the csp-data validator has already proven parents
    /// exist and terminate at the star. `radius_m` lands in the side
    /// table the encounter-prediction query reads.
    pub fn from_body_defs(defs: &[BodyDef]) -> Result<Self, SimError> {
        let mut bodies = BTreeMap::new();
        let mut radii = BTreeMap::new();
        for def in defs {
            if bodies
                .insert(
                    def.id.clone(),
                    BodyOrbit {
                        mu_m3_per_s2: def.mu_m3_per_s2,
                        soi_m: def.soi_m,
                        parent: def.parent.clone(),
                        elements: def.orbit.clone(),
                    },
                )
                .is_some()
            {
                return Err(SimError::SaveBlocking("duplicate body id in system model"));
            }
            radii.insert(def.id.clone(), def.radius_m);
        }
        Ok(Self { bodies, radii })
    }

    /// A body's physical radius (m), when the model carries it —
    /// `from_body_defs` models always do; `new` models only via
    /// [`Self::set_body_radius`]. Query metadata, never sim state.
    pub fn body_radius(&self, id: &BodyId) -> Option<f64> {
        self.radii.get(id).copied()
    }

    /// Record a body's physical radius on a directly-constructed model —
    /// the test/tooling counterpart of `from_body_defs`' automatic fill.
    /// Construction-style setter: call before queries read it.
    pub fn set_body_radius(&mut self, id: &BodyId, radius_m: f64) {
        self.radii.insert(id.clone(), radius_m);
    }

    /// Public read of a body's orbit record — `World` composes
    /// vessel/body states across §2.6.6 transitions through it (the
    /// patched-conics stitch needs the child's elements, the old
    /// parent's parent link, and the new parent's μ). Not part of the
    /// solver or the scan.
    pub fn body_orbit(&self, id: &BodyId) -> Result<&BodyOrbit, SimError> {
        self.body(id)
    }

    /// Iterate the whole body table — `BTreeMap` order is the §2.2
    /// deterministic body order. The map view (M1-D) walks this to lay
    /// out the system; read-only, like [`Self::body_orbit`].
    pub fn bodies(&self) -> &BTreeMap<BodyId, BodyOrbit> {
        &self.bodies
    }

    fn body(&self, id: &BodyId) -> Result<&BodyOrbit, SimError> {
        self.bodies
            .get(id)
            .ok_or(SimError::Recoverable("orbit model: unknown body"))
    }

    /// Parent-centered state of a system body at `t`. `Err(Recoverable)`
    /// for the star — it has no elements; it IS the frame origin. Public
    /// for the transfer planner (`crate::transfer` composes body
    /// ephemerides into Lambert endpoints) — read-only, same purity
    /// contract as [`OrbitModel::state_at`].
    pub fn body_state(&self, id: &BodyId, t: SimTime) -> Result<(DVec3, DVec3), SimError> {
        let b = self.body(id)?;
        let elements = b
            .elements
            .as_ref()
            .ok_or(SimError::Recoverable("orbit model: body has no orbit"))?;
        let parent_mu = match &b.parent {
            Some(p) => self.body(p)?.mu_m3_per_s2,
            None => return Err(SimError::Recoverable("orbit model: root has no parent")),
        };
        elements_state_at(elements, parent_mu, t.seconds())
    }

    /// The §2.6.6 patched-conics stitch, as a pure function: express
    /// `s`'s state at `t` in the new parent's frame and re-derive its
    /// elements — the same math `World::apply_soi_transition` performs
    /// (it calls this), extracted so `crate::transfer`'s hypothetical-
    /// conic queries (`predict_encounter`) traverse the SAME seam the
    /// sim itself does. Two callers, one stitch — the prediction can
    /// never drift from the transition it predicts.
    ///
    /// - `Enter{body}`: `body` must be a child of `s.parent` with
    ///   elements; the relative state is `vessel − child`, both in the
    ///   old-parent frame, and the new conic's parent is `body`.
    /// - `Exit{body}`: `body` must BE `s.parent` and carry a parent of
    ///   its own; the new frame is the grandparent and the state
    ///   composes `vessel_in_parent + parent_in_grandparent`. Exiting
    ///   past a parentless body (the star, or a root-level body) has no
    ///   frame to re-base into — `Recoverable`; callers treat that as
    ///   "left the system".
    ///
    /// The result's epoch is `t` — epoch-anchored like every conic
    /// this model evaluates.
    pub fn rebase(
        &self,
        s: &OrbitState,
        body: &BodyId,
        direction: SoiDirection,
        t: SimTime,
    ) -> Result<OrbitState, SimError> {
        let (rv, vv) = self.state_at(s, t)?;
        match direction {
            SoiDirection::Enter => {
                let child = self.body_orbit(body)?;
                if child.parent.as_ref() != Some(&s.parent) {
                    return Err(SimError::Recoverable(
                        "soi enter: target is not a child of the vessel's parent",
                    ));
                }
                let child_el = child
                    .elements
                    .clone()
                    .ok_or(SimError::Recoverable("soi enter: child has no orbit"))?;
                // The child's elements are relative to the vessel's
                // current parent with epoch = sim-time 0 (body.ron
                // convention), so its state evaluates in the same frame.
                let child_state = OrbitState {
                    parent: s.parent.clone(),
                    elements: child_el,
                    epoch: SimTime::ZERO,
                };
                let (rb, vb) = self.state_at(&child_state, t)?;
                let elements = cartesian_to_elements(rv - rb, vv - vb, child.mu_m3_per_s2)?;
                Ok(OrbitState {
                    parent: body.clone(),
                    elements,
                    epoch: t,
                })
            }
            SoiDirection::Exit => {
                if &s.parent != body {
                    return Err(SimError::Recoverable(
                        "soi exit: event body is not the vessel's parent",
                    ));
                }
                let old_parent = self.body_orbit(body)?;
                let grandparent = old_parent
                    .parent
                    .clone()
                    .ok_or(SimError::Recoverable("soi exit: parent has no parent"))?;
                let parent_el = old_parent
                    .elements
                    .clone()
                    .ok_or(SimError::Recoverable("soi exit: parent has no orbit"))?;
                // The old parent's elements are relative to the
                // grandparent at epoch 0 — compose `vessel_in_parent +
                // parent_in_grandparent` for the grandparent-frame state.
                let parent_state = OrbitState {
                    parent: grandparent.clone(),
                    elements: parent_el,
                    epoch: SimTime::ZERO,
                };
                let (rp, vp) = self.state_at(&parent_state, t)?;
                let mu_g = self.body_orbit(&grandparent)?.mu_m3_per_s2;
                let elements = cartesian_to_elements(rv + rp, vv + vp, mu_g)?;
                Ok(OrbitState {
                    parent: grandparent,
                    elements,
                    epoch: t,
                })
            }
        }
    }
}

impl OrbitModel for PatchedConics {
    fn state_at(&self, s: &OrbitState, t: SimTime) -> Result<(DVec3, DVec3), SimError> {
        let mu = self.body(&s.parent)?.mu_m3_per_s2;
        elements_state_at(&s.elements, mu, tick_delta_seconds(t, s.epoch))
    }

    /// The §2.6.6 scan, plainly:
    /// 1. Per candidate (the parent's own SOI + each child's):
    ///    `h = max(1, ceil(0.1·r_soi / v_rel_max · TICK_HZ))` ticks — one
    ///    shared formula, so [`Self::scan_strides`] reports the stride the
    ///    scan actually used.
    /// 2. Sign-change bracketing on `d(t) = |r_v(t) − r_b(t)| − r_soi`
    ///    sampled on the `h`-stride grid, `horizon` always sampled; the
    ///    per-call sample count is bounded by [`SOI_SCAN_MAX_SAMPLES`]
    ///    (§2.6.5 back-pressure) — on exhaustion the call fails
    ///    `Recoverable` rather than running an unbounded grid or
    ///    answering `None` about an unscanned suffix.
    /// 3. Brent refinement of the earliest bracketed crossing per
    ///    candidate, to `≤ 1 tick`, cap 64.
    /// 4. Guarantee: a residence shorter than `h` may be missed —
    ///    [`Self::scan_strides`] reports the `h` values for the overlay.
    /// 5. The reported tick is a whole tick within 1 tick of the refined
    ///    crossing — on *either* side of it — always inside
    ///    `(now, horizon]`.
    fn next_soi_event(
        &self,
        s: &OrbitState,
        now: SimTime,
        horizon: SimTime,
    ) -> Result<Option<SoiEvent>, SimError> {
        self.next_soi_event_within(s, now, horizon, SOI_SCAN_MAX_SAMPLES, None, None)
            .map(|(ev, _samples)| ev)
    }
}

/// One candidate scan's run parameters — grouped so [`PatchedConics::
/// scan_candidate`] stays inside the arg-count lint.
struct CandidateScan<'a> {
    /// The stride `h` for this candidate (ticks).
    stride: u64,
    /// Inclusive-lower bound of the scan window.
    now: SimTime,
    /// Inclusive-upper bound of the scan window.
    horizon: SimTime,
    /// The caller's shared sample budget, spent per grid evaluation.
    budget: &'a mut u64,
    /// First-stride suppression direction — see `scan_candidate`'s doc.
    suppress_dir: Option<SoiDirection>,
}

/// The §2.6.6 publish horizon's physical scale: [`PatchedConics::
/// scan_horizon`] spans this many periods of the *tightest* orbit involved
/// in the scan — a few full traversals of the fastest-moving geometry, so
/// a single publish almost always covers the next crossing instead of
/// degenerating into a rescan chain.
const SOI_SCAN_HORIZON_PERIODS: u64 = 4;

impl PatchedConics {
    /// The §2.6.6 publish horizon, in ticks. The old rule derived the span
    /// from the sample cap (`h_min·MAX/(k+1)`) — backwards: the horizon is
    /// a physical quantity (how far ahead the subsystem is asked to look),
    /// and the sample budget is the *clamp* on it.
    ///
    /// Scale: `SOI_SCAN_HORIZON_PERIODS` periods of the tightest closed
    /// orbit in play — the vessel's own period, and each SOI-carrying
    /// child's period (a child's boundary moves on its orbital period; the
    /// parent's own boundary is stationary in the parent-centered frame,
    /// so the parent's orbit contributes no timescale). Open orbits
    /// contribute no period; when nothing in play is closed the span
    /// falls back to the sample-cap bound. The floor `4·h_min` keeps a
    /// pathologically tight orbit from degenerating the publish into a
    /// per-tick rescan.
    ///
    /// Clamp: the span never exceeds `h_min·MAX_SAMPLES/(k+1)` — the bound
    /// that keeps `Σ_c (span/h_c + 2)` under [`SOI_SCAN_MAX_SAMPLES`], so a
    /// publish derived this way cannot exhaust the scan budget.
    ///
    /// `Ok(None)` = no SOI candidates exist (the vessel can never cross —
    /// the caller schedules nothing).
    pub(crate) fn scan_horizon(&self, s: &OrbitState) -> Result<Option<u64>, SimError> {
        let cands = self.soi_candidates(s);
        if cands.is_empty() {
            return Ok(None);
        }
        let parent_mu = self.body(&s.parent)?.mu_m3_per_s2;
        let vessel_bound = speed_bound(&s.elements, parent_mu)?;
        let mut tightest_s = f64::INFINITY;
        if s.elements.eccentricity < 1.0 - E_PARABOLIC_TOL {
            tightest_s = tightest_s.min(TAU / mean_motion(&s.elements, parent_mu)?);
        }
        let mut h_min = u64::MAX;
        for cand in &cands {
            // Same `v_rel_max` machinery as the scan and overlay — a
            // broken child errors identically here, so publish and scan
            // can never disagree about the candidate set.
            let v_rel_max = self.candidate_v_rel_max(cand, vessel_bound, parent_mu)?;
            h_min = h_min.min(stride_ticks(cand.r_soi, v_rel_max));
            if !cand.is_parent
                && let Some(el) = self.body(&cand.body)?.elements.as_ref()
                && el.eccentricity < 1.0 - E_PARABOLIC_TOL
            {
                // The child's orbit is measured against the parent's μ.
                tightest_s = tightest_s.min(TAU / mean_motion(el, parent_mu)?);
            }
        }
        let physical = if tightest_s.is_finite() {
            (SOI_SCAN_HORIZON_PERIODS as f64 * tightest_s * TICK_HZ as f64) as u64
        } else {
            u64::MAX // no closed orbit anywhere in play → the clamp rules
        };
        let cap = h_min
            .saturating_mul(SOI_SCAN_MAX_SAMPLES / (cands.len() as u64 + 1))
            .max(1);
        // Floor first, cap last: the sample bound is the dominant clamp.
        Ok(Some(physical.max(h_min.saturating_mul(4)).min(cap).max(1)))
    }

    /// The §2.6.6 scan body behind [`OrbitModel::next_soi_event`], taking
    /// the sample budget explicitly so tests can exercise exhaustion
    /// cheaply; the trait impl binds [`SOI_SCAN_MAX_SAMPLES`]. Returns the
    /// event plus the coarse-scan samples the call spent (Brent refinement
    /// evaluations are bounded separately by `BRENT_MAX_ITERS` and not
    /// counted) — the spend is reported so callers/tests can pin the
    /// publish's cost.
    ///
    /// `suppress` names a just-applied §2.6.6 re-base `(body, direction)`:
    /// the reported tick may sit ≤1 tick on the *wrong* side of the true
    /// crossing (the Brent bound is a distance claim, not a side claim —
    /// module docs), so a vessel rebased just outside `body`'s SOI would
    /// otherwise re-detect the same boundary as a degenerate
    /// `Enter{own-parent}`/`Exit{non-parent}` one stride later. A
    /// first-stride detection on `body`'s boundary in `direction` is a
    /// tolerance artifact and is dropped; a later detection stands.
    ///
    /// `exclude` names a body whose SOI boundary is skipped **while the
    /// conic starts inside it at `now`** — the transfer-plan composition:
    /// `plan.conic` is stamped at the origin body's center (inside the
    /// origin's own SOI) so the earliest detection is that egress, which
    /// was never a residence in the excluded frame. The skip is evaluated
    /// once, up front; a re-entry later in the window is not reported.
    pub(crate) fn next_soi_event_within(
        &self,
        s: &OrbitState,
        now: SimTime,
        horizon: SimTime,
        max_samples: u64,
        suppress: Option<(BodyId, SoiDirection)>,
        exclude: Option<&BodyId>,
    ) -> Result<(Option<SoiEvent>, u64), SimError> {
        if horizon.ticks() <= now.ticks() {
            return Ok((None, 0));
        }
        let parent_mu = self.body(&s.parent)?.mu_m3_per_s2;
        let vessel_bound = speed_bound(&s.elements, parent_mu)?;
        let mut budget = max_samples;
        let mut best: Option<SoiEvent> = None;
        // Candidate set: the parent's own boundary (exit/re-entry) and
        // every child's (entry/exit). Nested SOIs make this complete.
        for cand in self.soi_candidates(s) {
            // The conic was stamped inside `exclude`'s SOI: it never
            // resided in that frame, so the egress is not an encounter.
            if exclude.is_some_and(|b| *b == cand.body)
                && self.signed_distance(s, &cand, now.ticks())? < 0.0
            {
                continue;
            }
            let v_rel_max = self.candidate_v_rel_max(&cand, vessel_bound, parent_mu)?;
            let stride = stride_ticks(cand.r_soi, v_rel_max);
            let suppress_dir = suppress
                .as_ref()
                .filter(|(body, _)| *body == cand.body)
                .map(|(_, dir)| *dir);
            let mut scan = CandidateScan {
                stride,
                now,
                horizon,
                budget: &mut budget,
                suppress_dir,
            };
            if let Some(ev) = self.scan_candidate(s, &cand, &mut scan)?
                && best.as_ref().is_none_or(|b0| ev.tick < b0.tick)
            {
                best = Some(ev);
            }
        }
        Ok((best, max_samples - budget))
    }

    /// The §2.6.6 candidate set — exactly `{parent} ∪ children(parent)`
    /// (the star's infinite SOI contributes no exit candidate), in
    /// `BTreeMap` order so every consumer sees the same enumeration.
    /// Shared by the scan and [`Self::scan_strides`]: a child carrying
    /// an SOI is a candidate *whether or not it has elements* — the
    /// `v_rel_max` step is where a missing-elements child fails, and it
    /// fails identically in both callers.
    fn soi_candidates(&self, s: &OrbitState) -> Vec<SoiCandidate> {
        let mut out = Vec::new();
        if let Some(r_soi) = self.bodies.get(&s.parent).and_then(|b| b.soi_m) {
            out.push(SoiCandidate {
                body: s.parent.clone(),
                r_soi,
                is_parent: true,
            });
        }
        for (id, b) in &self.bodies {
            if b.parent.as_ref() == Some(&s.parent)
                && let Some(r_soi) = b.soi_m
            {
                out.push(SoiCandidate {
                    body: id.clone(),
                    r_soi,
                    is_parent: false,
                });
            }
        }
        out
    }

    /// `v_rel_max` for one candidate: the vessel's speed bound for the
    /// parent's own boundary, vessel + child bounds for a child's —
    /// `|v_v − v_b| ≤ |v_v| + |v_b|` (the parent contributes no second
    /// term; vessel elements are already in its frame). A child carrying
    /// an SOI but no elements is `Recoverable` here — the scan and the
    /// overlay reach the same verdict because both come through this fn.
    fn candidate_v_rel_max(
        &self,
        cand: &SoiCandidate,
        vessel_bound: f64,
        parent_mu: f64,
    ) -> Result<f64, SimError> {
        let bound = if cand.is_parent {
            vessel_bound
        } else {
            let child_el = self
                .body(&cand.body)?
                .elements
                .as_ref()
                .ok_or(SimError::Recoverable("soi scan: child has no orbit"))?;
            vessel_bound + speed_bound(child_el, parent_mu)?
        };
        if !(bound.is_finite() && bound > 0.0) {
            return Err(SimError::Recoverable("soi scan: zero v_rel bound"));
        }
        Ok(bound)
    }

    /// The debug-overlay surface for §2.6.6 item 4: every candidate body
    /// with the stride `h` its scan actually uses, in ticks. A residence
    /// shorter than `h` may be missed — this is the number to show. Same
    /// candidate enumeration, `v_rel_max`, and stride helpers as
    /// [`OrbitModel::next_soi_event`], so the report *is* the scan's own.
    pub fn scan_strides(&self, s: &OrbitState) -> Result<Vec<SoiScan>, SimError> {
        let parent_mu = self.body(&s.parent)?.mu_m3_per_s2;
        let vessel_bound = speed_bound(&s.elements, parent_mu)?;
        let mut out = Vec::new();
        for cand in self.soi_candidates(s) {
            let v_rel_max = self.candidate_v_rel_max(&cand, vessel_bound, parent_mu)?;
            out.push(SoiScan {
                body: cand.body,
                stride_ticks: stride_ticks(cand.r_soi, v_rel_max),
            });
        }
        Ok(out)
    }

    /// `d(t) = |r_vessel(t) − r_body(t)| − r_soi` at a whole-tick time, in
    /// parent-centered coordinates: for `is_parent` the body is the frame
    /// origin; otherwise the child's state is composed in the same frame.
    fn signed_distance(
        &self,
        s: &OrbitState,
        cand: &SoiCandidate,
        t_ticks: u64,
    ) -> Result<f64, SimError> {
        let t = SimTime::from_ticks(t_ticks);
        let (rv, _) = self.state_at(s, t)?;
        let sep = if cand.is_parent {
            rv.norm()
        } else {
            let (rb, _) = self.body_state(&cand.body, t)?;
            rv.distance(rb)
        };
        Ok(sep - cand.r_soi)
    }

    /// Scan one candidate's boundary on the stride grid and refine the
    /// earliest crossing found, if any (§2.6.6 items 1–3). `scan.budget`
    /// is the caller's shared sample counter — every grid evaluation
    /// spends one; exhaustion is `Recoverable`, never a truncated `Ok`
    /// over an unscanned suffix (see [`SOI_SCAN_MAX_SAMPLES`]).
    ///
    /// `scan.suppress_dir` implements the post-rebase tolerance rule from
    /// [`PatchedConics::next_soi_event_within`]: a crossing detected in
    /// the scan's FIRST stride whose direction is `suppress_dir` is the
    /// ≤1-tick Brent wrong-side artifact re-detecting the boundary the
    /// caller's re-base just crossed — dropped, and the scan continues
    /// from the next sample. Detections past the first stride are real
    /// geometry and stand.
    fn scan_candidate(
        &self,
        s: &OrbitState,
        cand: &SoiCandidate,
        scan: &mut CandidateScan,
    ) -> Result<Option<SoiEvent>, SimError> {
        let mut prev_t = scan.now.ticks();
        spend_sample(scan.budget)?;
        let mut prev_d = self.signed_distance(s, cand, prev_t)?;
        loop {
            let first_stride = prev_t == scan.now.ticks();
            let sample_t = prev_t.saturating_add(scan.stride).min(scan.horizon.ticks());
            spend_sample(scan.budget)?;
            let d = self.signed_distance(s, cand, sample_t)?;
            // Crossing detection. `d == 0` is a boundary touch at the
            // sample itself; a zero-valued previous sample means the
            // boundary was hit exactly there. Otherwise, sign change on
            // the (prev_t, sample_t] interval → Brent.
            let (crossing_tick, entered) = if d == 0.0 {
                (Some(sample_t), prev_d >= 0.0)
            } else if prev_d == 0.0 {
                // Boundary hit exactly at prev_t — direction read off the
                // new sign (inside ⇒ Enter, outside ⇒ Exit).
                (Some(prev_t), d < 0.0)
            } else if (prev_d < 0.0) != (d < 0.0) {
                (
                    Some(self.brent_root(s, cand, prev_t, sample_t, prev_d)?),
                    d < 0.0,
                )
            } else {
                (None, d < 0.0)
            };
            // A crossing at `now` falls outside the open-left interval —
            // skip it but keep scanning for a later re-crossing.
            if let Some(tick) = crossing_tick
                && tick > scan.now.ticks()
            {
                let direction = if entered {
                    SoiDirection::Enter
                } else {
                    SoiDirection::Exit
                };
                if !(first_stride && scan.suppress_dir == Some(direction)) {
                    return Ok(Some(SoiEvent {
                        body: cand.body.clone(),
                        direction,
                        tick,
                    }));
                }
            }
            if sample_t >= scan.horizon.ticks() {
                return Ok(None);
            }
            prev_t = sample_t;
            prev_d = d;
        }
    }

    /// Brent's method on the **tick axis** (x in whole-tick units, f64;
    /// the underlying function is only evaluable at integer ticks —
    /// `SimTime` is integral, §2.3 — so evaluation is at `round(x)`).
    /// Refines an already-sign-bracketed crossing to `|Δt| ≤ 1 tick`
    /// (§2.6.6 item 3), cap [`BRENT_MAX_ITERS`]. On a tick-quantized
    /// function the interpolation steps degenerate and Brent's bisection
    /// guards dominate — which is exactly why it can never do worse than
    /// bisection here.
    ///
    /// Canonical (Numerical Recipes `zbrent`) structure: `b` is the best
    /// current estimate, `c` the contrapoint bracketing it; returns a
    /// whole tick within the `≤ 1 tick` bound of the crossing — on
    /// *either* side of it (the guarantee is a distance bound, not a
    /// direction claim).
    fn brent_root(
        &self,
        s: &OrbitState,
        cand: &SoiCandidate,
        a_ticks: u64,
        b_ticks: u64,
        fa: f64,
    ) -> Result<u64, SimError> {
        let eval = |x_ticks: f64| -> Result<f64, SimError> {
            self.signed_distance(s, cand, x_ticks.round() as u64)
        };
        let mut a = a_ticks as f64;
        let mut b = b_ticks as f64;
        let mut fa = fa;
        let mut fb = eval(b)?;
        let mut c = a;
        let mut fc = fa;
        let mut e = 0.0;
        let mut d = 0.0;
        for _ in 0..BRENT_MAX_ITERS {
            // If b and c don't bracket, move c to the older point.
            if (fb > 0.0) == (fc > 0.0) {
                c = a;
                fc = fa;
                e = b - a;
                d = e;
            }
            // Keep |f(b)| ≤ |f(c)|: b is always the best estimate.
            if fc.abs() < fb.abs() {
                a = b;
                b = c;
                c = a;
                fa = fb;
                fb = fc;
                fc = fa;
            }
            // Half the target bracket width, plus the machine term.
            let tol1 = 2.0 * f64::EPSILON * b.abs() + 0.5;
            let xm = 0.5 * (c - b);
            if xm.abs() <= tol1 || fb == 0.0 {
                break;
            }
            if e.abs() >= tol1 && fa.abs() > fb.abs() {
                let s_ratio = fb / fa;
                let (p, q) = if a == c {
                    // Secant step.
                    (2.0 * xm * s_ratio, 1.0 - s_ratio)
                } else {
                    // Inverse quadratic interpolation.
                    let q1 = fa / fc;
                    let r = fb / fc;
                    (
                        s_ratio * (2.0 * xm * q1 * (q1 - r) - (b - a) * (r - 1.0)),
                        (q1 - 1.0) * (r - 1.0) * (s_ratio - 1.0),
                    )
                };
                let (p, q) = if p > 0.0 { (p, -q) } else { (-p, q) };
                // Accept the interpolated step only if it lands well inside
                // the bracket and beats half the previous step.
                if 2.0 * p < (3.0 * xm * q - (tol1 * q).abs()).min((e * q).abs()) {
                    e = d;
                    d = p / q;
                } else {
                    d = xm;
                    e = d;
                }
            } else {
                d = xm;
                e = d;
            }
            a = b;
            fa = fb;
            b += if d.abs() > tol1 {
                d
            } else if xm > 0.0 {
                tol1
            } else {
                -tol1
            };
            fb = eval(b)?;
            if !fb.is_finite() {
                return Err(SimError::Recoverable("soi brent: non-finite residual"));
            }
        }
        // b is the refined crossing on the tick axis; ceil keeps the
        // event tick integral — within the ≤1-tick bound of the crossing,
        // on whichever side b converged to (no side is claimed).
        Ok(b.ceil() as u64)
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // §3.0 fixture constants — the home system's real numbers.
    const MU_SOLEN: f64 = 1.1723e18;
    const MU_TERRIN: f64 = 3.5316e12;
    const MU_RASK: f64 = 6.5138e10;
    const TERRIN_SOI: f64 = 8.4e7;
    const TERRIN_A: f64 = 1.36e10;

    fn vec_rel_err(got: DVec3, want: DVec3) -> f64 {
        (got - want).norm() / want.norm()
    }

    /// A cartesian state on a conic with periapsis distance `r_p` and
    /// eccentricity `e`, at true anomaly `nu`, rotated perifocal→inertial
    /// by (Ω, i, ω). Works uniformly for e < 1, = 1, > 1 via the
    /// semi-latus rectum `p = r_p(1 + e)` — no `a` singularities.
    fn conic_state(
        mu: f64,
        r_p: f64,
        e: f64,
        nu: f64,
        node: f64,
        incl: f64,
        argp: f64,
    ) -> (DVec3, DVec3) {
        let p = r_p * (1.0 + e);
        let r = p / (1.0 + e * libm::cos(nu));
        let h = libm::sqrt(mu * p);
        let r_pf = DVec3::new(r * libm::cos(nu), r * libm::sin(nu), 0.0);
        let v_pf = DVec3::new(-(h / p) * libm::sin(nu), (h / p) * (e + libm::cos(nu)), 0.0);
        (
            perifocal_to_inertial(r_pf, node, incl, argp),
            perifocal_to_inertial(v_pf, node, incl, argp),
        )
    }

    /// A state on an orbit with periapsis `r_p`, eccentricity `e`, at true
    /// anomaly `nu`, under nonzero (Ω, i, ω) so the angle path is covered.
    fn elements_for(mu: f64, r_p: f64, e: f64, nu: f64) -> (KeplerElements, DVec3, DVec3) {
        let (r0, v0) = conic_state(mu, r_p, e, nu, 0.7, 0.4, 1.1);
        let el = cartesian_to_elements(r0, v0, mu).unwrap();
        (el, r0, v0)
    }

    fn circular_elements(a: f64, m0: f64) -> KeplerElements {
        KeplerElements {
            semi_major_axis_m: a,
            eccentricity: 0.0,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: m0,
            period_s: f64::INFINITY, // unused by the model
        }
    }

    #[test]
    fn stumpff_series_matches_closed_form_at_boundary() {
        for &z in &[1e-6, -1e-6] {
            let (cs, ss) = stumpff(z * (1.0 - 1e-12));
            let (cc, sc) = stumpff(z * (1.0 + 1e-12));
            // Tolerance reflects the closed forms' own subtraction-error
            // at s ≈ 1e-3 (sinh(s) − s is ~1e-10), not a real discontinuity.
            assert!(rel_err(cs, cc) < 1e-7, "C discontinuity at z={z}");
            assert!(rel_err(ss, sc) < 1e-7, "S discontinuity at z={z}");
        }
        let (c0, s0) = stumpff(0.0);
        assert_eq!(c0, 0.5);
        assert!((s0 - 1.0 / 6.0).abs() < 1e-17);
        // Known: C(π²) = (1 − cosπ)/π² = 2/π².
        let pi2 = PI * PI;
        assert!(rel_err(stumpff(pi2).0, 2.0 / pi2) < 1e-14);
    }

    fn rel_err(got: f64, want: f64) -> f64 {
        ((got - want) / want).abs()
    }

    /// §2.8 round-trip budget at epoch: elements → cartesian must
    /// reproduce the state the elements were derived from — the tightest
    /// form of the conversion check (zero propagation blur).
    #[test]
    fn elements_round_trip_at_epoch() {
        let mut worst = 0.0f64;
        for &e in &[0.0, 0.5, 0.9, 0.999, 1.0, 1.001, 3.0] {
            for &nu in &[0.0, 0.3, 1.5, 2.8] {
                // Hyperbolic ν must stay inside the asymptote
                // |ν| < acos(−1/e).
                if e > 1.0 && nu >= libm::acos(-1.0 / e) - 0.05 {
                    continue;
                }
                let (el, r0, v0) = elements_for(MU_TERRIN, 6.75e5, e, nu);
                let (r1, v1) = elements_to_cartesian(&el, MU_TERRIN).unwrap();
                let er = vec_rel_err(r1, r0);
                let ev = vec_rel_err(v1, v0);
                worst = worst.max(er).max(ev);
                assert!(
                    er < 1e-6 && ev < 1e-6,
                    "epoch round-trip e={e} nu={nu}: r_err={er:e} v_err={ev:e}"
                );
            }
        }
        eprintln!("epoch round-trip worst rel err: {worst:e}");
    }

    /// The §2.8 property test: `e ∈ {0, 0.5, 0.9, 0.999, 1.0, 1.001, 3.0}`
    /// × `Δt` over 12 decades (1e-3 … 1e9 s). cartesian → elements →
    /// propagate agrees with a direct universal propagation of the seed
    /// state within the 1e-6 relative budget, position AND velocity. For
    /// elliptic orbits `Δt` ranges well past one period — the mod-period
    /// path — and the spec budget's "one period (elliptic) or Δt = 10⁷ s
    /// (open)" is covered by the sweep spanning both.
    #[test]
    fn round_trip_property_e_dt_sweep() {
        let mut worst_r = 0.0f64;
        let mut worst_v = 0.0f64;
        let mut worst_at = (0.0, 0.0);
        for &e in &[0.0, 0.5, 0.9, 0.999, 1.0, 1.001, 3.0] {
            let nu0 = if e > 1.0 { 0.4 } else { 1.2 };
            let (el, r0, v0) = elements_for(MU_TERRIN, 6.75e5, e, nu0);
            for k in 0..=12u32 {
                // 1e-3·10^k — powers of ten are exact in f64 up to 10^22,
                // and the crate clippy.toml bans f64::powi in test code too.
                let dt = (0..k).fold(1e-3, |d, _| d * 10.0);
                let (r_ref, v_ref) = propagate(r0, v0, MU_TERRIN, dt).unwrap();
                let (r_got, v_got) = elements_state_at(&el, MU_TERRIN, dt).unwrap();
                let er = vec_rel_err(r_got, r_ref);
                let ev = vec_rel_err(v_got, v_ref);
                if er > worst_r {
                    worst_r = er;
                    worst_at = (e, dt);
                }
                worst_v = worst_v.max(ev);
                assert!(
                    er < 1e-6 && ev < 1e-6,
                    "round-trip e={e} dt={dt}: r_err={er:e} v_err={ev:e}"
                );
            }
        }
        eprintln!("sweep worst: r {worst_r:e} @ (e,dt)={worst_at:?}, v {worst_v:e}");
    }

    /// Circular-orbit analytic check (75 km Terrin LEO, the §3.0 canonical
    /// number): a quarter period is a quarter turn; radius and speed stay
    /// constant; a full period returns the seed state.
    #[test]
    fn circular_quarter_period_is_analytic() {
        let a = 6.75e5;
        let (r0, v0) = conic_state(MU_TERRIN, a, 0.0, 0.0, 0.0, 0.0, 0.0);
        let period = TAU * libm::sqrt(a * a * a / MU_TERRIN);
        let (rq, vq) = propagate(r0, v0, MU_TERRIN, period / 4.0).unwrap();
        assert!(vec_rel_err(rq, DVec3::new(0.0, a, 0.0)) < 1e-9);
        assert!(vec_rel_err(vq, DVec3::new(-v0.norm(), 0.0, 0.0)) < 1e-9);
        let (rt, vt) = propagate(r0, v0, MU_TERRIN, period).unwrap();
        assert!(vec_rel_err(rt, r0) < 1e-9);
        assert!(vec_rel_err(vt, v0) < 1e-9);
    }

    /// `state_at` purity (§2.8 "cheap, pure"): identical inputs give
    /// bitwise-identical outputs.
    #[test]
    fn state_at_is_bitwise_pure() {
        let model = star_only_model();
        let (el, _, _) = elements_for(MU_SOLEN, TERRIN_A, 0.051, 1.0);
        let s = OrbitState {
            parent: BodyId::from("sol"),
            elements: el,
            epoch: SimTime::from_ticks(0),
        };
        let (r1, v1) = model.state_at(&s, SimTime::from_ticks(6_400_000)).unwrap();
        let (r2, v2) = model.state_at(&s, SimTime::from_ticks(6_400_000)).unwrap();
        assert_eq!(r1.x.to_bits(), r2.x.to_bits());
        assert_eq!(r1, r2);
        assert_eq!(v1, v2);
    }

    /// §2.8 item 4 failure path: a hyperbolic state asked `1e300 s` ahead
    /// can never have its χ bracketed within 64 iterations —
    /// `Recoverable`; `state_at` on broken elements errors and the state
    /// is untouched by construction (the model takes `&OrbitState` — the
    /// "previous valid state" fallback is structural now that the §2.8
    /// sketch's `advance` re-stamp is deliberately absent, trait doc).
    #[test]
    fn non_convergence_is_recoverable_and_preserves_state() {
        let (el, r0, v0) = elements_for(MU_TERRIN, 6.75e5, 3.0, 0.4);
        assert!(matches!(
            propagate(r0, v0, MU_TERRIN, 1e300),
            Err(SimError::Recoverable(_))
        ));
        assert!(matches!(
            elements_state_at(&el, MU_TERRIN, 1e300),
            Err(SimError::Recoverable(_))
        ));
        // Garbage elements are Recoverable too, never a panic.
        let mut bad = el.clone();
        bad.semi_major_axis_m = f64::NAN;
        assert!(matches!(
            elements_to_cartesian(&bad, MU_TERRIN),
            Err(SimError::Recoverable(_))
        ));
        // Each orientation angle NaN'd in turn is rejected at the single
        // decode funnel — before the finiteness gate a NaN angle came
        // back as Ok((NaN, NaN, NaN)) and silently blinded the SOI solver.
        for field in 0..3u8 {
            let mut bad_angle = el.clone();
            match field {
                0 => bad_angle.inclination_rad = f64::NAN,
                1 => bad_angle.ascending_node_rad = f64::NAN,
                _ => bad_angle.arg_periapsis_rad = f64::NAN,
            }
            assert!(
                matches!(
                    elements_to_cartesian(&bad_angle, MU_TERRIN),
                    Err(SimError::Recoverable(_))
                ),
                "NaN angle field {field} must be Recoverable"
            );
        }
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("terrin"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: None,
                elements: None,
            },
        );
        let model = PatchedConics::new(bodies);
        // A finite-but-degenerate elements set for the state_at check:
        // evaluation errors and — the model taking `&OrbitState` — the
        // state is untouched by construction.
        bad.semi_major_axis_m = 0.0; // hyperbolic e with a = 0 → invalid
        let s = OrbitState {
            parent: BodyId::from("terrin"),
            elements: bad,
            epoch: SimTime::from_ticks(0),
        };
        assert!(matches!(
            model.state_at(&s, SimTime::from_ticks(64)),
            Err(SimError::Recoverable(_))
        ));
    }

    // -- SOI scan fixtures ---------------------------------------------------
    //
    // `star_only_model`: star `sol` only — for pure state_at tests.
    // `flyby_system`: star + planet `p` on a circular `a_p` orbit — the
    //   graze test's stage.
    // The ordering test builds its own star+planet+moon inline.
    //
    // The graze construction: the vessel is given a tangential velocity
    // ≈ the planet's at encounter, so the *relative* speed is the
    // difference (~0.4·v_circ) — a slow skim that makes the SOI residence
    // `chord/|w|` controllable in units of the scan stride `h`.

    fn star_only_model() -> PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        PatchedConics::new(bodies)
    }

    fn flyby_system(a_p: f64) -> PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("p"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: Some(BodyId::from("sol")),
                elements: Some(circular_elements(a_p, 0.0)),
            },
        );
        PatchedConics::new(bodies)
    }

    /// The scan stride `h` (seconds and ticks) for a vessel with speed
    /// bound `v_v` crossing a body with speed bound `v_b` and `r_soi` —
    /// the same formula the model uses: `0.1·r_soi/(v_v + v_b)`.
    fn stride_h(v_v: f64, v_b: f64, r_soi: f64) -> (f64, u64) {
        let h_s = 0.1 * r_soi / (v_v + v_b);
        (h_s, (h_s * TICK_HZ as f64).ceil().max(1.0) as u64)
    }

    /// Build the grazing vessel: at `t_enc` it sits at `p`'s position plus
    /// `b` radially outward, moving tangentially at `1.4·v_circ` — so its
    /// relative path is a line offset `b` from `p`'s center and the SOI
    /// residence is ≈ `2·sqrt(r_soi² − b²) / (0.4·v_circ)`.
    ///
    /// Returns the vessel `OrbitState` (epoch = the encounter tick) and
    /// `t_enc` in seconds.
    fn graze_vessel(a_p: f64, residence_s: f64, t_enc: f64) -> OrbitState {
        let v_circ = libm::sqrt(MU_SOLEN / a_p);
        let n_p = libm::sqrt(MU_SOLEN / (a_p * a_p * a_p));
        let w = 0.4 * v_circ;
        let chord = residence_s * w;
        assert!(chord < 2.0 * TERRIN_SOI, "chord must fit inside the SOI");
        let b = libm::sqrt(TERRIN_SOI * TERRIN_SOI - (chord / 2.0) * (chord / 2.0));
        let theta = n_p * t_enc;
        let aim = DVec3::new(
            (a_p + b) * libm::cos(theta),
            (a_p + b) * libm::sin(theta),
            0.0,
        );
        let vel = DVec3::new(
            -1.4 * v_circ * libm::sin(theta),
            1.4 * v_circ * libm::cos(theta),
            0.0,
        );
        OrbitState {
            parent: BodyId::from("sol"),
            elements: cartesian_to_elements(aim, vel, MU_SOLEN).unwrap(),
            epoch: SimTime::from_ticks((t_enc * TICK_HZ as f64) as u64),
        }
    }

    /// §2.6.6 item 5 (upper half): a grazing pass whose SOI residence
    /// exceeds `h` is always detected — and re-querying past the entry
    /// finds the matching exit (sign changes in both directions).
    #[test]
    fn soi_graze_residence_above_h_is_detected() {
        let a_p = TERRIN_A;
        let model = flyby_system(a_p);
        // First design pass at h with the expected bounds (vessel at its
        // periapsis ≈ 1.4·v_circ; planet circular ⇒ bound = v_circ).
        let v_circ = libm::sqrt(MU_SOLEN / a_p);
        let (h_est, _) = stride_h(1.4 * v_circ, v_circ, TERRIN_SOI);
        let residence = 1.4 * h_est;
        let t_enc = 1.0e6;
        let s = graze_vessel(a_p, residence, t_enc);
        // Honest check: the residence really is above the model's h.
        let vessel_bound = speed_bound(&s.elements, MU_SOLEN).unwrap();
        let (h, _) = stride_h(vessel_bound, v_circ, TERRIN_SOI);
        assert!(
            residence > h,
            "designed residence {residence} must exceed h {h}"
        );
        let now = SimTime::from_ticks(((t_enc - residence) * TICK_HZ as f64) as u64);
        let horizon = SimTime::from_ticks(((t_enc + 2.0 * residence) * TICK_HZ as f64) as u64);
        let ev = model.next_soi_event(&s, now, horizon).unwrap().unwrap();
        assert_eq!(ev.body, BodyId::from("p"));
        assert_eq!(ev.direction, SoiDirection::Enter);
        assert!(ev.tick > now.ticks() && ev.tick <= horizon.ticks());
        // The next event from the same pass is the Exit.
        let ev2 = model
            .next_soi_event(&s, SimTime::from_ticks(ev.tick), horizon)
            .unwrap()
            .unwrap();
        assert_eq!(ev2.body, BodyId::from("p"));
        assert_eq!(ev2.direction, SoiDirection::Exit);
        assert!(ev2.tick > ev.tick);
        // The detected dwell should track the straight-line design
        // estimate to ~20% — the design ignores trajectory curvature in
        // both bodies' frames, so it can't hold tighter.
        let dwell = (ev2.tick - ev.tick) as f64 / TICK_HZ as f64;
        assert!(
            (dwell - residence).abs() < 0.2 * residence,
            "dwell {dwell} vs designed {residence}"
        );
    }

    /// §2.6.6 item 5 (lower half): residence below `h` may be missed — and
    /// with the scan grid phased so no sample lands inside the short
    /// residence, it *is* missed.
    #[test]
    fn soi_graze_residence_below_h_is_missed() {
        let a_p = TERRIN_A;
        let model = flyby_system(a_p);
        let v_circ = libm::sqrt(MU_SOLEN / a_p);
        let (h_est, _) = stride_h(1.4 * v_circ, v_circ, TERRIN_SOI);
        let residence = 0.6 * h_est;
        let t_enc = 1.0e6;
        let s = graze_vessel(a_p, residence, t_enc);
        let vessel_bound = speed_bound(&s.elements, MU_SOLEN).unwrap();
        let (h, h_ticks) = stride_h(vessel_bound, v_circ, TERRIN_SOI);
        assert!(
            residence < h,
            "designed residence {residence} must be below h {h}"
        );
        // Residence ≈ [t_enc − residence/2, t_enc + residence/2]. Phase
        // `now` so a grid point lands ~0.2·h before entry and the next
        // lands ~0.2·h after exit: no sample inside the residence.
        let t_in_ticks = ((t_enc - residence / 2.0) * TICK_HZ as f64) as u64;
        let now_ticks = t_in_ticks - 2 * h_ticks - h_ticks / 5;
        let now = SimTime::from_ticks(now_ticks);
        let horizon = SimTime::from_ticks(now_ticks + 6 * h_ticks);
        let ev = model.next_soi_event(&s, now, horizon).unwrap();
        assert!(
            ev.is_none(),
            "residence {residence}s < h {h}s must be missed, got {ev:?}"
        );
    }

    /// Earliest-event ordering across bodies: star + planet `p` + moon `m`
    /// — a vessel (parent `p`) escapes `p` on a hyperbola steered through
    /// `m`'s SOI; events must be Enter{m} → Exit{m} → Exit{p}.
    #[test]
    fn soi_events_order_across_bodies() {
        let moon_a = 1.2e7; // Rask's semi-major axis (§3.0)
        let moon_soi = 2.43e6;
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("p"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: Some(BodyId::from("sol")),
                elements: Some(circular_elements(TERRIN_A, 0.0)),
            },
        );
        // Vessel: hyperbola about `p`, periapsis 7e5 on +x at t = 0, e = 1.5.
        let e_v = 1.5;
        let r_p = 7.0e5;
        let a_v: f64 = -r_p / (e_v - 1.0);
        // Where/when the vessel crosses r = moon_a outbound: the same
        // hyperbolic anomaly chain the conversion uses (in-test, with libm).
        let p_latus = a_v.abs() * (e_v * e_v - 1.0);
        let cos_nu = (p_latus / moon_a - 1.0) / e_v;
        let nu_cross = libm::acos(cos_nu);
        let f_anom =
            libm::asinh(libm::sqrt(e_v * e_v - 1.0) * libm::sin(nu_cross) / (1.0 + e_v * cos_nu));
        let m_hyp = e_v * libm::sinh(f_anom) - f_anom;
        let n_v = libm::sqrt(MU_TERRIN / (a_v.abs() * a_v.abs() * a_v.abs()));
        let t_cross = m_hyp / n_v;
        // Aim the moon at the crossing point: its mean anomaly at epoch is
        // set so its true position at t_cross equals the vessel's.
        let n_moon = libm::sqrt(MU_TERRIN / (moon_a * moon_a * moon_a));
        let moon_m0 = nu_cross - n_moon * t_cross;
        bodies.insert(
            BodyId::from("m"),
            BodyOrbit {
                mu_m3_per_s2: MU_RASK,
                soi_m: Some(moon_soi),
                parent: Some(BodyId::from("p")),
                elements: Some(circular_elements(moon_a, moon_m0)),
            },
        );
        let model = PatchedConics::new(bodies);
        let s = OrbitState {
            parent: BodyId::from("p"),
            elements: KeplerElements {
                semi_major_axis_m: a_v,
                eccentricity: e_v,
                inclination_rad: 0.0,
                ascending_node_rad: 0.0,
                arg_periapsis_rad: 0.0, // periapsis on +x, departure toward +y
                mean_anomaly_epoch_rad: 0.0,
                period_s: f64::INFINITY,
            },
            epoch: SimTime::from_ticks(0),
        };
        let horizon = SimTime::from_ticks(200_000 * TICK_HZ);
        let ev1 = model
            .next_soi_event(&s, SimTime::from_ticks(0), horizon)
            .unwrap()
            .unwrap();
        assert_eq!(ev1.body, BodyId::from("m"));
        assert_eq!(ev1.direction, SoiDirection::Enter);
        assert!(ev1.tick as f64 / TICK_HZ as f64 > t_cross * 0.5);
        let ev2 = model
            .next_soi_event(&s, SimTime::from_ticks(ev1.tick), horizon)
            .unwrap()
            .unwrap();
        assert_eq!(ev2.body, BodyId::from("m"));
        assert_eq!(ev2.direction, SoiDirection::Exit);
        let ev3 = model
            .next_soi_event(&s, SimTime::from_ticks(ev2.tick), horizon)
            .unwrap()
            .unwrap();
        assert_eq!(ev3.body, BodyId::from("p"));
        assert_eq!(ev3.direction, SoiDirection::Exit);
        assert!(ev1.tick < ev2.tick && ev2.tick < ev3.tick);
    }

    /// An orbit parked mid-SOI produces no events inside the horizon.
    #[test]
    fn soi_quiet_orbit_reports_none() {
        let model = flyby_system(TERRIN_A);
        let s = OrbitState {
            parent: BodyId::from("sol"),
            elements: circular_elements(TERRIN_A * 1.5, 0.0),
            epoch: SimTime::from_ticks(0),
        };
        let ev = model
            .next_soi_event(&s, SimTime::from_ticks(0), SimTime::from_ticks(1_000_000))
            .unwrap();
        assert!(ev.is_none());
    }

    /// The equatorial `ω` convention must carry the orbit's own `ŵ`
    /// direction — the general in-plane formula (q̂ = ŵ×n̂) does; the
    /// deleted `atan2(e_y, e_x)` special case measured about +ẑ and was
    /// off by 2ω at `i = π` with zero test coverage. Round-trips at
    /// `i = π` and `i = π − 1e-12` (still inside `NODE_TOL`), elliptic
    /// and hyperbolic, over several anomalies.
    #[test]
    fn retrograde_equatorial_arg_periapsis_round_trips() {
        let mut worst = 0.0f64;
        for &(e, incl) in &[(0.3, PI), (1.5, PI), (0.3, PI - 1e-12), (1.5, PI - 1e-12)] {
            for &nu in &[0.0, 0.4, 1.2] {
                if e > 1.0 && nu >= libm::acos(-1.0 / e) - 0.05 {
                    continue;
                }
                let (r0, v0) = conic_state(MU_TERRIN, 6.75e5, e, nu, 0.0, incl, 0.9);
                let el = cartesian_to_elements(r0, v0, MU_TERRIN).unwrap();
                // The recovered inclination is the retrograde plane
                // itself, and the decoded state is the seed state.
                assert!(rel_err(el.inclination_rad, incl) < 1e-9);
                let (r1, v1) = elements_to_cartesian(&el, MU_TERRIN).unwrap();
                let er = vec_rel_err(r1, r0);
                let ev = vec_rel_err(v1, v0);
                worst = worst.max(er).max(ev);
                assert!(
                    er < 1e-6 && ev < 1e-6,
                    "retrograde round-trip e={e} i={incl} nu={nu}: r={er:e} v={ev:e}"
                );
                // Idempotence: re-extracting elements from the decoded
                // state reproduces the same ω — the convention is stable.
                let el2 = cartesian_to_elements(r1, v1, MU_TERRIN).unwrap();
                assert!(
                    (el2.arg_periapsis_rad - el.arg_periapsis_rad).abs() < 1e-9,
                    "ω idempotence e={e} i={incl} nu={nu}: {} vs {}",
                    el2.arg_periapsis_rad,
                    el.arg_periapsis_rad
                );
            }
        }
        eprintln!("retrograde-equatorial round-trip worst rel err: {worst:e}");
    }

    /// Analytic anchor, hyperbola — hand-computed with no solver code
    /// on the expected side: at F = 1 the state sits at
    /// `r = |a|·(e·cosh F − 1)`, `ν = 2·atan(√((e+1)/(e−1))·tanh(F/2))`,
    /// reached `(e·sinh F − F)/n` seconds after periapsis.
    #[test]
    fn hyperbolic_anchor_point_matches_closed_form() {
        let a: f64 = -1.4e6; // r_p = |a|·(e−1) = 7e5
        let e = 1.5;
        let f_anom = 1.0;
        let n = libm::sqrt(MU_TERRIN / (a.abs() * a.abs() * a.abs()));
        let m = e * libm::sinh(f_anom) - f_anom; // Kepler: n·(t − τ)
        let el = KeplerElements {
            semi_major_axis_m: a,
            eccentricity: e,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: 0.0,
            period_s: f64::INFINITY,
        };
        let (r, v) = elements_state_at(&el, MU_TERRIN, m / n).unwrap();
        let r_mag = a.abs() * (e * libm::cosh(f_anom) - 1.0);
        let nu = 2.0 * libm::atan(libm::sqrt((e + 1.0) / (e - 1.0)) * libm::tanh(f_anom / 2.0));
        let want = DVec3::new(r_mag * libm::cos(nu), r_mag * libm::sin(nu), 0.0);
        assert!(vec_rel_err(r, want) < 1e-9, "r {r:?} vs {want:?}");
        // Vis-viva: v²/2 − μ/r = −μ/(2a) — the anchor for velocity too.
        let energy = v.norm_squared() / 2.0 - MU_TERRIN / r.norm();
        assert!(rel_err(energy, -MU_TERRIN / (2.0 * a)) < 1e-9);
    }

    /// Analytic anchor, parabola — Barker's equation by hand: with
    /// `D = tan(ν/2)`, `r = p(1+D²)/2` at `t − τ = √(p³/μ)·(D + D³/3)/2`
    /// (the module doc's `M_par` encoding). ν = 60° gives `D² = 1/3`
    /// exactly, so the expected radius is `2p/3` with no solver code on
    /// the expected side.
    #[test]
    fn parabolic_anchor_point_matches_barker() {
        let p: f64 = 1.4e6; // encoded as a := −p
        let nu = PI / 3.0;
        let d = libm::tan(nu / 2.0);
        let n = libm::sqrt(MU_TERRIN / (p * p * p));
        let m = 0.5 * (d + d * d * d / 3.0);
        let el = KeplerElements {
            semi_major_axis_m: -p,
            eccentricity: 1.0,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: 0.0,
            period_s: f64::INFINITY,
        };
        let (r, v) = elements_state_at(&el, MU_TERRIN, m / n).unwrap();
        let r_mag = p * (1.0 + d * d) / 2.0; // = 2p/3 at ν = 60°
        let want = DVec3::new(r_mag * libm::cos(nu), r_mag * libm::sin(nu), 0.0);
        assert!(vec_rel_err(r, want) < 1e-9, "r {r:?} vs {want:?}");
        // Parabolic speed is escape speed at every point: v² = 2μ/r.
        assert!(rel_err(v.norm_squared(), 2.0 * MU_TERRIN / r_mag) < 1e-9);
    }

    /// Near-parabolic elliptic backward propagation: `a³` overflows to
    /// +∞ so the period is unrepresentable — the reduction must be
    /// skipped (`rem_euclid(∞)` turned a negative Δt into NaN, a spurious
    /// `Recoverable`) and the unreduced solve must still work.
    #[test]
    fn near_parabolic_backward_step_skips_period_reduction() {
        let r_p = 6.75e5;
        let a = 1.0e110; // a³ overflows f64 → T = +∞
        let v_p = libm::sqrt(MU_TERRIN * (2.0 / r_p - 1.0 / a));
        let r0 = DVec3::new(r_p, 0.0, 0.0);
        let v0 = DVec3::new(0.0, v_p, 0.0);
        let (r1, v1) = propagate(r0, v0, MU_TERRIN, -60.0).unwrap();
        assert!(r1.is_finite() && v1.is_finite());
        // Forward again by the same step closes back on periapsis.
        let (r2, _) = propagate(r1, v1, MU_TERRIN, 60.0).unwrap();
        assert!(vec_rel_err(r2, r0) < 1e-9);
    }

    /// Tick-lattice differencing: `state_at` computes `t − epoch` in ticks
    /// before converting to seconds. At `ticks ≈ 2⁵²`, `t.seconds()` and
    /// `epoch.seconds()` differ by less than one f64 ulp — the old
    /// `seconds() − seconds()` path collapsed single-tick steps to
    /// exactly 0.
    #[test]
    fn state_at_preserves_tick_precision_at_large_ticks() {
        let model = star_only_model();
        let (el, _, _) = elements_for(MU_SOLEN, TERRIN_A, 0.051, 1.0);
        let big: u64 = 1 << 52; // seconds ≈ 7e13; ulp(seconds) ≫ 1 tick
        let s = OrbitState {
            parent: BodyId::from("sol"),
            elements: el,
            epoch: SimTime::from_ticks(big),
        };
        let (r0, _) = model.state_at(&s, SimTime::from_ticks(big)).unwrap();
        let (r1, _) = model.state_at(&s, SimTime::from_ticks(big + 1)).unwrap();
        assert_ne!(r0, r1, "one tick of motion must survive at t=2^52");
        // The step is one tick of orbital motion: bounded by the
        // orbit's own speed bound / TICK_HZ, and non-negligible.
        let bound = speed_bound(&s.elements, MU_SOLEN).unwrap();
        let step = r0.distance(r1);
        assert!(step > 0.0 && step <= bound / TICK_HZ as f64 * 1.0001);
    }

    /// `PatchedConics::from_body_defs` over the real §3.0 data —
    /// `data/systems/home/` — must reproduce the authored body set
    /// exactly: μ, SOI, parent, and elements carried through verbatim.
    #[test]
    fn from_body_defs_matches_authored_home_system() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("data/systems/home");
        let sys = csp_data::load_system(&dir).unwrap();
        let defs: Vec<csp_data::BodyDef> = sys.bodies.iter().map(|b| b.def.clone()).collect();
        let model = PatchedConics::from_body_defs(&defs).unwrap();
        assert_eq!(model.bodies.len(), 4);
        for def in &defs {
            let b = &model.bodies[&def.id];
            assert_eq!(b.mu_m3_per_s2, def.mu_m3_per_s2, "{}", def.id);
            assert_eq!(b.soi_m, def.soi_m, "{}", def.id);
            assert_eq!(b.parent, def.parent, "{}", def.id);
            assert_eq!(b.elements, def.orbit, "{}", def.id);
        }
        // And the model is live: Terrin's epoch state is its authored
        // circular orbit at r = a about Solen.
        let (r, v) = model
            .body_state(&BodyId::from("terrin"), SimTime::from_ticks(0))
            .unwrap();
        assert!(rel_err(r.norm(), TERRIN_A) < 1e-9);
        assert!(rel_err(v.norm(), libm::sqrt(MU_SOLEN / TERRIN_A)) < 1e-9);
    }

    /// §2.6.6 item 4's debug surface reports exactly what the scan
    /// walks — same candidate set and same stride formula because both
    /// come through the same helpers. Asserted against an independent
    /// in-test recomputation.
    #[test]
    fn scan_strides_reports_the_scans_own_stride() {
        let moon_a = 1.2e7;
        let moon_soi = 2.43e6;
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("p"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: Some(BodyId::from("sol")),
                elements: Some(circular_elements(TERRIN_A, 0.0)),
            },
        );
        bodies.insert(
            BodyId::from("m"),
            BodyOrbit {
                mu_m3_per_s2: MU_RASK,
                soi_m: Some(moon_soi),
                parent: Some(BodyId::from("p")),
                elements: Some(circular_elements(moon_a, 0.0)),
            },
        );
        let model = PatchedConics::new(bodies);
        let s = OrbitState {
            parent: BodyId::from("p"),
            elements: KeplerElements {
                semi_major_axis_m: -1.4e6,
                eccentricity: 1.5,
                inclination_rad: 0.0,
                ascending_node_rad: 0.0,
                arg_periapsis_rad: 0.0,
                mean_anomaly_epoch_rad: 0.0,
                period_s: f64::INFINITY,
            },
            epoch: SimTime::from_ticks(0),
        };
        let scans = model.scan_strides(&s).unwrap();
        // The candidate set the scan walks: {parent p} ∪ {child m}.
        assert_eq!(scans.len(), 2);
        assert_eq!(scans[0].body, BodyId::from("p"));
        assert_eq!(scans[1].body, BodyId::from("m"));
        // The reported strides are the spec formula evaluated
        // independently — the same values `scan_candidate` steps by.
        let v_v = speed_bound(&s.elements, MU_TERRIN).unwrap();
        let v_m = speed_bound(&circular_elements(moon_a, 0.0), MU_TERRIN).unwrap();
        assert_eq!(scans[0].stride_ticks, stride_h(v_v, 0.0, TERRIN_SOI).1);
        assert_eq!(scans[1].stride_ticks, stride_h(v_v, v_m, moon_soi).1);
    }

    /// A child carrying an SOI but no elements is a model error — and
    /// the scan and the overlay reach the *same* verdict now that both
    /// enumerate candidates and compute `v_rel_max` through one path
    /// (previously the scan erred while `scan_strides` skipped it).
    #[test]
    fn child_soi_without_elements_errors_in_scan_and_overlay() {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("p"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: Some(BodyId::from("sol")),
                elements: Some(circular_elements(TERRIN_A, 0.0)),
            },
        );
        bodies.insert(
            BodyId::from("m"),
            BodyOrbit {
                mu_m3_per_s2: MU_RASK,
                soi_m: Some(2.43e6),
                parent: Some(BodyId::from("p")),
                elements: None, // SOI with no orbit — the broken case
            },
        );
        let model = PatchedConics::new(bodies);
        let s = OrbitState {
            parent: BodyId::from("p"),
            elements: circular_elements(7.0e5, 0.0),
            epoch: SimTime::from_ticks(0),
        };
        assert!(matches!(
            model.next_soi_event(&s, SimTime::from_ticks(0), SimTime::from_ticks(640)),
            Err(SimError::Recoverable(_))
        ));
        assert!(matches!(
            model.scan_strides(&s),
            Err(SimError::Recoverable(_))
        ));
    }

    /// A NaN orientation angle must surface as `Err` on *every* decode
    /// path — including `next_soi_event`, where an unchecked
    /// `Ok((NaN,…))` vessel state would have silently scanned NaN
    /// distances and reported `None` forever.
    #[test]
    fn nan_angle_orbit_state_fails_next_soi_event() {
        let model = flyby_system(TERRIN_A);
        for field in 0..3u8 {
            let mut el = circular_elements(TERRIN_A * 1.5, 0.0);
            match field {
                0 => el.inclination_rad = f64::NAN,
                1 => el.ascending_node_rad = f64::NAN,
                _ => el.arg_periapsis_rad = f64::NAN,
            }
            let s = OrbitState {
                parent: BodyId::from("sol"),
                elements: el,
                epoch: SimTime::from_ticks(0),
            };
            assert!(
                matches!(
                    model.next_soi_event(
                        &s,
                        SimTime::from_ticks(0),
                        SimTime::from_ticks(1_000_000)
                    ),
                    Err(SimError::Recoverable(_))
                ),
                "NaN angle field {field} must error, never scan silently"
            );
        }
    }

    /// §2.6.5 back-pressure on the §2.6.6 coarse scan: a grid needing
    /// more than the sample budget fails `Recoverable` — never
    /// `Ok(None)` over an unscanned suffix, never an unbounded loop in
    /// the warp path. Exercised through the explicit-budget entry point
    /// the trait impl binds `SOI_SCAN_MAX_SAMPLES` onto.
    #[test]
    fn soi_scan_sample_budget_is_enforced() {
        let model = flyby_system(TERRIN_A);
        // Vessel well outside p's orbit: d(t) never approaches the SOI,
        // so the scan walks the whole grid — the worst case for the
        // budget — instead of returning early on a bracket.
        let s = OrbitState {
            parent: BodyId::from("sol"),
            elements: circular_elements(TERRIN_A * 3.0, 0.0),
            epoch: SimTime::from_ticks(0),
        };
        // Stride here is ~3.7e4 ticks; the 2e6-tick horizon needs ~55
        // samples, so a 32-sample budget is exhausted mid-scan.
        let horizon = SimTime::from_ticks(2_000_000);
        assert!(matches!(
            model.next_soi_event_within(&s, SimTime::from_ticks(0), horizon, 32, None, None),
            Err(SimError::Recoverable(_))
        ));
        // With headroom the identical call completes honestly — and its
        // reported spend matches the stride-grid bound `span/h + 2`.
        let (ev, samples) = model
            .next_soi_event_within(&s, SimTime::from_ticks(0), horizon, 1_000_000, None, None)
            .unwrap();
        assert!(ev.is_none());
        let span = horizon.ticks();
        let h = stride_h(
            speed_bound(&s.elements, MU_SOLEN).unwrap(),
            libm::sqrt(MU_SOLEN / TERRIN_A),
            TERRIN_SOI,
        )
        .1;
        assert_eq!(samples, 1 + span.div_ceil(h));
    }

    /// §2.6.6 wrong-side tolerance: a vessel re-based onto `m` whose Brent
    /// tick landed a hair early can sit marginally OUTSIDE `m`'s SOI — the
    /// first-stride `Enter{m}` the scan would report is a degenerate
    /// `Enter{own-parent}` artifact, not a real crossing. With
    /// `suppress = Some((m, Enter))` that detection is dropped and the
    /// scan walks on; without it the artifact fires (and the world maps
    /// the failed apply to `degraded`, never `Err`).
    #[test]
    fn first_stride_suppression_drops_wrong_side_redetection() {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("p"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: Some(BodyId::from("sol")),
                elements: Some(circular_elements(TERRIN_A, 0.0)),
            },
        );
        bodies.insert(
            BodyId::from("m"),
            BodyOrbit {
                mu_m3_per_s2: MU_RASK,
                soi_m: Some(2.43e6),
                parent: Some(BodyId::from("p")),
                elements: Some(circular_elements(1.2e7, 0.0)),
            },
        );
        let model = PatchedConics::new(bodies);
        // Vessel *parented to m* but parked just outside its SOI (the
        // marginal wrong-side position a sub-tick Brent landing leaves),
        // on an inbound leg: state_at gives a conic genuinely crossing
        // inward.
        let r0 = DVec3::new(2.44e6, 0.0, 0.0); // just above m's r_soi 2.43e6
        let v0 = DVec3::new(-80.0, 150.0, 0.0); // inbound, sub-circular
        let el = cartesian_to_elements(r0, v0, MU_RASK).unwrap();
        let s = OrbitState {
            parent: BodyId::from("m"),
            elements: el,
            epoch: SimTime::from_ticks(0),
        };
        let horizon = SimTime::from_ticks(4_000_000);
        let (unsup, _) = model
            .next_soi_event_within(
                &s,
                SimTime::from_ticks(0),
                horizon,
                SOI_SCAN_MAX_SAMPLES,
                None,
                None,
            )
            .unwrap();
        let ev = unsup.expect("an inbound vessel must cross inward");
        assert_eq!(ev.body, BodyId::from("m"));
        assert_eq!(ev.direction, SoiDirection::Enter); // the degenerate artifact
        // The same scan with the just-crossed boundary suppressed drops
        // exactly that detection; whatever it reports next is a different,
        // later event (or none within the window) — never the first-stride
        // Enter again.
        let (sup, _) = model
            .next_soi_event_within(
                &s,
                SimTime::from_ticks(0),
                horizon,
                SOI_SCAN_MAX_SAMPLES,
                Some((BodyId::from("m"), SoiDirection::Enter)),
                None,
            )
            .unwrap();
        match sup {
            None => {}
            Some(e2) => {
                assert!(
                    !(e2.direction == SoiDirection::Enter && e2.tick == ev.tick),
                    "the first-stride Enter{{m}} must be suppressed: {e2:?}"
                );
            }
        }
    }
}
