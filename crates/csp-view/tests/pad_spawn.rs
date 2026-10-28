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
/// stays under 1 m/s from the first tick, the full 5-part stack
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
            5, // pod + tank + engine + canister + battery (W1.5-b)
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

/// The second half of "rests on the pad": the pad must sit where the
/// RENDERED ground is — the sim's contact surface is the datum sphere,
/// but `sites.ron`'s launch site is authored against the baked
/// heightfield (the body `noise_seed`'s terrain is decoration that can
/// sit tens of metres above the datum: the lon-π pad site was under a
/// ~+57 m plateau and the datum spawn rendered the craft buried inside
/// the hill). This pins the authored site's terrain-vs-datum delta so
/// a re-authored coordinate or a seed change that re-buries the pad
/// fails here instead of in a screenshot.
#[test]
fn launch_site_sits_on_rendered_ground() {
    let view = View::new().expect("data/ load");
    let dominant = view.sim.dominant_body().cloned().expect("no dominant body");
    let site = view
        .sim
        .config()
        .sites
        .into_values()
        .find(|s| s.launch && s.body == dominant)
        .expect("dominant body has no launch site");
    let terrain = view.world.resource::<csp_view::terrain::TerrainConfig>();
    let body = terrain
        .bodies
        .get(dominant.as_str())
        .expect("no terrain config for the dominant body");
    // The same 1024×512 equirect bake `terrain::build_packet` ships to
    // the GPU — `sample_dir` is the texel-exact CPU mirror of the
    // vertex shader's `height_at`, so this measures the exact surface
    // the pass draws at the pad.
    let field = csp_render::terrain::HeightField::bake(body.seed, 1024, 512);
    let (sl, cl) = site.latitude_rad.sin_cos();
    let (so, co) = site.longitude_rad.sin_cos();
    let dir = csp_core::DVec3::new(cl * co, cl * so, sl);
    let terrain_m = field.sample_dir(dir) * body.amplitude_m;
    let delta_m = terrain_m - site.altitude_m;
    assert!(
        delta_m.abs() <= 5.0,
        "launch site {:?}: rendered terrain is {terrain_m:+.1} m over \
         the pad's datum altitude {:.1} m — a datum-resting spawn \
         renders {delta_m:+.1} m off the visible ground",
        site.id,
        site.altitude_m,
    );
}

/// Launching while the pad holds a parked build recovers that build
/// (credits its value, frees the pad) instead of dead-refusing — the
/// seeded starter craft is the same working copy the VAB edits, so a
/// launch must not refuse on it.
#[test]
fn launch_recovers_the_parked_build_instead_of_refusing() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    frame(&mut view, 1);
    // The seeded craft is parked surface-latched, occupying the pad.
    let funds0 = view.sim.funds_micros();
    // Launch the same session again — the parked build is recovered
    // (its value credited) and the new build orders, not refused.
    view.launch_craft()
        .expect("launch over a parked build must recover it, not refuse");
    // The parked build is recovered and the new craft berths in the
    // SAME drain — `LaunchCraft` is atomic, so one frame lands the
    // fresh hull on the pad.
    frame(&mut view, 1);
    assert_eq!(
        view.sim.vessels().len(),
        1,
        "the launch berthed the new craft in one drain"
    );
    frame(&mut view, 8);
    // The recovery credited the parked craft's value back.
    let funds1 = view.sim.funds_micros();
    assert!(
        funds1 >= funds0,
        "recovering the parked build should not lose funds ({funds0} -> {funds1})"
    );
    // One vessel on the pad now — the new craft, not two stacked.
    assert!(view.flight_focus().is_some());
}
