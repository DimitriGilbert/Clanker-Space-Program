//! The transfer planner (PLAN M5 exit row "Lambert/porkchop planner;
//! encounter prediction; transfer-window events" — the machinery behind
//! §2.11's *Transfer* job: "Lambert solver + porkchop scan + local
//! refinement; outputs maneuver nodes").
//!
//! # Where this lives, and why
//!
//! Everything here is a **pure query** over [`PatchedConics`]: nothing in
//! this module mutates sim state, holds a clock, or reads one — times
//! arrive as explicit ticks. The §2.11 *Transfer* job itself (a csp-ai
//! autopilot, M9a) is a thin orchestrator that calls these functions and
//! issues the `SetManeuver` / `WatchTransferWindow` commands the results
//! imply. The planner lives in `csp-sim` rather than `csp-ai` for the
//! same reason `maneuver::predict_maneuver` does: it is built entirely
//! on the orbit model's closed forms, and keeping it inside the
//! determinism domain (`libm`-only, fixed iteration counts, no
//! hash-map iteration) means its answers are exactly reproducible —
//! which is also what lets a command carry "the planned departure tick"
//! without the sim needing to re-run the scan.
//!
//! # Pieces
//!
//! - [`lambert`] / [`lambert_multi`] — the classical two-position +
//!   time-of-flight solve in the universal-variable (Stumpff `ψ`)
//!   form, the same conic machinery `orbit` propagates with.
//! - [`porkchop_scan`] — the (departure-date, transfer-duration) Δv
//!   surface over a bounded grid, plus its strict local minima.
//! - [`plan_transfer`] — scan + local refinement → a [`TransferPlan`]:
//!   the window's optimal leg, the Δv split, the v∞ vectors, and the
//!   transfer conic itself.
//! - [`predict_encounter`] — what a hypothetical conic commits to:
//!   the §2.6.6 scan over a conic that was never spawned, the patched-
//!   conics stitch ([`PatchedConics::rebase`]) for the approach
//!   geometry, and the post-transition conic for chaining.
//! - [`departure_node`] — the plan → `SetManeuver` bridge: the
//!   single-impulse ejection geometry for a vessel on a closed parking
//!   orbit, resolved to `{tick, prograde, normal, radial}`.
//!   **Limitation:** the burn fires up to one parking period before
//!   `departure_tick` and the SOI egress transit is modeled as
//!   instantaneous, so the node does NOT compose to arrival on its
//!   own — the vessel misses `r2` at `arrival_tick` without correction.
//!   The [`predict_encounter`] → re-plan correction loop is REQUIRED,
//!   not optional (see the function's docs).
//!
//! # Solver coverage (stated honestly)
//!
//! - **Zero-rev** (the porkchop domain): both ways — `Short` (the
//!   sub-π arc, `sinΔν > 0`) and `Long` (the sup-π arc, `sinΔν < 0`).
//!   The zero-rev time-of-flight spans `(0, ∞)` on the branch
//!   `ψ ∈ (−∞, (2π)²)`, so a solution exists for every `tof > 0`.
//! - **Multi-rev** (`revs ≥ 1`): the `ψ` branch
//!   `((2πN)², (2π(N+1))²)`, whose TOF curve goes `∞` at both poles
//!   with a single interior minimum — both roots are returned when
//!   `tof > T_min`, empty when `tof ≤ T_min`.
//! - **Validation** (the real hazard): `z_solved`'s residual check
//!   validates the TOF formula *against itself*, which is not
//!   sufficient alone — for Long-way hyperbolic solves the two terms
//!   of `√μT = x³S + A√y` catastrophically cancel, so a corrupted `T`
//!   can sit inside the tolerance at a wrong root. Every accepted
//!   root is therefore validated by propagation: `(r1, v1, tof)` must
//!   land on `r2` (the lands check). A root that fails is dropped,
//!   never returned as a wrong answer.
//! - **Degenerate**: collinear endpoints (no orbital plane),
//!   `tof ≤ 0`, non-finite or non-positive μ are `Recoverable`.
//!
//! Determinism note: the solves are f64-precision, not tick-quantized —
//! the planner's job is *which* tick to burn at; the sim then owns the
//! tick-exact answer.

use csp_core::{DVec3, SimError, SimTime, TICK_HZ};
use csp_data::{BodyId, KeplerElements};

use crate::maneuver::burn_frame;
use crate::orbit::{
    E_PARABOLIC_TOL, OrbitModel, OrbitState, PatchedConics, SoiDirection, SoiEvent,
    cartesian_to_elements, perifocal_to_inertial, propagate, stumpff,
};

const TAU: f64 = core::f64::consts::TAU;

/// Iteration caps — every solve in this module is fixed-bound
/// (determinism domain).
const LAMBERT_MAX_ITERS: u32 = 64;
/// Golden-section iterations locating a multi-rev branch's TOF
/// minimum.
const BRANCH_MIN_ITERS: u32 = 64;
/// `departure_node`'s fixed-point iteration on the burn radius — the
/// escape eccentricity depends on `r_b`, which depends on the burn
/// longitude it feeds back into. Bounded, converged-checked.
const EJECTION_MAX_ITERS: u32 = 16;
/// The porkchop grid's total-cell ceiling — the §2.6.5 back-pressure
/// philosophy applied to a planning query: `(2¹⁶)` cells is ~30× a
/// useful 48×48 scan while staying a bounded amount of work per call.
/// Larger scans are the caller's to partition.
pub const PORKCHOP_MAX_CELLS: u64 = 65_536;
/// The local refinement's evaluation budget: a halving pattern search
/// on the tick lattice needs ≈ 8·log₂(cell_span) evals when strides
/// shrink monotonically — but improvement moves keep the stride
/// unhalved, so a descending-staircase surface iterates far longer.
/// This cap bounds that reachable adversarial case explicitly.
const REFINE_MAX_EVALS: u32 = 512;
/// A Lambert ψ residual counts as solved below this relative TOF
/// error — planner-grade precision: the burn lands on an integer tick
/// regardless, so sub-second solves buy nothing.
fn lambert_tol_t(tof_s: f64) -> f64 {
    (tof_s.abs() * 1e-10).max(1e-6)
}

// ---------------------------------------------------------------------------
// Lambert solver — universal-variable (Stumpff ψ) form
// ---------------------------------------------------------------------------

/// Which arc connects the endpoints — the two solutions of the planar
/// boundary problem. `Short` travels the arc subtending `Δν < π` (the
/// transfer's angular momentum points along `r1 × r2`); `Long` travels
/// the `Δν > π` arc (angular momentum along `−r1 × r2`). A porkchop
/// evaluates both and keeps the cheaper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LambertWay {
    /// The `Δν < π` arc — `A > 0` in the BMW form.
    Short,
    /// The `Δν > π` arc — `A < 0`.
    Long,
}

/// One Lambert solve's output: the transfer conic's velocity at each
/// endpoint, in the same parent-centered inertial frame the inputs
/// were given in.
#[derive(Clone, Debug, PartialEq)]
pub struct LambertSolution {
    /// Required velocity at `r1` (m/s) — the conic departs along this.
    pub v1: DVec3,
    /// Velocity at `r2` (m/s) on arrival.
    pub v2: DVec3,
    /// Whole revolutions the conic completes between the endpoints —
    /// always 0 from [`lambert`]; the branch index from
    /// [`lambert_multi`].
    pub revs: u32,
}

/// The BMW geometry constants of one Lambert problem: `A` is the
/// signed parameter `±√(r1·r2·(1+cosΔν))` — the `sinΔν`-factored form
/// `sinΔν·√(r1r2/(1−cosΔν))` with the sign folded out, which is what
/// [`LambertWay`] selects.
struct LambertGeom {
    /// Departure position (m).
    r1: DVec3,
    /// Arrival position (m).
    r2: DVec3,
    /// `|r1|`, `|r2|` — kept for the f&g coefficient forms.
    r1n: f64,
    r2n: f64,
    /// Signed `A`: `+√(r1·r2·(1+cosΔν))` for `Short`, `−` for `Long`.
    a: f64,
}

fn lambert_geom(r1: DVec3, r2: DVec3, way: LambertWay) -> Result<LambertGeom, SimError> {
    if !(r1.is_finite() && r2.is_finite()) {
        return Err(SimError::Recoverable("lambert: non-finite endpoints"));
    }
    let (r1n, r2n) = (r1.norm(), r2.norm());
    if !(r1n > 0.0 && r2n > 0.0) {
        return Err(SimError::Recoverable("lambert: zero endpoint radius"));
    }
    let cos_dnu = (r1.dot(r2) / (r1n * r2n)).clamp(-1.0, 1.0);
    // Collinear endpoints (Δν = 0 or π) define no orbital plane —
    // `1 + cosΔν → 0` also sends `A → 0`, which is why the check is
    // phrased on the geometry, not on `A`.
    let one_p = 1.0 + cos_dnu;
    if one_p <= 1e-14 {
        return Err(SimError::Recoverable("lambert: collinear endpoints"));
    }
    if r1.cross(r2).norm_squared() <= 1e-14 * r1n * r1n * r2n * r2n {
        return Err(SimError::Recoverable("lambert: collinear endpoints"));
    }
    let sign = match way {
        LambertWay::Short => 1.0,
        LambertWay::Long => -1.0,
    };
    Ok(LambertGeom {
        r1,
        r2,
        r1n,
        r2n,
        a: sign * libm::sqrt(r1n * r2n * one_p),
    })
}

/// The BMW time-of-flight at `ψ = z`:
///
/// ```text
/// y(z) = r1 + r2 + A·(z·S − 1)/√C     x(z) = √(y/C)
/// √μ · T(z) = x³·S + A·√y
/// ```
///
/// `None` where the geometry is infeasible at `z`: `C ≤ 0` (the
/// multi-rev poles `z = (2kπ)²`), `y ≤ 0` (the ψ-formulation's
/// infeasible pockets — no conic of this way reaches `r2` at that
/// energy), or a non-finite result.
fn tof_eval(g: &LambertGeom, mu: f64, z: f64) -> Option<f64> {
    let (c2, c3) = stumpff(z);
    if !(c2.is_finite() && c2 > 0.0) {
        return None;
    }
    let y = g.r1n + g.r2n + g.a * (z * c3 - 1.0) / libm::sqrt(c2);
    if !(y.is_finite() && y > 0.0) {
        return None;
    }
    let x = libm::sqrt(y / c2);
    let t = (x * x * x * c3 + g.a * libm::sqrt(y)) / libm::sqrt(mu);
    t.is_finite().then_some(t)
}

/// The sign function the bracket searches: `T(z) − tof` where
/// [`tof_eval`] is defined, **extended** where it is not — the
/// extension is chosen to preserve each infeasible region's behavior
/// at its own edge, so a sign-bracket always bounds a real root:
///
/// - `y ≤ 0` → `T → 0` at the pocket's edge (`x → 0`), so the
///   extension returns `−tof` (below any positive target). This makes
///   the ψ-formulation's infeasible pockets read as `T = 0` plateaus —
///   consistent with the true `T → 0` approach — and keeps brackets
///   spanning a pocket honest.
/// - `C ≤ 0` / non-finite (the poles) → `+∞`.
fn tof_residual(g: &LambertGeom, mu: f64, tof: f64, z: f64) -> f64 {
    match tof_eval(g, mu, z) {
        Some(t) => t - tof,
        None => {
            let (c2, c3) = stumpff(z);
            if c2 > 0.0 {
                let y = g.r1n + g.r2n + g.a * (z * c3 - 1.0) / libm::sqrt(c2);
                if y <= 0.0 {
                    return -tof;
                }
            }
            f64::MAX
        }
    }
}

/// Solve one sign-bracketed `T(z) = tof` root with the canonical
/// `zbrent` structure — the same Brent the SOI refiner runs, on the
/// continuous ψ axis here. Returns the final iterate `b` — best by
/// construction, since `b` is always the smaller-|f| side of the
/// maintained bracket; the loop exits early once the residual sits
/// under [`lambert_tol_t`]. The caller still validates because an
/// extension-flat region (`f64::MAX`/pocket plateaus carry no slope
/// information) can stall interpolation steps.
fn brent_z(
    g: &LambertGeom,
    mu: f64,
    tof: f64,
    mut a: f64,
    mut b: f64,
    mut fa: f64,
    mut fb: f64,
) -> f64 {
    let mut c = a;
    let mut fc = fa;
    let mut e = 0.0;
    let mut d = 0.0;
    let tol_x = 1e-13;
    for _ in 0..LAMBERT_MAX_ITERS {
        if (fb > 0.0) == (fc > 0.0) {
            c = a;
            fc = fa;
            e = b - a;
            d = e;
        }
        if fc.abs() < fb.abs() {
            a = b;
            b = c;
            c = a;
            fa = fb;
            fb = fc;
            fc = fa;
        }
        let tol1 = 2.0 * f64::EPSILON * b.abs() + 0.5 * tol_x;
        let xm = 0.5 * (c - b);
        // Bracket consumed, exact root, or residual already inside the
        // solve tolerance — `b` passes `z_solved` either way.
        if xm.abs() <= tol1 || fb.abs() <= lambert_tol_t(tof) {
            break;
        }
        if e.abs() >= tol1 && fa.abs() > fb.abs() {
            let s = fb / fa;
            let (p, q) = if a == c {
                (2.0 * xm * s, 1.0 - s)
            } else {
                let q1 = fa / fc;
                let r = fb / fc;
                (
                    s * (2.0 * xm * q1 * (q1 - r) - (b - a) * (r - 1.0)),
                    (q1 - 1.0) * (r - 1.0) * (s - 1.0),
                )
            };
            let (p, q) = if p > 0.0 { (p, -q) } else { (-p, q) };
            if q.is_finite()
                && p.is_finite()
                && 2.0 * p < (3.0 * xm * q - (tol1 * q).abs()).min((e * q).abs())
            {
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
        fb = tof_residual(g, mu, tof, b);
    }
    b
}

/// A solved-`z` validity check: the residual must sit under
/// [`lambert_tol_t`] AND the point must be geometrically feasible
/// (`y > 0`, `C > 0`) — a bracket that converged into an infeasible
/// pocket fails here instead of producing velocities.
fn z_solved(g: &LambertGeom, mu: f64, tof: f64, z: f64) -> bool {
    tof_eval(g, mu, z).is_some_and(|t| (t - tof).abs() <= lambert_tol_t(tof))
}

/// The lands check — post-solve validation that does NOT validate the
/// formula against itself: propagate `(r1, v1, tof)` with the orbit
/// model's universal solver and require arrival on `r2`. `z_solved`
/// can accept a wrong root when `√μT = x³S + A√y` catastrophically
/// cancels (the Long-way hyperbolic regime — a corrupted `T` inside
/// [`lambert_tol_t`]); this check catches it because a wrong `z`
/// produces a `v1` whose conic reaches `r2` at a *different* time.
/// Cost: one universal-variable solve per accepted root — cheap
/// against the scan's Brent iteration.
///
/// Tolerance `1e-6·|r2|`: orders of magnitude above the propagator's
/// own precision yet ~10⁶× below an O(1) wrong-root miss — the check
/// exists to reject corrupted roots, not adjudicate last-ulp noise.
fn lands_on(g: &LambertGeom, mu: f64, tof: f64, v1: DVec3) -> bool {
    match propagate(g.r1, v1, mu, tof) {
        Ok((rp, _)) => (rp - g.r2).norm() <= g.r2n * 1e-6,
        Err(_) => false,
    }
}

/// Zero-rev root bracket: `ψ ∈ (−∞, (2π)²)`. `T(0)` partitions it —
/// `T(0) > tof` puts the root on the hyperbolic leg (expand downward
/// until the residual flips, which happens no later than the `y ≤ 0`
/// pocket where the extension reads `−tof`); `T(0) < tof` puts it on
/// `(0, pole)` where `T → ∞` at the pole. Exactly one root exists for
/// every `tof > 0` (the extension plateaus cannot add roots — they
/// only carry sign).
fn zero_rev_z(g: &LambertGeom, mu: f64, tof: f64) -> Option<f64> {
    let f0 = tof_residual(g, mu, tof, 0.0);
    if f0 == 0.0 {
        return Some(0.0);
    }
    let (lo, hi, flo, fhi) = if f0 > 0.0 {
        // Hyperbolic leg: expand downward geometrically until the
        // residual flips — guaranteed by the `y ≤ 0` extension or a
        // genuinely small `T`.
        let mut step = 1.0;
        let mut lo = -step;
        let mut flo = tof_residual(g, mu, tof, lo);
        while flo > 0.0 {
            if step > 1e30 {
                return None; // unreachable for tof > 0 — honest bail
            }
            step *= 4.0;
            lo = -step;
            flo = tof_residual(g, mu, tof, lo);
        }
        (lo, 0.0, flo, f0)
    } else {
        // Elliptic side: the pole at `(2π)²` sends `T → ∞` — sample
        // just inside it.
        let hi = (TAU * TAU) * (1.0 - 1e-12);
        let fhi = tof_residual(g, mu, tof, hi);
        if fhi <= 0.0 {
            return None; // pole extension should never read below
        }
        (0.0, hi, f0, fhi)
    };
    let z = brent_z(g, mu, tof, lo, hi, flo, fhi);
    z_solved(g, mu, tof, z).then_some(z)
}

/// Multi-rev `ψ` solutions for branch `revs ≥ 1`: the branch
/// `(z_lo, z_hi) = ((2πN)², (2π(N+1))²)` has `T → ∞` at both poles and
/// a single interior minimum, so two roots exist when `tof > T_min`
/// (the low- and high-energy legs), none below it. Both candidates are
/// validated post-solve; a stalled bracket drops its root rather than
/// returning garbage.
fn multi_rev_z(g: &LambertGeom, mu: f64, tof: f64, revs: u32) -> Vec<f64> {
    let n = f64::from(revs);
    let z_lo = (TAU * n) * (TAU * n);
    let z_hi = (TAU * (n + 1.0)) * (TAU * (n + 1.0));
    let w = (z_hi - z_lo) * 1e-6;
    let (a, b) = (z_lo + w, z_hi - w);
    // Branch minimum by golden section on the extended residual —
    // extension plateaus carry sign only, so the minimizer lands in a
    // genuinely feasible region whenever the branch's TOF dips below
    // `tof` at all.
    let z_min = {
        let (mut ga, mut gb) = (a, b);
        let invphi = 0.618_033_988_749_894_9_f64; // (√5−1)/2
        let invphi2 = invphi * invphi;
        let mut x1 = ga + invphi2 * (gb - ga);
        let mut x2 = ga + invphi * (gb - ga);
        let mut f1 = tof_residual(g, mu, tof, x1);
        let mut f2 = tof_residual(g, mu, tof, x2);
        for _ in 0..BRANCH_MIN_ITERS {
            if f1 < f2 {
                gb = x2;
                x2 = x1;
                f2 = f1;
                x1 = ga + invphi2 * (gb - ga);
                f1 = tof_residual(g, mu, tof, x1);
            } else {
                ga = x1;
                x1 = x2;
                f1 = f2;
                x2 = ga + invphi * (gb - ga);
                f2 = tof_residual(g, mu, tof, x2);
            }
        }
        0.5 * (ga + gb)
    };
    // `f(z_min) ≥ 0` means `T` never reaches `tof` on this branch.
    let f_min = tof_residual(g, mu, tof, z_min);
    if f_min >= 0.0 {
        return Vec::new();
    }
    let fa = tof_residual(g, mu, tof, a);
    let fb = tof_residual(g, mu, tof, b);
    let mut out = Vec::with_capacity(2);
    // Left root: T falls from ∞ at z_lo to the min → sign bracket.
    if fa > 0.0 {
        let z = brent_z(g, mu, tof, a, z_min, fa, f_min);
        if z_solved(g, mu, tof, z) {
            out.push(z);
        }
    }
    // Right root: symmetric.
    if fb > 0.0 {
        let z = brent_z(g, mu, tof, z_min, b, f_min, fb);
        if z_solved(g, mu, tof, z) {
            out.push(z);
        }
    }
    out
}

/// Recover the endpoint velocities from a solved `ψ` via the Lagrange
/// f&g coefficients:
///
/// ```text
/// f = 1 − y/r1    g = A·√(y/μ)    ġ = 1 − y/r2
/// v1 = (r2 − f·r1)/g              v2 = (ġ·r2 − r1)/g
/// ```
fn lambert_velocities(
    g: &LambertGeom,
    mu: f64,
    z: f64,
    revs: u32,
) -> Result<LambertSolution, SimError> {
    let (c2, c3) = stumpff(z);
    if !(c2.is_finite() && c2 > 0.0) {
        return Err(SimError::Recoverable("lambert: infeasible solution point"));
    }
    let y = g.r1n + g.r2n + g.a * (z * c3 - 1.0) / libm::sqrt(c2);
    if !(y.is_finite() && y > 0.0) {
        return Err(SimError::Recoverable("lambert: infeasible solution point"));
    }
    let f = 1.0 - y / g.r1n;
    let gcoef = g.a * libm::sqrt(y / mu);
    let gdot = 1.0 - y / g.r2n;
    if !(gcoef.is_finite() && gcoef != 0.0) {
        return Err(SimError::Recoverable("lambert: degenerate g coefficient"));
    }
    let v1 = (g.r2 - g.r1 * f) / gcoef;
    let v2 = (g.r2 * gdot - g.r1) / gcoef;
    if !(v1.is_finite() && v2.is_finite()) {
        return Err(SimError::Recoverable("lambert: non-finite velocities"));
    }
    Ok(LambertSolution { v1, v2, revs })
}

/// The classical Lambert solve: the conic connecting `r1` to `r2`
/// (parent-centered inertial, m) in exactly `tof_s` seconds about a
/// primary of gravitational parameter `mu`, along the [`LambertWay`]
/// arc. Zero-rev only — the porkchop domain; [`lambert_multi`] covers
/// the multi-rev branches.
///
/// `Ok(None)` = no zero-rev solution for this `tof` (see module docs:
/// the zero-rev branch covers all `tof > 0`, so `None` means the solve
/// failed validation — the residual gate or the lands check dropped
/// the root, reported honestly rather than guessed).
/// `Err(Recoverable)` = ill-posed input (collinear endpoints,
/// non-positive `tof`/`mu`, non-finite).
pub fn lambert(
    r1: DVec3,
    r2: DVec3,
    tof_s: f64,
    mu: f64,
    way: LambertWay,
) -> Result<Option<LambertSolution>, SimError> {
    if !(mu.is_finite() && mu > 0.0) {
        return Err(SimError::Recoverable("lambert: non-positive mu"));
    }
    if !(tof_s.is_finite() && tof_s > 0.0) {
        return Err(SimError::Recoverable("lambert: non-positive tof"));
    }
    let g = lambert_geom(r1, r2, way)?;
    let Some(z) = zero_rev_z(&g, mu, tof_s) else {
        return Ok(None);
    };
    let sol = lambert_velocities(&g, mu, z, 0)?;
    // The lands check: the residual gate alone can accept a corrupted
    // Long-way hyperbolic root — drop it rather than return it.
    if !lands_on(&g, mu, tof_s, sol.v1) {
        return Ok(None);
    }
    Ok(Some(sol))
}

/// The multi-rev Lambert solve for branch `revs ≥ 1`: up to two
/// solutions (the low- and high-energy legs) when the branch's TOF
/// minimum lies below `tof_s`; an empty `Vec` when it does not.
/// `revs = 0` delegates to [`lambert`] and wraps the `Option` for a
/// uniform call shape. Every candidate root passes the residual gate
/// AND the lands check (propagation to `r2`) — a root that fails
/// either is dropped, never returned as a wrong answer.
pub fn lambert_multi(
    r1: DVec3,
    r2: DVec3,
    tof_s: f64,
    mu: f64,
    way: LambertWay,
    revs: u32,
) -> Result<Vec<LambertSolution>, SimError> {
    if revs == 0 {
        return lambert(r1, r2, tof_s, mu, way).map(|s| s.into_iter().collect());
    }
    if !(mu.is_finite() && mu > 0.0) {
        return Err(SimError::Recoverable("lambert: non-positive mu"));
    }
    if !(tof_s.is_finite() && tof_s > 0.0) {
        return Err(SimError::Recoverable("lambert: non-positive tof"));
    }
    let g = lambert_geom(r1, r2, way)?;
    let mut out = Vec::new();
    for z in multi_rev_z(&g, mu, tof_s, revs) {
        if let Ok(sol) = lambert_velocities(&g, mu, z, revs)
            && lands_on(&g, mu, tof_s, sol.v1)
        {
            out.push(sol);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Porkchop scan
// ---------------------------------------------------------------------------

/// One body's parking-orbit convention for the cost model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EndCost {
    /// The raw hyperbolic excess speed — a flyby metric, or a vessel
    /// already on the asymptotic path.
    AtInfinity,
    /// COPLANAR LOWER BOUND: the single-impulse Oberth burn from a
    /// circular parking orbit at this radius (m), priced as if the
    /// ejection were coplanar with the parking plane:
    /// `√(v∞² + 2μ/r) − √(μ/r)`. When `v∞` carries a component normal
    /// to the parking plane, the real ejection (cone ∩ parking-plane —
    /// see [`departure_node`]) costs MORE; this objective intentionally
    /// prices the lower bound and leaves the plane-change component
    /// unpriced. The bound and the actual burn relate as
    /// `|dv_world| ≥ dv_depart_mps`, the gap being the plane-change
    /// component.
    CircularPark(f64),
}

/// A (departure-date, transfer-duration) porkchop query — the bounded
/// grid [`porkchop_scan`] evaluates.
#[derive(Clone, Debug, PartialEq)]
pub struct PorkchopQuery {
    /// The body the transfer departs (must carry elements and share a
    /// parent with `destination`).
    pub origin: BodyId,
    /// The body it targets.
    pub destination: BodyId,
    /// Departure-tick window, inclusive: `[depart_lo, depart_hi]`.
    pub depart_lo: u64,
    /// See [`Self::depart_lo`].
    pub depart_hi: u64,
    /// Transfer-duration window in ticks, inclusive.
    pub duration_lo: u64,
    /// See [`Self::duration_lo`].
    pub duration_hi: u64,
    /// Sample count on the departure axis (≥ 2 when the span is
    /// nonzero; the product with `duration_cells` is capped by
    /// [`PORKCHOP_MAX_CELLS`]).
    pub depart_cells: u32,
    /// Sample count on the duration axis.
    pub duration_cells: u32,
    /// Departure-side cost model ([`EndCost`]).
    pub depart_cost: EndCost,
    /// Arrival-side cost model.
    pub arrive_cost: EndCost,
    /// Highest multi-rev branch to evaluate per cell — `0` = zero-rev
    /// only (the fast path; interplanetary porkchops almost never want
    /// a multi-rev leg).
    pub max_revs: u32,
}

/// The best leg found at one grid cell.
#[derive(Clone, Debug, PartialEq)]
pub struct TransferLeg {
    /// Which arc won.
    pub way: LambertWay,
    /// Multi-rev branch of the winning solution (0 = zero-rev).
    pub revs: u32,
    /// The Lambert endpoint velocities of the winning leg, common-
    /// parent frame — kept so [`plan_transfer`] stamps the transfer
    /// conic without re-solving.
    pub v1: DVec3,
    /// See [`Self::v1`].
    pub v2: DVec3,
    /// Hyperbolic excess velocity at departure (`v1 − v_origin(t_dep)`,
    /// origin-relative direction in the common-parent frame).
    pub v_inf_depart: DVec3,
    /// Hyperbolic excess velocity at arrival (`v2 − v_dest(t_arr)`).
    pub v_inf_arrive: DVec3,
    /// Departure cost under the query's [`EndCost`] (m/s).
    pub dv_depart_mps: f64,
    /// Arrival cost (m/s).
    pub dv_arrive_mps: f64,
    /// `dv_depart + dv_arrive` — the cell's ranking number.
    pub total_dv_mps: f64,
}

/// One evaluated grid cell.
#[derive(Clone, Debug, PartialEq)]
pub struct PorkchopCell {
    /// The cell's departure tick.
    pub depart_tick: u64,
    /// The cell's transfer duration (ticks).
    pub duration_ticks: u64,
    /// The best feasible leg at this cell — `None` when neither way
    /// (nor any requested rev branch) solves. Infeasible cells count
    /// as `+∞` in the minima walk.
    pub leg: Option<TransferLeg>,
}

/// The scan's 2D surface plus its strict local minima.
#[derive(Clone, Debug, PartialEq)]
pub struct Porkchop {
    /// Axis resolutions, matching [`PorkchopQuery`].
    pub depart_cells: u32,
    /// See [`Self::depart_cells`].
    pub duration_cells: u32,
    /// Row-major `[depart][duration]` cells.
    pub cells: Vec<PorkchopCell>,
    /// Indices into `cells` of the strict local minima: finite-cost
    /// cells strictly below every existing 8-neighbor. Edge cells can
    /// be minima — a minimum against the window's own boundary reads
    /// "the valley continues past the query range", which is the
    /// honest thing to report.
    pub minima: Vec<usize>,
}

impl Porkchop {
    /// `(depart_idx, duration_idx)` → `cells` index.
    pub fn cell_index(&self, depart: u32, duration: u32) -> usize {
        depart as usize * self.duration_cells as usize + duration as usize
    }

    /// The cheapest feasible cell anywhere on the surface — the global
    /// min the refinement starts from.
    pub fn cheapest(&self) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, c) in self.cells.iter().enumerate() {
            if let Some(leg) = &c.leg
                && best.is_none_or(|(_, b)| leg.total_dv_mps < b)
            {
                best = Some((i, leg.total_dv_mps));
            }
        }
        best.map(|(i, _)| i)
    }
}

/// The transfer's shared frame: `origin` and `destination` must be
/// siblings under a common parent (the transfer conic's central body).
/// Returns `(parent, mu_parent, mu_origin, mu_destination)`.
fn shared_frame(
    model: &PatchedConics,
    q: &PorkchopQuery,
) -> Result<(BodyId, f64, f64, f64), SimError> {
    if q.origin == q.destination {
        return Err(SimError::Recoverable("porkchop: origin == destination"));
    }
    let o = model.body_orbit(&q.origin)?;
    let d = model.body_orbit(&q.destination)?;
    if o.elements.is_none() || d.elements.is_none() {
        return Err(SimError::Recoverable("porkchop: endpoint has no orbit"));
    }
    match (&o.parent, &d.parent) {
        (Some(p), Some(pd)) if p == pd => Ok((
            p.clone(),
            model.body_orbit(p)?.mu_m3_per_s2,
            o.mu_m3_per_s2,
            d.mu_m3_per_s2,
        )),
        _ => Err(SimError::Recoverable("porkchop: bodies share no parent")),
    }
}

/// One [`EndCost`]'s Δv at `v_inf`.
fn end_cost(cost: EndCost, v_inf: f64, mu: f64) -> f64 {
    match cost {
        EndCost::AtInfinity => v_inf,
        EndCost::CircularPark(r) => libm::sqrt(v_inf * v_inf + 2.0 * mu / r) - libm::sqrt(mu / r),
    }
}

/// Evaluate the cheapest Lambert leg at one `(depart_tick,
/// duration_ticks)` — both ways plus every requested rev branch, min
/// total Δv wins. `Ok(None)` when nothing solves (an infeasible cell,
/// not an error). Endpoint `state_at` failures propagate — a body the
/// model cannot evaluate is a broken input, not a bad cell.
fn leg_at(
    model: &PatchedConics,
    q: &PorkchopQuery,
    mu_p: f64,
    mu_o: f64,
    mu_d: f64,
    depart_tick: u64,
    duration_ticks: u64,
) -> Result<Option<TransferLeg>, SimError> {
    let t_dep = SimTime(depart_tick);
    let t_arr = SimTime(depart_tick.saturating_add(duration_ticks));
    let tof_s = duration_ticks as f64 / TICK_HZ as f64;
    if tof_s <= 0.0 {
        return Ok(None);
    }
    let (r1, vo) = model.body_state(&q.origin, t_dep)?;
    let (r2, vd) = model.body_state(&q.destination, t_arr)?;
    let mut best: Option<TransferLeg> = None;
    let mut consider = |way: LambertWay, sol: LambertSolution| {
        let v_inf_d = sol.v1 - vo;
        let v_inf_a = sol.v2 - vd;
        let dv_dep = end_cost(q.depart_cost, v_inf_d.norm(), mu_o);
        let dv_arr = end_cost(q.arrive_cost, v_inf_a.norm(), mu_d);
        let total = dv_dep + dv_arr;
        if total.is_finite() && best.as_ref().is_none_or(|b| total < b.total_dv_mps) {
            best = Some(TransferLeg {
                way,
                revs: sol.revs,
                v1: sol.v1,
                v2: sol.v2,
                v_inf_depart: v_inf_d,
                v_inf_arrive: v_inf_a,
                dv_depart_mps: dv_dep,
                dv_arrive_mps: dv_arr,
                total_dv_mps: total,
            });
        }
    };
    for way in [LambertWay::Short, LambertWay::Long] {
        if let Some(sol) = lambert(r1, r2, tof_s, mu_p, way)? {
            consider(way, sol);
        }
        for revs in 1..=q.max_revs {
            for sol in lambert_multi(r1, r2, tof_s, mu_p, way, revs)? {
                consider(way, sol);
            }
        }
    }
    Ok(best)
}

/// Even-spaced sample ticks over `[lo, hi]` — `n` points, integer
/// rounding on the tick lattice. `hi < lo` swaps to the empty range's
/// honest reading (callers validate before this runs).
fn axis_ticks(lo: u64, hi: u64, n: u32) -> Vec<u64> {
    if n <= 1 || hi <= lo {
        return vec![lo];
    }
    let span = hi - lo;
    (0..n)
        .map(|k| lo + ((span as u128 * k as u128 + (n as u128 - 1) / 2) / (n as u128 - 1)) as u64)
        .collect()
}

/// The (departure-date, transfer-duration) Δv surface: at every grid
/// cell, the cheapest feasible Lambert leg under the query's
/// [`EndCost`] model — plus the strict local minima, the "porkchop
/// valleys". Bounded work: the cell product is capped at
/// [`PORKCHOP_MAX_CELLS`] and each cell costs a bounded number of
/// Lambert solves (`2·(1 + max_revs)`).
///
/// `Err(Recoverable)`: malformed query (reversed ranges, a zero cell
/// count on a nonzero span, cells over the cap, a cell count finer
/// than the tick lattice, non-sibling or orbit-less endpoints,
/// non-positive parking radii, or a parking radius the model's own
/// body data rules out — under the surface or outside the SOI).
pub fn porkchop_scan(model: &PatchedConics, q: &PorkchopQuery) -> Result<Porkchop, SimError> {
    let (_parent, mu_p, mu_o, mu_d) = shared_frame(model, q)?;
    porkchop_scan_inner(model, q, mu_p, mu_o, mu_d)
}

/// The scan body behind [`porkchop_scan`], taking the already-resolved
/// shared frame so [`plan_transfer`] pays one frame resolution.
fn porkchop_scan_inner(
    model: &PatchedConics,
    q: &PorkchopQuery,
    mu_p: f64,
    mu_o: f64,
    mu_d: f64,
) -> Result<Porkchop, SimError> {
    if q.depart_hi < q.depart_lo || q.duration_hi < q.duration_lo {
        return Err(SimError::Recoverable("porkchop: reversed range"));
    }
    if q.depart_cells == 0
        || q.duration_cells == 0
        || (q.depart_cells < 2 && q.depart_hi > q.depart_lo)
        || (q.duration_cells < 2 && q.duration_hi > q.duration_lo)
    {
        return Err(SimError::Recoverable("porkchop: degenerate cell count"));
    }
    // A cell count finer than the tick lattice produces duplicate
    // sample ticks — identical-cost cells that the strict `<` minima
    // walk reads as a flat surface with no valley at all.
    let (d_span, t_span) = (q.depart_hi - q.depart_lo, q.duration_hi - q.duration_lo);
    if (q.depart_cells > 1 && d_span < u64::from(q.depart_cells - 1))
        || (q.duration_cells > 1 && t_span < u64::from(q.duration_cells - 1))
    {
        return Err(SimError::Recoverable("porkchop: cells exceed axis lattice"));
    }
    let cells = q.depart_cells as u64 * q.duration_cells as u64;
    if cells > PORKCHOP_MAX_CELLS {
        return Err(SimError::Recoverable("porkchop: grid over cell cap"));
    }
    // `CircularPark` radii are validated against the model's own body
    // data when carried: a parking orbit below the surface or outside
    // the SOI is a malformed cost model, not a cheap leg.
    for (cost, body, side) in [
        (q.depart_cost, &q.origin, "depart"),
        (q.arrive_cost, &q.destination, "arrive"),
    ] {
        let EndCost::CircularPark(r) = cost else {
            continue;
        };
        let (pos_msg, surf_msg, soi_msg) = match side {
            "depart" => (
                "porkchop: non-positive depart park radius",
                "porkchop: depart park radius under the surface",
                "porkchop: depart park radius outside the SOI",
            ),
            _ => (
                "porkchop: non-positive arrive park radius",
                "porkchop: arrive park radius under the surface",
                "porkchop: arrive park radius outside the SOI",
            ),
        };
        if !(r.is_finite() && r > 0.0) {
            return Err(SimError::Recoverable(pos_msg));
        }
        if model.body_radius(body).is_some_and(|rad| r <= rad) {
            return Err(SimError::Recoverable(surf_msg));
        }
        if model.body_orbit(body)?.soi_m.is_some_and(|soi| r >= soi) {
            return Err(SimError::Recoverable(soi_msg));
        }
    }
    let departs = axis_ticks(q.depart_lo, q.depart_hi, q.depart_cells);
    let durations = axis_ticks(q.duration_lo, q.duration_hi, q.duration_cells);
    let mut out = Vec::with_capacity(cells as usize);
    for &dt in &departs {
        for &dur in &durations {
            out.push(PorkchopCell {
                depart_tick: dt,
                duration_ticks: dur,
                leg: leg_at(model, q, mu_p, mu_o, mu_d, dt, dur)?,
            });
        }
    }
    let mut surface = Porkchop {
        depart_cells: q.depart_cells,
        duration_cells: q.duration_cells,
        cells: out,
        minima: Vec::new(),
    };
    let (nd, nt) = (q.depart_cells as usize, q.duration_cells as usize);
    for i in 0..nd {
        for j in 0..nt {
            let idx = surface.cell_index(i as u32, j as u32);
            let Some(cost) = surface.cells[idx].leg.as_ref().map(|l| l.total_dv_mps) else {
                continue;
            };
            let mut is_min = true;
            for di in -1i32..=1 {
                for dj in -1i32..=1 {
                    if di == 0 && dj == 0 {
                        continue;
                    }
                    let (ii, jj) = (i as i32 + di, j as i32 + dj);
                    if ii < 0 || jj < 0 || ii >= nd as i32 || jj >= nt as i32 {
                        continue;
                    }
                    let nidx = surface.cell_index(ii as u32, jj as u32);
                    if let Some(nc) = surface.cells[nidx].leg.as_ref()
                        && nc.total_dv_mps <= cost
                    {
                        is_min = false;
                    }
                }
            }
            if is_min {
                surface.minima.push(idx);
            }
        }
    }
    Ok(surface)
}

// ---------------------------------------------------------------------------
// Plan = scan + local refinement (§2.11 verbatim)
// ---------------------------------------------------------------------------

/// The refined transfer the planner produces: the cheapest
/// `(departure, duration)` leg the scan + local lattice refinement
/// found (a lattice optimum, not a proven global optimum), its Δv
/// split and v∞ vectors, and the transfer conic itself for
/// [`predict_encounter`].
#[derive(Clone, Debug, PartialEq)]
pub struct TransferPlan {
    /// Bodies carried through from the query.
    pub origin: BodyId,
    /// See [`Self::origin`].
    pub destination: BodyId,
    /// The departure tick the lattice search settled on.
    pub departure_tick: u64,
    /// `departure_tick + duration_ticks`.
    pub arrival_tick: u64,
    /// The transfer's duration in ticks.
    pub duration_ticks: u64,
    /// The winning leg's full detail (way, revs, velocities, Δv).
    pub leg: TransferLeg,
    /// The transfer conic in the common parent's frame, epoch =
    /// `departure_tick` — patched-conics' model of the cruise segment.
    /// Feed to [`predict_encounter`].
    pub conic: OrbitState,
}

/// Scan + local refinement (§2.11's recipe, verbatim): the cheapest
/// cell on the [`porkchop_scan`] surface seeds a discrete pattern
/// search on the tick lattice — evaluate the 8 neighbors at
/// `(±sd, ±st)`, move on improvement, halve the stride otherwise —
/// until the stride drops below 1 tick. Deterministic by construction:
/// fixed offset order, strict `<`, a hard evaluation budget
/// ([`REFINE_MAX_EVALS`]).
///
/// Why discrete refinement and not a continuous optimizer: the
/// deliverable is an integer-tick `(departure, duration)` — the
/// objective on the tick lattice is a step function of the underlying
/// smooth surface, and a lattice search finds the lattice optimum
/// exactly where a continuous method still needs a final snap.
///
/// `Err(Recoverable)`: the query's own failures, or a surface with no
/// feasible cell at all (the window contains no Lambert leg — e.g.
/// every duration below the transfer's physical minimum).
pub fn plan_transfer(model: &PatchedConics, q: &PorkchopQuery) -> Result<TransferPlan, SimError> {
    let (parent, mu_p, mu_o, mu_d) = shared_frame(model, q)?;
    let surface = porkchop_scan_inner(model, q, mu_p, mu_o, mu_d)?;
    let best_idx = surface
        .cheapest()
        .ok_or(SimError::Recoverable("plan: no feasible cell in window"))?;
    let mut pd = surface.cells[best_idx].depart_tick;
    let mut pt = surface.cells[best_idx].duration_ticks;
    let mut best_cost = surface.cells[best_idx]
        .leg
        .as_ref()
        .map(|l| l.total_dv_mps)
        .ok_or(SimError::Recoverable("plan: cheapest cell has no leg"))?;
    // Seed strides at half the cell spacing — the refinement's domain
    // is the query's own window, clamped.
    let cell_d = (q.depart_hi - q.depart_lo)
        .checked_div(q.depart_cells.max(2) as u64 - 1)
        .unwrap_or(0);
    let cell_t = (q.duration_hi - q.duration_lo)
        .checked_div(q.duration_cells.max(2) as u64 - 1)
        .unwrap_or(0);
    let mut sd = (cell_d / 2).max(1);
    let mut st = (cell_t / 2).max(1);
    let mut evals = 0u32;
    while (sd >= 1 || st >= 1) && evals < REFINE_MAX_EVALS {
        let mut improved = false;
        for di in -1i64..=1 {
            // A zero stride makes di≠0 candidates identical to di=0 —
            // skip them rather than paying 3× for the same cell.
            if di != 0 && sd == 0 {
                continue;
            }
            for dj in -1i64..=1 {
                if dj != 0 && st == 0 {
                    continue;
                }
                if di == 0 && dj == 0 {
                    continue;
                }
                // i128 offsets: `stride·±1` can exceed i64 on a huge
                // window — clamp in i128 before narrowing to the
                // lattice, never wrap.
                let cand_d = (i128::from(pd) + i128::from(di) * i128::from(sd))
                    .clamp(i128::from(q.depart_lo), i128::from(q.depart_hi))
                    as u64;
                let cand_t = (i128::from(pt) + i128::from(dj) * i128::from(st))
                    .clamp(i128::from(q.duration_lo), i128::from(q.duration_hi))
                    as u64;
                if (cand_d, cand_t) == (pd, pt) {
                    continue;
                }
                evals += 1;
                if evals > REFINE_MAX_EVALS {
                    break;
                }
                if let Some(leg) = leg_at(model, q, mu_p, mu_o, mu_d, cand_d, cand_t)?
                    && leg.total_dv_mps < best_cost
                {
                    best_cost = leg.total_dv_mps;
                    pd = cand_d;
                    pt = cand_t;
                    improved = true;
                }
            }
        }
        if !improved {
            sd /= 2;
            st /= 2;
        }
    }
    let leg = leg_at(model, q, mu_p, mu_o, mu_d, pd, pt)?
        .ok_or(SimError::Recoverable("plan: refined point is infeasible"))?;
    let (r1, _) = model.body_state(&q.origin, SimTime(pd))?;
    let conic = OrbitState {
        parent,
        elements: cartesian_to_elements(r1, leg.v1, mu_p)?,
        epoch: SimTime(pd),
    };
    Ok(TransferPlan {
        origin: q.origin.clone(),
        destination: q.destination.clone(),
        departure_tick: pd,
        arrival_tick: pd.saturating_add(pt),
        duration_ticks: pt,
        leg,
        conic,
    })
}

// ---------------------------------------------------------------------------
// Encounter prediction — the §2.6.6 scan over a hypothetical conic
// ---------------------------------------------------------------------------

/// The approach geometry of a predicted SOI `Enter`: the conic as it
/// appears in the encountered body's frame.
#[derive(Clone, Debug, PartialEq)]
pub struct EncounterApproach {
    /// The approach conic's elements about the encountered body,
    /// epoch = the crossing tick — hyperbolic for any genuine SOI
    /// entry.
    pub elements: KeplerElements,
    /// Periapsis distance (m) on the approach conic.
    pub periapsis_m: f64,
    /// Hyperbolic excess speed `√(μ/|a|)` (m/s) — `None` for a closed
    /// approach (a capture-looking entry on a bound conic; the model
    /// still reports it honestly).
    pub v_inf_mps: Option<f64>,
    /// `periapsis < body radius` — `None` when the model carries no
    /// radius for the body (only `PatchedConics::from_body_defs` and
    /// `set_body_radius` models do; the flag stays `None` rather than
    /// guessing).
    pub impacts: Option<bool>,
}

/// What a hypothetical conic commits to: the earliest §2.6.6
/// transition the scan finds in `(now, horizon]`, the approach
/// geometry for an `Enter`, and the post-transition conic for
/// chaining.
#[derive(Clone, Debug, PartialEq)]
pub struct EncounterPrediction {
    /// The crossing the §2.6.6 scan found — body, direction, tick
    /// (within 1 tick of the refined crossing, same contract as a
    /// scheduled `SoiEvent`).
    pub event: SoiEvent,
    /// The conic re-based into the new parent's frame at the event
    /// tick — `Some` whenever [`PatchedConics::rebase`] can produce it
    /// (an `Exit` past a parentless body has no frame and is `None`).
    /// Feed it back into [`predict_encounter`] at `now = event.tick`
    /// to chain multi-encounter predictions.
    pub rebased: Option<OrbitState>,
    /// For `Enter` events: the approach geometry in the encountered
    /// body's frame. `None` for `Exit` (leaving a parent's SOI has no
    /// approach — the new frame is just a new conic).
    pub approach: Option<EncounterApproach>,
}

/// Predict the first SOI encounter of a conic that was never spawned —
/// a pure query over [`PatchedConics::next_soi_event`]'s machinery
/// applied to a hypothetical [`OrbitState`], exactly the scan the
/// sim itself would run if the conic were live: same candidate set,
/// same stride `h`, same Brent refinement, same "a residence shorter
/// than `h` may be missed" guarantee.
///
/// For an `Enter` the approach conic is the rebased state in the
/// child's frame — the same [`PatchedConics::rebase`] stitch the
/// world's fired event performs, so the prediction cannot drift from
/// the transition it predicts. `rebased` is provided on every
/// direction for multi-encounter chaining (grazes, moon-assist
/// chains).
///
/// `Err` propagates the model's own failures (unsolvable elements,
/// scan budget exhaustion — the caller clamps the horizon and
/// retries, exactly as the live scan prescribes).
pub fn predict_encounter(
    model: &PatchedConics,
    conic: &OrbitState,
    now: SimTime,
    horizon: SimTime,
) -> Result<Option<EncounterPrediction>, SimError> {
    predict_encounter_excluding(model, conic, now, horizon, None)
}

/// [`predict_encounter`] with one transfer-plan composition rule:
/// `exclude` names a body whose SOI boundary is skipped **while the
/// conic starts inside it at `now`**. [`plan_transfer`] stamps
/// `plan.conic` at the origin body's center — inside the origin's own
/// SOI — so an unfiltered scan reports the egress (`Exit{origin}`),
/// which was never a residence in the origin's frame and has no
/// approach geometry. Pass `plan.origin` to read through to the real
/// encounter — e.g. `Enter{destination}`.
pub fn predict_encounter_excluding(
    model: &PatchedConics,
    conic: &OrbitState,
    now: SimTime,
    horizon: SimTime,
    exclude: Option<&BodyId>,
) -> Result<Option<EncounterPrediction>, SimError> {
    let Some(event) = model
        .next_soi_event_within(
            conic,
            now,
            horizon,
            crate::orbit::SOI_SCAN_MAX_SAMPLES,
            None,
            exclude,
        )?
        .0
    else {
        return Ok(None);
    };
    let t = SimTime(event.tick);
    // The only rebased=None case is an Exit past a parentless body —
    // it has no frame to re-base into. Every other failure (including
    // Fatal) propagates rather than degrading the prediction silently.
    let rebased = match model.rebase(conic, &event.body, event.direction, t) {
        Ok(s) => Some(s),
        Err(e) => {
            let parentless_exit = event.direction == SoiDirection::Exit
                && model
                    .body_orbit(&event.body)
                    .is_ok_and(|b| b.parent.is_none());
            if parentless_exit {
                None
            } else {
                return Err(e);
            }
        }
    };
    let approach = match (event.direction, &rebased) {
        (SoiDirection::Enter, Some(new_orbit)) => Some(encounter_approach(
            &new_orbit.elements,
            model.body_orbit(&event.body)?.mu_m3_per_s2,
            model.body_radius(&event.body),
        )),
        _ => None,
    };
    Ok(Some(EncounterPrediction {
        event,
        rebased,
        approach,
    }))
}

/// The approach geometry of one rebased conic — extracted so the
/// parabolic-encoding branches are unit-testable. `e ∈ (1, 1 + ε]` is
/// the `a := −p` parabolic encoding (see [`E_PARABOLIC_TOL`]): its
/// periapsis reads `p/2` and its true `v∞` is ≈ 0 — reporting
/// `√(μ/p)` there would be a guard-inversion bug.
fn encounter_approach(el: &KeplerElements, mu_b: f64, radius: Option<f64>) -> EncounterApproach {
    let a = el.semi_major_axis_m;
    let e = el.eccentricity;
    let periapsis_m = if (e - 1.0).abs() <= E_PARABOLIC_TOL {
        a.abs() / 2.0 // parabolic encoding: a = −p, r_p = p/2
    } else {
        a * (1.0 - e) // a(1−e) covers elliptic and hyperbolic
    };
    // Same difference form as the parabolic classification — comparing
    // `e > 1 + ε` directly disagrees with `(e−1) ≤ ε` at the boundary
    // because `1 + ε` itself rounds.
    let v_inf_mps = ((e - 1.0) > E_PARABOLIC_TOL && a < 0.0).then(|| libm::sqrt(mu_b / a.abs()));
    EncounterApproach {
        elements: el.clone(),
        periapsis_m,
        v_inf_mps,
        impacts: radius.map(|r| periapsis_m < r),
    }
}

// ---------------------------------------------------------------------------
// Departure node — plan → SetManeuver
// ---------------------------------------------------------------------------

/// The `SetManeuver` arguments the plan implies for one vessel: the
/// burn tick and the node's Δv in the burn point's §2.8 velocity
/// frame, ready to hand to `SimCommand::SetManeuver` verbatim.
#[derive(Clone, Debug, PartialEq)]
pub struct DepartureNode {
    /// The tick the burn executes — the vessel's last passage through
    /// the ejection longitude at-or-before the plan's departure tick
    /// (the "node at window" convention; see [`departure_node`]).
    pub tick: u64,
    /// Δv components — `SimCommand::SetManeuver`'s fields verbatim.
    pub dv_prograde_mps: f64,
    /// See [`Self::dv_prograde_mps`].
    pub dv_normal_mps: f64,
    /// See [`Self::dv_prograde_mps`].
    pub dv_radial_mps: f64,
    /// The inertial Δv vector in the origin's frame — diagnostics for
    /// the UI (the components above are the same vector resolved at
    /// the burn point).
    pub dv_world: DVec3,
}

/// Resolve a [`TransferPlan`] into a [`DepartureNode`] for a vessel on
/// a **closed** (`e < 1`) orbit about `plan.origin` — the
/// single-impulse ejection, done honestly:
///
/// 1. The escape hyperbola must end with outgoing asymptote `v∞̂`
///    (`plan.leg.v_inf_depart`, in the origin's frame). At burn speed
///    `v_esc = √(v∞² + 2μ_o/r_b)`, the hyperbola's eccentricity is
///    `e_h = 1 + r_b·v∞²/μ_o`, so its outgoing asymptote sits at true
///    anomaly `ν∞ = acos(−1/e_h)` from the burn-point radius `p̂`.
/// 2. The burn point must lie on the parking orbit → `p̂ ⊥ n̂_park`.
///    The cone `{p̂ : p̂·v∞̂ = cos ν∞}` ∩ the parking plane gives up to
///    two candidates; the prograde-consistent one (the burn direction
///    keeping the vessel's orbital sense) wins. `r_b ↔ ν∞` is a
///    fixed point (the parking orbit's radius at the burn longitude
///    feeds `e_h`), iterated to convergence, cap
///    [`EJECTION_MAX_ITERS`].
/// 3. The node ticks at the vessel's **last passage through the burn
///    longitude at-or-before `departure_tick`** — the "node at window"
///    convention: patched-conics treats SOI egress as instantaneous,
///    so the burn rides at the window, slightly ahead of the conic's
///    nominal start, exactly like the classic maneuver planner.
///
/// **Limitation — this node does NOT compose to arrival on its own.**
/// The burn can fire up to one parking period before `departure_tick`
/// and the egress transit through the origin's SOI is modeled as
/// instantaneous, so the vessel's real post-egress state is the
/// transfer conic shifted in time: it misses `r2` at `arrival_tick`
/// without a mid-course correction. The correction loop is REQUIRED,
/// not optional — [`predict_encounter`] the resulting trajectory,
/// measure the missed approach, and re-plan around the remainder
/// until the encounter closes.
///
/// `Recoverable` when the geometry can't produce the asymptote: an
/// open parking orbit, a `v∞` pointing nearly along the parking
/// normal (the cone never reaches the plane — the honest answer is a
/// split plane change, which is the AI job's to decompose), a burn
/// longitude the iteration fails to converge, or a burn tick that
/// lands at-or-before epoch 0.
pub fn departure_node(
    model: &PatchedConics,
    vessel_orbit: &OrbitState,
    plan: &TransferPlan,
) -> Result<DepartureNode, SimError> {
    if vessel_orbit.parent != plan.origin {
        return Err(SimError::Recoverable(
            "ejection: vessel does not orbit the plan's origin",
        ));
    }
    let el = &vessel_orbit.elements;
    let e_v = el.eccentricity;
    if !(0.0..1.0).contains(&e_v) {
        return Err(SimError::Recoverable("ejection: parking orbit not closed"));
    }
    let v_inf = plan.leg.v_inf_depart;
    let vinf = v_inf.norm();
    if !(vinf.is_finite() && vinf > 0.0) {
        return Err(SimError::Recoverable("ejection: zero departure v_inf"));
    }
    let mu_o = model.body_orbit(&plan.origin)?.mu_m3_per_s2;
    let v_hat = v_inf / vinf;
    let t_dep = SimTime(plan.departure_tick);
    // The parking orbit's inertial basis: periapsis direction ê,
    // orbit normal ŵ, and the in-plane quadrature q̂ = ŵ×ê.
    let e_hat = perifocal_to_inertial(
        DVec3::X,
        el.ascending_node_rad,
        el.inclination_rad,
        el.arg_periapsis_rad,
    );
    let w_hat = perifocal_to_inertial(
        DVec3::Z,
        el.ascending_node_rad,
        el.inclination_rad,
        el.arg_periapsis_rad,
    );
    if !w_hat.is_finite() {
        return Err(SimError::Recoverable("ejection: degenerate parking plane"));
    }
    let q_hat = w_hat.cross(e_hat);
    let a_v = el.semi_major_axis_m;
    if !(a_v.is_finite() && a_v > 0.0) {
        return Err(SimError::Recoverable("ejection: degenerate parking orbit"));
    }
    // The ejection point: cone ∩ parking-plane, iterated on the burn
    // radius — extracted so the arg-count and fn-length lints hold.
    let (p_hat, u_hat, v_esc) = ejection_point(el, v_hat, vinf, mu_o, w_hat, e_hat, q_hat, a_v)?;
    let v_post = u_hat * v_esc;
    // Burn tick: the last burn-longitude passage at-or-before
    // departure. ν_b from the converged p̂; the vessel's ν at t_dep;
    // ΔM between them maps to time via mean motion.
    let nu_b = libm::atan2(p_hat.dot(q_hat), p_hat.dot(e_hat));
    let (rv, _vv) = model.state_at(vessel_orbit, t_dep)?;
    let nu_now = libm::atan2(rv.dot(q_hat), rv.dot(e_hat));
    let n_v = libm::sqrt(mu_o / (a_v * a_v * a_v));
    let period_s = TAU / n_v;
    let m_now = mean_anomaly_of_nu(e_v, nu_now);
    let m_burn = mean_anomaly_of_nu(e_v, nu_b);
    let dm = (m_burn - m_now).rem_euclid(TAU);
    let dep_s = t_dep.seconds();
    // `dm == 0` ⇒ the vessel sits at the burn longitude at t_dep →
    // burn at the window itself.
    let burn_s = if dm <= 1e-9 {
        dep_s
    } else {
        dep_s + dm / n_v - period_s
    };
    if !(burn_s.is_finite() && burn_s >= 0.0) {
        return Err(SimError::Recoverable("ejection: burn precedes epoch"));
    }
    let burn_tick = (burn_s * TICK_HZ as f64).round() as u64;
    let (r_bv, v_bv) = model.state_at(vessel_orbit, SimTime(burn_tick))?;
    let dv_world = v_post - v_bv;
    let (pro, nrm, rad) = burn_frame(r_bv, v_bv)?;
    Ok(DepartureNode {
        tick: burn_tick,
        dv_prograde_mps: dv_world.dot(pro),
        dv_normal_mps: dv_world.dot(nrm),
        dv_radial_mps: dv_world.dot(rad),
        dv_world,
    })
}

/// The ejection geometry solve, extracted from [`departure_node`]:
/// returns `(p̂, û, v_esc)` — the burn-point radius direction in the
/// parking plane, the periapsis-velocity direction, and the escape
/// speed at the converged burn radius. The cone
/// `{p̂ : p̂·v∞̂ = cos ν∞}` ∩ parking plane produces up to two
/// candidates; the prograde-consistent one (the burn keeping the
/// vessel's orbital sense) wins. `r_b ↔ ν∞` is a fixed point — the
/// parking orbit's radius at the burn longitude feeds `e_h` — iterated
/// to convergence, cap [`EJECTION_MAX_ITERS`].
#[allow(clippy::too_many_arguments)]
fn ejection_point(
    el: &KeplerElements,
    v_hat: DVec3,
    vinf: f64,
    mu_o: f64,
    w_hat: DVec3,
    e_hat: DVec3,
    q_hat: DVec3,
    a_v: f64,
) -> Result<(DVec3, DVec3, f64), SimError> {
    // In-plane component of v∞̂: the cone ∩ parking-plane solve needs
    // |cos ν∞| ≤ s where s = |v∞̂ − (v∞̂·n̂)n̂|.
    let v_proj = v_hat - w_hat * v_hat.dot(w_hat);
    let s = v_proj.norm();
    if !(s.is_finite() && s > 1e-9) {
        return Err(SimError::Recoverable(
            "ejection: v_inf normal to the parking plane",
        ));
    }
    let e1 = v_proj / s;
    let e2 = w_hat.cross(e1);
    let e_v = el.eccentricity;
    let p_latus = a_v * (1.0 - e_v * e_v);
    let mut r_b = a_v;
    let mut p_hat = DVec3::ZERO;
    let mut converged = false;
    for _ in 0..EJECTION_MAX_ITERS {
        let e_h = 1.0 + r_b * vinf * vinf / mu_o;
        let nu_inf = libm::acos((-1.0 / e_h).clamp(-1.0, 1.0));
        let cos_psi = libm::cos(nu_inf) / s;
        if cos_psi.abs() > 1.0 {
            return Err(SimError::Recoverable(
                "ejection: parking plane cannot reach the asymptote",
            ));
        }
        let psi = libm::acos(cos_psi.clamp(-1.0, 1.0));
        // Two cone∩plane candidates; keep the one whose periapsis
        // velocity direction preserves the vessel's orbital sense
        // (the prograde-consistent burn — the other ejects backward
        // through the parking orbit's motion).
        let mut best: Option<(f64, DVec3)> = None;
        for ang in [psi, -psi] {
            let ph = e1 * libm::cos(ang) + e2 * libm::sin(ang);
            let uh = v_hat - ph * v_hat.dot(ph);
            if !(uh.norm().is_finite() && uh.norm() > 1e-9) {
                continue; // p̂ ∥ v∞̂ — not an asymptote geometry
            }
            let u_dir = uh / uh.norm();
            let score = u_dir.dot(w_hat.cross(ph));
            if best.is_none_or(|(sc, _)| score > sc) {
                best = Some((score, ph));
            }
        }
        let Some((_, ph)) = best else {
            return Err(SimError::Recoverable("ejection: no burn candidate"));
        };
        p_hat = ph;
        let nu_b = libm::atan2(p_hat.dot(q_hat), p_hat.dot(e_hat));
        let r_new = p_latus / (1.0 + e_v * libm::cos(nu_b));
        if !(r_new.is_finite() && r_new > 0.0) {
            return Err(SimError::Recoverable("ejection: degenerate burn radius"));
        }
        if (r_new - r_b).abs() <= 1e-10 * r_b {
            r_b = r_new;
            converged = true;
            break;
        }
        r_b = r_new;
    }
    if !converged {
        return Err(SimError::Recoverable(
            "ejection: burn radius did not converge",
        ));
    }
    let v_esc = libm::sqrt(vinf * vinf + 2.0 * mu_o / r_b);
    let u_hat_raw = v_hat - p_hat * v_hat.dot(p_hat);
    if !(u_hat_raw.norm().is_finite() && u_hat_raw.norm() > 1e-9) {
        return Err(SimError::Recoverable("ejection: degenerate burn direction"));
    }
    let u_hat = u_hat_raw / u_hat_raw.norm();
    Ok((p_hat, u_hat, v_esc))
}

/// `M(ν)` for a closed orbit: `E = atan2(√(1−e²)·sinν, e + cosν)`,
/// `M = E − e·sinE` — the elliptic half of `cartesian_to_elements`'s
/// anomaly math, needed by the burn-longitude → burn-tick solve.
fn mean_anomaly_of_nu(e: f64, nu: f64) -> f64 {
    let big_e = libm::atan2(libm::sqrt(1.0 - e * e) * libm::sin(nu), e + libm::cos(nu));
    big_e - e * libm::sin(big_e)
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::orbit::{BodyOrbit, propagate};
    use std::collections::BTreeMap;

    const MU_SOLEN: f64 = 1.1723e18;
    const MU_TERRIN: f64 = 3.5316e12;
    const MU_DUVA: f64 = 3.0137e11;
    const TERRIN_SOI: f64 = 8.4e7;
    const DUVA_SOI: f64 = 4.79e7;
    const TERRIN_A: f64 = 1.36e10;
    const DUVA_A: f64 = 2.07e10;
    const TERRIN_R: f64 = 6.0e5;
    const DUVA_R: f64 = 3.2e5;

    fn vec_rel_err(got: DVec3, want: DVec3) -> f64 {
        (got - want).norm() / want.norm()
    }

    fn circular_elements(a: f64, m0: f64) -> KeplerElements {
        KeplerElements {
            semi_major_axis_m: a,
            eccentricity: 0.0,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: m0,
            period_s: f64::INFINITY,
        }
    }

    /// solen → terrin (circular, e=0) → duva (e=0.051, i≈0.001) — the
    /// real §3.0 home system's interplanetary leg, radii included so
    /// the encounter impact flag is exercised.
    fn home_system() -> PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("solen"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("terrin"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: Some(BodyId::from("solen")),
                elements: Some(circular_elements(TERRIN_A, 0.0)),
            },
        );
        bodies.insert(
            BodyId::from("duva"),
            BodyOrbit {
                mu_m3_per_s2: MU_DUVA,
                soi_m: Some(DUVA_SOI),
                parent: Some(BodyId::from("solen")),
                elements: Some(KeplerElements {
                    semi_major_axis_m: DUVA_A,
                    eccentricity: 0.051,
                    inclination_rad: 0.0010471975511965976,
                    ascending_node_rad: 0.0,
                    arg_periapsis_rad: 0.0,
                    mean_anomaly_epoch_rad: 0.0,
                    period_s: f64::INFINITY,
                }),
            },
        );
        let mut m = PatchedConics::new(bodies);
        m.set_body_radius(&BodyId::from("terrin"), TERRIN_R);
        m.set_body_radius(&BodyId::from("duva"), DUVA_R);
        m
    }

    // -- Lambert -------------------------------------------------------

    /// The propagation check — the solver's own strongest assertion:
    /// `propagate(r1, v1, tof)` must land on `r2` (and produce `v2`).
    /// This is definition-level: the solved conic IS the answer.
    fn check_lands(r1: DVec3, r2: DVec3, tof: f64, mu: f64, sol: &LambertSolution, tag: &str) {
        let (rp, vp) = propagate(r1, sol.v1, mu, tof).unwrap();
        let er = vec_rel_err(rp, r2);
        let ev = vec_rel_err(vp, sol.v2);
        assert!(
            er < 1e-8 && ev < 1e-8,
            "{tag}: propagation does not land on r2 (r_err={er:e} v_err={ev:e})"
        );
    }

    /// Curtis "Orbital Mechanics" Example 5.2 — the standard published
    /// Lambert case (Earth μ = 398600 km³/s², SI below):
    /// r1 = (5000,10000,2100) km, r2 = (−14600,2500,7000) km, Δt=3600 s,
    /// prograde. Published answer:
    /// v1 = (−5.9925, 1.9254, 3.2457) km/s,
    /// v2 = (−3.3125, −4.1966, −0.3853) km/s.
    #[test]
    fn lambert_curtis_example_5_2() {
        let mu = 3.986e14; // m³/s²
        let r1 = DVec3::new(5.0e6, 1.0e7, 2.1e6);
        let r2 = DVec3::new(-1.46e7, 2.5e6, 7.0e6);
        let sol = lambert(r1, r2, 3600.0, mu, LambertWay::Short)
            .unwrap()
            .unwrap();
        assert!(vec_rel_err(sol.v1, DVec3::new(-5992.5, 1925.4, 3245.7)) < 1e-3);
        assert!(vec_rel_err(sol.v2, DVec3::new(-3312.5, -4196.6, -385.3)) < 1e-3);
        check_lands(r1, r2, 3600.0, mu, &sol, "curtis");
    }

    /// Same geometry, long way — a different (valid) conic that must
    /// still satisfy the boundary problem.
    #[test]
    fn lambert_long_way_also_lands() {
        let mu = 3.986e14;
        let r1 = DVec3::new(5.0e6, 1.0e7, 2.1e6);
        let r2 = DVec3::new(-1.46e7, 2.5e6, 7.0e6);
        let sol = lambert(r1, r2, 3600.0, mu, LambertWay::Long)
            .unwrap()
            .unwrap();
        check_lands(r1, r2, 3600.0, mu, &sol, "long-way");
        // The two ways are distinct conics.
        let short = lambert(r1, r2, 3600.0, mu, LambertWay::Short)
            .unwrap()
            .unwrap();
        assert!(vec_rel_err(sol.v1, short.v1) > 0.01);
    }

    /// Deep-hyperbolic Long way — the catastrophic-cancellation regime:
    /// at very short `tof` the two terms of `√μT = x³S + A√y` are huge
    /// and nearly equal (A < 0 for `Long`), so the residual formula is
    /// numerically fragile. The solver must still produce a root whose
    /// conic propagates to `r2` — the lands check is what makes that
    /// guarantee trustworthy here, not the residual gate alone.
    #[test]
    fn lambert_deep_hyperbolic_long_way_lands() {
        let mu = 3.986e14;
        let r1 = DVec3::new(5.0e6, 1.0e7, 2.1e6);
        let r2 = DVec3::new(-1.46e7, 2.5e6, 7.0e6);
        // ~20× under the elliptic solve's 3600 s — firmly hyperbolic.
        let tof = 180.0;
        let sol = lambert(r1, r2, tof, mu, LambertWay::Long)
            .unwrap()
            .expect("a deep-hyperbolic long-way root must exist");
        check_lands(r1, r2, tof, mu, &sol, "deep-hyp-long");
        // Sanity: the transfer is genuinely hyperbolic at departure —
        // |v1| exceeds escape speed at r1.
        let v_esc = libm::sqrt(2.0 * mu / r1.norm());
        assert!(
            sol.v1.norm() > v_esc,
            "expected hyperbolic departure: |v1|={} v_esc={v_esc}",
            sol.v1.norm()
        );
    }

    /// A Hohmann-ish case with a checkable answer: circular orbits at
    /// r1 = 1.36e10 m and r2 = 2.07e10 m around solen — the real
    /// Terrin→Duva legs. The minimum-energy transfer (near-Hohmann,
    /// 180° is degenerate so the endpoints sit at ~170°) must carry a
    /// semi-major axis between the Hohmann value `a_t = (r1+r2)/2` for
    /// a 180° arc and infinity — and land on r2.
    #[test]
    fn lambert_hohmann_like_170deg() {
        let r1 = DVec3::new(TERRIN_A, 0.0, 0.0);
        let ang = 170.0_f64.to_radians();
        let r2 = DVec3::new(DUVA_A * libm::cos(ang), DUVA_A * libm::sin(ang), 0.0);
        // Hohmann TOF for the 180° case — a sane duration for the
        // near-180° arc.
        let a_t = 0.5 * (TERRIN_A + DUVA_A);
        let tof = (TAU / 2.0) * libm::sqrt(a_t * a_t * a_t / MU_SOLEN);
        let sol = lambert(r1, r2, tof, MU_SOLEN, LambertWay::Short)
            .unwrap()
            .unwrap();
        check_lands(r1, r2, tof, MU_SOLEN, &sol, "hohmann-170");
        // The departure velocity should exceed circular at r1 (it's an
        // escape toward the outer orbit) but by less than ~50% — a
        // Hohmann-family transfer, not a radial blast.
        let v_circ = libm::sqrt(MU_SOLEN / TERRIN_A);
        assert!(sol.v1.norm() > v_circ);
        assert!(sol.v1.norm() < 1.5 * v_circ);
    }

    /// Multi-rev: a long TOF over a short hop — the 1-rev branch must
    /// produce conics that close the loop and still land on r2. Both
    /// branch roots (low/high energy) are exercised when present.
    #[test]
    fn lambert_one_rev_lands() {
        let mu = MU_SOLEN;
        let r1 = DVec3::new(TERRIN_A, 0.0, 0.0);
        let r2 = DVec3::new(0.0, TERRIN_A, 0.0);
        // Generous tof: ~1.5 periods of a conic sized between the
        // endpoints — enough for a full extra revolution.
        let tof = 5.0e7;
        let sols = lambert_multi(r1, r2, tof, mu, LambertWay::Short, 1).unwrap();
        assert!(!sols.is_empty(), "1-rev branch should solve");
        for (i, sol) in sols.iter().enumerate() {
            assert_eq!(sol.revs, 1);
            check_lands(r1, r2, tof, mu, sol, &format!("rev1[{i}]"));
        }
    }

    /// Degenerate inputs refuse — collinear endpoints have no orbital
    /// plane; non-positive tof/mu are ill-posed.
    #[test]
    fn lambert_rejects_degenerate_inputs() {
        let mu = MU_SOLEN;
        let r1 = DVec3::new(1e10, 0.0, 0.0);
        let r2 = DVec3::new(2e10, 0.0, 0.0); // collinear
        assert!(lambert(r1, r2, 1e6, mu, LambertWay::Short).is_err());
        assert!(lambert(r1, -r1, 1e6, mu, LambertWay::Short).is_err());
        assert!(lambert(r1, DVec3::new(0.0, 2e10, 0.0), 0.0, mu, LambertWay::Short).is_err());
        assert!(lambert(r1, DVec3::new(0.0, 2e10, 0.0), 1e6, -mu, LambertWay::Short).is_err());
    }

    /// Bitwise determinism: same inputs, same bits (§2.7 T1 — the
    /// planner is inside the determinism domain).
    #[test]
    fn lambert_is_bitwise_deterministic() {
        let r1 = DVec3::new(5.0e6, 1.0e7, 2.1e6);
        let r2 = DVec3::new(-1.46e7, 2.5e6, 7.0e6);
        let a = lambert(r1, r2, 3600.0, 3.986e14, LambertWay::Short).unwrap();
        let b = lambert(r1, r2, 3600.0, 3.986e14, LambertWay::Short).unwrap();
        assert_eq!(a, b);
    }

    // -- Porkchop / plan_transfer --------------------------------------

    /// The Terrin→Duva window query: a ~1.5-synodic-period departure
    /// range and a duration range bracketing the Hohmann TOF.
    fn duva_query(cells: u32) -> PorkchopQuery {
        // Hohmann TOF for r1 = 1.36e10, r2 = 2.07e10 ≈ 1.28e7 s.
        let a_t = (TERRIN_A + DUVA_A) / 2.0;
        let t_h = (TAU / 2.0) * libm::sqrt(a_t * a_t * a_t / MU_SOLEN);
        let synodic = {
            let n1 = libm::sqrt(MU_SOLEN / (TERRIN_A * TERRIN_A * TERRIN_A));
            let n2 = libm::sqrt(MU_SOLEN / (DUVA_A * DUVA_A * DUVA_A));
            TAU / (n1 - n2)
        };
        PorkchopQuery {
            origin: BodyId::from("terrin"),
            destination: BodyId::from("duva"),
            depart_lo: 0,
            depart_hi: (synodic * 0.9 * TICK_HZ as f64) as u64,
            duration_lo: (t_h * 0.4 * TICK_HZ as f64) as u64,
            duration_hi: (t_h * 1.6 * TICK_HZ as f64) as u64,
            depart_cells: cells,
            duration_cells: cells,
            depart_cost: EndCost::CircularPark(TERRIN_R + 75e3), // LEO
            arrive_cost: EndCost::AtInfinity,
            max_revs: 0,
        }
    }

    /// The scan finds the porkchop valley: at least one strict local
    /// minimum exists, and the plan's refined optimum lands near the
    /// analytic Hohmann phasing — the departure where Duva sits
    /// `π − n_d·tof` ahead of Terrin (the lead-angle condition,
    /// evaluated at the actual eccentric orbit's mean longitude).
    #[test]
    fn porkchop_finds_the_window_valley() {
        let model = home_system();
        let q = duva_query(24);
        let surface = porkchop_scan(&model, &q).unwrap();
        assert!(!surface.minima.is_empty(), "no valley found");
        let plan = plan_transfer(&model, &q).unwrap();
        // The plan is feasible and beats every cell it left behind.
        let surface_best = surface.cells[surface.cheapest().unwrap()]
            .leg
            .as_ref()
            .unwrap()
            .total_dv_mps;
        assert!(plan.leg.total_dv_mps <= surface_best * 1.0001);
        // Lead-angle check, ±15°: at departure, the angle Terrin→Duva
        // (in the system plane) should satisfy the Hohmann phasing
        // π − n_d·tof within the eccentric/finite-burn slack.
        let (rt, _) = model
            .body_state(&BodyId::from("terrin"), SimTime(plan.departure_tick))
            .unwrap();
        let (rd, _) = model
            .body_state(&BodyId::from("duva"), SimTime(plan.departure_tick))
            .unwrap();
        let phase = libm::atan2(rt.cross(rd).z, rt.dot(rd));
        let n_d = libm::sqrt(MU_SOLEN / (DUVA_A * DUVA_A * DUVA_A));
        let tof_s = plan.duration_ticks as f64 / TICK_HZ as f64;
        let want = TAU / 2.0 - n_d * tof_s; // rad, ~ideal lead angle
        let mut err = (phase - want).rem_euclid(TAU);
        if err > TAU / 2.0 {
            err = TAU - err;
        }
        assert!(
            err < 0.30,
            "refined departure misses Hohmann phasing: phase={phase} want={want} err={err}"
        );
        // The refined plan is bitwise reproducible.
        let plan2 = plan_transfer(&model, &q).unwrap();
        assert_eq!(plan, plan2);
    }

    /// Validation: non-sibling bodies and degenerate queries refuse.
    #[test]
    fn porkchop_rejects_bad_queries() {
        let model = home_system();
        let mut q = duva_query(8);
        q.destination = BodyId::from("solen"); // star: no elements
        assert!(porkchop_scan(&model, &q).is_err());
        let mut q = duva_query(8);
        q.depart_lo = q.depart_hi + 64;
        assert!(porkchop_scan(&model, &q).is_err());
        let mut q = duva_query(8);
        q.depart_cells = 0;
        assert!(porkchop_scan(&model, &q).is_err());
        let mut q = duva_query(8);
        q.depart_cells = 300;
        q.duration_cells = 300; // over PORKCHOP_MAX_CELLS
        assert!(porkchop_scan(&model, &q).is_err());
        let mut q = duva_query(8);
        q.origin = q.destination.clone();
        assert!(porkchop_scan(&model, &q).is_err());
    }

    // -- Encounter prediction ------------------------------------------

    /// The orbit.rs graze construction, verbatim: at `t_enc` the vessel
    /// sits at `p`'s position plus `b` radially outward, moving
    /// tangentially at `1.4·v_circ` — the relative path is a line
    /// offset `b` from the planet's center, so the SOI residence is
    /// `≈ 2·sqrt(r_soi² − b²)/(0.4·v_circ)` and the approach periapsis
    /// is ≈ `b` (slightly less, bending inward).
    fn graze_conic(b: f64, t_enc: f64) -> OrbitState {
        let v_circ = libm::sqrt(MU_SOLEN / TERRIN_A);
        let n_p = libm::sqrt(MU_SOLEN / (TERRIN_A * TERRIN_A * TERRIN_A));
        let theta = n_p * t_enc;
        let aim = DVec3::new(
            (TERRIN_A + b) * libm::cos(theta),
            (TERRIN_A + b) * libm::sin(theta),
            0.0,
        );
        let vel = DVec3::new(
            -1.4 * v_circ * libm::sin(theta),
            1.4 * v_circ * libm::cos(theta),
            0.0,
        );
        OrbitState {
            parent: BodyId::from("solen"),
            elements: cartesian_to_elements(aim, vel, MU_SOLEN).unwrap(),
            epoch: SimTime((t_enc * TICK_HZ as f64) as u64),
        }
    }

    /// A conic on a nearly head-on pass (`b < radius`) through Terrin's
    /// SOI: the prediction must find `Enter{terrin}`, re-base into
    /// Terrin's frame with a hyperbolic approach, and flag the impact
    /// against the carried body radius.
    #[test]
    fn encounter_prediction_reads_the_approach() {
        let model = home_system();
        let t_enc = 2.0e6;
        let conic = graze_conic(4.0e5, t_enc); // 400 km offset < 600 km radius
        let now = SimTime(((t_enc - 6.0e5).max(0.0) * TICK_HZ as f64) as u64);
        let horizon = SimTime(((t_enc + 6.0e5) * TICK_HZ as f64) as u64);
        let pred = predict_encounter(&model, &conic, now, horizon)
            .unwrap()
            .expect("a transit this direct must encounter");
        assert_eq!(pred.event.direction, SoiDirection::Enter);
        assert_eq!(pred.event.body, BodyId::from("terrin"));
        let approach = pred.approach.expect("Enter carries approach geometry");
        // Any real SOI entry is hyperbolic in the child's frame.
        assert!(approach.elements.eccentricity > 1.0);
        assert!(approach.v_inf_mps.unwrap() > 0.0);
        assert!(approach.periapsis_m < TERRIN_R);
        assert_eq!(approach.impacts, Some(true));
        // The rebased conic is Terrin-parented at the crossing tick.
        let rebased = pred.rebased.unwrap();
        assert_eq!(rebased.parent, BodyId::from("terrin"));
        assert_eq!(rebased.epoch, SimTime(pred.event.tick));
    }

    /// Chaining: a graze (`b` far above the surface) ENTERS Terrin's
    /// SOI and leaves again — `Enter` first, then `Exit` on the
    /// chained query driven by the rebased conic.
    #[test]
    fn encounter_prediction_chains_through_rebase() {
        let model = home_system();
        let t_enc = 2.0e6;
        // b = 6e7 m — deep inside the 8.4e7 SOI but nowhere near the
        // 6e5 surface: a clean enter-then-exit residence.
        let conic = graze_conic(6.0e7, t_enc);
        let now = SimTime(((t_enc - 6.0e5).max(0.0) * TICK_HZ as f64) as u64);
        let horizon = SimTime(((t_enc + 6.0e5) * TICK_HZ as f64) as u64);
        let first = predict_encounter(&model, &conic, now, horizon)
            .unwrap()
            .expect("designed graze must encounter");
        assert_eq!(first.event.direction, SoiDirection::Enter);
        let rebased = first.rebased.unwrap();
        let second =
            predict_encounter(&model, &rebased, SimTime(first.event.tick), horizon).unwrap();
        // The residence ends inside the horizon → the Exit reports.
        let second = second.expect("graze must exit the SOI it entered");
        assert_eq!(second.event.direction, SoiDirection::Exit);
        assert_eq!(second.event.body, BodyId::from("terrin"));
        // Exiting re-bases to the grandparent (solen).
        assert_eq!(second.rebased.unwrap().parent, BodyId::from("solen"));
    }

    /// The plan → predict composition (the BLOCK-1 contract):
    /// `plan.conic` is stamped at the origin's center — inside the
    /// origin's own SOI — so the unfiltered scan's earliest detection
    /// is that egress (useless: no approach, no rebase target).
    /// `predict_encounter_excluding(plan.origin)` must skip it and
    /// report `Enter{destination}` with a real approach geometry.
    #[test]
    fn plan_conic_composes_into_encounter_prediction() {
        let model = home_system();
        let q = duva_query(12);
        let plan = plan_transfer(&model, &q).unwrap();
        let horizon = SimTime(plan.arrival_tick.saturating_add(TICK_HZ * 86_400));
        let pred = predict_encounter_excluding(
            &model,
            &plan.conic,
            SimTime(plan.departure_tick),
            horizon,
            Some(&plan.origin),
        )
        .unwrap()
        .expect("the transfer conic must encounter its destination");
        assert_eq!(pred.event.direction, SoiDirection::Enter);
        assert_eq!(pred.event.body, plan.destination);
        // SOI entry precedes closest approach — the conic reaches the
        // destination's center at arrival_tick.
        assert!(pred.event.tick <= plan.arrival_tick);
        // The approach is a genuine hyperbolic entry into duva's frame.
        let approach = pred.approach.expect("Enter carries approach geometry");
        assert!(approach.elements.eccentricity > 1.0);
        assert!(approach.v_inf_mps.unwrap() > 0.0);
        assert_eq!(pred.rebased.unwrap().parent, plan.destination);
    }

    /// M-2: `e ∈ (1, 1 + ε]` is the `a := −p` parabolic encoding —
    /// periapsis `p/2`, true `v∞ ≈ 0`. The old guard read `√(μ/p)` for
    /// it; the shared [`E_PARABOLIC_TOL`] classification must not.
    #[test]
    fn encounter_approach_reads_parabolic_encoding() {
        let p = 8.0e6;
        // Stay inside the band but off the exact `1 + ε` boundary —
        // `1 + ε` itself rounds in f64, so the exact edge is
        // representation-dependent, not a contract.
        for e in [
            1.0,
            1.0 + 0.5 * E_PARABOLIC_TOL,
            1.0 + 0.9 * E_PARABOLIC_TOL,
        ] {
            let mut el = circular_elements(1.0, 0.0);
            el.eccentricity = e;
            el.semi_major_axis_m = -p; // a := −p encoding
            let ap = encounter_approach(&el, MU_TERRIN, Some(6.0e5));
            assert_eq!(ap.v_inf_mps, None, "e={e}: parabolic v_inf must be absent");
            assert!((ap.periapsis_m - p / 2.0).abs() < 1.0, "e={e}: r_p = p/2");
        }
        // Just past the band: genuinely hyperbolic — v_inf reports.
        let mut el = circular_elements(1.0, 0.0);
        el.eccentricity = 1.0 + 2.0 * E_PARABOLIC_TOL;
        el.semi_major_axis_m = -p;
        let ap = encounter_approach(&el, MU_TERRIN, None);
        assert!(ap.v_inf_mps.unwrap() > 0.0);
    }

    /// A conic that never crosses any SOI boundary reports `None`.
    #[test]
    fn encounter_prediction_reports_none_for_a_quiet_orbit() {
        let model = home_system();
        let conic = OrbitState {
            parent: BodyId::from("solen"),
            elements: circular_elements(2.5e10, 0.0), // beyond Duva, coplanar
            epoch: SimTime(0),
        };
        let horizon = SimTime((4.0e6 * TICK_HZ as f64) as u64);
        assert!(
            predict_encounter(&model, &conic, SimTime(0), horizon)
                .unwrap()
                .is_none()
        );
    }

    // -- Departure node ------------------------------------------------

    /// The ejection produces a node whose post-burn conic escapes with
    /// the required v∞: propagate the post-burn state far forward and
    /// the velocity asymptotes to `plan.leg.v_inf_depart`.
    #[test]
    fn departure_node_ejects_along_v_inf() {
        let model = home_system();
        // A small window query just to get a real plan's v_inf vector.
        let q = PorkchopQuery {
            depart_cells: 12,
            duration_cells: 12,
            depart_cost: EndCost::CircularPark(TERRIN_R + 75e3),
            ..duva_query(12)
        };
        let plan = plan_transfer(&model, &q).unwrap();
        // Vessel on a 75 km circular orbit about terrin, coplanar with
        // the ecliptic — the canonical LEO departure.
        let vessel = OrbitState {
            parent: BodyId::from("terrin"),
            elements: circular_elements(TERRIN_R + 75e3, 0.0),
            epoch: SimTime(0),
        };
        let node = departure_node(&model, &vessel, &plan).unwrap();
        // The node is finite and sits at-or-before the window.
        assert!(node.tick <= plan.departure_tick);
        assert!(node.dv_prograde_mps.is_finite());
        // Post-burn conic about terrin: hyperbolic, and its outgoing
        // asymptote matches the plan's v_inf direction.
        let (rb, vb) = model.state_at(&vessel, SimTime(node.tick)).unwrap();
        let dv = crate::maneuver::node_dv_world(
            rb,
            vb,
            node.dv_prograde_mps,
            node.dv_normal_mps,
            node.dv_radial_mps,
        )
        .unwrap();
        let (r_far, v_far) = propagate(rb, vb + dv, MU_TERRIN, 2.0e5).unwrap();
        let v_inf_hat = plan.leg.v_inf_depart / plan.leg.v_inf_depart.norm();
        let got_hat = v_far / v_far.norm();
        // The far-field velocity direction ≈ v∞̂ (patched-conics:
        // the hyperbola's asymptote). Direction agreement is the
        // ejection's real assertion; magnitude follows by energy.
        assert!(
            vec_rel_err(got_hat, v_inf_hat) < 0.05,
            "ejection asymptote off target: r_far={r_far:?} got={got_hat:?} want={v_inf_hat:?}"
        );
        // Magnitude: |v_far| should be within a few % of v_inf.
        let mag_err =
            (v_far.norm() - plan.leg.v_inf_depart.norm()).abs() / plan.leg.v_inf_depart.norm();
        assert!(mag_err < 0.05, "v_inf magnitude off: {mag_err}");
    }

    /// BLOCK-3: the porkchop's `CircularPark` objective is a COPLANAR
    /// LOWER BOUND — the departure node's solved impulse must cost at
    /// least the bound; the gap is the unpriced plane-change component
    /// (≈ 0 for this near-coplanar fixture, ≥ 0 always).
    #[test]
    fn departure_node_cost_covers_the_coplanar_bound() {
        let model = home_system();
        let park_r = TERRIN_R + 75e3;
        let q = PorkchopQuery {
            depart_cells: 12,
            duration_cells: 12,
            depart_cost: EndCost::CircularPark(park_r),
            ..duva_query(12)
        };
        let plan = plan_transfer(&model, &q).unwrap();
        // Vessel parked exactly at the bound's radius, coplanar — the
        // bound's assumptions hold, so the node should nearly saturate it.
        let vessel = OrbitState {
            parent: BodyId::from("terrin"),
            elements: circular_elements(park_r, 0.0),
            epoch: SimTime(0),
        };
        let node = departure_node(&model, &vessel, &plan).unwrap();
        let bound = plan.leg.dv_depart_mps;
        let actual = node.dv_world.norm();
        assert!(
            actual >= bound * (1.0 - 1e-6),
            "node undercuts the coplanar bound: actual={actual} bound={bound}"
        );
        // The gap is the plane-change component — small here because
        // the fixture is near-coplanar, but it is what the bound does
        // not price. Assert it stays within a sane multiple of the
        // bound rather than demanding exact equality (burn longitude
        // quantization adds a few m/s either way).
        let gap = actual - bound;
        assert!(
            gap < 0.25 * bound,
            "plane-change gap unexpectedly large: gap={gap} bound={bound}"
        );
    }

    /// `departure_node` refuses the cases it cannot honestly serve:
    /// a vessel not orbiting the origin, an open parking orbit.
    #[test]
    fn departure_node_refuses_unservable_cases() {
        let model = home_system();
        let q = duva_query(12);
        let plan = plan_transfer(&model, &q).unwrap();
        // Heliocentric vessel — not a terrin parking orbit.
        let helio = OrbitState {
            parent: BodyId::from("solen"),
            elements: circular_elements(1.5e10, 0.0),
            epoch: SimTime(0),
        };
        assert!(departure_node(&model, &helio, &plan).is_err());
        // Open parking orbit.
        let mut hyper = circular_elements(1e6, 0.0);
        hyper.eccentricity = 1.5;
        hyper.semi_major_axis_m = -1e6;
        let open = OrbitState {
            parent: BodyId::from("terrin"),
            elements: hyper,
            epoch: SimTime(0),
        };
        assert!(departure_node(&model, &open, &plan).is_err());
    }
}
