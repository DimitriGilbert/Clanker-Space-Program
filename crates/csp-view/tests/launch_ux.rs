//! Launch-UX regressions — the "build in flight" player experience
//! that used to read as a dead scene (the launch_ux_trace scratch's
//! findings, pinned as contracts):
//!
//! - The tutorial's `vessel_lost` rewind must NOT fire while a
//!   `ManufactureCraft` order rides — an empty arena with a pending
//!   reservation is a delivery in flight, and "vessel lost — LAUNCH
//!   again" was the wrong coaching for it. `LaunchCraft` itself
//!   berths instantly, so the pending order here is the colony/
//!   foreign-order path the surface still exists for.
//! - A second LAUNCH while an order is pending refuses loudly (the
//!   button used to arm a second build — double-charge, and a
//!   delivery queued behind the single berth).
//! - `CancelBuild` on an empty arena in Flight is a dead-scene edge
//!   of its own — the builds-edge bounce lands the player in the
//!   VAB.
//! - The empty flight scene keeps its camera while a build rides
//!   (the pad stays drawn — terrain + launch complex — instead of
//!   the fallback void), and `FlightUiData.builds` carries the
//!   delivery countdown the HUD names.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_view::flight::FlightSfx;
use csp_view::{SimCommand, SourceClass, View, ViewMode};

/// One rendered frame advancing `ticks` sim ticks — the app's
/// begin/sim_step/end contract headlessly.
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
}

/// A real session build — pod + tank + engine, the tutorial's
/// flyable stack.
fn build_craft(view: &mut View) {
    view.with_vab_session(|v| {
        use csp_ui::vab::VabAction;
        v.apply(VabAction::New).expect("new");
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
        v.recompute();
    });
}

/// Seed, launch through the real path (`LaunchCraft` — the craft
/// berths in one drain), then re-create the "empty arena, order in
/// flight" state the pending-build surfaces exercise: terminate the
/// berthed craft and place a colony/agent-style `ManufactureCraft`
/// reservation in the same drain. `LaunchCraft` never mints a build,
/// so a pending order can now only come from the reservation verb —
/// which is exactly what stays real (colony builds, MCP/bench
/// orders). Returns the delivery deadline.
fn launch_then_foreign_build(view: &mut View) -> u64 {
    view.seed_starter_craft().expect("starter seed");
    frame(view, 1);
    build_craft(view);
    frame(view, 1);
    view.launch_craft().expect("launch enqueued");
    assert_eq!(view.mode(), ViewMode::Flight);
    frame(view, 1);
    assert_eq!(
        view.sim.vessels().len(),
        1,
        "the launch berthed its craft in one drain"
    );
    // The launch's own declaration rides the log verbatim — reuse it
    // as a foreign reservation's inputs so the order passes the
    // same bill check the launch did.
    let (site, craft, bom) = view
        .sim
        .command_log()
        .iter()
        .rev()
        .find_map(|(_, c)| match c {
            SimCommand::LaunchCraft {
                site,
                craft,
                build_resources,
            } => Some((site.clone(), craft.clone(), build_resources.clone())),
            _ => None,
        })
        .expect("the launch logged a LaunchCraft");
    let berthed = *view.sim.vessels().keys().next().expect("the berthed craft");
    view.enqueue_command(
        SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        SimCommand::TerminateVessel { vessel: berthed },
    );
    view.enqueue_command(
        SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        SimCommand::ManufactureCraft {
            site,
            craft,
            build_resources: bom,
        },
    );
    frame(view, 1);
    assert_eq!(view.sim.builds().len(), 1, "the reservation is in flight");
    assert!(
        view.sim.vessels().is_empty(),
        "the berthed craft is gone — the arena is empty mid-build"
    );
    view.sim.builds().values().next().unwrap().complete_tick
}

/// The tutorial's `vessel_lost` rewind must not fire for the whole
/// build — the empty arena is the delivery the player is waiting
/// on, not a lost vessel. (Pre-fix this banner showed for the
/// entire ~118 s order: the trace's headline defect.)
#[test]
fn pending_build_suppresses_vessel_lost() {
    let mut view = View::new().expect("view");
    let complete = launch_then_foreign_build(&mut view);

    // Mid-build samples across the wait — every frame must hold the
    // checklist off the loss rewind and keep the launch step's
    // credit (the order IS the launch).
    let mut min_step = usize::MAX;
    while view.sim.clock().ticks() < complete {
        frame(&mut view, 64);
        let t = view.tutorial_state();
        assert!(
            !t.vessel_lost,
            "vessel_lost fired mid-build at step {}",
            t.step
        );
        min_step = min_step.min(t.step);
    }
    assert!(
        min_step > csp_ui::tutorial::STEP_LAUNCH,
        "the checklist rewound to launch during the build (min step {min_step})"
    );
    // Delivery lands: the checklist is untouched, the craft takes
    // focus, still no loss banner.
    frame(&mut view, 8);
    assert_eq!(view.sim.vessels().len(), 1, "the delivery materialized");
    assert!(view.flight_focus().is_some());
    assert!(!view.tutorial_state().vessel_lost);
}

/// A second LAUNCH press while the order rides must refuse with the
/// in-flight reason — no second `ManufactureCraft`, no double
/// charge. The reason lands on the VAB notice row (persistent) and
/// the caller's `Err` (the app forwards it to the cue channel).
#[test]
fn second_launch_during_build_refuses_loudly() {
    let mut view = View::new().expect("view");
    launch_then_foreign_build(&mut view);
    let funds = view.sim.funds_micros();

    let err = match view.launch_craft() {
        Ok(()) => panic!("a second launch mid-build must refuse"),
        Err(e) => e,
    };
    assert!(
        err.contains("already in flight"),
        "the refusal must name the in-flight order: {err}"
    );
    frame(&mut view, 1);
    assert_eq!(
        view.sim.builds().len(),
        1,
        "no second order was armed: {:?}",
        view.sim.builds()
    );
    assert_eq!(
        view.sim.funds_micros(),
        funds,
        "the refused press charged nothing"
    );
}

/// The builds-edge of the dead-scene bounce: `CancelBuild` drops
/// the last order with the arena empty — nothing was removed, so
/// the `had_vessels` edge can't see it. The player in Flight must
/// land back in the VAB instead of stranding on the empty scene.
#[test]
fn cancel_build_on_empty_arena_bounces_to_vab() {
    let mut view = View::new().expect("view");
    launch_then_foreign_build(&mut view);
    assert_eq!(view.mode(), ViewMode::Flight);

    let build = *view.sim.builds().keys().next().unwrap();
    view.enqueue_command(
        SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        SimCommand::CancelBuild { build },
    );
    frame(&mut view, 1);

    assert!(view.sim.builds().is_empty(), "the cancel drained");
    assert!(view.sim.vessels().is_empty());
    assert_eq!(
        view.mode(),
        ViewMode::Vab,
        "the cancelled wait must not strand the player in Flight"
    );
    // And the tutorial's honest judgment lands the same frame: the
    // order's exit IS a loss to re-guide from.
    assert!(view.tutorial_state().vessel_lost);
}

/// The persistent build status the HUD's `!has_vessel` branch
/// draws: `FlightUiData.builds` mirrors the staged FACILITIES rows
/// — the order's id, label, and live countdown — for the whole
/// wait, long after the placement toast faded.
#[test]
fn flight_ui_mirrors_the_pending_build() {
    let mut view = View::new().expect("view");
    launch_then_foreign_build(&mut view);

    frame(&mut view, 8);
    let ui = view.world.resource::<csp_view::flight::FlightUiData>();
    assert!(!ui.has_vessel, "the arena is empty mid-build");
    let b = ui
        .builds
        .first()
        .expect("the build row must mirror into FlightUiData");
    assert!(b.label.contains("part craft"), "label: {}", b.label);
    assert!(b.eta_s > 0.0, "the countdown is live: {}", b.eta_s);
}

/// The camera hold end-to-end: while the order rides, the solved
/// chase pose survives (`FlightCameraF64.valid` stays true), so
/// `extract_flight`'s terrain gate stays open — the pad and launch
/// complex keep drawing instead of the fallback void gradient.
#[test]
fn pending_build_keeps_the_pad_camera() {
    let mut view = View::new().expect("view");
    launch_then_foreign_build(&mut view);

    frame(&mut view, 8);
    assert!(
        view.world
            .resource::<csp_view::flight::FlightCameraF64>()
            .0
            .valid,
        "the last chase pose must hold while the build rides"
    );
    let packet = view
        .take_flight_packet()
        .expect("the extract emitted a flight packet");
    assert!(
        packet.terrain.is_some(),
        "the terrain pass must keep drawing the pad — the empty \
         arena mid-build is a delivery in flight, not the void"
    );
    // Sanity the other way: no order → the invalid camera path is
    // untouched (covered by the cancel bounce + the unit test in
    // flight.rs).
}

/// The boot seed's launch cue is honest: `LaunchCraft` berths in one
/// drain, so the scene opens on "CRAFT ON THE PAD" — accurate the
/// moment it appears — and never a stale build-order countdown.
#[test]
fn seed_leaves_no_stale_build_toast() {
    let mut view = View::new().expect("view");
    view.seed_starter_craft().expect("starter seed");
    let cues = view.world.resource::<FlightSfx>();
    assert!(
        cues.cues.iter().all(|c| !c.text.contains("BUILD ORDER")),
        "no build-order toast may survive into the scene: {:?}",
        cues.cues.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert!(
        cues.cues
            .iter()
            .any(|c| c.text.contains("CRAFT ON THE PAD")),
        "the instant-berth cue is the seed's honest launch note: {:?}",
        cues.cues.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
}
