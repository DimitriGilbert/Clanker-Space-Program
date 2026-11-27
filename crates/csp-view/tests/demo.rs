//! §6.3 web-demo tests (M10-C): the embedded data set loads and
//! validates, the fixed scenario builds natively through
//! `View::new_demo` (the same constructor the wasm `View::new`
//! delegates to), and the demo constraints are wired in.
//!
//! Everything exercised is target-agnostic code — the file compiles
//! for wasm too, it just only *runs* under `cargo test` natively.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use csp_view::demo;
use csp_view::{View, ViewMode};

fn src() -> csp_data::EmbeddedSource {
    demo::source()
}

fn root() -> &'static Path {
    Path::new(demo::DEMO_ROOT)
}

/// The embedded set parses AND validates: same loader + same
/// §3.0/§3.1 validator entry points the desktop `data/` gates run,
/// driven over the embedded source.
#[test]
fn demo_dataset_loads_and_validates() {
    let s = src();
    let r = root();
    let sys = csp_data::load_system_from(&s, &r.join("systems/home")).unwrap();
    let res = csp_data::load_resources_from(&s, &r.join("resources.ron")).unwrap();
    let parts = csp_data::load_parts_from(&s, &r.join("parts")).unwrap();
    let sites = csp_data::load_sites_from(&s, &r.join("systems/home/sites.ron")).unwrap();
    let experiments = csp_data::load_experiments_from(&s, &r.join("experiments")).unwrap();
    let techtree = csp_data::load_techtree_from(&s, &r.join("techtree")).unwrap();
    let contracts = csp_data::load_contracts_from(&s, &r.join("contracts")).unwrap();
    let recipes = csp_data::load_recipes_from(&s, &r.join("recipes")).unwrap();
    let colonies = csp_data::load_colonies_from(&s, &r.join("systems/home/colonies.ron")).unwrap();
    let balance = csp_data::load_balance_from(&s, &r.join("balance.ron")).unwrap();

    let cases: [(&str, Vec<csp_data::ValidationError>); 9] = [
        ("system", csp_data::validate_system(&sys)),
        ("resources", csp_data::validate_resources(&res)),
        (
            "parts",
            csp_data::validate_parts(&parts, &res, &recipes, &experiments),
        ),
        ("sites", csp_data::validate_sites(&sites, &sys, &res)),
        (
            "experiments",
            csp_data::validate_experiments(&experiments, &sys),
        ),
        ("techtree", csp_data::validate_techtree(&techtree, &parts)),
        (
            "contracts",
            csp_data::validate_contracts(&contracts, &sys, &experiments, &techtree, &res, &sites),
        ),
        ("recipes", csp_data::validate_recipes(&recipes, &res)),
        ("balance", csp_data::validate_balance(&balance, &res)),
    ];
    for (name, errors) in cases {
        assert!(errors.is_empty(), "demo set: {name} invalid: {errors:?}");
    }
    assert!(
        csp_data::validate_colonies(&colonies, &sites, &res, &recipes).is_empty(),
        "demo set: colonies invalid"
    );

    // The demo ships its own reduced loc table — every LocKey the
    // embedded set references must resolve in it (same gate family
    // the desktop tests run against data/loc/en.ron).
    let loc = csp_data::load_loc_from(&s, &r.join("loc/en.ron")).unwrap();
    assert!(
        csp_data::validate_loc(&loc).is_empty(),
        "demo set: loc/en.ron invalid"
    );
    let loc_cases: [(&str, Vec<csp_data::ValidationError>); 8] = [
        ("system loc", csp_data::validate_loc_keys(&sys, &loc)),
        ("site loc", csp_data::validate_site_loc_keys(&sites, &loc)),
        (
            "part loc",
            csp_data::validate_part_loc_keys(&parts, &res, &loc),
        ),
        (
            "experiment loc",
            csp_data::validate_experiment_loc_keys(&experiments, &loc),
        ),
        (
            "techtree loc",
            csp_data::validate_techtree_loc_keys(&techtree, &loc),
        ),
        (
            "contract loc",
            csp_data::validate_contract_loc_keys(&contracts, &loc),
        ),
        (
            "recipe loc",
            csp_data::validate_recipe_loc_keys(&recipes, &loc),
        ),
        (
            "colony loc",
            csp_data::validate_colony_loc_keys(&colonies, &loc),
        ),
    ];
    for (name, errors) in loc_cases {
        assert!(errors.is_empty(), "demo set: {name} invalid: {errors:?}");
    }
}

/// §6.3's "Terrin only" — the demo system's manifest carries exactly
/// solen + terrin; no moon, no second planet, nothing interplanetary
/// exists to reach.
#[test]
fn demo_system_is_terrin_only() {
    let sys = csp_data::load_system_from(&src(), &root().join("systems/home")).unwrap();
    let ids: Vec<&str> = sys.bodies.iter().map(|b| b.def.id.as_str()).collect();
    assert_eq!(ids, ["solen", "terrin"]);
    // Sites are Terrin-scoped too — the one Rask site is gone.
    let sites = csp_data::load_sites_from(&src(), &root().join("systems/home/sites.ron")).unwrap();
    for site in &sites.sites {
        assert_eq!(site.body.as_str(), "terrin", "off-Terrin site {site:?}");
    }
}

/// The fixed scenario builds: the demo flag is set, the Terrin physics
/// environment is installed, the VAB session carries the starter craft
/// under the §6.3 constraints, the orbit model knows only the demo
/// bodies, and the seeded spawn lands a vessel on the pad on the first
/// stepped tick.
#[test]
fn new_demo_builds_scenario_and_enforces_constraints() {
    let mut view = View::new_demo().unwrap();
    assert!(view.is_demo());
    assert_eq!(
        view.sim.dominant_body().map(|b| b.as_str()),
        Some("terrin"),
        "the demo bubble is Terrin's"
    );
    assert!(
        view.sim.environment().is_some(),
        "the demo world has a physics bubble"
    );
    let body_ids: Vec<&str> = view
        .sim
        .orbits()
        .bodies()
        .keys()
        .map(|b| b.as_str())
        .collect();
    assert_eq!(body_ids, ["solen", "terrin"]);

    // The seeded session craft + the §6.3 limits on it.
    view.with_vab_session(|v| {
        assert_eq!(v.part_limit(), Some(demo::DEMO_PART_LIMIT));
        assert!(!v.file_io());
        // W1.5-b: pod + tank + engine + canister + battery — the
        // science leg ships on the starter — and the chute on the
        // pod's top node (M4c landing).
        assert_eq!(v.session().len(), 6, "the seeded starter craft");
        assert_eq!(v.session().name(), "Demo Hopper");
    });

    // The seed is a `LaunchCraft` — `seed_pad_craft` steps the sim
    // through the command's drain at construction, so the starter
    // craft is already berthed surface-latched (the
    // pad's berth) when the scenario opens; a throttle unlatches it.
    assert_eq!(view.sim.vessels().len(), 1, "pad craft delivered");
    let vessel = view.sim.vessels().values().next().unwrap();
    assert!(
        matches!(vessel.state, csp_sim::VesselState::SurfaceLatched(_)),
        "pad craft delivered surface-latched, got {:?}",
        vessel.state
    );
    // A frame stages the latched craft into the flight view.
    view.begin_frame();
    view.sim_step(view.sim.clock().ticks() + 1).unwrap();
    view.end_frame();
    // Seeded into the flight view — the demo opens on the pad.
    assert_eq!(view.mode(), ViewMode::Flight);
    assert!(view.flight_focus().is_some());
}

/// Hold `vessel`'s body +z (thrust axis) on local radial-up — the
/// same scripted-attitude convention the tutorial flight leg uses.
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
        csp_view::SourceClass::Ui,
        1,
        csp_view::SimCommand::SetAttitude {
            vessel,
            rot,
            ang_vel: csp_core::DVec3::ZERO,
        },
    );
}

/// The seeded "Demo Hopper" is flyable — the M10-D AttachAuto
/// regression end-to-end on the §6.3 data set: the pod→tank→engine
/// stack must mount its engine nozzle-down, so throttle-up accelerates
/// the craft *away* from the pad instead of thrusting into it.
#[test]
fn demo_seed_craft_ascends() {
    let mut view = View::new_demo().unwrap();
    // The seed is already berthed surface-latched (the `LaunchCraft`
    // drain at construction) — a frame
    // stages it, then the throttle command unlatches it.
    view.begin_frame();
    view.sim_step(view.sim.clock().ticks() + 1).unwrap();
    view.end_frame();
    let vessel = view.flight_focus().expect("pad craft delivered");
    let env = view.sim.environment().unwrap().clone();
    let pad_alt = match &view.sim.vessels()[&vessel].state {
        csp_sim::VesselState::SurfaceLatched(p) => p.altitude_m,
        s => panic!("pad craft not latched: {s:?}"),
    };

    view.enqueue_command(
        csp_view::SourceClass::Ui,
        1,
        csp_view::SimCommand::SetThrottle {
            vessel,
            throttle: 1.0,
        },
    );
    let mut best_alt = pad_alt;
    let mut radial_vel = 0.0;
    for _ in 0..10 {
        hold_radial_up(&mut view, vessel);
        view.begin_frame();
        let target = view.sim.clock().ticks() + 64;
        view.sim_step(target).unwrap();
        view.end_frame();
        let Some(body) = view.sim.vessels().get(&vessel).and_then(|v| v.body()) else {
            break;
        };
        let rel = body.com_world() - env.body_center;
        best_alt = best_alt.max(rel.norm() - env.radius_m);
        radial_vel = body.vel.dot(rel / rel.norm());
    }
    assert!(
        best_alt > pad_alt + 50.0 && radial_vel > 0.0,
        "Demo Hopper did not climb (best altitude {best_alt:.1} m from \
         {pad_alt:.1} m, radial velocity {radial_vel:.1} m/s) — the \
         engine must mount nozzle-down"
    );
}
