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
//! - The plan is session state: a revert used to wipe it with the
//!   other mirrors and a save never carried it, so after either the
//!   craft answered "SINGLE-STAGE CRAFT". A revert now puts back the
//!   plans its point was captured with, and a load rebuilds them from
//!   the restored crafts.
//! - A plan binds inside its launch window — the ids minted between
//!   its own launch and the next — so a plan nobody bound (a reverted
//!   plan, a flight recovered without a stage press, a launch that
//!   rejected) never claims the next launch of the same craft ahead of
//!   that craft's own plan.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_core::DVec3;
use csp_view::View;
use csp_view::flight::{CanopiesStaged, FlightSfx, StageActions, StagePlan, StagePlans};

/// One app frame's §2.5 loop, headlessly.
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
}

/// Throttle the parked craft into the bubble — the command is what
/// unlatches it, no engine needed. One tick: too few for the §2.6.4
/// settle latch (128 ticks) to reclaim it.
fn unlatch(view: &mut View, vessel: csp_sim::VesselId) {
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::SetThrottle {
            vessel,
            throttle: 1.0,
        },
    );
    frame(view, 1);
    assert!(
        matches!(
            view.sim.vessels()[&vessel].state,
            csp_sim::VesselState::InBubble(_)
        ),
        "throttle did not unlatch the pad craft"
    );
}

/// Save and load through the player's own codec path; the world that
/// comes back is what a saved game reopens as.
fn save_and_load(view: &mut View) {
    let save = view.save_game().expect("save at drained boundary");
    let bytes = csp_view::save::encode_save(&save).expect("encode save");
    let decoded = csp_view::save::decode_save(&bytes).expect("decode save");
    let warnings = view.load_game(decoded.snapshot).expect("load");
    assert!(warnings.is_empty(), "{warnings:?}");
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
    // The starter craft's own plan holds one firing (its chute);
    // replace it with a two-firing plan bound to this vessel — the
    // `vessel` preset skips the lazy bind.
    view.world
        .insert_resource(StagePlans::single(StagePlan::bound_to(
            vec![
                StageActions {
                    jettisons: vec![drop_node],
                    ..StageActions::default()
                },
                StageActions {
                    jettisons: vec![drop_node],
                    ..StageActions::default()
                },
            ],
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
    // `LaunchCraft` is atomic — the second vessel berths at the
    // first drain and takes focus there.
    for _ in 0..200 {
        frame(&mut view, 64);
        if view.sim.vessels().len() > 1 {
            break;
        }
    }
    let focus = view.flight_focus().expect("second spawn focused");
    assert_ne!(first, focus, "the second spawn did not take focus");

    let drop_node = jettisonable(&view, first);
    view.world
        .insert_resource(StagePlans::single(StagePlan::bound_to(
            vec![StageActions {
                jettisons: vec![drop_node],
                ..StageActions::default()
            }],
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
        6, // starter craft: pod + tank + engine + canister + battery + chute
        "the focused vessel lost a node it never owned"
    );
}

/// The KSP staged chute: a parachute assigned to a stage deploys on
/// SPACE — `stage_plan_of` folds the stage's `ModuleDef::Parachute`
/// nodes into the firing's deploy set, and `fire_stage` issues a
/// logged `DeployPart` next to the drop-set jettisons. A stage
/// holding only the chute (no decoupler) is a real firing.
#[test]
fn staged_parachute_deploys_on_stage_press() {
    let mut view = View::new().expect("data/ load");
    // Pod root + a chute snapped to its free 0.625 "top" node
    // (`AttachAuto` finds the only size-class-matched pair), the
    // chute staged at 1 — fires before the stage-0 floor the rest
    // of the craft sits in. Built before the first frame so the
    // progression gate is still the ungated `None` (§3.3 stages the
    // tech mirror on `end_frame`).
    let chute = view.with_vab_session(|v| {
        v.apply(csp_ui::vab::VabAction::SetRoot(csp_data::PartId::from(
            "csp.pod.mk1",
        )))
        .expect("pod root");
        let pod = v.selected().expect("pod placed");
        v.apply(csp_ui::vab::VabAction::AttachAuto {
            part: csp_data::PartId::from("csp.chute.s125"),
            parent: pod,
        })
        .expect("chute attach");
        let chute = v.selected().expect("chute placed");
        v.apply(csp_ui::vab::VabAction::SetStage {
            node: chute,
            stage: 1,
        })
        .expect("stage the chute");
        chute
    });

    view.launch_craft().expect("launch");
    // `LaunchCraft` berth — the vessel materializes surface-latched
    // on the pad at the first drain.
    for _ in 0..200 {
        frame(&mut view, 64);
        if !view.sim.vessels().is_empty() {
            break;
        }
    }
    let (&id, _) = view.sim.vessels().iter().next().unwrap();

    // The launch captured a firing whose deploy set names the chute
    // — the session stage survives the spawn (the graph drops stage
    // metadata, so the plan is the carrier).
    {
        let plans = view.world.resource::<StagePlans>();
        assert!(
            plans
                .plans
                .iter()
                .any(|p| p.stages.iter().any(|s| s.deploys == [chute])),
            "no staged deploy captured for the chute"
        );
    }

    // Throttle unlatches the parked craft — the command is what
    // unlatches, no engine needed. One tick: too few for the §2.6.4
    // settle latch (128 ticks) to reclaim it.
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::SetThrottle {
            vessel: id,
            throttle: 1.0,
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

    assert_eq!(view.fire_stage(), 1, "the staged chute did not fire");
    frame(&mut view, 1);
    assert!(
        view.sim.vessels()[&id].deployed.contains_key(&chute),
        "the chute never deployed"
    );
    // The press says the chute was fired — never that its canopy is
    // open: on the pad the gates already hold, and the canopy starts
    // two ticks later on its own.
    assert!(
        cues(&view).iter().any(|c| c.contains("CHUTE(S) FIRED")),
        "no chute-fired cue landed: {:?}",
        cues(&view)
    );
    assert!(
        !cues(&view).iter().any(|c| c.contains("DEPLOYED")),
        "the stage press must not claim a deploy it cannot see: {:?}",
        cues(&view)
    );
}

/// The chute a craft carries — the node its stage plan's firing and
/// its deploy record both name.
fn chute_node(craft: &csp_sim::CraftDef) -> csp_phys::PartNodeId {
    craft
        .graph
        .nodes
        .iter()
        .find(|(_, node)| {
            craft.parts[node.effective_index as usize]
                .parachute
                .is_some()
        })
        .map(|(id, _)| *id)
        .expect("the craft carries a chute")
}

/// How far the canopy over `vessel`'s chute is staged — `None` while
/// the frame stages none.
fn staged_inflation(
    view: &View,
    vessel: csp_sim::VesselId,
    chute: csp_phys::PartNodeId,
) -> Option<f64> {
    view.world
        .resource::<CanopiesStaged>()
        .0
        .iter()
        .find(|c| c.vessel == vessel.0 && c.node == chute.0)
        .map(|c| c.inflation)
}

/// `vessel`'s altitude over the datum, m.
fn altitude_m(view: &View, vessel: csp_sim::VesselId) -> f64 {
    view.sim
        .telemetry(vessel)
        .altitude_m
        .expect("a craft in the bubble has an altitude")
}

/// Frames of `ticks` until `done` — `false` if `limit` frames pass
/// first.
fn run_until(view: &mut View, ticks: u64, limit: usize, done: impl Fn(&View) -> bool) -> bool {
    for _ in 0..limit {
        if done(view) {
            return true;
        }
        frame(view, ticks);
    }
    done(view)
}

/// A copy of the seeded starter craft in the air `alt_m` over its own
/// pad, rising through the air at `air_mps` (negative: sinking), with
/// its chute's one-firing plan bound to it and the focus on it. No
/// player verb puts a craft where its chute's gates fail, so the
/// scenario spawns one there; the sandbox spawn takes no build bill, so
/// the copy carries none.
fn stock_craft_in_the_air(
    view: &mut View,
    alt_m: f64,
    air_mps: f64,
) -> (csp_sim::VesselId, csp_phys::PartNodeId) {
    view.seed_starter_craft().expect("starter seed");
    for _ in 0..40 {
        frame(view, 64);
        if !view.sim.vessels().is_empty() {
            break;
        }
    }
    // The starter parked on its pad: the craft it is, and where, how
    // and at what speed it stands there.
    let (craft, park) = {
        let (&parked, _) = view
            .sim
            .vessels()
            .iter()
            .next()
            .expect("the starter is spawned");
        let mut craft = view.sim.vessels()[&parked].craft.clone().expect("a craft");
        craft.part_build_resources.clear();
        let park = view
            .world
            .resource::<csp_view::flight::FlightSnapshot>()
            .vessels
            .iter()
            .find(|b| b.id == parked.0)
            .expect("the parked starter is staged")
            .clone();
        (craft, park)
    };
    let body_center = view.sim.environment().expect("physics world").body_center;
    let up = (park.com - body_center) / (park.com - body_center).norm();
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::SpawnPhysicalVessel {
            craft: craft.clone(),
            pos: park.pos + up * alt_m,
            rot: park.rot,
            // The pad's own speed through the air is nothing: its
            // velocity is the ground's.
            vel: park.vel + up * air_mps,
            ang_vel: DVec3::ZERO,
        },
    );
    view.begin_frame();
    let spawn = view
        .sim_step(view.sim.clock().ticks() + 1)
        .expect("sim step");
    assert!(
        spawn.rejected.is_empty(),
        "the spawn was refused: {:?}",
        spawn.rejected
    );
    view.end_frame();
    for _ in 0..8 {
        frame(view, 1);
    }
    let (&id, _) = view
        .sim
        .vessels()
        .iter()
        .find(|(_, v)| v.body().is_some())
        .expect("the copy flies in the bubble");
    assert_eq!(
        view.flight_focus(),
        Some(id),
        "the flying copy takes the focus"
    );
    let chute = chute_node(&craft);
    view.world
        .insert_resource(StagePlans::single(StagePlan::bound_to(
            vec![StageActions {
                deploys: vec![chute],
                ..StageActions::default()
            }],
            id,
        )));
    (id, chute)
}

/// Ticks between the state's clock and `opened` — where the canopy's
/// ramp stands.
fn ticks_past(view: &View, opened: u64) -> u64 {
    view.sim.clock().ticks() - opened
}

/// The visible parachute, a chute fired above its gates. SPACE fires
/// the chute's stage, and what the player sees of it from there is the
/// simulation's own doing — the view shows exactly that and says so
/// when it opens:
///
/// - Over the altitude gate the press fires the chute and nothing is
///   staged over the craft, tick after tick; the cue says the chute was
///   FIRED, never that its canopy is open.
/// - The first tick the simulation applies the canopy is the first the
///   view stages it — two past the catch's stamp, two ninety-sixths
///   inflated — and `CANOPY OPEN` lands that frame.
/// - The staged inflation is the simulation's ramp: ninety-six ticks of
///   it to a whole canopy, 700 m² read as a 29.9 m disc.
#[test]
fn a_staged_chute_shows_its_canopy_from_the_tick_the_simulation_opens_it() {
    let mut view = View::new().expect("data/ load");
    let (id, chute) = stock_craft_in_the_air(&mut view, 17_000.0, 0.0);

    // Over the gate: the stage press fires the chute, and no canopy is
    // staged over the craft while the simulation holds none.
    assert_eq!(view.fire_stage(), 1, "the chute's stage did not fire");
    frame(&mut view, 1);
    let record = view.sim.vessels()[&id].deployed[&chute];
    assert_eq!(record.opened_tick, None, "17 km is over the altitude gate");
    assert!(
        cues(&view).iter().any(|c| c == "STAGED — 1 CHUTE(S) FIRED"),
        "{:?}",
        cues(&view)
    );
    for _ in 0..64 {
        frame(&mut view, 1);
    }
    assert_eq!(
        staged_inflation(&view, id, chute),
        None,
        "packed: no canopy is staged"
    );
    assert!(
        cues(&view).iter().all(|c| !c.contains("CANOPY")),
        "{:?}",
        cues(&view)
    );

    // The fall reaches the gate; the first tick the simulation applies
    // the canopy is the first the view stages it.
    assert!(
        run_until(&mut view, 64, 60, |v| altitude_m(v, id) < 16_300.0),
        "the craft never fell to its gate"
    );
    assert!(
        run_until(&mut view, 1, 400, |v| staged_inflation(v, id, chute)
            .is_some()),
        "the canopy never opened"
    );
    let opened = view.sim.vessels()[&id].deployed[&chute]
        .opened_tick
        .expect("caught");
    assert_eq!(ticks_past(&view, opened), 2, "two ticks past the stamp");
    assert_eq!(staged_inflation(&view, id, chute), Some(2.0 / 96.0));
    assert_eq!(
        cues(&view).iter().filter(|c| *c == "CANOPY OPEN").count(),
        1,
        "CANOPY OPEN lands the frame the canopy starts: {:?}",
        cues(&view)
    );

    // The staged inflation is the ramp, to a whole canopy and no
    // further.
    for (ticks, k) in [
        (1, 3),
        (7, 10),
        (40, 50),
        (45, 95),
        (1, 96),
        (1, 97),
        (100, 197),
    ] {
        frame(&mut view, ticks);
        assert_eq!(ticks_past(&view, opened), k);
        assert_eq!(
            staged_inflation(&view, id, chute),
            Some((k as f64 / 96.0).min(1.0)),
            "{k} ticks past the stamp"
        );
    }
    let canopy = view
        .world
        .resource::<CanopiesStaged>()
        .0
        .iter()
        .find(|c| c.vessel == id.0)
        .expect("the whole canopy is staged");
    assert_eq!(canopy.drag_area_m2, 700.0);
    assert!((canopy.radius_m() - (700.0 / core::f64::consts::PI).sqrt()).abs() < 1e-9);
    assert_eq!(
        cues(&view).iter().filter(|c| *c == "CANOPY OPEN").count(),
        1
    );
}

/// The visible parachute, a canopy back past its gate. Fired while the
/// craft climbs at 420 m/s from 15.2 km, the canopy is whole while the
/// craft is still under the gate — and the climb carries it through:
/// from that tick nothing is staged, the record's anchor untouched;
/// whole again, not re-inflated, the tick the craft falls back under
/// it — and said again, as any canopy appearing over the craft is.
#[test]
fn a_canopy_back_past_its_gate_is_not_staged_and_is_whole_when_it_holds() {
    let mut view = View::new().expect("data/ load");
    let (id, chute) = stock_craft_in_the_air(&mut view, 15_200.0, 420.0);

    assert_eq!(view.fire_stage(), 1, "the chute's stage did not fire");
    assert!(
        run_until(&mut view, 1, 8, |v| staged_inflation(v, id, chute)
            .is_some()),
        "the gates hold: the canopy must open at once"
    );
    let opened = view.sim.vessels()[&id].deployed[&chute]
        .opened_tick
        .expect("caught at once");
    assert_eq!(ticks_past(&view, opened), 2);
    assert_eq!(staged_inflation(&view, id, chute), Some(2.0 / 96.0));
    assert_eq!(
        cues(&view).iter().filter(|c| *c == "CANOPY OPEN").count(),
        1,
        "{:?}",
        cues(&view)
    );

    // Whole a ramp time on, still under the gate.
    frame(&mut view, 94);
    assert_eq!(ticks_past(&view, opened), 96);
    assert_eq!(staged_inflation(&view, id, chute), Some(1.0));
    assert!(altitude_m(&view, id) < 16_000.0, "still under the gate");

    // The climb carries the craft up through the gate: no canopy is
    // staged from then, and the simulation keeps the anchor.
    assert!(
        run_until(&mut view, 8, 400, |v| altitude_m(v, id) > 16_000.0),
        "the craft never rose through the gate: {}",
        altitude_m(&view, id)
    );
    assert_eq!(
        staged_inflation(&view, id, chute),
        None,
        "over the gate no canopy is staged at {} m",
        altitude_m(&view, id)
    );
    assert_eq!(
        view.sim.vessels()[&id].deployed[&chute].opened_tick,
        Some(opened),
        "the anchor is the simulation's to keep"
    );

    let said = cues(&view).iter().filter(|c| *c == "CANOPY OPEN").count();

    // The fall brings it back under the gate: whole at once, and said
    // again — any canopy appearing over the craft is.
    assert!(
        run_until(&mut view, 64, 240, |v| altitude_m(v, id) < 15_990.0),
        "the craft never fell back under the gate"
    );
    assert_eq!(
        staged_inflation(&view, id, chute),
        Some(1.0),
        "whole again the tick the flow holds"
    );
    assert_eq!(
        cues(&view).iter().filter(|c| *c == "CANOPY OPEN").count(),
        said + 1,
        "{:?}",
        cues(&view)
    );
}

/// The shipped starter flies with its chute (M4c landing): the seed
/// places `csp.chute.s125` on the pod's free top node — along the
/// stack, where its pull is axial — in stage 1, so the launch captures
/// a one-firing plan whose deploy set is the chute and nothing else,
/// and SPACE after liftoff deploys it — the press coming after the
/// engine has burned, when the craft's tank mass no longer matches its
/// launch-time fingerprint.
#[test]
fn starter_craft_fires_its_chute_from_its_own_stage() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    let (pod, chute) = view.with_vab_session(|v| {
        assert_eq!(
            v.session().len(),
            6,
            "the starter craft: pod, canister, battery, tank, engine, chute"
        );
        let node_of = |id: &str| {
            v.session()
                .iter()
                .find(|(_, n)| n.built().part_id.as_str() == id)
                .expect("starter part")
                .0
        };
        (node_of("csp.pod.mk1"), node_of("csp.chute.s125"))
    });
    view.with_vab_session(|v| {
        let edge = v.session().node(chute).expect("chute placed").edge();
        let edge = edge.expect("the chute is attached");
        assert_eq!(edge.parent, pod, "the chute rides the pod");
        assert_eq!(
            (edge.parent_node.as_str(), edge.child_node.as_str()),
            ("top", "mount"),
            "on the pod's free top node"
        );
        assert_eq!(
            v.session().stage_of(chute),
            Some(1),
            "the chute is its own stage"
        );
    });
    frame(&mut view, 1);

    {
        let plans = view.world.resource::<StagePlans>();
        assert_eq!(plans.plans.len(), 1, "{:?}", plans.plans.len());
        let stages = &plans.plans[0].stages;
        assert_eq!(stages.len(), 1, "one firing: the chute");
        assert_eq!(stages[0].deploys, [chute]);
        assert!(stages[0].jettisons.is_empty());
    }

    // Throttle to liftoff — the engine burns, and the tank's mass
    // moves with its fuel bucket — then the staged chute fires: the
    // plan binds on its first press against a craft that no longer
    // weighs what it did at the launch.
    let id = view.flight_focus().expect("first spawn focused");
    view.enqueue_command(
        csp_view::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_view::SimCommand::SetThrottle {
            vessel: id,
            throttle: 1.0,
        },
    );
    frame(&mut view, 1);
    assert_eq!(
        view.fire_stage(),
        1,
        "the starter's chute stage did not fire"
    );
    frame(&mut view, 1);
    assert!(
        view.sim.vessels()[&id].deployed.contains_key(&chute),
        "SPACE did not deploy the starter's chute"
    );
}

/// The node the starter craft's chute occupies — the node the launch
/// plan and the vessel's deploy record both name.
fn starter_chute(view: &mut View) -> csp_phys::PartNodeId {
    view.with_vab_session(|v| {
        v.session()
            .iter()
            .find(|(_, n)| n.built().part_id.as_str() == "csp.chute.s125")
            .expect("the starter carries a chute")
            .0
    })
}

/// The nodes of [`pod_with_chute_and_drop_stage`] that its stage plans
/// name.
struct StackNodes {
    decoupler: csp_phys::PartNodeId,
    tank: csp_phys::PartNodeId,
    engine: csp_phys::PartNodeId,
    chute: csp_phys::PartNodeId,
}

/// A pod with a chute on its top node and, under it, a decoupler with
/// a tank and an engine below, every part in stage 0. Built before the
/// first frame so the progression gate is still the ungated `None`
/// (§3.3).
fn pod_with_chute_and_drop_stage(view: &mut View) -> StackNodes {
    let (decoupler, tank, engine, chute) = view.with_vab_session(|v| {
        use csp_ui::vab::VabAction;
        let part = csp_data::PartId::from;
        let mut place = |action: VabAction, what: &str| {
            v.apply(action).expect(what);
            v.selected().expect(what)
        };
        let pod = place(VabAction::SetRoot(part("csp.pod.mk1")), "pod root");
        let chute = place(
            VabAction::AttachAuto {
                part: part("csp.chute.s125"),
                parent: pod,
            },
            "chute attach",
        );
        let decoupler = place(
            VabAction::AttachAuto {
                part: part("csp.decoupler.s125"),
                parent: pod,
            },
            "decoupler attach",
        );
        let tank = place(
            VabAction::AttachAuto {
                part: part("csp.tank.s160"),
                parent: decoupler,
            },
            "tank attach",
        );
        let engine = place(
            VabAction::AttachAuto {
                part: part("csp.engine.sparc"),
                parent: tank,
            },
            "engine attach",
        );
        (decoupler, tank, engine, chute)
    });
    StackNodes {
        decoupler,
        tank,
        engine,
        chute,
    }
}

/// The one craft in the arena.
fn only_vessel(view: &View) -> csp_sim::VesselId {
    let mut vessels: Vec<csp_sim::VesselId> = view.sim.vessels().keys().copied().collect();
    assert_eq!(vessels.len(), 1, "exactly one craft, got {vessels:?}");
    vessels.pop().expect("one vessel")
}

/// [`pod_with_chute_and_drop_stage`] as a two-stage craft: the chute on
/// its own stage above the decoupler's drop stage — two firings, the
/// chute's, then the drop's.
fn two_stage_craft(view: &mut View) -> StackNodes {
    let nodes = pod_with_chute_and_drop_stage(view);
    view.with_vab_session(|v| {
        v.apply(csp_ui::vab::VabAction::SetStage {
            node: nodes.chute,
            stage: 1,
        })
        .expect("stage the chute");
    });
    nodes
}

/// Restage the drop above the chute — the decoupler to stage 2, so the
/// stack leaves first and the chute deploys second. The reverse of the
/// order [`two_stage_craft`] launches with: a press that answers with
/// the launch-time order is a stale plan speaking.
fn drop_stage_first(view: &mut View, nodes: &StackNodes) {
    view.with_vab_session(|v| {
        v.apply(csp_ui::vab::VabAction::SetStage {
            node: nodes.decoupler,
            stage: 2,
        })
        .expect("restage the decoupler");
    });
}

/// SPACE on `vessel` fires the restaged order: the drop stage leaves —
/// two nodes acted on — and the chute stays armed until the press
/// after. `stale` names the plan that answered in its place.
fn fires_drop_stage_first(
    view: &mut View,
    vessel: csp_sim::VesselId,
    nodes: &StackNodes,
    stale: &str,
) {
    unlatch(view, vessel);
    let fired = view.fire_stage();
    assert_eq!(
        fired,
        2,
        "the restaged plan did not fire (SPACE returned {fired}) — {stale}: {:?}",
        cues(view)
    );
    frame(view, 1);
    let craft = &view.sim.vessels()[&vessel];
    let graph = &craft.craft.as_ref().expect("craft").graph;
    assert!(
        !graph.nodes.contains_key(&nodes.tank) && !graph.nodes.contains_key(&nodes.engine),
        "the restaged drop stage left its subtree on the craft: {:?}",
        graph.nodes.keys().collect::<Vec<_>>()
    );
    assert!(
        !craft.deployed.contains_key(&nodes.chute),
        "the chute deployed on the first press — the launch-time order answered ({stale})"
    );
    assert_eq!(
        view.fire_stage(),
        1,
        "the chute's stage did not fire second"
    );
    frame(view, 1);
    assert!(
        view.sim.vessels()[&vessel]
            .deployed
            .contains_key(&nodes.chute),
        "SPACE did not deploy the chute after the drop"
    );
}

/// Revert-to-launch restores the world the launch left — and now the
/// stage plan the launch captured with it. The craft comes back on the
/// pad with its chute armed and its chute stage whole, so the retry
/// stages exactly as the first flight did instead of answering
/// "SINGLE-STAGE CRAFT — NOTHING TO STAGE".
#[test]
fn revert_to_launch_keeps_the_stage_the_craft_launched_with() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    let chute = starter_chute(&mut view);
    frame(&mut view, 1);
    let id = only_vessel(&view);

    // The first flight: liftoff, then SPACE fires the chute's stage.
    unlatch(&mut view, id);
    assert_eq!(view.fire_stage(), 1, "the first flight staged the chute");
    frame(&mut view, 1);
    assert!(view.sim.vessels()[&id].deployed.contains_key(&chute));

    // Revert: the craft is on the pad with the chute armed again, and
    // the plan is the launch's own — one firing, the chute, unspent.
    view.revert_to_launch().expect("revert");
    assert!(
        !view.sim.vessels()[&id].deployed.contains_key(&chute),
        "the chute is armed again"
    );
    let plans = view.world.resource::<StagePlans>();
    assert_eq!(plans.plans.len(), 1, "one craft, one plan");
    assert_eq!(plans.plans[0].total, 1);
    assert_eq!(plans.plans[0].stages.len(), 1);
    assert_eq!(
        plans.plans[0].stages[0],
        StageActions {
            jettisons: vec![],
            deploys: vec![chute],
        },
        "the launch plan is whole again, not spent"
    );

    // The same stage fires: the retry deploys the chute as the first
    // flight did.
    unlatch(&mut view, id);
    assert_eq!(
        view.fire_stage(),
        1,
        "the chute stage did not fire after the revert"
    );
    frame(&mut view, 1);
    assert!(
        view.sim.vessels()[&id].deployed.contains_key(&chute),
        "SPACE did not deploy the starter's chute after the revert"
    );
}

/// A save stores the sim, not the stage plans: a loaded craft used to
/// answer "SINGLE-STAGE CRAFT". The plan is rebuilt from the restored
/// craft — the starter's chute is its deploy stage — and a chute the
/// save shows already deployed is not offered a second time.
#[test]
fn loading_a_save_rebuilds_the_chute_stage() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    let chute = starter_chute(&mut view);
    frame(&mut view, 1);
    let id = only_vessel(&view);

    // Save the craft latched on the pad, then reopen that save.
    save_and_load(&mut view);
    let plans = view.world.resource::<StagePlans>();
    assert_eq!(plans.plans.len(), 1, "one restored craft, one plan");
    assert_eq!(plans.plans[0].vessel, Some(id), "bound to its own vessel");
    assert_eq!(plans.plans[0].total, 1);
    assert_eq!(
        plans.plans[0].stages,
        vec![StageActions {
            jettisons: vec![],
            deploys: vec![chute],
        }],
        "the restored craft's chute is a deploy stage"
    );

    unlatch(&mut view, id);
    assert_eq!(
        view.fire_stage(),
        1,
        "the loaded craft's chute stage did not fire"
    );
    frame(&mut view, 1);
    assert!(view.sim.vessels()[&id].deployed.contains_key(&chute));

    // Saved again with the chute out: the next load offers nothing.
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::SetThrottle {
            vessel: id,
            throttle: 0.0,
        },
    );
    frame(&mut view, 256);
    save_and_load(&mut view);
    assert!(
        view.world.resource::<StagePlans>().plans.is_empty(),
        "a chute already deployed stages nothing"
    );
    assert_eq!(view.fire_stage(), 0, "nothing to stage after the load");
    assert!(
        cues(&view).iter().any(|c| c.contains("SINGLE-STAGE CRAFT")),
        "no single-stage cue landed: {:?}",
        cues(&view)
    );
}

/// The drop half of a rebuilt plan: a craft saved with a decoupler
/// holding a tank-and-engine subtree comes back with that subtree as a
/// drop stage ahead of the chute's deploy stage, and SPACE fires the
/// two in that order.
#[test]
fn loading_a_save_rebuilds_the_drop_stage() {
    let mut view = View::new().expect("data/ load");
    let StackNodes {
        tank,
        engine,
        chute,
        ..
    } = pod_with_chute_and_drop_stage(&mut view);

    view.launch_craft().expect("launch");
    frame(&mut view, 1);
    let id = only_vessel(&view);

    save_and_load(&mut view);
    let plans = view.world.resource::<StagePlans>();
    assert_eq!(plans.plans.len(), 1);
    assert_eq!(
        plans.plans[0].stages,
        vec![
            StageActions {
                jettisons: vec![tank, engine],
                deploys: vec![],
            },
            StageActions {
                jettisons: vec![],
                deploys: vec![chute],
            },
        ],
        "the decoupler's subtree drops, then the chute deploys"
    );

    unlatch(&mut view, id);
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::SetThrottle {
            vessel: id,
            throttle: 0.0,
        },
    );
    frame(&mut view, 1);

    // The first press drops the tank and engine — two nodes acted on;
    // the pod and the chute it carries stay. The second deploys the chute.
    assert_eq!(
        view.fire_stage(),
        2,
        "the drop stage did not fire its two parts"
    );
    frame(&mut view, 1);
    let graph = &view.sim.vessels()[&id].craft.as_ref().expect("craft").graph;
    assert!(
        !graph.nodes.contains_key(&tank) && !graph.nodes.contains_key(&engine),
        "the drop stage left its subtree on the craft: {:?}",
        graph.nodes.keys().collect::<Vec<_>>()
    );
    assert!(graph.nodes.contains_key(&chute), "the drop took the chute");
    assert_eq!(view.fire_stage(), 1, "the deploy stage did not fire");
    frame(&mut view, 1);
    assert!(
        view.sim.vessels()[&id].deployed.contains_key(&chute),
        "the chute never deployed"
    );
}

/// A reverted plan must not shadow the next launch's staging. The
/// revert point used to keep the launch's plan unbound (its vessel's id
/// is not known until the spawn lands) and a revert put that plan back
/// into the world its own craft still occupies. The next launch then
/// recovered that craft and berthed the restaged one — same parts, so
/// the same launch key — and the old plan bound to it first: SPACE
/// fired the pre-revert staging and the restaged craft's own plan was
/// never reached. The revert point now holds its plans bound to the
/// craft they were captured with.
#[test]
fn restaged_relaunch_fires_its_new_stage_set_not_the_reverted_plan() {
    let mut view = View::new().expect("data/ load");
    let nodes = two_stage_craft(&mut view);

    view.launch_craft().expect("launch");
    frame(&mut view, 1);
    let first = only_vessel(&view);

    // The first flight stages once; the revert brings the craft back to
    // the pad with the plan the launch captured, whole.
    unlatch(&mut view, first);
    assert_eq!(view.fire_stage(), 1, "the first flight staged the chute");
    frame(&mut view, 1);
    view.revert_to_launch().expect("revert");
    assert_eq!(
        only_vessel(&view),
        first,
        "the revert restores the same craft"
    );
    {
        let plans = view.world.resource::<StagePlans>();
        assert_eq!(plans.plans.len(), 1, "one craft, one plan");
        assert_eq!(
            plans.plans[0].vessel,
            Some(first),
            "the restored plan is bound to the craft it was captured with"
        );
        assert_eq!(plans.plans[0].stages.len(), 2, "the plan is whole again");
    }

    drop_stage_first(&mut view, &nodes);
    view.launch_craft().expect("relaunch");
    // The parked craft is recovered and the restaged one berths in the
    // same drain under a new id.
    frame(&mut view, 1);
    let second = only_vessel(&view);
    assert_ne!(second, first, "the relaunch minted a new vessel");

    fires_drop_stage_first(&mut view, second, &nodes, "the pre-revert plan answered");
}

/// A plan nobody ever bound must not shadow the next launch either.
/// Only a stage press binds a plan, so a flight that staged nothing
/// leaves its plan unbound; recovering that craft removes its vessel,
/// and the next launch of the same craft berths one the old plan could
/// claim — ahead of the new plan. A plan now binds only inside its
/// launch window: the ids minted between its own launch and the next.
#[test]
fn relaunch_after_an_unstaged_flight_fires_the_new_stage_set() {
    let mut view = View::new().expect("data/ load");
    let nodes = two_stage_craft(&mut view);

    view.launch_craft().expect("launch");
    frame(&mut view, 1);
    let first = only_vessel(&view);

    // Fly without staging and settle back on the pad.
    unlatch(&mut view, first);
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::SetThrottle {
            vessel: first,
            throttle: 0.0,
        },
    );
    frame(&mut view, 256);
    let state = &view.sim.vessels()[&first].state;
    assert!(
        matches!(state, csp_sim::VesselState::SurfaceLatched(_)),
        "the craft did not settle back on the pad: {state:?}"
    );
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::RecoverVessel {
            vessel: first,
            site: Some(csp_data::SiteId::from("terrin-launch")),
        },
    );
    frame(&mut view, 1);
    assert!(
        view.sim.vessels().is_empty(),
        "the recovery left the craft in the arena"
    );
    {
        let plans = view.world.resource::<StagePlans>();
        assert_eq!(plans.plans.len(), 1, "the flight's plan stays in the table");
        assert_eq!(
            plans.plans[0].vessel, None,
            "no stage was pressed — nothing bound the plan"
        );
    }

    drop_stage_first(&mut view, &nodes);
    view.launch_craft().expect("relaunch");
    frame(&mut view, 1);
    let second = only_vessel(&view);
    assert_ne!(second, first, "the relaunch minted a new vessel");

    fires_drop_stage_first(
        &mut view,
        second,
        &nodes,
        "the recovered flight's plan answered",
    );
    let plans = view.world.resource::<StagePlans>();
    assert_eq!(
        plans.plans[0].vessel, None,
        "the recovered flight's plan bound to the relaunch"
    );
    assert_eq!(
        plans.plans[1].vessel,
        Some(second),
        "the relaunch's own plan"
    );
}

/// A launch that rejects at the drain leaves its plan behind: the plan
/// is pushed when the command enqueues, and no vessel ever lands to
/// bind it. That orphan used to claim the next launch of the same
/// craft. Here the second press comes before the first craft berths, so
/// the pre-flight sees a free pad — and the order rejects behind it.
#[test]
fn relaunch_after_a_rejected_launch_fires_the_new_stage_set() {
    let mut view = View::new().expect("data/ load");
    let nodes = two_stage_craft(&mut view);

    view.launch_craft().expect("first launch");
    view.launch_craft().expect("second launch enqueues");
    view.begin_frame();
    let report = view
        .sim_step(view.sim.clock().ticks() + 1)
        .expect("sim step");
    view.end_frame();
    assert!(
        !report.rejected.is_empty(),
        "the second launch must reject at the drain"
    );
    let first = only_vessel(&view);
    {
        let plans = view.world.resource::<StagePlans>();
        assert_eq!(
            plans.plans.len(),
            2,
            "the rejected launch's plan stays in the table"
        );
    }

    drop_stage_first(&mut view, &nodes);
    view.launch_craft().expect("relaunch");
    frame(&mut view, 1);
    let second = only_vessel(&view);
    assert_ne!(second, first, "the relaunch minted a new vessel");

    fires_drop_stage_first(
        &mut view,
        second,
        &nodes,
        "a plan from before the restage answered",
    );
    // The relaunch recovered the first craft, so both earlier plans
    // are orphans here: neither may own the new vessel.
    let plans = view.world.resource::<StagePlans>();
    assert_eq!(
        plans
            .plans
            .iter()
            .filter(|p| p.vessel == Some(second))
            .count(),
        1,
        "exactly the relaunch's own plan owns the new vessel: {:?}",
        plans.plans.iter().map(|p| p.vessel).collect::<Vec<_>>()
    );
    assert_eq!(
        plans.plans[2].vessel,
        Some(second),
        "the relaunch's own plan"
    );
}

/// A retired plan must not come back. When its vessel has left the
/// arena the gone-vessel branch of `fire_stage` retires a plan — but it
/// cleared the bound id as well, leaving the plan free to bind again:
/// the next launch of the same craft was claimed by a plan with
/// nothing left to fire, and SPACE answered "ALL STAGES FIRED" for a
/// craft that had never staged. A retired plan stays inside its launch
/// window like any other.
#[test]
fn relaunch_after_a_retired_plan_stages_normally() {
    let mut view = View::new().expect("data/ load");
    let nodes = two_stage_craft(&mut view);

    view.launch_craft().expect("launch");
    frame(&mut view, 1);
    let first = only_vessel(&view);

    // Stage once — the press binds the plan to this craft — then lose
    // the craft and press again: the plan retires.
    unlatch(&mut view, first);
    assert_eq!(view.fire_stage(), 1, "the flight staged the chute");
    frame(&mut view, 1);
    view.enqueue_command(
        csp_sim::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::TerminateVessel { vessel: first },
    );
    frame(&mut view, 1);
    assert!(
        view.sim.vessels().is_empty(),
        "the terminated craft left something in the arena"
    );
    assert_eq!(view.fire_stage(), 0, "a craft-less press staged something");
    {
        let plans = view.world.resource::<StagePlans>();
        assert!(
            plans.plans[0].stages.is_empty() && plans.plans[0].vessel.is_none(),
            "the plan did not retire: {:?}",
            plans.plans[0].vessel
        );
        assert!(
            cues(&view)
                .iter()
                .any(|c| c.contains("LAUNCHED CRAFT GONE")),
            "no retirement cue landed: {:?}",
            cues(&view)
        );
    }

    drop_stage_first(&mut view, &nodes);
    view.launch_craft().expect("relaunch");
    frame(&mut view, 1);
    let second = only_vessel(&view);
    assert_ne!(second, first, "the relaunch minted a new vessel");

    fires_drop_stage_first(&mut view, second, &nodes, "the retired plan answered");
    let plans = view.world.resource::<StagePlans>();
    assert_eq!(
        plans.plans[0].vessel, None,
        "the retired plan bound to the relaunch"
    );
    assert_eq!(
        plans.plans[1].vessel,
        Some(second),
        "the relaunch's own plan"
    );
}
