//! Pad-spawn regression — the shipped bug was a starter craft that
//! spawned 12 m in the air with the inertial co-rotation velocity,
//! free-fell, broke its stack on touchdown, and read as "~175 m/s
//! sliding" on the HUD. The spawn must land the craft *resting* on
//! the pad: surface-relative speed ~0, no part loss, settle → latch,
//! and still rendered/focusable (a throttle command unlatches it).
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

/// The seeded starter craft rests on the pad: surface-relative speed
/// stays under 1 m/s from the first tick, the full 3-part stack
/// survives (no jettison, no debris), and within a few seconds it
/// settles onto §2.6.4 surface rails — where the flight HUD keeps it
/// visible and reads `latched`.
#[test]
fn starter_craft_rests_on_the_pad() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    let env = view.sim.environment().unwrap().clone();
    let mut latched = false;
    // 10 s of sim — the pre-fix build shed two parts in the first 2 s.
    for s in 0..10 {
        frame(&mut view, 64);
        assert_eq!(
            view.sim.vessels().len(),
            1,
            "t={}s: vessel set changed",
            s + 1
        );
        let (id, vessel) = view.sim.vessels().iter().next().unwrap();
        assert!(
            vessel.debris_since.is_none(),
            "t={}s: starter craft became debris",
            s + 1
        );
        assert_eq!(
            vessel.craft.as_ref().unwrap().graph.nodes.len(),
            3,
            "t={}s: a part was lost — the touchdown broke the stack",
            s + 1
        );
        match &vessel.state {
            csp_sim::VesselState::InBubble(b) => {
                let rel = b.com_world() - env.body_center;
                let v_rel = b.vel - env.body_spin.cross(rel);
                assert!(
                    v_rel.norm() < 1.0,
                    "t={}s: surface-relative speed {} m/s — the craft is sliding",
                    s + 1,
                    v_rel.norm()
                );
            }
            csp_sim::VesselState::SurfaceLatched(_) => latched = true,
            csp_sim::VesselState::OnRails(_) => panic!("pad craft went to rails"),
        }
        let _ = id;
    }
    assert!(
        latched,
        "the resting craft never settled onto surface rails"
    );
    // The HUD still sees the parked craft — `flight_focus` stays
    // `Some` so the throttle path (Z → unlatch + spool) works.
    let ui = view.world.resource::<csp_view::flight::FlightUiData>();
    assert!(
        ui.has_vessel && ui.latched,
        "latched pad craft left the HUD"
    );
    assert!(ui.surface_speed_mps < 1.0);
    assert!(view.flight_focus().is_some());
    // A throttle command on the latched craft unlatches it (§2.6.4).
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        1,
        csp_sim::SimCommand::SetThrottle {
            vessel: view.flight_focus().unwrap(),
            throttle: 1.0,
        },
    );
    frame(&mut view, 1);
    let vessel = view.sim.vessels().values().next().unwrap();
    assert!(
        matches!(vessel.state, csp_sim::VesselState::InBubble(_)),
        "throttle did not unlatch the pad craft"
    );
}
