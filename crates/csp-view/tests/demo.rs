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
    let _balance = csp_data::load_balance_from(&s, &r.join("balance.ron")).unwrap();

    let cases: [(&str, Vec<csp_data::ValidationError>); 8] = [
        ("system", csp_data::validate_system(&sys)),
        ("resources", csp_data::validate_resources(&res)),
        ("parts", csp_data::validate_parts(&parts, &res)),
        ("sites", csp_data::validate_sites(&sites, &sys, &res)),
        ("experiments", csp_data::validate_experiments(&experiments)),
        ("techtree", csp_data::validate_techtree(&techtree, &parts)),
        (
            "contracts",
            csp_data::validate_contracts(&contracts, &sys, &experiments, &techtree, &res),
        ),
        ("recipes", csp_data::validate_recipes(&recipes, &res)),
    ];
    for (name, errors) in cases {
        assert!(errors.is_empty(), "demo set: {name} invalid: {errors:?}");
    }
    assert!(
        csp_data::validate_colonies(&colonies, &sites, &res, &recipes).is_empty(),
        "demo set: colonies invalid"
    );
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
        assert_eq!(v.session().len(), 3, "the seeded starter craft");
        assert_eq!(v.session().name(), "Demo Hopper");
    });

    // The seed rides the §7.6 command log — it lands at the first
    // stepped tick like every other spawn.
    view.begin_frame();
    view.sim_step(1).unwrap();
    view.end_frame();
    assert_eq!(view.sim.vessels().len(), 1, "pad craft spawned");
    let vessel = view.sim.vessels().values().next().unwrap();
    assert!(
        vessel.body().is_some(),
        "pad craft is a bubble-resident welded body, got {:?}",
        vessel.state
    );
    // Seeded into the flight view — the demo opens on the pad.
    assert_eq!(view.mode(), ViewMode::Flight);
    assert!(view.flight_focus().is_some());
}
