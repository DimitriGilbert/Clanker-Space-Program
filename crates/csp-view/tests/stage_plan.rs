//! Staging regressions — the shipped bug class "control does nothing".
//!
//! - `JettisonPart` is `InBubble`-only in v1, but `fire_stage` used to
//!   pop the plan's front drop set unconditionally: on a pad-latched
//!   craft the queued commands all rejected and the stage was eaten —
//!   a silent no-op that permanently spent the firing. The pop now
//!   waits for a bubble-resident target and every refusal lands a HUD
//!   cue (`FlightSfx` — the visible in-flight feedback channel).
//! - The plan used to fire at `flight_focus` — the newest staged
//!   vessel — so a second launch retargeted the first craft's stage
//!   plan onto the wrong graph. The plan binds to the vessel its
//!   launch spawned (`StagePlan::bound_to`/`vessel_for`), not focus.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_view::View;
use csp_view::flight::{FlightSfx, StagePlan, StagePlans};

/// One app frame's §2.5 loop, headlessly.
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
}

/// A non-root node of `vessel`'s craft graph — the node a jettison can
/// legally drop.
fn jettisonable(view: &View, id: csp_sim::VesselId) -> csp_phys::PartNodeId {
    let g = &view.sim.vessels()[&id].craft.as_ref().unwrap().graph;
    *g.nodes
        .keys()
        .find(|n| **n != g.root)
        .expect("craft has a non-root node")
}

/// The live cue texts (the HUD's §3.7 channel).
fn cues(view: &View) -> Vec<String> {
    view.world
        .resource::<FlightSfx>()
        .cues
        .iter()
        .map(|c| c.text.clone())
        .collect()
}

/// Space on a parked craft must not eat the stage: the plan keeps its
/// firings and a cue tells the player why. After liftoff the same
/// press fires for real.
#[test]
fn stage_press_on_a_parked_craft_refuses_instead_of_eating_the_stage() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    // ~10 s of sim settles the pad spawn onto §2.6.4 surface rails.
    for _ in 0..10 {
        frame(&mut view, 64);
    }
    let (&id, _) = view.sim.vessels().iter().next().unwrap();
    assert!(
        matches!(
            view.sim.vessels()[&id].state,
            csp_sim::VesselState::SurfaceLatched(_)
        ),
        "craft never latched"
    );
    let drop_node = jettisonable(&view, id);
    // The starter craft is single-stage (no decouplers), so the launch
    // plan was empty; install a two-firing plan bound to this vessel —
    // `vessel` preset skips the lazy fingerprint bind.
    view.world
        .insert_resource(StagePlans::single(StagePlan::bound_to(
            vec![vec![drop_node], vec![drop_node]],
            id,
        )));

    assert_eq!(view.fire_stage(), 0, "a latched craft staged anyway");
    assert_eq!(
        view.world.resource::<StagePlans>().plans[0].stages.len(),
        2,
        "the refused press ate a stage"
    );
    assert!(
        cues(&view).iter().any(|c| c.contains("NOT STAGED")),
        "no refusal cue landed: {:?}",
        cues(&view)
    );

    // §2.6.4: a throttle command unlatches the parked craft — then the
    // same press stages for real.
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        1,
        csp_sim::SimCommand::SetThrottle {
            vessel: id,
            throttle: 1.0,
        },
    );
    frame(&mut view, 1);
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        1,
        csp_sim::SimCommand::SetThrottle {
            vessel: id,
            throttle: 0.0,
        },
    );
    frame(&mut view, 1);
    assert!(
        matches!(
            view.sim.vessels()[&id].state,
            csp_sim::VesselState::InBubble(_)
        ),
        "throttle did not unlatch the pad craft"
    );

    assert_eq!(view.fire_stage(), 1, "an in-bubble press did not fire");
    frame(&mut view, 1);
    assert!(
        !view.sim.vessels()[&id]
            .craft
            .as_ref()
            .unwrap()
            .graph
            .nodes
            .contains_key(&drop_node),
        "the jettisoned node survived"
    );
    assert!(
        cues(&view).iter().any(|c| c.contains("STAGED")),
        "no firing cue landed: {:?}",
        cues(&view)
    );
}

/// The plan targets the vessel it was bound to — not the flight focus.
/// A second launch takes focus; firing still drops a node off the
/// FIRST craft.
#[test]
fn stage_plan_fires_its_own_vessel_not_the_focus() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    frame(&mut view, 1);
    let first = view.flight_focus().expect("first spawn focused");
    // `seed_starter_craft` leaves the same craft in the VAB session —
    // a second `launch_craft` spawns a second vessel and takes focus.
    // The pad is a single berth (the spawn-occupancy gate refuses a
    // launch that would materialize inside the parked craft), so fly
    // the first vessel clear before launching again.
    view.enqueue_command(
        csp_view::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_view::SimCommand::SetThrottle {
            vessel: first,
            throttle: 1.0,
        },
    );
    for _ in 0..8 {
        frame(&mut view, 64); // ~8 s — well clear of the pad volume
    }
    view.launch_craft().expect("second launch");
    frame(&mut view, 1);
    let focus = view.flight_focus().expect("second spawn focused");
    assert_ne!(first, focus, "the second spawn did not take focus");

    let drop_node = jettisonable(&view, first);
    view.world
        .insert_resource(StagePlans::single(StagePlan::bound_to(
            vec![vec![drop_node]],
            first,
        )));

    assert_eq!(view.fire_stage(), 1);
    frame(&mut view, 1);
    assert!(
        !view.sim.vessels()[&first]
            .craft
            .as_ref()
            .unwrap()
            .graph
            .nodes
            .contains_key(&drop_node),
        "the bound vessel kept its jettisoned node"
    );
    assert_eq!(
        view.sim.vessels()[&focus]
            .craft
            .as_ref()
            .unwrap()
            .graph
            .nodes
            .len(),
        5, // starter craft: pod + tank + engine + canister + battery
        "the focused vessel lost a node it never owned"
    );
}
