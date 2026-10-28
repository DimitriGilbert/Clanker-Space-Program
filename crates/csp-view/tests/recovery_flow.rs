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
//! - `launch_craft`'s own pad-clearing `RecoverVessel` must not trip
//!   that same edge: a pending `ActiveBuild` is a delivery in flight,
//!   not a dead scene — the player stays in Flight to watch it land.
//!   And the drained-stockpile refusal stays in the VAB with the
//!   reason on the notice row + the global cue channel.

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

/// Seed + step until the starter is surface-latched on the pad —
/// the state a real VAB launch iterates on (the pad-occupant path).
fn deliver_latched(view: &mut View) -> csp_sim::VesselId {
    view.seed_starter_craft().expect("starter seed");
    for _ in 0..80 {
        frame(view, 8);
        if view
            .sim
            .vessels()
            .values()
            .any(|v| matches!(v.state, csp_sim::VesselState::SurfaceLatched(_)))
        {
            break;
        }
    }
    let (id, v) = view.sim.vessels().iter().next().unwrap();
    assert!(
        matches!(v.state, csp_sim::VesselState::SurfaceLatched(_)),
        "starter never latched: {:?}",
        v.state
    );
    *id
}

/// The launch path's own pad-clearing `RecoverVessel` must NOT trip
/// the non-empty→empty arena edge: `LaunchCraft` berths the new
/// craft in the SAME drain the recovery empties it, so the player
/// stays in Flight with the fresh hull in view (the "nothing
/// happened" regression from fe4054a — the bounce fired in the SAME
/// frame the launch switched modes).
#[test]
fn launch_with_parked_build_stays_in_flight() {
    let mut view = View::new().expect("data/ load");
    let seeded = deliver_latched(&mut view);
    assert_eq!(view.flight_focus(), Some(seeded));

    view.launch_craft().expect("launch must place the order");
    // The launch arms RecoverVessel(occupant) + LaunchCraft, then
    // flips to Flight BEFORE the drain — like a real click.
    assert_eq!(view.mode(), ViewMode::Flight, "launch_craft armed Flight");

    // One frame — the drain consumes both commands: the occupant
    // recovers and the new craft berths in the same drain.
    frame(&mut view, 8);

    assert!(view.sim.builds().is_empty(), "a launch mints no build");
    assert_eq!(
        view.sim.vessels().len(),
        1,
        "the new craft berthed in the drain"
    );
    assert_ne!(
        *view.sim.vessels().keys().next().unwrap(),
        seeded,
        "the occupant recovery ran — the berthed hull is the new one"
    );
    assert_eq!(
        view.mode(),
        ViewMode::Flight,
        "the instant berth leaves no empty-arena gap to bounce on"
    );
}

/// The other side of the launch affordance: a stockpile that cannot
/// pay the BOM refuses BEFORE the enqueues/mode switch, keeping the
/// player in the VAB with the reason on the notice row (and the
/// global cue channel) instead of half-launching.
#[test]
fn launch_with_drained_stockpile_refuses_loudly() {
    let mut view = View::new().expect("data/ load");
    let _seeded = deliver_latched(&mut view);
    // Drain the pad's stockpile with real orders (~12 starter BOMs vs
    // the 2000-metal authored stockpile — the pre-refund-fix
    // soft-lock save state). `LaunchCraft` berths instantly, so the
    // drain rides the reservation verb's channel — parallel builds
    // are the sim's own foreign-order path — and every delivered
    // hull is terminated, never recovered (a pad recovery refunds
    // the embodied bill).
    view.launch_craft().expect("first launch");
    frame(&mut view, 8);
    // The launch berths instantly now — its logged `LaunchCraft`
    // declaration is the same `(site, craft, bom)` the foreign
    // orders below spend against.
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
    let mut drained = false;
    for _ in 0..40 {
        view.enqueue_command(
            SourceClass::Ui,
            csp_view::PLAYER_SOURCE_ID,
            SimCommand::ManufactureCraft {
                site: site.clone(),
                craft: craft.clone(),
                build_resources: bom.clone(),
            },
        );
        view.begin_frame();
        let report = view.sim_step(view.sim.clock().ticks() + 8).unwrap();
        view.end_frame();
        // A drain-side "insufficient site inventory" reject is the
        // wall — the order stops debiting past it.
        if !report.rejected.is_empty() {
            drained = true;
            break;
        }
    }
    assert!(drained, "the stockpile never ran dry in 40 orders");
    // Clear the books: every armed order delivers (the berth-defers
    // retry chain spaces them a sim-second apart) into an immediate
    // termination — `builds()` must empty with nothing materialized
    // left so the button's own gates run again.
    for _ in 0..400 {
        frame(&mut view, 64);
        if let Some(&vid) = view.sim.vessels().keys().next() {
            view.enqueue_command(
                SourceClass::Ui,
                csp_view::PLAYER_SOURCE_ID,
                SimCommand::TerminateVessel { vessel: vid },
            );
            frame(&mut view, 8);
        }
        if view.sim.builds().is_empty() && view.sim.vessels().is_empty() {
            break;
        }
    }
    assert!(view.sim.builds().is_empty(), "an order never delivered");
    assert!(view.sim.vessels().is_empty(), "a hull never terminated");

    // The button press now reaches the affordability gates — and
    // refuses with the shortfall named, keeping the player in the
    // VAB. Mirror what `App::handle_ui_outbox` does on `Err` —
    // forward to the global cue channel.
    view.set_mode(ViewMode::Vab);
    let err = match view.launch_craft() {
        Ok(()) => panic!("still drained, must still refuse"),
        Err(reason) => {
            view.flight_notice(reason.to_uppercase());
            reason
        }
    };
    assert!(err.contains("insufficient"), "{err}");
    assert_eq!(view.mode(), ViewMode::Vab, "a refused launch keeps VAB");
    // Drain the staged VabEvent::Notice into the session like Input does.
    view.begin_frame();
    view.end_frame();
    let notice = view.with_vab_session(|v| v.notice().map(str::to_string));
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("insufficient")),
        "the refusal must persist on the VAB notice row: {notice:?}"
    );
    // And the global cue channel carried it too (transient, ~90 frames).
    let cues = &view.world.resource::<csp_view::flight::FlightSfx>().cues;
    assert!(
        cues.iter()
            .any(|c| c.text.to_lowercase().contains("insufficient")),
        "the refusal must reach the global cue channel: {cues:?}"
    );
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
        assert!(delivered, "cycle {cycle}: launch never berthed");
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

/// The stranded-Flight gap the `had_vessels` edge can't cover: a
/// `ManufactureCraft` that REJECTS at its drain while the arena was
/// already empty (`had_vessels == false`) leaves `mode = Flight` with
/// no vessel, no build, and no exit path — `launch_craft` switched
/// modes at enqueue and only a transient "COMMAND REFUSED" cue says
/// why. The drain-reject is now a dead-scene edge of its own: the
/// player lands in the VAB with the sim's reason on the notice row.
/// (`RejectKind::LaunchCraft` matches the same edge — the applies
/// share their gate set; the enqueue here proves the strand directly;
/// a real launch reaches the same reject through a second source
/// racing the drain.)
#[test]
fn rejected_order_on_empty_arena_bounces_to_vab() {
    let mut view = View::new().expect("data/ load");
    // No seed — arena empty from boot. In Flight (the boot default).
    assert_eq!(view.mode(), ViewMode::Flight);
    assert!(view.sim.vessels().is_empty());

    // A ManufactureCraft the apply must refuse — bogus site.
    view.enqueue_command(
        SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        SimCommand::ManufactureCraft {
            site: csp_data::SiteId::from("no-such-site"),
            craft: csp_sim::CraftDef {
                hash_memo: Default::default(),
                graph: csp_phys::graph::PartGraph::new(csp_phys::PartNodeId(0)),
                parts: Vec::new(),
                part_costs: Vec::new(),
                reliability: Vec::new(),
                part_ids: Vec::new(),
                part_build_resources: Vec::new(),
            },
            build_resources: Vec::new(),
        },
    );
    let target = view.sim.clock().ticks() + 8;
    view.begin_frame();
    let r = view.sim_step(target).unwrap();
    view.end_frame();
    assert!(
        !r.rejected.is_empty(),
        "the bogus order must reject at drain"
    );
    assert!(view.sim.builds().is_empty());
    assert!(view.sim.vessels().is_empty());
    assert_eq!(
        view.mode(),
        ViewMode::Vab,
        "an empty-arena reject must still land the player in the VAB"
    );
    // The reason rides the VAB notice row — persistent, not a
    // transient cue the dead scene would have hidden.
    view.begin_frame();
    view.end_frame();
    let notice = view.with_vab_session(|v| v.notice().map(str::to_string));
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("unknown site")),
        "the drain-reject reason must persist on the notice row: {notice:?}"
    );
}

/// The preflight mirror: `apply_manufacture_craft`'s facility gate
/// (`facility.vab` + `facility.launchpad`) is a drain-time reject the
/// button-side checks used to skip — an order enqueued anyway would
/// land the player in Flight with only a reject cue. The launch must
/// refuse at the button with the sim's real reason and keep the
/// player in the VAB.
#[test]
fn launch_preflight_mirrors_the_facility_gate() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");

    // Re-install the tech tree minus its facility grants — the
    // parts stay unlocked (`tech.start` is still researched and the
    // part gate checks `tech_requirement`, not the unlocks table),
    // but `facility.vab`/`facility.launchpad` are no longer granted.
    let mut tree = view.sim.tech_tree().cloned().expect("home tree");
    for node in &mut tree.nodes {
        node.unlocks.facilities.clear();
    }
    view.sim.set_tech_tree(tree);

    view.set_mode(ViewMode::Vab);
    let err = match view.launch_craft() {
        Ok(()) => panic!("a facility-locked build must refuse at the button"),
        Err(e) => e,
    };
    assert!(
        err.contains("facility"),
        "the refusal must name the real gate: {err}"
    );
    assert_eq!(view.mode(), ViewMode::Vab, "a refused launch keeps VAB");
    // No order was armed — the drain never sees it.
    assert!(view.sim.builds().is_empty());
}
