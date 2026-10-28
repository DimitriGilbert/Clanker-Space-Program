//! Post-recovery player-flow regressions (the "purple void" +
//! stockpile soft-lock pair):
//!
//! - `RecoverVessel` draining the LAST vessel while in Flight used to
//!   leave a dead purple frame — no focus, `FlightCameraF64.valid =
//!   false`, `extract_flight` emitting only the fallback sky, and no
//!   code path that took the player anywhere. The sim↔view sync now
//!   catches the non-empty → empty arena edge in Flight mode and
//!   lands the player in the VAB, where the working session still
//!   holds the craft (the C24-F1 boot-void precedent, mid-session).
//! - The same recovery now credits the craft's embodied build bill
//!   (`CraftDef::part_build_resources`) back into the site inventory
//!   at the recovery efficiency, so the launch→recover loop is
//!   stockpile-neutral at the pad instead of permanently sinking the
//!   BOM (~11 cycles to a hard "insufficient metal" before).

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_view::{SimCommand, SourceClass, View, ViewMode};

fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).unwrap();
    view.end_frame();
}

/// Seed the starter craft and step until its build order delivers a
/// surface-latched vessel; asserts the seed contract held.
fn deliver_seeded(view: &mut View) -> csp_sim::VesselId {
    view.seed_starter_craft().expect("starter seed");
    for _ in 0..40 {
        frame(view, 8);
        if !view.sim.vessels().is_empty() {
            break;
        }
    }
    frame(view, 8);
    assert!(
        !view.sim.vessels().is_empty(),
        "the starter seed never delivered"
    );
    *view.sim.vessels().keys().next().unwrap()
}

/// The void regression, inverted into its fix's contract: recovering
/// the last vessel in Flight mode transitions the view out of Flight
/// — the empty purple frame is never presented as the resting state.
#[test]
fn recovering_the_last_vessel_leaves_flight_mode() {
    let mut view = View::new().expect("data/ load");
    let vid = deliver_seeded(&mut view);
    assert_eq!(view.mode(), ViewMode::Flight, "the seed opens on Flight");
    assert_eq!(view.flight_focus(), Some(vid));

    view.enqueue_command(
        SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        SimCommand::RecoverVessel {
            vessel: vid,
            site: Some(csp_data::SiteId::from("terrin-launch")),
        },
    );
    frame(&mut view, 8);

    assert!(
        view.sim.vessels().is_empty(),
        "the recovery must drain the arena"
    );
    assert_eq!(
        view.mode(),
        ViewMode::Vab,
        "an empty arena in Flight must transition out — the purple \
         void is a dead scene with no input path"
    );
    // The working session is the point of landing in the VAB: the
    // craft the player just flew is still there, re-launchable.
    assert!(
        view.with_vab_session(|v| !v.session().is_empty()),
        "the VAB must still hold the working craft"
    );
    // The UI pass still produces a frame — the VAB editor chrome,
    // not a dead purple void.
    view.set_egui_input(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    });
    frame(&mut view, 1);
    let mut out = view.take_ui_frame().expect("UiBuild produced a frame");
    // Consume the delta the way `paint` does — a dropped
    // `TexturesDelta` debug-asserts; test harness, not a product bug.
    out.textures_delta.clear();
    assert!(!out.shapes.is_empty());
}

/// The edge must not fire for a world that never held a vessel —
/// boot stays on Flight until the seed delivers (the "just lost"
/// vs "never had one" gate).
#[test]
fn empty_boot_world_stays_in_flight() {
    let mut view = View::new().expect("data/ load");
    // No seed: several frames over an empty arena must NOT trip the
    // transition (the edge requires a non-empty drain, not a level).
    for _ in 0..4 {
        frame(&mut view, 8);
    }
    assert_eq!(view.mode(), ViewMode::Flight);
}

/// The stockpile soft-lock fuse: launch→recover cycles at the pad
/// refund the embodied BOM at efficiency 1.0, so the loop is
/// stockpile-neutral — the pre-fix drain hit a permanent
/// "insufficient metal" refusal around cycle 11. Twelve cycles
/// comfortably proves the wall is gone; the inventory is asserted
/// non-degrading rather than just "no refusal".
#[test]
fn launch_recover_cycles_do_not_drain_the_stockpile() {
    let mut view = View::new().expect("data/ load");
    deliver_seeded(&mut view);
    let site = csp_data::SiteId::from("terrin-launch");
    let inv = |view: &View| {
        view.sim
            .economy()
            .sites
            .get(&site)
            .map(|s| s.inventory.clone())
            .unwrap_or_default()
    };
    let metal0 = inv(&view)
        .get(&csp_data::ResourceId::from("metal"))
        .copied()
        .unwrap_or(0.0);
    let composite0 = inv(&view)
        .get(&csp_data::ResourceId::from("composite"))
        .copied()
        .unwrap_or(0.0);

    for cycle in 0..12 {
        view.set_mode(ViewMode::Vab);
        view.launch_craft()
            .unwrap_or_else(|e| panic!("cycle {cycle}: launch refused: {e}"));
        let mut delivered = false;
        for _ in 0..400 {
            frame(&mut view, 64);
            if !view.sim.vessels().is_empty() {
                delivered = true;
                break;
            }
        }
        assert!(delivered, "cycle {cycle}: build never delivered");
        let vid = *view.sim.vessels().keys().next().unwrap();
        view.enqueue_command(
            SourceClass::Ui,
            csp_view::PLAYER_SOURCE_ID,
            SimCommand::RecoverVessel {
                vessel: vid,
                site: Some(site.clone()),
            },
        );
        frame(&mut view, 8);
        assert!(
            view.sim.vessels().is_empty(),
            "cycle {cycle}: recovery never applied"
        );
    }
    let metal1 = inv(&view)
        .get(&csp_data::ResourceId::from("metal"))
        .copied()
        .unwrap_or(0.0);
    let composite1 = inv(&view)
        .get(&csp_data::ResourceId::from("composite"))
        .copied()
        .unwrap_or(0.0);
    // Pad recovery refunds the whole embodied BOM — the stockpile
    // must not have shrunk past float noise.
    assert!(
        metal1 >= metal0 - 1e-6,
        "metal drained across cycles: {metal0} -> {metal1}"
    );
    assert!(
        composite1 >= composite0 - 1e-6,
        "composite drained across cycles: {composite0} -> {composite1}"
    );
}
