//! Maneuver nodes + the burn executor (PLAN M5 exit row: "Maneuver
//! nodes + executor").
//!
//! A [`ManeuverNode`] is a scheduled Δv: `{tick, dv_prograde, dv_normal,
//! dv_radial}` on a vessel's orbit — the gameplay mechanic for orbital
//! transfers. The components are expressed in the node point's orbital
//! velocity frame (§2.8 conventions): **prograde** is the velocity
//! direction `v̂`, **normal** the orbit-plane normal `(r × v)̂`, and
//! **radial** the out-from-parent direction `r̂`. The inertial Δv vector
//! the node produces is therefore a pure function of the node's tick and
//! the conic it sits on — see [`node_dv_world`].
//!
//! # Execution model — thrust-integrated, never impulsive
//!
//! Setting a node schedules [`EventKind::ManeuverNodeStart`] up to a
//! half-estimated-burn BEFORE the node's tick — the C06-F2 arming
//! lead. The executor burns *forward* from whatever tick arms it, so
//! an event at `node.tick` would centre the delivered impulse at
//! `node.tick + T_burn/2` — first-order late against the impulsive
//! model [`predict_maneuver`] and the planner solve against. Arming
//! at `node.tick − ⌈T_burn/2⌉` (the [`est_burn_ticks`] estimate)
//! centres the delivered impulse on the node tick instead (±1 tick on
//! odd burn lengths). The event is still a §2.6.5 `Clamping` member,
//! so warp demotes before the burn and the player cannot overshoot.
//! When the event fires the vessel's conic is evaluated at the arming
//! tick, the `WeldedBody` re-materializes in the bubble there, and the
//! node becomes [`ManeuverStatus::Burning`]: the **executor** — a
//! first-class mechanism inside `World::step_bubble_range`, not a
//! command stream — owns `throttle` and `attitude` each integer tick,
//! aligning the weld's net thrust axis to the inertial burn direction
//! (the node's Δv resolved at the *node* tick — the design point the
//! components were authored against, not the earlier arming point)
//! and driving throttle so the delivered Δv (`throttle · F / m · dt`,
//! the exact quantity the §2.9 integrator applies) counts down to zero
//! without overshoot. On completion the node is consumed, throttle
//! zeroes, and the vessel returns to rails — the osculating state
//! stamps epoch-anchored elements at the conversion tick (best-effort:
//! a conic that no longer clears the surface leaves the vessel
//! bubble-resident, which is the honest outcome of a suicide burn).
//!
//! v1 scope, stated honestly:
//!
//! - **One node per vessel** (`Vessel::maneuver` is a single slot; a
//!   second `SetManeuver` replaces a `Scheduled` node and is refused
//!   while a burn is executing).
//! - **Rails-scheduled, dominant-parent only.** `SetManeuver` requires
//!   the vessel `OnRails` with `orbit.parent == physics.dominant` and a
//!   craft carrying at least one positive-rated `Thrust` site — the
//!   executor materially cannot run elsewhere (the bubble is a
//!   dominant-centred frame; an elements-only vessel has no thrust
//!   machinery). A vessel that re-parents before its node fires has the
//!   firing degrade and the node dropped (§2.13's channel).
//! - **Mass changes only on the §2.4 quantum.** Engines debit
//!   propellant every tick (see [`crate::transfers`]); the weld's
//!   mass moves only when a 1/1024 contents bucket flips, so the
//!   executor's `throttle·F/m` bookkeeping reads a mass that steps
//!   discretely rather than draining continuously — the documented
//!   v1 reading. A burn that starves its tanks mid-run stalls at the
//!   fuel-limited `net_thrust_axis` like any other lost-thrust stall:
//!   the executor finishes with whatever Δv was actually delivered.
//! - **While `Burning`, the executor owns throttle and attitude.** The
//!   same absolute-control convention `SetThrottle`/`SetAttitude` use
//!   (`rot`/`ang_vel` writes, not torque loops): manual writes during a
//!   burn are overridden on the next executor tick. `DeleteManeuver`
//!   cancels — throttle zeroes and the vessel keeps its residence.
//! - **Residence transitions discard the scheduled intent.** A manual
//!   `ToBubble` (the player took the stick) or `ToRails` clears the
//!   node and its pending event; the executor's own materialization is
//!   the one path that keeps it. A `Burning` node survives the §2.6.4
//!   latch/unlatch round-trip (a latched vessel cannot burn; unlatching
//!   resumes the countdown where it stopped).

use csp_core::{DQuat, DVec3, SimTime, StableDiscriminant, StateHasher, TICK_HZ};
use serde::{Deserialize, Serialize};

use crate::orbit::{OrbitModel, OrbitState, PatchedConics};

/// The scheduled-Δv record on a vessel — sim state: serialized on the
/// vessel, hashed by [`hash_maneuver`], mutated only through
/// `SimCommand::SetManeuver`/`DeleteManeuver` and the executor itself.
///
/// `tick` is the node's design point — the tick the Δv components
/// resolve against and [`predict_maneuver`] models the impulse at.
/// The `ManeuverNodeStart` event arms the executor up to a half-burn
/// EARLIER (the [`est_burn_ticks`] lead, C06-F2) so the delivered
/// impulse centres on `tick` rather than trailing it. `tick` itself
/// is always strictly ahead of the clock at schedule time;
/// `SetManeuver` clamps at-or-past ticks to `clock + 1`, matching
/// `SetAlarm`'s may-not-rewrite-the-past rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManeuverNode {
    /// The sim tick the delivered impulse centres on (the node point).
    pub tick: u64,
    /// Δv along the node point's prograde direction `v̂`, m/s.
    pub dv_prograde_mps: f64,
    /// Δv along the node point's orbit normal `(r × v)̂`, m/s.
    pub dv_normal_mps: f64,
    /// Δv along the node point's outward radial `r̂`, m/s.
    pub dv_radial_mps: f64,
    /// Where the node is in its lifecycle.
    pub status: ManeuverStatus,
}

impl ManeuverNode {
    /// The node's commanded Δv magnitude in m/s — the same number the
    /// executor's `dv_total_mps` captures when the burn arms.
    pub fn dv_magnitude(&self) -> f64 {
        libm::sqrt(
            self.dv_prograde_mps * self.dv_prograde_mps
                + self.dv_normal_mps * self.dv_normal_mps
                + self.dv_radial_mps * self.dv_radial_mps,
        )
    }
}

/// Where a [`ManeuverNode`] is in its lifecycle — the §7.1 rule-7 hash
/// encoding is the declared `u16`, append-only.
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ManeuverStatus {
    /// Waiting: a `ManeuverNodeStart` event sits in the heap at
    /// `node.tick`. Invariant: `Scheduled` nodes exist only on
    /// `OnRails` vessels, and exactly one pending event names them
    /// (`World::restore` validates both directions).
    Scheduled = 0,
    /// Executing: the `ManeuverNodeStart` fired, the vessel is
    /// bubble-resident (or §2.6.4-latched mid-burn — the countdown
    /// holds while latched and resumes on unlatch), and the executor
    /// owns throttle/attitude until [`ActiveBurn::dv_remaining_mps`]
    /// reaches zero.
    Burning(ActiveBurn) = 1,
}

impl StableDiscriminant for ManeuverStatus {
    fn discriminant(&self) -> u16 {
        match self {
            Self::Scheduled => 0,
            Self::Burning(_) => 1,
        }
    }
}

/// The executor's in-flight record — sim state: a save taken mid-burn
/// resumes the countdown bit-identically (§7.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveBurn {
    /// The inertial burn direction in the bubble frame (unit vector),
    /// frozen at burn start — the node's Δv vector evaluated at the
    /// node point. Fixed inertially like the KSP convention: with
    /// perfect attitude the remaining-Δv direction never rotates.
    pub burn_dir: DVec3,
    /// The Δv the node commanded, m/s — `burn_dir · dv_total_mps` is
    /// the full commanded vector (display + restore sanity).
    pub dv_total_mps: f64,
    /// The Δv still owed, m/s — counts down by exactly the thrust
    /// impulse the integrator applies each tick.
    pub dv_remaining_mps: f64,
}

/// The `r × v` degeneracy bound for [`burn_frame`], relative to
/// `|r|·|v|`: `h/(r·v) = |sin θ|`, so the gate refuses trajectories
/// this close to rectilinear. At the bound the normal's direction
/// error — O(`eps·|r|·|v|/h`) — already exceeds the §2.8 element
/// budget, so below it the orbit normal is a rounding artifact, not a
/// direction.
const BURN_FRAME_H_TOL: f64 = 1e-10;

/// The node's orbital velocity frame at a state `(r, v)` —
/// parent-centred inertial: `(prograde, normal, radial)` =
/// `(v̂, (r × v)̂, r̂)`. `Recoverable` when the frame is degenerate —
/// a singular state (`r = 0`, `v = 0`) or a near-radial trajectory
/// (`|r × v| ≤ `[`BURN_FRAME_H_TOL`]`·|r|·|v|`), where "normal" and
/// "prograde" have no honest reading.
pub fn burn_frame(r: DVec3, v: DVec3) -> Result<(DVec3, DVec3, DVec3), csp_core::SimError> {
    let rn = r.norm();
    let vn = v.norm();
    let hn = r.cross(v).norm();
    if !(rn.is_finite() && vn.is_finite() && hn.is_finite())
        || rn <= 0.0
        || vn <= 0.0
        || hn <= BURN_FRAME_H_TOL * rn * vn
    {
        return Err(csp_core::SimError::Recoverable(
            "maneuver: degenerate orbital frame at node point",
        ));
    }
    Ok((v / vn, r.cross(v) / hn, r / rn))
}

/// The inertial Δv vector a node produces at `(r, v)` — the
/// prograde/normal/radial components resolved through [`burn_frame`].
pub fn node_dv_world(
    r: DVec3,
    v: DVec3,
    dv_prograde_mps: f64,
    dv_normal_mps: f64,
    dv_radial_mps: f64,
) -> Result<DVec3, csp_core::SimError> {
    let (pro, nrm, rad) = burn_frame(r, v)?;
    Ok(pro * dv_prograde_mps + nrm * dv_normal_mps + rad * dv_radial_mps)
}

/// The post-burn osculating conic a `Scheduled` node produces — the
/// map view's predicted-orbit preview and the executor test's
/// expectation. The node's components are resolved against the conic's
/// state at `node.tick`, added impulsively (the prediction's model of
/// the finite burn — the executor delivers the same *total* Δv spread
/// across its burn arc, so the achieved conic tracks this prediction
/// to finite-burn accuracy, not bitwise).
///
/// `Err` propagates the underlying `state_at`/`cartesian_to_elements`
/// failure — a node whose point the model cannot solve has no honest
/// prediction.
pub fn predict_maneuver(
    model: &PatchedConics,
    orbit: &OrbitState,
    node: &ManeuverNode,
) -> Result<OrbitState, csp_core::SimError> {
    let t = SimTime::from_ticks(node.tick);
    let (r, v) = model.state_at(orbit, t)?;
    let dv = node_dv_world(
        r,
        v,
        node.dv_prograde_mps,
        node.dv_normal_mps,
        node.dv_radial_mps,
    )?;
    let mu = model.body_orbit(&orbit.parent)?.mu_m3_per_s2;
    let elements = crate::orbit::cartesian_to_elements(r, v + dv, mu)?;
    Ok(OrbitState {
        parent: orbit.parent.clone(),
        elements,
        epoch: t,
    })
}

/// The weld's net thrust axis in body frame — `Σ dir·rated` over
/// `ForceSiteKind::Thrust` sites — and its magnitude (the max thrust
/// the executor can command, N). Linear addition is exact: aligning
/// the *net* axis to the burn direction delivers `throttle·|net|`
/// along it regardless of per-site splay. `(ZERO, 0)` on a craft with
/// no live thrust — the executor's stalled-burn input.
pub fn net_thrust_axis(body: &csp_phys::WeldedBody) -> (DVec3, f64) {
    let mut f = DVec3::ZERO;
    for site in &body.force_sites {
        if site.kind == csp_phys::ForceSiteKind::Thrust {
            f = f + site.local_dir * site.rated_n;
        }
    }
    (f, f.norm())
}

/// Does the stored craft carry at least one positive-rated thrust
/// site? The `SetManeuver` apply's executor-feasibility gate — a
/// craft that can never thrust gets its node refused up front rather
/// than discovered stalled at the node tick.
pub fn craft_has_thrust(craft: &crate::CraftDef) -> bool {
    craft.parts.iter().any(|p| {
        p.force_sites
            .iter()
            .any(|s| s.kind == csp_phys::ForceSiteKind::Thrust && s.rated_n > 0.0)
    })
}

/// The burn-duration estimate `SetManeuver` spends on the C06-F2
/// arming lead: `dv_mps ÷ (rated thrust ÷ part-mass sum)`, in ticks —
/// the same estimate the map's node label shows as "~Ns burn". An
/// *estimate*, honestly documented: it ignores propellant draw (the
/// weld's mass steps discretely anyway — module doc) and reads rated
/// thrust, so the real burn runs a touch shorter or longer; the lead
/// only needs the centre, not the endpoint. `None` for a thrustless
/// or massless craft or a non-positive Δv — all cases `SetManeuver`
/// refuses before the lead matters.
///
/// The formula lives here, not in `csp-view`, so the snapshot's
/// node↔event pairing check recomputes the identical window — a
/// drift between estimate and check would read a legitimate event as
/// a forged one.
pub fn est_burn_ticks(craft: &crate::CraftDef, dv_mps: f64) -> Option<u64> {
    let mass: f64 = craft.parts.iter().map(|p| p.mass_kg).sum();
    let thrust: f64 = craft
        .parts
        .iter()
        .flat_map(|p| p.force_sites.iter())
        .filter(|s| s.kind == csp_phys::ForceSiteKind::Thrust && s.rated_n > 0.0)
        .map(|s| s.rated_n)
        .sum();
    if !(mass.is_finite()
        && mass > 0.0
        && thrust.is_finite()
        && thrust > 0.0
        && dv_mps.is_finite()
        && dv_mps > 0.0)
    {
        return None;
    }
    Some((dv_mps * mass / thrust * TICK_HZ as f64).ceil() as u64)
}

/// The arming lead — half the [`est_burn_ticks`] estimate, ceilinged:
/// a burn starting at `node.tick − lead` centres its delivered
/// impulse at-or-within a tick of `node.tick` (never after — a late
/// impulse is the bug C06-F2 fixes). `0` for a craft with no honest
/// estimate — the event then arms at the node tick, the pre-lead
/// behaviour for a burn the estimate cannot see anyway.
pub fn node_lead_ticks(craft: &crate::CraftDef, dv_mps: f64) -> u64 {
    est_burn_ticks(craft, dv_mps).unwrap_or(0).div_ceil(2)
}

/// The near-aligned shortcut bound of [`align_rotation`]: a `cos` above
/// this returns `IDENTITY` instead of the exact axis-angle. `1 − 1e-12`
/// caps the residual pointing error at ~1.4e-6 rad; the earlier
/// `0.999_999` bound could leave ~1.4e-3 rad of slop — ~1.4 m/s of
/// transverse Δv on a 1 km/s burn (C06-F5) — and the exact branch is
/// only a cross product + one `libm::acos`, so the looser gate bought
/// nothing. `csp-bench`'s scripted controller (`thrust_attitude`,
/// `scenarios.rs`) shares this constant — the duplicated convention
/// must never drift.
pub const NEAR_ALIGNED_COS: f64 = 1.0 - 1e-12;

/// Rotation taking the body-frame unit `from` to the bubble-frame unit
/// `to` — the executor's attitude solve. Axis-angle on the cross
/// product; the `to` axis's orthogonal complement is used for the
/// anti-parallel case (the same convention `csp-bench`'s scripted
/// controller's `thrust_attitude` uses).
pub fn align_rotation(from: DVec3, to: DVec3) -> DQuat {
    let cos = from.dot(to);
    if cos > NEAR_ALIGNED_COS {
        return DQuat::IDENTITY;
    }
    if cos < -0.999_999 {
        // Anti-parallel: any perpendicular axis works — pick one from
        // the target's least-aligned basis direction, deterministically.
        let ortho = if to.x.abs() <= to.y.abs() && to.x.abs() <= to.z.abs() {
            DVec3::X
        } else if to.y.abs() <= to.z.abs() {
            DVec3::Y
        } else {
            DVec3::Z
        };
        let axis = to.cross(ortho);
        return DQuat::from_axis_angle(axis, core::f64::consts::PI);
    }
    DQuat::from_axis_angle(from.cross(to), libm::acos(cos))
}

/// The §7.1 field walk for one node — called from `hash_vessel` under
/// the `Option` wrapper (absent node = `opt` none-tag only).
pub fn hash_maneuver(h: &mut StateHasher, node: &ManeuverNode) {
    h.u64(node.tick);
    h.f64(node.dv_prograde_mps);
    h.f64(node.dv_normal_mps);
    h.f64(node.dv_radial_mps);
    h.stable_enum(&node.status);
    if let ManeuverStatus::Burning(burn) = &node.status {
        h.dvec3(burn.burn_dir);
        h.f64(burn.dv_total_mps);
        h.f64(burn.dv_remaining_mps);
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The velocity frame on a circular equatorial orbit at (r,0,0)
    /// moving +y: prograde +y, normal +z, radial +x.
    #[test]
    fn burn_frame_axes_on_circular_state() {
        let (pro, nrm, rad) =
            burn_frame(DVec3::new(7.0e5, 0.0, 0.0), DVec3::new(0.0, 2.0e3, 0.0)).unwrap();
        assert!((pro - DVec3::Y).norm() < 1e-12);
        assert!((nrm - DVec3::Z).norm() < 1e-12);
        assert!((rad - DVec3::X).norm() < 1e-12);
    }

    /// Degenerate states refuse — a node at a singular point or on a
    /// radial (zero-angular-momentum) trajectory has no frame, and
    /// neither does a NEAR-radial one: `|r × v|/(|r|·|v|)` below the
    /// relative gate means the orbit normal is rounding noise.
    #[test]
    fn burn_frame_refuses_degenerate_states() {
        assert!(burn_frame(DVec3::ZERO, DVec3::X).is_err());
        assert!(burn_frame(DVec3::X, DVec3::ZERO).is_err());
        // Radial trajectory: r ∥ v ⇒ r × v = 0.
        assert!(burn_frame(DVec3::X * 1.0e6, DVec3::X * 500.0).is_err());
        // Near-radial: h/(r·v) ≈ 8e-11 < BURN_FRAME_H_TOL — refuses
        // rather than return a noise-defined normal.
        assert!(burn_frame(DVec3::X * 1.0e6, DVec3::new(500.0, 4.0e-8, 0.0)).is_err());
        // Just above the gate (h/(r·v) ≈ 1e-9) still resolves.
        assert!(burn_frame(DVec3::X * 1.0e6, DVec3::new(500.0, 5.0e-7, 0.0)).is_ok());
    }

    /// `align_rotation` lands `from` exactly on `to`, including the
    /// anti-parallel and already-aligned edges.
    #[test]
    fn align_rotation_maps_axes() {
        let q = align_rotation(DVec3::Z, DVec3::X);
        assert!((q.rotate(DVec3::Z) - DVec3::X).norm() < 1e-12);
        let same = align_rotation(DVec3::Z, DVec3::Z);
        assert!((same.rotate(DVec3::Z) - DVec3::Z).norm() < 1e-12);
        let anti = align_rotation(DVec3::Z, -DVec3::Z);
        assert!((anti.rotate(DVec3::Z) + DVec3::Z).norm() < 1e-9);
        // A non-axis pair.
        let dv = DVec3::new(1.0, 2.0, 3.0);
        let tv = DVec3::new(-0.5, 1.0, 0.25);
        let d = dv / dv.norm();
        let t = tv / tv.norm();
        assert!((align_rotation(d, t).rotate(d) - t).norm() < 1e-12);
    }

    /// C06-F5: the near-aligned shortcut's band is `1 − 1e-12` — a
    /// `cos` in the old `0.999_999` shortcut range (≈1.4e-4 rad of
    /// slop below) now gets the exact axis-angle, not `IDENTITY`.
    #[test]
    fn align_rotation_band_uses_the_exact_branch() {
        // cos ≈ 1 − 2e-8: inside the retired `0.999_999` shortcut, well
        // outside the new `1 − 1e-12` one.
        let v = DVec3::Z + DVec3::X * 2.0e-4;
        let from = v / v.norm();
        let q = align_rotation(from, DVec3::Z);
        assert_ne!(q, DQuat::IDENTITY, "the old band must not short-circuit");
        assert!((q.rotate(from) - DVec3::Z).norm() < 1e-12);
        // And the surviving shortcut still holds: an exactly-aligned
        // pair stays `IDENTITY`.
        assert_eq!(align_rotation(DVec3::Z, DVec3::Z), DQuat::IDENTITY);
    }

    /// `node_dv_world` composes components through the frame — a pure
    /// prograde node on the circular state is exactly `dv·ŷ`.
    #[test]
    fn node_dv_world_resolves_components() {
        let dv = node_dv_world(
            DVec3::new(7.0e5, 0.0, 0.0),
            DVec3::new(0.0, 2.0e3, 0.0),
            100.0,
            -50.0,
            25.0,
        )
        .unwrap();
        assert!((dv - DVec3::new(25.0, 100.0, -50.0)).norm() < 1e-9);
    }
}
