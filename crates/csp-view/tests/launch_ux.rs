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
//! - Arrival: the view draws the simulation's own heightfield (one bake,
//!   no sky-only wait at startup or after New Game, Load or Revert), and
//!   the loading screen lifts only onto the whole frame — surface, ocean,
//!   shadows, pad, craft and HUD together, no panel joining afterwards.
//!   The panels' entrance is timed from its own first pass, so the offset
//!   a host's clock carries (startup, New Game) cannot cut it short.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

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

/// What a frame's egui pass drew, flattened in paint order — the
/// topmost shape last.
fn drawn(view: &mut View) -> Vec<egui::Shape> {
    fn flat(shape: &egui::Shape, out: &mut Vec<egui::Shape>) {
        match shape {
            egui::Shape::Vec(inner) => inner.iter().for_each(|s| flat(s, out)),
            s => out.push(s.clone()),
        }
    }
    let mut out = view.take_ui_frame().expect("UiBuild ran");
    let mut leaves = Vec::new();
    for clipped in &out.shapes {
        flat(&clipped.shape, &mut leaves);
    }
    out.textures_delta.clear();
    leaves
}

/// How a frame looks.
#[derive(Debug, PartialEq, Eq)]
enum UiFrame {
    /// The loading screen: its backdrop and its `line` are the last two
    /// shapes — nothing is drawn above them — over the panels built
    /// underneath it.
    Screen { line: String },
    /// The panels uncovered; `texts` is every line the frame drew.
    Panels(Vec<String>),
}

fn ui_frame(view: &mut View) -> UiFrame {
    let leaves = drawn(view);
    ui_frame_of(&leaves, view.egui_context().viewport_rect())
}

fn ui_frame_of(leaves: &[egui::Shape], viewport: egui::Rect) -> UiFrame {
    let texts = |leaves: &[egui::Shape]| -> Vec<String> {
        leaves
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    };
    match leaves
        .len()
        .checked_sub(2)
        .map(|i| (&leaves[i], &leaves[i + 1]))
    {
        Some((egui::Shape::Rect(r), egui::Shape::Text(t)))
            if r.fill == csp_ui::LOADING_BACKDROP =>
        {
            assert_eq!(r.rect, viewport, "the backdrop covers the viewport");
            UiFrame::Screen {
                line: t.galley.text().to_owned(),
            }
        }
        _ => {
            let texts = texts(leaves);
            assert!(
                !texts
                    .iter()
                    .any(|t| t == csp_ui::LOADING_TERRAIN || t == csp_ui::LOADING_SESSION),
                "a loading line without the screen: {texts:?}"
            );
            UiFrame::Panels(texts)
        }
    }
}

/// The panels every flight frame carries — the chrome whose arrival
/// after the scene is what the loading screen exists to prevent.
const PANEL_ANCHORS: [&str; 6] = [
    "Clanker Space Program",
    "First flight",
    "Actions",
    "── ORBIT ",
    "── SURFACE ",
    "── PROP ",
];

/// A frame's panel labels: the lines carrying no digits, so a readout
/// that merely counts is not mistaken for a panel that arrived.
fn labels(texts: &[String]) -> Vec<&str> {
    texts
        .iter()
        .map(String::as_str)
        .filter(|t| !t.bytes().any(|b| b.is_ascii_digit()))
        .collect()
}

/// The frame draws the simulation's own heightfield. The pointer
/// identity is the proof that no second bake ran: a bake of the view's
/// own could not return the simulation's allocation.
fn assert_draws_the_simulations_field(view: &View, packet: &csp_render::flight::FlightPacket) {
    let field = packet
        .terrain
        .as_ref()
        .and_then(|t| t.heightfield.as_ref())
        .expect("the frame draws the displaced surface — no sky-only wait");
    let baked = view
        .sim
        .environment()
        .and_then(|e| e.terrain.as_ref())
        .and_then(|t| t.baked_field())
        .expect("the simulation bakes the field when its backend is built");
    assert!(
        std::sync::Arc::ptr_eq(field, baked),
        "the view must draw the simulation's own field"
    );
}

/// The frame carries the whole flight scene — the displaced surface,
/// the ocean, armed shadows, and the staged geometry: craft and launch
/// complex — with every panel.
fn assert_frame_whole(view: &View, texts: &[String], packet: &csp_render::flight::FlightPacket) {
    let terrain = packet
        .terrain
        .as_ref()
        .expect("the frame builds the terrain pass");
    assert!(
        terrain.heightfield.is_some(),
        "the frame draws the displaced surface — no sky-only wait"
    );
    for anchor in PANEL_ANCHORS {
        assert!(
            texts.iter().any(|t| t.starts_with(anchor)),
            "`{anchor}` missing from the frame: {texts:?}"
        );
    }
    assert!(
        !terrain.terrain.is_empty(),
        "the frame selects terrain leaves"
    );
    assert!(!terrain.ocean.is_empty(), "the frame draws the ocean");
    assert!(
        packet.shadow.armed(),
        "the frame solves the shadow cascades"
    );
    assert!(
        !packet.vertices.is_empty(),
        "the frame stages the craft and the launch complex"
    );
    assert!(
        view.world
            .resource::<csp_view::flight::FlightUiData>()
            .has_vessel,
        "the craft is staged"
    );
}

/// One presentation frame with NO sim advance — the terrain cache still
/// polls its bake while the scene clock stands still.
fn idle_frame(view: &mut View) {
    view.begin_frame();
    view.end_frame();
}

/// Run frames while the loading screen is up and return the first
/// uncovered frame's panel texts with its flight packet.
fn wait_out_the_screen(view: &mut View) -> (Vec<String>, csp_render::flight::FlightPacket) {
    for _ in 0..64 {
        frame(view, 1);
        match ui_frame(view) {
            UiFrame::Screen { .. } => {}
            UiFrame::Panels(texts) => {
                return (texts, view.take_flight_packet().expect("the extract ran"));
            }
        }
    }
    panic!("the loading screen never lifted");
}

/// Run `frames` more frames: none is the loading screen again.
fn assert_no_screen_again(view: &mut View, frames: usize) {
    for _ in 0..frames {
        frame(view, 1);
        view.take_flight_packet();
        assert!(
            matches!(ui_frame(view), UiFrame::Panels(_)),
            "nothing is pending — the screen must not come back"
        );
    }
}

/// Arrival. The view draws the simulation's own heightfield — ONE bake,
/// where the view used to bake a second one on its own thread and draw
/// sky, craft and HUD first and the surface seconds later — so no
/// startup frame waits on a bake. The loading screen covers the frames
/// until the frame is whole: what it lifts onto carries the surface, the
/// ocean, the shadows, the pad, the craft and the HUD together, and no
/// panel joins them afterwards.
#[test]
fn the_first_uncovered_frame_is_the_whole_game() {
    let mut view = View::new().expect("view");
    view.seed_starter_craft().expect("starter seed");
    frame(&mut view, 1);
    let packet = view.take_flight_packet().expect("the first frame's packet");
    assert_eq!(
        ui_frame(&mut view),
        UiFrame::Screen {
            line: csp_ui::LOADING_SESSION.to_owned()
        },
        "startup opens under the loading screen"
    );
    assert_draws_the_simulations_field(&view, &packet);
    let (texts, packet) = wait_out_the_screen(&mut view);
    assert_frame_whole(&view, &texts, &packet);
    assert_draws_the_simulations_field(&view, &packet);
    // Nothing pops in after the first visible frame: the screen never
    // returns, and no panel label joins that was not already there.
    let shown = labels(&texts);
    for _ in 0..30 {
        frame(&mut view, 1);
        view.take_flight_packet();
        match ui_frame(&mut view) {
            UiFrame::Panels(later) => assert!(
                labels(&later).iter().all(|l| shown.contains(l)),
                "a panel joined after the first visible frame: {later:?}"
            ),
            screen => panic!("nothing is pending — no screen: {screen:?}"),
        }
    }
}

/// The screen while the terrain really is pending — a body the
/// simulation has not baked for, so the view's own bake is in flight.
/// Every sky-only frame is the screen (the wait used to be a row inside
/// the HUD chip, drawn over the sky-only scene), and the first frame
/// after the wait is the whole scene with its HUD.
#[test]
fn the_screen_covers_a_terrain_bake() {
    let mut view = View::new().expect("view");
    view.seed_starter_craft().expect("starter seed");
    // A cache with nothing adopted takes its own bake.
    view.world
        .insert_resource(csp_view::terrain::TerrainCache::default());
    frame(&mut view, 1);
    let mut sky_only = 0;
    let (texts, packet) = loop {
        assert!(sky_only < 4_000, "the background bake never landed");
        let ui = ui_frame(&mut view);
        let packet = view.take_flight_packet().expect("the extract ran");
        if packet
            .terrain
            .as_ref()
            .is_some_and(|t| t.heightfield.is_none())
        {
            sky_only += 1;
            assert_eq!(
                ui,
                UiFrame::Screen {
                    line: csp_ui::LOADING_TERRAIN.to_owned()
                },
                "the sky-only frame {sky_only} is the loading screen"
            );
            idle_frame(&mut view);
            std::thread::sleep(Duration::from_millis(5));
        } else {
            // The bake landed: the screen lifts with this frame unless
            // the panels are still arriving under it.
            break match ui {
                UiFrame::Screen { .. } => wait_out_the_screen(&mut view),
                UiFrame::Panels(texts) => (texts, packet),
            };
        }
    };
    assert!(sky_only > 0, "the view never took a bake of its own");
    assert_frame_whole(&view, &texts, &packet);
    assert_no_screen_again(&mut view, 20);
}

/// The world-replacing ops. New Game, Load and Revert all find the
/// simulation's field adopted — none waits on a bake — and none lands on
/// a bare frame: New Game's fresh panels arrive under the screen, Load
/// and Revert restore the world in place onto panels that are warm.
#[test]
fn cold_loads_land_on_the_whole_frame() {
    let mut view = View::new().expect("view");
    view.seed_starter_craft().expect("starter seed");
    wait_out_the_screen(&mut view);
    let save = view.save_game().expect("save");

    view.new_game().expect("new game");
    view.seed_starter_craft().expect("starter seed");
    frame(&mut view, 1);
    let packet = view.take_flight_packet().expect("packet");
    assert!(
        matches!(ui_frame(&mut view), UiFrame::Screen { .. }),
        "a fresh view opens under the loading screen"
    );
    assert_draws_the_simulations_field(&view, &packet);
    let (texts, packet) = wait_out_the_screen(&mut view);
    assert_frame_whole(&view, &texts, &packet);

    view.load_game(save.snapshot).expect("load");
    frame(&mut view, 1);
    let packet = view.take_flight_packet().expect("packet");
    let texts = match ui_frame(&mut view) {
        UiFrame::Panels(texts) => texts,
        screen => panic!("a load onto warm panels opens on them: {screen:?}"),
    };
    assert_frame_whole(&view, &texts, &packet);
    assert_draws_the_simulations_field(&view, &packet);

    view.launch_craft().expect("launch enqueued");
    frame(&mut view, 1);
    view.revert_to_launch().expect("revert");
    frame(&mut view, 1);
    let packet = view.take_flight_packet().expect("packet");
    let texts = match ui_frame(&mut view) {
        UiFrame::Panels(texts) => texts,
        screen => panic!("the reverted world opens on the panels: {screen:?}"),
    };
    assert_frame_whole(&view, &texts, &packet);
    assert_draws_the_simulations_field(&view, &packet);
}

/// The host's half of the screen: raised, the frame is the screen
/// whatever the scene under it holds; lowered, the next frame is the
/// panels with their HUD — they kept building under the screen.
#[test]
fn loading_screen_follows_the_host() {
    let mut view = View::new().expect("view");
    view.seed_starter_craft().expect("starter seed");
    wait_out_the_screen(&mut view);
    view.set_loading(true);
    frame(&mut view, 1);
    view.take_flight_packet();
    assert_eq!(
        ui_frame(&mut view),
        UiFrame::Screen {
            line: csp_ui::LOADING_SESSION.to_owned()
        },
        "the session frame is the loading screen — no HUD above it"
    );
    view.set_loading(false);
    frame(&mut view, 1);
    let packet = view.take_flight_packet().expect("packet");
    match ui_frame(&mut view) {
        UiFrame::Panels(texts) => assert_frame_whole(&view, &texts, &packet),
        screen => panic!("the screen lifted: {screen:?}"),
    }
}

/// One frame as the host stages it (`App::run_frame`): the winit→egui
/// bridge stamps the input with the session time so far — the host's
/// clock, counted from its own epoch — and the window's rect.
fn host_frame(view: &mut View, time: f64) {
    view.set_egui_input(egui::RawInput {
        time: Some(time),
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 720.0),
        )),
        ..Default::default()
    });
    frame(view, 1);
}

/// The alpha the top bar's title was painted at — `None` on the pass
/// that lays it out, which paints nothing. egui fades a new `Area` in
/// by fading its colors, so the title reads how far the entrance has
/// run.
fn title_alpha(leaves: &[egui::Shape]) -> Option<u8> {
    let title = leaves.iter().find_map(|shape| match shape {
        egui::Shape::Text(text) if text.galley.text() == "Clanker Space Program" => Some(text),
        _ => None,
    })?;
    Some(
        title
            .override_text_color
            .unwrap_or(title.fallback_color)
            .a(),
    )
}

/// What a fresh view's frames read, up to the screen's lift.
struct Entrance {
    /// The top bar's title alpha on every covered frame.
    covered: Vec<Option<u8>>,
    /// The title's alpha on the frame the screen lifted.
    lift: Option<u8>,
    /// The title's alpha thirty frames later — the entrance long over.
    settled: Option<u8>,
    /// The lift frame's panel texts.
    texts: Vec<String>,
    /// The lift frame's flight packet.
    packet: csp_render::flight::FlightPacket,
}

/// Run a fresh view's frames under the host's clock — `time` on the
/// first, a 1/60 s step each after — until the loading screen lifts,
/// then thirty more.
fn entrance(view: &mut View, mut time: f64) -> Entrance {
    let step = 1.0 / 60.0;
    let mut covered = Vec::new();
    let (lift, texts, packet) = loop {
        assert!(covered.len() < 64, "the loading screen never lifted");
        host_frame(view, time);
        time += step;
        let leaves = drawn(view);
        let alpha = title_alpha(&leaves);
        match ui_frame_of(&leaves, view.egui_context().viewport_rect()) {
            UiFrame::Screen { .. } => covered.push(alpha),
            UiFrame::Panels(texts) => {
                let packet = view.take_flight_packet().expect("the extract ran");
                break (alpha, texts, packet);
            }
        }
    };
    let mut settled = None;
    for _ in 0..30 {
        host_frame(view, time);
        time += step;
        settled = title_alpha(&drawn(view));
    }
    Entrance {
        covered,
        lift,
        settled,
        texts,
        packet,
    }
}

/// The screen over a fresh view under the host's clock: up for the
/// first pass and the twelve that fade the panels in
/// (`Style::animation_time` is 0.2 s, a 1/60 s step a frame), the lift
/// decided on the frame after — so the top bar is at rest when the
/// screen lifts, and still fading on the third frame, where an entrance
/// counted from the host's epoch lifted.
fn assert_entrance(view: &View, entrance: &Entrance) {
    let settled = entrance
        .settled
        .expect("the title is drawn once the entrance is over");
    assert_eq!(
        entrance.covered.len(),
        13,
        "covered frames — the first pass and twelve of entrance: {:?}",
        entrance.covered
    );
    assert!(
        entrance.covered[1..11]
            .iter()
            .all(|alpha| alpha.is_some_and(|alpha| alpha < settled)),
        "the top bar must still be fading under frames 2 to 11: {:?}",
        entrance.covered
    );
    assert!(
        entrance.covered[2].is_some_and(|alpha| alpha < settled / 2),
        "on the third frame the top bar is more transparent than not: {:?}",
        entrance.covered
    );
    assert_eq!(
        entrance.covered[12],
        Some(settled),
        "the entrance is over on the thirteenth frame: {:?}",
        entrance.covered
    );
    assert_eq!(
        entrance.lift,
        Some(settled),
        "the screen lifted before the top bar was at rest"
    );
    assert_frame_whole(view, &entrance.texts, &entrance.packet);
}

/// The entrance is timed from where it starts, not from the host's
/// clock. A host stamps egui's time from its own epoch — at startup
/// the time spent before the first frame, after New Game the whole
/// session so far, the `View` being new under the one
/// `egui_winit::State` — and a fresh egui context reads 0.0. The first
/// pass's stamp is thus an offset: counted as an age, it ended the
/// entrance on the second pass, and the third showed the HUD, the top
/// bar and the tutorial card fading in over the scene. The first pass
/// is now where the entrance begins, whatever the stamp.
#[test]
fn the_entrance_is_timed_from_its_first_pass() {
    let mut view = View::new().expect("view");
    view.seed_starter_craft().expect("starter seed");
    // Startup, the first frame 100 s after the host's epoch.
    let lift = entrance(&mut view, 100.0);
    assert_entrance(&view, &lift);
    assert_no_screen_again(&mut view, 20);
    let save = view.save_game().expect("save");

    // New Game an hour in: a new View, a new context, the same clock.
    view.new_game().expect("new game");
    view.seed_starter_craft().expect("starter seed");
    let lift = entrance(&mut view, 3_600.0);
    assert_entrance(&view, &lift);

    // Load and Revert keep the context, whose panels are long at rest:
    // no frame is covered, and the first is the whole game's.
    view.load_game(save.snapshot).expect("load");
    let lift = entrance(&mut view, 7_200.0);
    assert!(lift.covered.is_empty(), "a load covers: {:?}", lift.covered);
    assert_frame_whole(&view, &lift.texts, &lift.packet);

    view.launch_craft().expect("launch enqueued");
    host_frame(&mut view, 7_200.0 + 31.0 / 60.0);
    view.revert_to_launch().expect("revert");
    let lift = entrance(&mut view, 7_200.0 + 32.0 / 60.0);
    assert!(
        lift.covered.is_empty(),
        "a revert covers: {:?}",
        lift.covered
    );
    assert_frame_whole(&view, &lift.texts, &lift.packet);
}
