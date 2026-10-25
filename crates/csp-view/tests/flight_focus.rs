//! Focus regression — the shipped bug pinned the chase camera, HUD,
//! and `SetThrottle` routing to `vessels.first()` (the lowest
//! `VesselId`) forever, so a second launch left the player staring at
//! the parked starter craft. The focus rule is the most recently
//! spawned staged vessel.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_view::View;

/// One app frame's §2.5 loop, headlessly.
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
}

/// Two launches through the real `launch_craft` command path: the
/// second spawn must take `flight_focus` — camera, HUD, and the
/// throttle command target all follow the newest `VesselId`.
#[test]
fn second_launch_moves_focus_to_the_new_vessel() {
    let mut view = View::new().expect("data/ load");
    // `seed_starter_craft` is the app's own first launch — it leaves
    // the same craft in the VAB session, so a second `launch_craft`
    // spawns a second vessel through the identical path.
    view.seed_starter_craft().expect("starter seed");
    frame(&mut view, 1);
    let first = view.flight_focus().expect("first spawn focused");

    // The pad is a single berth — the spawn-occupancy gate refuses a
    // launch that would materialize inside the parked craft, so fly
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

    assert_eq!(view.sim.vessels().len(), 2, "second launch did not spawn");
    let newest = *view.sim.vessels().keys().max().expect("vessels present");
    assert_ne!(newest, first, "the second spawn minted a fresh VesselId");
    assert_eq!(
        view.flight_focus(),
        Some(newest),
        "focus must follow the most recently spawned vessel"
    );
}
