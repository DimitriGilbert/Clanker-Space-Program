//! Plume regression — the reported bug drew the exhaust plume
//! whenever `throttle > 0`, even with the tanks dry and zero
//! delivered thrust: the staged emitter list carried every Thrust
//! site regardless of the sim's fuel-coverage scaling, and the
//! `throttle` command survives the dry-out (the `SetThrottle` gate
//! only clamps *new* commands on an all-starved vessel). The staging
//! now carries only sites the sim still rates (`rated_n =
//! base·factor·coverage > 0`), so a dry-but-throttled engine stages
//! no emitter and draws no plume.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_core::DVec3;
use csp_view::{View, art::ArtAssets, flight};

/// One frame's §2.5 loop, headlessly — commands enqueued between
/// `sim_step` calls drain at the next step's tick.
fn step(view: &mut View, ticks: u64) {
    view.sim_step(view.sim.clock().ticks() + ticks)
        .expect("sim step");
}

/// A single-part fueled-engine hopper: one liquid_fuel tank plus one
/// Thrust site with a matching `EngineSpec` — the same shape
/// csp-sim's own `fueled_engine_craft` coverage fixture uses. `cap`
/// units of propellant spawn aboard; the engine's full-throttle draw
/// per tick is `thrust·dt/(isp·g₀·mpu)` units.
fn hopper_craft(cap: f64, mpu: f64, thrust_n: f64, isp: f64) -> csp_sim::CraftDef {
    let fuel = csp_data::ResourceId::from("liquid_fuel");
    let mass = 500.0 + cap * mpu;
    let half = DVec3::new(0.5, 0.5, 0.5);
    let mut graph = csp_phys::PartGraph::new(csp_phys::PartNodeId(0));
    graph.insert(
        csp_phys::PartNodeId(0),
        csp_phys::PartNode::new(None, csp_phys::PartTransform::IDENTITY, 0),
    );
    csp_sim::CraftDef {
        part_build_resources: Vec::new(),
        hash_memo: Default::default(),
        part_ids: vec![],
        graph,
        parts: vec![csp_phys::WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: Some(csp_phys::EngineSpec {
                isp_vac_s: isp,
                propellants: vec![(fuel.clone(), 1.0)],
            }),
            parachute: None,
            dock_ports: vec![],
            tanks: vec![csp_phys::TankSpec {
                resource: fuel,
                capacity: cap,
                mass_per_unit_kg: mpu,
            }],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: mass,
            com: DVec3::ZERO,
            inertia: csp_phys::SymMat3::solid_box(mass, half),
            collider: csp_phys::ColliderShape::Box { half_extents: half },
            force_sites: vec![csp_phys::ForceSite::new(
                DVec3::new(0.0, 0.0, -half.z),
                DVec3::Z,
                csp_phys::ForceSiteKind::Thrust,
                thrust_n,
            )],
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: csp_data::ThermalDef {
                heat_capacity_j_per_k: 400_000.0,
                max_temp_k: 1_100.0,
                emissivity: 0.8,
                conduction_w_per_k: 60.0,
            },
        }],
        part_costs: vec![0.0],
        reliability: vec![],
    }
}

/// Spawn the hopper onto a 200 km dipping trajectory — the auto
/// bubble→rails edge keeps it `InBubble` only while the engine
/// runs, and even then the 0.6-circular dipper is the honest shape:
/// a clearing conic would rail the moment the burn ended. Throttle
/// to full, burn the tank dry mid-run — the exact bug shape: the
/// command stays open after the engine starves. The staged `plumes`
/// must go from one emitter (delivered ≈ 1, full coverage) to NONE
/// once the tanks read zero — while `throttle` still reports the
/// held command.
#[test]
fn dry_engine_stages_no_plume() {
    let mut view = View::new().expect("data/ load");
    let (body_center, radius_m, mu) = {
        let env = view.sim.environment().expect("data world has physics");
        (env.body_center, env.radius_m, env.mu_m3_per_s2)
    };
    let r = radius_m + 200_000.0;
    let pos = body_center + DVec3::new(r, 0.0, 0.0);
    // 0.6-circular: a dipping conic — the auto-rails edge only fires
    // on a conic clearing the surface/atmosphere floor, and the
    // vessel stays staged for the whole (short) run.
    let vel = DVec3::new(0.0, 0.6 * (mu / r).sqrt(), 0.0);
    // 0.1 u of fuel ≈ a few seconds at 200 N — it drains mid-run.
    view.enqueue_command(
        csp_view::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::SpawnPhysicalVessel {
            craft: hopper_craft(0.1, 5.0, 200.0, 300.0),
            pos,
            rot: csp_core::DQuat::IDENTITY,
            vel,
            ang_vel: DVec3::ZERO,
        },
    );
    step(&mut view, 1);
    let id = *view.sim.vessels().keys().next_back().expect("spawned");
    view.enqueue_command(
        csp_view::SourceClass::Ui,
        csp_view::PLAYER_SOURCE_ID,
        csp_sim::SimCommand::SetThrottle {
            vessel: id,
            throttle: 1.0,
        },
    );
    step(&mut view, 1);

    // Fueled and throttled: one emitter at full delivery.
    let staged = flight::capture_sync_input(
        &view.sim,
        &mut flight::LatchedWeldCache::default(),
        &ArtAssets::default(),
        &[],
    );
    let b = staged
        .vessels
        .iter()
        .find(|b| b.id == id.0)
        .expect("vessel staged");
    assert!(b.throttle > 0.0);
    assert_eq!(b.plumes.len(), 1, "a live engine stages one emitter");
    assert!(
        (b.plumes[0].2 - 1.0).abs() < 1e-9,
        "full coverage stages delivered = 1, got {}",
        b.plumes[0].2
    );

    // Burn dry: 1024 ticks (~16 s) at full throttle empties 0.1 u —
    // csp-sim's `dry_engine_produces_no_thrust` drains the same
    // fixture in ~7.4 s. The command stays open across the dry-out.
    step(&mut view, 1024);
    let staged = flight::capture_sync_input(
        &view.sim,
        &mut flight::LatchedWeldCache::default(),
        &ArtAssets::default(),
        &[],
    );
    let b = staged
        .vessels
        .iter()
        .find(|b| b.id == id.0)
        .expect("vessel still staged");
    assert!(
        b.throttle > 0.0,
        "the throttle command survives the dry-out — the bug's shape"
    );
    assert!(
        b.plumes.is_empty(),
        "a dry engine must stage no plume emitter (no thrust, no exhaust)"
    );
}
