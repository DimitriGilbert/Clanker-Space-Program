#![allow(clippy::unwrap_used, clippy::expect_used)]
//! M10-D — the first-flight tutorial exercised through the real
//! `View` pipeline: VAB → assemble → launch → throttle → climb →
//! space. Every step's completion is asserted against the sim's own
//! mirrors — the tutorial advances only when the player's action
//! actually happened, which is the whole design.
//!
//! The orbit milestone is covered two ways honestly: the scripted
//! run below asserts the checklist sits on Orbit after a real ascent
//! (the starter Δv may not close one), and `csp_view::tutorial`'s
//! staged-driver test completes the whole checklist off a staged
//! orbital snapshot.
//!
//! The flight leg launches the VAB-assembled craft itself through
//! `View::launch_craft` — the same path the LAUNCH button emits —
//! which is also the AttachAuto mount-orientation regression: the
//! pod→tank→engine stack must spawn with its engine nozzle-down or
//! the ascent below flies into the pad.

#![cfg(not(target_arch = "wasm32"))]

use csp_view::{SimCommand, SourceClass, View, ViewMode};

/// One rendered frame advancing `ticks` sim ticks — the app's
/// begin/sim_step/end contract headlessly.
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
}

/// Parts for the tutorial's assemble step — a real session build whose
/// analysis resolves positive thrust.
fn build_craft(view: &mut View) {
    view.with_vab_session(|v| {
        use csp_ui::vab::VabAction;
        v.apply(VabAction::SetRoot(csp_data::PartId::from("csp.pod.mk1")))
            .expect("root");
        for part in ["csp.tank.s160", "csp.engine.sparc"] {
            let parent = v.selected().expect("selection");
            v.apply(VabAction::AttachAuto {
                part: csp_data::PartId::from(part),
                parent,
            })
            .expect("attach");
        }
        // The tutorial's has-thrust predicate reads the cached
        // analysis — recompute like the editor does.
        v.recompute();
    });
}

/// Hold the vessel's body +z (thrust axis) on local radial-up — the
/// same attitude convention the flight scenario's scripted controller
/// uses. `None` while the vessel or the environment is absent.
fn hold_radial_up(view: &mut View, vessel: csp_sim::VesselId) {
    let Some(rot) = (|| {
        let body = view.sim.vessels().get(&vessel)?.body()?;
        let env = view.sim.environment()?;
        let rel = body.com_world() - env.body_center;
        let n = rel.norm();
        if n <= 0.0 {
            return None;
        }
        let up = rel / n;
        let cos = csp_core::DVec3::Z.dot(up);
        if cos > 0.999_999 {
            return Some(csp_core::DQuat::IDENTITY);
        }
        if cos < -0.999_999 {
            return Some(csp_core::DQuat::from_axis_angle(
                csp_core::DVec3::X,
                core::f64::consts::PI,
            ));
        }
        Some(csp_core::DQuat::from_axis_angle(
            csp_core::DVec3::Z.cross(up),
            f64::acos(cos),
        ))
    })() else {
        return;
    };
    view.enqueue_command(
        SourceClass::Ui,
        1,
        SimCommand::SetAttitude {
            vessel,
            rot,
            ang_vel: csp_core::DVec3::ZERO,
        },
    );
}

/// The scripted first flight: the checklist must track the player's
/// real progress, step for step.
#[test]
fn tutorial_first_flight_progression() {
    let mut view = View::new().expect("view");
    let t = view.tutorial_state();
    assert!(t.active && t.step == 0);

    // 1. Open the VAB — the first step completes on the mode change.
    view.set_mode(ViewMode::Vab);
    frame(&mut view, 1);
    assert_eq!(view.tutorial_state().step, 1, "after opening the VAB");

    // 2. Assemble — the analysis resolves real thrust.
    build_craft(&mut view);
    frame(&mut view, 1);
    assert_eq!(view.tutorial_state().step, 2, "after assembling a craft");

    // 3. Launch — `View::launch_craft` spawns the working craft on
    // the pad, the same command path the LAUNCH button emits. The
    // VAB-built pod→tank→engine must mount its engine nozzle-down —
    // the AttachAuto regression this test pins end-to-end.
    view.launch_craft();
    frame(&mut view, 1);
    let vessel = view.flight_focus().expect("vessel on the pad");
    assert_eq!(view.tutorial_state().step, 3, "after launch");

    // 4. Throttle — the command is the player's throttle key.
    view.enqueue_command(
        SourceClass::Ui,
        1,
        SimCommand::SetThrottle {
            vessel,
            throttle: 1.0,
        },
    );
    frame(&mut view, 2);
    assert_eq!(view.tutorial_state().step, 4, "after throttle-up");

    // 5–6. Climb through 10 km and on to space — the sim's own
    // altitude mirror drives both steps. The craft holds radial-up
    // attitude under throttle (the player-side control input, one
    // `SetAttitude` per frame like the bench's scripted controller);
    // Terrin's atmosphere tops at 70 km.
    let mut climbed = false;
    for _ in 0..400 {
        hold_radial_up(&mut view, vessel);
        frame(&mut view, 64);
        if view.tutorial_state().step >= 6 {
            climbed = true;
            break;
        }
    }
    assert!(
        climbed,
        "the burn never cleared the atmosphere (step {}, best {:.0} m, vessels {})",
        view.tutorial_state().step,
        view.tutorial_state().best_alt_m,
        view.sim.vessels().len()
    );
    // Orbit is the pending step — an honest "successful launch": the
    // checklist saw VAB, assemble, launch, throttle, climb, space.
    assert_eq!(view.tutorial_state().step, 6);
    assert!(!view.tutorial_state().finished);
    assert!(view.tutorial_state().best_alt_m > 70_000.0);
}

/// Dismiss/reopen plumbing: the overlay hides on request and the
/// checklist resumes with its progress intact.
#[test]
fn tutorial_dismiss_and_resume() {
    let mut view = View::new().expect("view");
    assert!(view.tutorial_active());
    view.set_tutorial_active(false);
    assert!(!view.tutorial_active());
    // A frame with the overlay hidden still renders fine.
    frame(&mut view, 1);
    view.set_tutorial_active(true);
    assert!(view.tutorial_active());
    frame(&mut view, 1);
    assert_eq!(view.tutorial_state().step, 0);
}
