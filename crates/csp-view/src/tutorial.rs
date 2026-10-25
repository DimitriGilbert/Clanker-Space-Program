//! M10 — the first-flight tutorial driver (PLAN M10 exit row:
//! "tutorial/first-flight").
//!
//! The session model and step predicates live in
//! [`csp_ui::tutorial`]; this module is the §2.5 plumbing that feeds
//! it. `tutorial_drive` runs in the Presentation schedule — the same
//! slot the SFX cue edges take — and folds the *staged* mirrors into a
//! [`csp_ui::tutorial::TutorialInput`] each frame: the
//! [`crate::flight::FlightSnapshot`] for the focused vessel's
//! altitude/throttle and the environment's μ (for the osculating
//! apsides), the VAB input flag and session analysis for the
//! editor-side steps. It never reads the sim — presentation derives
//! only from staged state (§2.5).

use bevy_ecs::prelude::*;
use csp_core::DVec3;

use crate::flight::FlightSnapshot;
use crate::vab::{VabInput, VabSessionRes};

/// The tutorial session resource — `csp-view` owns it (§2.2-C), the
/// Presentation schedule's [`tutorial_drive`] observes staged mirrors
/// into it, and `build_ui`'s overlay draws/mutates it (Dismiss,
/// Restart).
#[derive(Resource, Default)]
pub struct TutorialRes(pub csp_ui::tutorial::TutorialState);

/// Osculating `(apoapsis, periapsis)` *radii* from cartesian state —
/// the same closed form the bench's scripted controller uses (here in
/// plain f64: presentation derivation, not sim math). `None` when the
/// state is degenerate or the trajectory is open (`energy ≥ 0` — an
/// escape has no closed orbit to credit the tutorial's orbit step
/// with, even though a hyperbolic periapsis exists).
fn osculating_apsides(rel: DVec3, vel: DVec3, mu: f64) -> Option<(f64, f64)> {
    let r = rel.norm();
    if r <= 0.0 || mu <= 0.0 {
        return None;
    }
    let p = rel.cross(vel).norm_squared() / mu;
    let energy = vel.norm_squared() / 2.0 - mu / r;
    if energy >= 0.0 {
        return None;
    }
    let a = -mu / (2.0 * energy);
    let e = f64::sqrt((1.0 - p / a).max(0.0));
    if e >= 1.0 {
        return None;
    }
    Some((p / (1.0 - e), p / (1.0 + e)))
}

/// `Presentation` schedule — fold the staged mirrors into
/// [`csp_ui::tutorial::TutorialInput`] and drive the checklist. Reads
/// [`FlightSnapshot`] directly (not `FlightUiData`, so it stays
/// independent of system ordering within the schedule); the tracked
/// vessel is the pin-resolved focus (`FlightFocusOverride`'s
/// [`resolve`](crate::flight::FlightFocusOverride::resolve) — pin
/// target when staged, newest otherwise) minus wreckage: a
/// recover-control pin must measure the recovered vessel, not the
/// newest spawn (P12-F1), while debris still must not satisfy (or
/// suppress) progress.
pub(crate) fn tutorial_drive(
    snap: Res<FlightSnapshot>,
    live: Res<crate::flight::LiveVesselCount>,
    focus_override: Res<crate::flight::FlightFocusOverride>,
    vab_input: Res<VabInput>,
    vab: Res<VabSessionRes>,
    mut tutorial: ResMut<TutorialRes>,
) {
    let env = snap.env.as_ref();
    // The tutorial's vessel is the focused NON-debris staged vessel —
    // the pin-aware resolver picks the pinned target first, then
    // falls back to the newest non-debris entry (debris must not
    // count as "a vessel" at all: a climb that shatters leaves only
    // wreckage, and counting it would suppress the vessel_lost
    // rewind the checklist needs to re-guide the player).
    let focus = focus_override
        .resolve(&snap.vessels)
        .filter(|v| !v.debris)
        .or_else(|| snap.vessels.iter().rev().find(|v| !v.debris));
    let mut apoapsis_m = None;
    let mut periapsis_m = None;
    if let (Some(v), Some(env)) = (focus, env)
        && let Some((ra, rp)) = osculating_apsides(v.com - env.body_center, v.vel, env.mu_m3_per_s2)
    {
        apoapsis_m = Some(ra - env.radius_m);
        periapsis_m = Some(rp - env.radius_m);
    }
    let session = vab.0.session();
    let input = csp_ui::tutorial::TutorialInput {
        in_vab: vab_input.open,
        craft_parts: session.len(),
        craft_has_thrust: vab
            .0
            .analysis()
            .is_some_and(|a| a.stages.iter().any(|s| s.thrust_vac_n > 0.0)),
        // A vessel is "had" in ANY residence — `live` is the staged
        // count of non-debris vessels across ALL residences, so a live
        // craft that finished its burn back onto rails does not read
        // as lost (P12-F2). `focus` covers only the InBubble/latched
        // staged set — it is a subset of the count.
        has_vessel: live.0 > 0,
        throttle: focus.map_or(0.0, |v| v.throttle),
        alt_m: match (focus, env) {
            (Some(v), Some(env)) => (v.com - env.body_center).norm() - env.radius_m,
            _ => 0.0,
        },
        apoapsis_m,
        periapsis_m,
        atmosphere_top_m: env.map_or(0.0, |e| e.atm_top_m),
    };
    tutorial.0.observe(&input);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::flight::{FlightBody, FlightEnv};
    use bevy_ecs::schedule::Schedule;
    use bevy_ecs::world::World;

    /// Terrin-shaped environment for the staged snapshots.
    fn env() -> FlightEnv {
        FlightEnv {
            body_center: DVec3::ZERO,
            radius_m: 6.0e5,
            atm_top_m: 70_000.0,
            atm_scale_height_m: 5_600.0,
            mu_m3_per_s2: 3.5316e12,
            body: None,
            sun_dir: DVec3::Z,
            spin_axis: DVec3::Z,
            spin_phase: 0.0,
            body_spin: DVec3::ZERO,
        }
    }

    /// A staged vessel at `rel`/`vel` — the driver reads only the
    /// staged snapshot, so the test stages the same shape production
    /// feeds it (no sim needed).
    fn vessel(rel: DVec3, vel: DVec3, throttle: f64) -> FlightBody {
        FlightBody {
            id: 0,
            pos: rel,
            rot: csp_core::DQuat::IDENTITY,
            vel,
            throttle,
            debris: false,
            contact: false,
            plasma_w_m2: 0.0,
            nodes: 3,
            parts: Vec::new(),
            plumes: Vec::new(),
            com: rel,
            latched: false,
            tanks: Vec::new(),
        }
    }

    /// Run `tutorial_drive` on a hand-staged presentation world —
    /// the system under test, on the same resources it reads in
    /// production. `live` stages the cross-residence non-debris count
    /// `sync_flight` would have written from the sim.
    fn drive(snap: FlightSnapshot, live: usize, vab_open: bool, tutorial: &mut TutorialRes) {
        let mut world = World::new();
        world.insert_resource(snap);
        world.insert_resource(crate::flight::LiveVesselCount(live));
        world.insert_resource(crate::flight::FlightFocusOverride::default());
        world.insert_resource(VabInput {
            open: vab_open,
            ..VabInput::default()
        });
        world.insert_resource(VabSessionRes::default());
        world.insert_resource(std::mem::take(tutorial));
        let mut sched = Schedule::default();
        sched.add_systems(tutorial_drive);
        sched.run(&mut world);
        *tutorial = world.remove_resource::<TutorialRes>().expect("resource");
    }

    #[test]
    fn apsides_known_circular_orbit() {
        // Circular at 100 km altitude: r = 700 km, v = sqrt(μ/r).
        let (mu, r) = (3.5316e12, 7.0e5);
        let v = f64::sqrt(mu / r);
        let (ra, rp) = osculating_apsides(DVec3::new(r, 0.0, 0.0), DVec3::new(0.0, v, 0.0), mu)
            .expect("closed orbit");
        assert!((ra - r).abs() < 1.0, "ra={ra}");
        assert!((rp - r).abs() < 1.0, "rp={rp}");
        // An escape trajectory reports no closed apsides.
        let vesc = f64::sqrt(2.0 * mu / r) * 1.1;
        assert!(
            osculating_apsides(DVec3::new(r, 0.0, 0.0), DVec3::new(0.0, vesc, 0.0), mu).is_none()
        );
    }

    /// The orbit step completing end-to-end through the driver: a
    /// staged 100 km circular-orbit snapshot drives the checklist to
    /// `finished` — the milestone the starter craft may not physically
    /// reach is still exercised through the real pipeline.
    #[test]
    fn orbital_snapshot_completes_tutorial() {
        let (mu, r) = (3.5316e12, 7.0e5);
        let v = f64::sqrt(mu / r);
        let snap = FlightSnapshot {
            env: Some(env()),
            vessels: vec![vessel(
                DVec3::new(r, 0.0, 0.0),
                DVec3::new(0.0, v, 0.0),
                1.0,
            )],
        };
        let mut t = TutorialRes::default();
        drive(snap, 1, false, &mut t);
        assert!(t.0.finished, "orbital snapshot must finish the checklist");
    }

    /// R15-F4: debris is not "a vessel" — a mid-flight shattering
    /// leaves only wreckage staged, and the checklist must rewind to
    /// LAUNCH rather than wait on telemetry wreckage will never
    /// produce. A real vessel alongside debris is still tracked.
    #[test]
    fn debris_only_snapshot_rewinds_as_vessel_lost() {
        let mut t = TutorialRes::default();
        // Climb progress first — put the checklist on a flight step.
        let climbing = FlightSnapshot {
            env: Some(env()),
            vessels: vec![vessel(
                DVec3::new(6.02e5, 0.0, 0.0),
                DVec3::new(0.0, 300.0, 400.0),
                1.0,
            )],
        };
        drive(climbing, 1, false, &mut t);
        assert!(t.0.step > csp_ui::tutorial::STEP_LAUNCH);
        // The craft shatters: only a debris fragment remains staged.
        let mut wreck = vessel(
            DVec3::new(6.02e5, 0.0, 0.0),
            DVec3::new(0.0, 300.0, 400.0),
            0.0,
        );
        wreck.debris = true;
        drive(
            FlightSnapshot {
                env: Some(env()),
                vessels: vec![wreck],
            },
            // Wreckage is not alive — the count is non-debris vessels
            // only, so a debris-only world reads as "vessel lost"
            // (P12-F2's complement).
            0,
            false,
            &mut t,
        );
        assert_eq!(t.0.step, csp_ui::tutorial::STEP_LAUNCH);
        assert!(t.0.vessel_lost);
    }

    /// A sub-orbital staged snapshot advances only through the
    /// throttle step — climb/space/orbit stay honestly pending.
    #[test]
    fn suborbital_snapshot_stops_at_climb() {
        let snap = FlightSnapshot {
            env: Some(env()),
            // 2 km up, climbing: throttle open, no closed apsides.
            vessels: vec![vessel(
                DVec3::new(6.02e5, 0.0, 0.0),
                DVec3::new(0.0, 300.0, 400.0),
                1.0,
            )],
        };
        let mut t = TutorialRes::default();
        drive(snap, 1, false, &mut t);
        assert_eq!(t.0.step, 4, "at 2 km the checklist sits on Climb");
        assert!(!t.0.finished);
    }
}
