//! M7-D §8.3 — the contract lifecycle probe: a world built from the
//! shipping data set (system + templates + tech tree + balance)
//! drives the whole §3.6 arc through the public `World` surface —
//! offer generation, acceptance, the advance credit, the
//! orbit-achieved completion, the completion payout, and the fact
//! record — then runs the whole thing twice and asserts bit-identical
//! state hashes (the §2.6.1 determinism witness for the
//! `{CONTRACTS, 0, offer_index}` generator).
//!
//! Everything runs through `enqueue`/`step_frame`/public observers —
//! the same path the replay driver walks.

use std::path::Path;

use csp_core::SimError;
use csp_data::{ContractParams, KeplerElements};
use csp_sim::{ContractConfig, SimCommand, SourceClass, World, orbit::PatchedConics};

use crate::replay::ReplayError;

/// The report a [`contract_run`] produces.
#[derive(Clone, Debug)]
pub struct ContractReport {
    /// The generated board, `(offer_index, template)` in offer order.
    pub offers: Vec<(u64, String)>,
    /// Funds after the accept (advance credited).
    pub funds_after_accept_micros: u64,
    /// Funds after the completion (completion reward credited).
    pub funds_after_complete_micros: u64,
    /// The completed-fact record at the end —
    /// `(kind discriminant, count)`.
    pub completed: Vec<(u16, u32)>,
    /// Bodies the fact record marked visited.
    pub bodies_visited: Vec<String>,
    /// The final canonical state hash — the determinism witness.
    pub final_hash: [u8; 32],
}

/// The probe's knobs: the save seed the generator draws under and
/// which generated offer index to accept.
pub struct ContractProbe {
    /// `save_seed` — selects the offer draws.
    pub seed: u64,
    /// Which board index to accept.
    pub offer: u64,
}

/// A circular bound orbit at `alt` over Terrin — always inside the
/// authored `contract.satellite` bands (Terrin datum 600 km; every
/// drawn band needs `rp ≥ min` and `ra ≤ max`, and a circular orbit
/// at the band midpoint satisfies both for the authored ranges? No —
/// a circular orbit needs `min ≤ alt ≤ max`; the midpoint of the
/// drawn band always fits).
pub fn terrin_circular(alt_m: f64) -> KeplerElements {
    KeplerElements {
        semi_major_axis_m: 600_000.0 + alt_m,
        eccentricity: 0.0,
        inclination_rad: 0.0,
        ascending_node_rad: 0.0,
        arg_periapsis_rad: 0.0,
        mean_anomaly_epoch_rad: 0.0,
        period_s: f64::INFINITY,
    }
}

/// Build the shipping-data world — the `csp-view` wiring distilled:
/// the home system, tech tree, experiment defs, sites, balance
/// tunables, and the contract template table.
pub fn contract_world(root: &Path, seed: u64) -> Result<World, ReplayError> {
    let sys = csp_data::load_system(&root.join("systems/home")).map_err(ReplayError::Load)?;
    let balance = csp_data::load_balance(&root.join("balance.ron")).map_err(ReplayError::Load)?;
    let mut world = World::with_system(
        PatchedConics::from_body_defs(
            &sys.bodies.iter().map(|b| b.def.clone()).collect::<Vec<_>>(),
        )
        .map_err(ReplayError::Sim)?,
    );
    world.set_save_seed(seed);
    world.set_science_config(csp_sim::ScienceConfig::from_balance(&balance));
    world.set_experiments(
        csp_data::load_experiments(&root.join("experiments"))
            .map_err(ReplayError::Load)?
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    world.set_tech_tree(
        csp_data::load_techtree(&root.join("techtree"))
            .map_err(ReplayError::Load)?
            .merged(),
    );
    world.set_economy_config(csp_sim::economy::EconomyConfig::from_balance(&balance))?;
    world.set_sites(
        csp_data::load_sites(&root.join("systems/home/sites.ron"))
            .map_err(ReplayError::Load)?
            .sites
            .iter()
            .map(|s| (s.id.clone(), s.clone()))
            .collect(),
    );
    world.set_contracts(
        csp_data::load_contracts(&root.join("contracts"))
            .map_err(ReplayError::Load)?
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    world.set_contract_config(ContractConfig::from_balance(&balance));
    Ok(world)
}

/// Fact injection through the save boundary — mark nodes researched
/// via snapshot/restore (the honest fact write; the probe's subject
/// is the contract generator + lifecycle, not the research grind).
/// The same injection for the §3.6 fact record — a completed-survey
/// history without flying the survey (the probe's subject is the
/// satellite lifecycle; the chain gate is the fact's, honestly
/// written).
fn mark_facts(
    w: &mut World,
    nodes: &[csp_data::TechNodeId],
    completed: &[(csp_data::ContractKind, u32)],
) -> Result<(), ReplayError> {
    // Restore re-derives construction config to empty — re-install
    // the world's data tables afterward (the config() capture), and
    // carry the physics env across (None would drop the bubble the
    // coverage probes' spawn/latch legs need).
    let cfg = w.config();
    let physics = cfg.physics.clone();
    let mut snap = w.snapshot().map_err(ReplayError::Sim)?;
    snap.tech.researched.extend(nodes.iter().cloned());
    for (k, n) in completed {
        *snap.contracts.facts.completed.entry(*k).or_insert(0) += n;
    }
    *w = World::restore(w.orbits().clone(), physics, snap).map_err(ReplayError::Sim)?;
    // `install_config` covers every data channel (the manual
    // re-install list kept dropping `parts`/`colony_env` — the
    // C30-F6 divergence class the fn exists to prevent).
    w.install_config(&cfg);
    Ok(())
}

/// Drive the lifecycle: publish the board, research `tech.start`
/// and `tech.rocketry.2` (the tech facts are the honest gate —
/// injected through the save boundary, never bypassed), accept the
/// chosen offer, then spawn a vessel inside the drawn orbit band and
/// watch the completion land.
pub fn contract_run(root: &Path, probe: &ContractProbe) -> Result<ContractReport, ReplayError> {
    let mut w = contract_world(root, probe.seed)?;
    // Publish the board.
    w.step_frame(csp_core::TICK_HZ).map_err(ReplayError::Sim)?;
    let offers: Vec<(u64, String)> = w
        .contracts()
        .offers
        .values()
        .map(|o| (o.index, o.template.as_str().to_string()))
        .collect();
    // Meet the satellite prereqs — the tech facts are the honest
    // gate (the probe's contract is the generator + lifecycle, not
    // the research grind).
    mark_facts(
        &mut w,
        &[
            csp_data::TechNodeId::from("tech.start"),
            csp_data::TechNodeId::from("tech.science.1"),
            csp_data::TechNodeId::from("tech.rocketry.1"),
            csp_data::TechNodeId::from("tech.rocketry.2"),
        ],
        // The satellite template's `ContractsCompleted` gate — one
        // completed survey in the record admits it to the board.
        &[(csp_data::ContractKind::SurveyRun, 1)],
    )?;
    w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
        .map_err(ReplayError::Sim)?;
    // Complete one survey contract by fact injection? No — the
    // survey contract itself is the chain: accept + satisfy it
    // honestly is a bigger probe. For the harness the board offers
    // whatever the facts allow; the probe accepts the first
    // satellite offer when the board carries one (the
    // completion-satisfying path below reads params, not template
    // ids), else the probe's named index — a rejection is the
    // probe's prereq signal.
    let offer = w
        .contracts()
        .offers
        .values()
        .find(|o| matches!(o.params, ContractParams::SatelliteOrbit { .. }))
        .map_or(probe.offer, |o| o.index);
    w.enqueue(SourceClass::Ui, 0, SimCommand::AcceptContract { offer });
    w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
        .map_err(ReplayError::Sim)?;
    let funds_after_accept_micros = w.funds_micros();
    // Satisfy the accepted contract: read its drawn params and put
    // a matching vessel on rails.
    let contract = w
        .contracts()
        .active
        .values()
        .next()
        .ok_or(SimError::Recoverable(
            "probe: accept left no active contract",
        ))
        .map_err(ReplayError::Sim)?;
    if let ContractParams::SatelliteOrbit {
        body,
        min_periapsis_alt_m,
        max_apoapsis_alt_m,
    } = &contract.params
    {
        let alt = (min_periapsis_alt_m + max_apoapsis_alt_m) / 2.0;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: body.clone(),
                elements: terrin_circular(alt),
            },
        );
        w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
            .map_err(ReplayError::Sim)?;
    }
    let funds_after_complete_micros = w.funds_micros();
    let completed: Vec<(u16, u32)> = w
        .contracts()
        .facts
        .completed
        .iter()
        .map(|(k, n)| (csp_core::StableDiscriminant::discriminant(k), *n))
        .collect();
    let bodies_visited: Vec<String> = w
        .contracts()
        .facts
        .bodies_visited
        .iter()
        .map(|b| b.as_str().to_string())
        .collect();
    Ok(ContractReport {
        offers,
        funds_after_accept_micros,
        funds_after_complete_micros,
        completed,
        bodies_visited,
        final_hash: w.state_hash().map_err(ReplayError::Sim)?,
    })
}

/// The coverage legs' residence splice — the same save-boundary
/// honesty class as `mark_facts` and the `PartTest` orbit splice:
/// snapshot, rewrite the first vessel's residence, restore, and
/// re-install the construction config restore drops.
fn splice_vessel_state(
    w: &mut World,
    balance: &csp_data::BalanceDef,
    seed: u64,
    state: csp_sim::VesselState,
) -> Result<(), ReplayError> {
    let cfg = w.config();
    let physics = cfg.physics.clone();
    let mut snap = w.snapshot().map_err(ReplayError::Sim)?;
    let v = snap
        .vessels
        .values_mut()
        .next()
        .ok_or(SimError::Fatal("coverage: no vessel to splice"))
        .map_err(ReplayError::Sim)?;
    // A colony join binds a LATCHED vessel — the restore gate
    // refuses a joined vessel that is not `SurfaceLatched`, so the
    // splice leaves the join behind when the residence does.
    if !matches!(state, csp_sim::VesselState::SurfaceLatched(_)) {
        v.joined_colony = None;
    }
    let vid = v.id;
    v.state = state.clone();
    // The pending rails-only events for that vessel die with the
    // residence the same way `apply_to_bubble` drops them — a
    // `SoiTransition`/`DebrisDecay`/`ManeuverNodeStart`/`PartFailure`
    // on a latched vessel is exactly what the restore gate fatals on.
    if matches!(state, csp_sim::VesselState::SurfaceLatched(_)) {
        snap.event_heap.retain(|e| {
            !matches!(
                &e.kind,
                csp_sim::EventKind::SoiTransition { vessel, .. }
                    | csp_sim::EventKind::SoiRescan { vessel, .. }
                    | csp_sim::EventKind::DebrisDecay { vessel, .. }
                    | csp_sim::EventKind::ManeuverNodeStart { vessel, .. }
                    | csp_sim::EventKind::PartFailure { vessel, .. }
                    if *vessel == vid
            )
        });
    }
    *w = World::restore(w.orbits().clone(), physics, snap).map_err(ReplayError::Sim)?;
    w.install_config(&cfg);
    retune(w, balance, seed)
}

/// Re-install the tuning channels `WorldConfig` doesn't carry — a
/// restore (`mark_facts`, the craft splice) resets them to the
/// no-data `DEFAULT_*` mirrors, so the coverage world re-derives
/// them from the shipped balance like `build_sim_from_dataset` did.
fn retune(w: &mut World, balance: &csp_data::BalanceDef, seed: u64) -> Result<(), ReplayError> {
    w.set_save_seed(seed);
    w.set_vessel_caps(csp_sim::VesselCaps {
        max_vessels: balance.max_tracked_vessels,
        max_debris: balance.max_debris,
    });
    w.set_script_op_budget(balance.script_op_budget);
    w.set_transfer_config(csp_sim::TransferConfig::from_balance(balance));
    w.set_reliability_config(csp_sim::ReliabilityConfig::from_balance(balance));
    w.set_science_config(csp_sim::ScienceConfig::from_balance(balance));
    w.set_economy_config(csp_sim::economy::EconomyConfig::from_balance(balance))
        .map_err(ReplayError::Sim)?;
    w.set_contract_config(csp_sim::ContractConfig::from_balance(balance));
    w.set_life_support_config(csp_sim::LifeSupportConfig::from_balance(balance));
    Ok(())
}

/// Which contract shape a coverage run exercises — the template's
/// drawn `ContractParams` arm, named so `survey.data` (a `SurveyRun`
/// fact) gets its own leg.
#[derive(Clone, Copy, Debug)]
pub enum CoverageKind {
    /// `contract.rescue` — `VesselRecovered` on a terrin draw.
    Rescue,
    /// `contract.survey.data` — returned units on a terrin draw.
    SurveyData,
    /// `contract.part_test` — module + orbital regime.
    PartTest,
    /// `contract.colony_resupply` — body-scoped delivery sums.
    ColonyResupply,
    /// `contract.crew_transport` — posted headcount at a site's colony.
    CrewTransport,
    /// `contract.tourist_flight` — regime witness + alive return.
    TouristFlight,
}

/// One coverage run's verdict — which template, which offer, whether
/// the drawn params were satisfied, and the deterministic hash.
#[derive(Debug)]
pub struct CoverageReport {
    /// The leg exercised.
    pub kind: CoverageKind,
    /// The accepted contract's id.
    pub contract: u64,
    /// The drawn params (`Debug`) — the report records what the run
    /// satisfied, not just that something completed.
    pub params: String,
    /// `facts.completed` for the leg's fact kind after the run.
    pub completed: u32,
    /// Σ `facts.colony_deliveries` — the delivery ledger the
    /// resupply predicate sums (the terrin depot's real deliveries).
    pub delivered_units: f64,
    /// Deterministic final hash.
    pub final_hash: String,
}

/// C30-F2: exercise `kind`'s lifecycle on the shipped-data world —
/// publish, gate the facts honestly, accept a fitting offer
/// (declining drawn params this sim tier cannot reach — a `rask`
/// rescue/survey has no reachable latch), and satisfy the params
/// with real commands:
///
/// - `Rescue` (terrin draws): ManufactureCraft → SurfaceLatched →
///   `RecoverVessel` — the transition-keyed `VesselRecovered` trigger.
/// - `SurveyData` (terrin draws): the science hopper's landed run →
///   recovery banks the pooled units → `DataBanked`.
/// - `PartTest`: the orbiter craft (crew seat + thrust site covers
///   either module draw) spliced onto a rails vessel at 500 km —
///   above every `min_alt_km` draw — completes at the accept's
///   instant check.
/// - `ColonyResupply`: every draw targets `rask`, which this sim
///   tier cannot latch a vessel on (the bubble lives over terrin) —
///   the probe runs the real terrin-side delivery loop into a
///   terrin depot, asserts the `colony_deliveries` fact grew while
///   the contract correctly did NOT complete (the sum is
///   body-scoped). The full join/deliver/complete loop is proven
///   by the committed `colony-supply` eval scenario on terrin.
#[allow(clippy::too_many_lines)] // one lifecycle = one readable pass
pub fn coverage_run(
    root: &Path,
    kind: CoverageKind,
    seed: u64,
) -> Result<CoverageReport, ReplayError> {
    let balance = csp_data::load_balance(&root.join("balance.ron")).map_err(ReplayError::Load)?;
    let mut w = csp_view::load_sim_world(root, seed)
        .map_err(|_| SimError::Fatal("coverage: data-world build failed"))
        .map_err(ReplayError::Sim)?;
    // Publish the board, then mark the gate facts honestly.
    w.step_frame(csp_core::TICK_HZ).map_err(ReplayError::Sim)?;
    let (nodes, done): (&[&str], &[(csp_data::ContractKind, u32)]) = match kind {
        CoverageKind::SurveyData => (
            &["tech.science.1"][..],
            &[(csp_data::ContractKind::SurveyRun, 1)][..],
        ),
        CoverageKind::Rescue | CoverageKind::PartTest | CoverageKind::ColonyResupply => (
            &["tech.science.1", "tech.rocketry.2", "tech.rocketry.3"][..],
            &[
                (csp_data::ContractKind::SurveyRun, 1),
                (csp_data::ContractKind::SatellitePlacement, 1),
            ][..],
        ),
        // The 7.3 kinds gate on `tech.rocketry.3` alone (the
        // tourist's `CrewLostAtMost(0)` holds on a no-death roster).
        CoverageKind::CrewTransport | CoverageKind::TouristFlight => (
            &["tech.science.1", "tech.rocketry.2", "tech.rocketry.3"][..],
            &[][..],
        ),
    };
    mark_facts(
        &mut w,
        &nodes
            .iter()
            .map(|n| csp_data::TechNodeId::from(*n))
            .collect::<Vec<_>>(),
        done,
    )?;
    retune(&mut w, &balance, seed)?;
    // The crewed legs' staging: a crew-taking depot at the launch
    // site BEFORE the publish re-draws — the transport draw's
    // viability gate reads live colonies, and the tourist leg's
    // roster→vessel posting path needs a colony to route through.
    // The `LifeSupport` module keeps it crew-accepting under either
    // life-support toggle (the authored rask-mines only qualifies
    // with the toggle off).
    if matches!(
        kind,
        CoverageKind::CrewTransport | CoverageKind::TouristFlight
    ) {
        let depot = csp_data::ColonyDef {
            site: csp_data::SiteId::from("terrin-launch"),
            display_name: csp_data::LocKey::from("colony.coverage-crew"),
            modules: vec![
                csp_data::ColonyModuleDef::Battery { capacity: 5_000.0 },
                csp_data::ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![],
                    power_ec_per_s: 0.0,
                },
            ],
            stores: vec![],
        };
        w.enqueue(SourceClass::Ui, 0, SimCommand::FoundColony { def: depot });
        w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
            .map_err(ReplayError::Sim)?;
    }
    w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
        .map_err(ReplayError::Sim)?;

    // `PartTest` is a state predicate — the accept's instant check
    // re-evaluates it, so the satisfying vessel must already exist.
    // The module gate reads the vessel's craft: spawn the orbiter
    // as a real physical hull (craft + thermal records coherent) at
    // the pad pose, then splice its residence to a 500 km terrin
    // orbit through the save boundary (the bubble can't loft it —
    // the same `mark_facts` honesty class). 500 km covers every
    // `min_alt_m` draw; the craft carries BOTH `ModuleReq`s the
    // template draws.
    if matches!(kind, CoverageKind::PartTest) {
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnPhysicalVessel {
                craft: crate::scenarios::orbiter_craft(),
                pos: csp_core::DVec3::new(600_003.5, 0.0, 0.0),
                rot: csp_core::DQuat::from_axis_angle(
                    csp_core::DVec3::Y,
                    core::f64::consts::FRAC_PI_2,
                ),
                vel: csp_core::DVec3::ZERO,
                ang_vel: csp_core::DVec3::ZERO,
            },
        );
        w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
            .map_err(ReplayError::Sim)?;
        let cfg = w.config();
        let physics = cfg.physics.clone();
        let mut snap = w.snapshot().map_err(ReplayError::Sim)?;
        let v = snap
            .vessels
            .values_mut()
            .next()
            .ok_or(SimError::Fatal("coverage: spawn produced no vessel"))
            .map_err(ReplayError::Sim)?;
        v.state = csp_sim::VesselState::OnRails(csp_sim::orbit::OrbitState {
            parent: csp_data::BodyId::from("terrin"),
            elements: terrin_circular(500_000.0),
            epoch: csp_core::SimTime::from_ticks(w.clock().ticks()),
        });
        w = World::restore(w.orbits().clone(), physics, snap).map_err(ReplayError::Sim)?;
        w.install_config(&cfg);
        retune(&mut w, &balance, seed)?;
    }

    // A draw this tier can't satisfy (non-terrin body) gets declined
    // so the board refills — bounded by the seeded draw sequence.
    let fact_kind = match kind {
        CoverageKind::Rescue => csp_data::ContractKind::Rescue,
        CoverageKind::SurveyData => csp_data::ContractKind::SurveyRun,
        CoverageKind::PartTest => csp_data::ContractKind::PartTest,
        CoverageKind::ColonyResupply => csp_data::ContractKind::ColonyResupply,
        CoverageKind::CrewTransport => csp_data::ContractKind::CrewTransport,
        CoverageKind::TouristFlight => csp_data::ContractKind::TouristFlight,
    };
    let baseline = *w.contracts().facts.completed.get(&fact_kind).unwrap_or(&0);
    let mut accepted_params: Option<ContractParams> = None;
    for _ in 0..64 {
        let fitting = w.contracts().offers.values().find(|o| match kind {
            CoverageKind::Rescue => matches!(
                &o.params,
                ContractParams::Rescue { body } if body.as_str() == "terrin"
            ),
            CoverageKind::SurveyData => matches!(
                &o.params,
                ContractParams::SurveyData { body, .. } if body.as_str() == "terrin"
            ),
            CoverageKind::PartTest => matches!(&o.params, ContractParams::PartTest { .. }),
            CoverageKind::ColonyResupply => {
                matches!(&o.params, ContractParams::ColonyResupply { .. })
            }
            CoverageKind::CrewTransport => {
                matches!(&o.params, ContractParams::CrewTransport { .. })
            }
            CoverageKind::TouristFlight => {
                matches!(&o.params, ContractParams::TouristFlight { .. })
            }
        });
        if let Some(o) = fitting {
            let offer = o.index;
            accepted_params = Some(o.params.clone());
            w.enqueue(SourceClass::Ui, 0, SimCommand::AcceptContract { offer });
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
            break;
        }
        // Decline the board's leftmost offer to force a redraw.
        let Some(o) = w.contracts().offers.values().next() else {
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
            continue;
        };
        let index = o.index;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::DeclineContract { offer: index },
        );
        w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
            .map_err(ReplayError::Sim)?;
    }
    let accepted_params = accepted_params
        .ok_or(SimError::Recoverable("coverage: no fitting offer accepted"))
        .map_err(ReplayError::Sim)?;
    let params = format!("{accepted_params:?}");
    // An instant check can complete a state predicate AT accept —
    // `PartTest`'s whole point — leaving nothing in `active` to
    // satisfy. Read the live contract when one remains.
    let live = w.contracts().active.values().next();
    let contract_id = live.map_or(0, |c| c.id.0);
    let live_params = live.map(|c| c.params.clone());

    match live_params.as_ref() {
        Some(ContractParams::Rescue { .. }) | Some(ContractParams::SurveyData { .. }) => {
            let science = matches!(live_params, Some(ContractParams::SurveyData { .. }));
            let (craft, bom) = if science {
                crate::scenarios::science_craft()
            } else {
                crate::scenarios::cargo_craft()
            };
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::ManufactureCraft {
                    site: csp_data::SiteId::from("terrin-launch"),
                    craft,
                    build_resources: bom,
                },
            );
            w.step_frame(w.clock().ticks() + 4_000)
                .map_err(ReplayError::Sim)?;
            if science {
                w.enqueue(
                    SourceClass::Ui,
                    0,
                    SimCommand::RunExperiment {
                        vessel: csp_sim::VesselId(0),
                        node: csp_phys::PartNodeId(0),
                        experiment: csp_data::ExperimentId::from("exp.materials_study"),
                        biome: csp_data::BiomeId::from("terrin.grassland"),
                        situation: csp_data::Situation::Landed,
                    },
                );
                w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                    .map_err(ReplayError::Sim)?;
            }
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::RecoverVessel {
                    vessel: csp_sim::VesselId(0),
                    site: Some(csp_data::SiteId::from("terrin-launch")),
                },
            );
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
        }
        Some(ContractParams::CrewTransport { site, count }) => {
            // The draw's viability gate picked a site a live
            // crew-taking colony occupies — post the (re-based)
            // headcount through the real posting verbs. `count` is
            // the active record's `posted_at_accept + drawn` ask.
            let site = site.clone();
            let need = *count;
            let colony = *w
                .colonies()
                .iter()
                .find(|(_, c)| c.site == site)
                .map(|(id, _)| id)
                .ok_or(SimError::Recoverable(
                    "coverage: no colony at the drawn site",
                ))
                .map_err(ReplayError::Sim)?;
            for i in 0..need {
                w.enqueue(
                    SourceClass::Ui,
                    0,
                    SimCommand::HireCrew {
                        name: format!("hand{i}"),
                        kind: csp_sim::CrewKind::Pilot,
                        skill: 1,
                        salary_per_s: 0.0,
                    },
                );
                w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                    .map_err(ReplayError::Sim)?;
                let crew = *w
                    .economy()
                    .crew
                    .keys()
                    .next_back()
                    .ok_or(SimError::Fatal("coverage: hire produced no member"))
                    .map_err(ReplayError::Sim)?;
                w.enqueue(
                    SourceClass::Ui,
                    0,
                    SimCommand::AssignCrewToColony { colony, crew },
                );
                w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                    .map_err(ReplayError::Sim)?;
            }
        }
        Some(ContractParams::TouristFlight { .. }) => {
            // The honest roster→vessel path: hire a tourist, post at
            // the depot colony (id 1 — rask-mines is authored at 0),
            // transfer aboard the latched crew hull, fly the regime,
            // and bring them home through `RecoverVessel`.
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::HireCrew {
                    name: "tourist".into(),
                    kind: csp_sim::CrewKind::Tourist,
                    skill: 1,
                    salary_per_s: 0.0,
                },
            );
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
            let tourist = *w
                .economy()
                .crew
                .keys()
                .next_back()
                .ok_or(SimError::Fatal("coverage: hire produced no member"))
                .map_err(ReplayError::Sim)?;
            let depot = csp_sim::ColonyId(1);
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::AssignCrewToColony {
                    colony: depot,
                    crew: tourist,
                },
            );
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
            let (craft, bom) = crate::scenarios::crew_craft();
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::ManufactureCraft {
                    site: csp_data::SiteId::from("terrin-launch"),
                    craft,
                    build_resources: bom,
                },
            );
            w.step_frame(w.clock().ticks() + 4_000)
                .map_err(ReplayError::Sim)?;
            let vessel = csp_sim::VesselId(0);
            for cmd in [
                SimCommand::JoinColonyInventory {
                    vessel,
                    colony: depot,
                },
                SimCommand::TransferColonyCrew {
                    vessel,
                    colony: depot,
                    crew: tourist,
                    to_colony: false,
                },
            ] {
                w.enqueue(SourceClass::Ui, 0, cmd);
                w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                    .map_err(ReplayError::Sim)?;
            }
            // The crew pipe's traversal delay lands the member aboard.
            w.step_frame(w.clock().ticks() + 640 + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
            // Splice the crewed hull onto a 500 km terrin orbit —
            // the PartTest leg's honesty class — then fly it back
            // through REAL commands: `ToBubble` + `ToRails`, whose
            // `OrbitAchieved` notify is the production witness.
            let epoch = csp_core::SimTime::from_ticks(w.clock().ticks());
            splice_vessel_state(
                &mut w,
                &balance,
                seed,
                csp_sim::VesselState::OnRails(csp_sim::orbit::OrbitState {
                    parent: csp_data::BodyId::from("terrin"),
                    elements: terrin_circular(500_000.0),
                    epoch,
                }),
            )?;
            w.enqueue(SourceClass::Ui, 0, SimCommand::ToBubble { vessel });
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
            w.enqueue(SourceClass::Ui, 0, SimCommand::ToRails { vessel });
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
            // The return leg: splice to a surface latch at the pad
            // (same honesty class), then the real `RecoverVessel`
            // walks the flown tourist home `Active` + `Unassigned`.
            splice_vessel_state(
                &mut w,
                &balance,
                seed,
                csp_sim::VesselState::SurfaceLatched(csp_sim::SurfacePose {
                    body: csp_data::BodyId::from("terrin"),
                    latitude_rad: -0.092,
                    longitude_rad: 2.916,
                    altitude_m: 0.0,
                    orientation: csp_core::DQuat::IDENTITY,
                }),
            )?;
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::RecoverVessel {
                    vessel,
                    site: Some(csp_data::SiteId::from("terrin-launch")),
                },
            );
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
        }
        Some(ContractParams::PartTest { .. }) | None => {
            // `PartTest`'s instant check at accept already evaluated
            // the spliced orbiter (the leg's work ran before the
            // offer loop); `None` is the same early exit — the
            // contract left `active` already. A settle frame.
            w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                .map_err(ReplayError::Sim)?;
        }
        Some(ContractParams::ColonyResupply { .. }) => {
            // Battery-ONLY depot: a solar charger would fill the EC
            // store to capacity before the delivery lands (the pipe's
            // `free < amount` rejection), and a full store would mask
            // a rejected delivery — an empty one can't.
            let depot = csp_data::ColonyDef {
                site: csp_data::SiteId::from("terrin-launch"),
                display_name: csp_data::LocKey::from("colony.coverage-depot"),
                modules: vec![csp_data::ColonyModuleDef::Battery { capacity: 5_000.0 }],
                stores: vec![],
            };
            w.enqueue(SourceClass::Ui, 0, SimCommand::FoundColony { def: depot });
            let (craft, bom) = crate::scenarios::cargo_craft();
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::ManufactureCraft {
                    site: csp_data::SiteId::from("terrin-launch"),
                    craft,
                    build_resources: bom,
                },
            );
            w.step_frame(w.clock().ticks() + 4_000)
                .map_err(ReplayError::Sim)?;
            // The depot is id 1 (rask-mines authored at 0).
            for cmd in [
                SimCommand::JoinColonyInventory {
                    vessel: csp_sim::VesselId(0),
                    colony: csp_sim::ColonyId(1),
                },
                SimCommand::TransferColonyResource {
                    vessel: csp_sim::VesselId(0),
                    colony: csp_sim::ColonyId(1),
                    resource: csp_data::ResourceId::from("electric_charge"),
                    amount: 200.0,
                    to_colony: true,
                },
            ] {
                w.enqueue(SourceClass::Ui, 0, cmd);
                w.step_frame(w.clock().ticks() + csp_core::TICK_HZ)
                    .map_err(ReplayError::Sim)?;
            }
            w.step_frame(w.clock().ticks() + 4_000)
                .map_err(ReplayError::Sim)?;
            // No abandon verb exists — "the deadline is the active
            // side's exit". The leg's proof is the delivery ledger
            // having grown on terrin while the contract correctly
            // did NOT complete (the sums are body-scoped to rask).
        }
        _ => {}
    }

    let completed = w
        .contracts()
        .facts
        .completed
        .get(&fact_kind)
        .copied()
        .unwrap_or(0)
        .saturating_sub(baseline);
    let delivered_units: f64 = w.contracts().facts.colony_deliveries.values().sum();
    Ok(CoverageReport {
        kind,
        contract: contract_id,
        params,
        completed,
        delivered_units,
        final_hash: w
            .state_hash()
            .map(|h| crate::replay::hex(&h))
            .map_err(ReplayError::Sim)?,
    })
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn data_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data")
    }

    /// The board publishes only what the facts allow: before the
    /// tech facts land, the shipping templates' `TechResearched`
    /// prereqs keep the board empty; after `tech.start` the starter
    /// survey contract generates.
    #[test]
    fn the_board_respects_the_fact_gate() {
        let root = data_root();
        let mut w = contract_world(&root, 3).unwrap();
        w.step_frame(csp_core::TICK_HZ).unwrap();
        assert!(w.contracts().offers.is_empty());
        mark_facts(&mut w, &[csp_data::TechNodeId::from("tech.start")], &[]).unwrap();
        w.step_frame(w.clock().ticks() + csp_core::TICK_HZ).unwrap();
        assert!(!w.contracts().offers.is_empty());
        assert!(
            w.contracts()
                .offers
                .values()
                .all(|o| o.template.as_str() == "contract.survey")
        );
    }

    /// Two runs of the same probe produce byte-identical final
    /// hashes — the deterministic-generator witness.
    #[test]
    fn the_lifecycle_is_deterministic() {
        let root = data_root();
        let probe = ContractProbe { seed: 5, offer: 0 };
        let a = contract_run(&root, &probe).unwrap();
        let b = contract_run(&root, &probe).unwrap();
        assert_eq!(a.final_hash, b.final_hash);
        assert_eq!(a.offers, b.offers);
        // The lifecycle actually ran: the satellite contract
        // accepted (advance landed), completed on the orbit
        // (completion reward landed), and the fact record stamped.
        assert!(
            a.funds_after_complete_micros > a.funds_after_accept_micros || a.completed.is_empty(),
            "completion paid only when the contract completed"
        );
        // The fact record holds the injected survey completion AND
        // the satellite completion the run landed (kind discriminant
        // 0 = SatellitePlacement, 1 = SurveyRun).
        assert_eq!(a.completed, vec![(0, 1), (1, 1)]);
        assert!(a.bodies_visited.contains(&"terrin".to_string()));
    }

    /// C30-F2: every shipped template beyond the satellite is
    /// exercisable end-to-end. Rescue/survey.data complete through
    /// real command verbs (manufacture → latch → run → recover);
    /// part_test completes on a craft-carrying rails vessel;
    /// colony_resupply's body-scoped sum is proven by NOT completing
    /// on real terrin deliveries while the ledger grows; 7.3's
    /// crew_transport completes on real `HireCrew` →
    /// `AssignCrewToColony` postings and tourist_flight on the
    /// roster→colony→vessel→`ToRails`→`RecoverVessel` arc.
    #[test]
    fn every_template_is_exercisable() {
        let root = data_root();
        for (kind, want_completed) in [
            (CoverageKind::Rescue, 1u32),
            (CoverageKind::SurveyData, 1),
            (CoverageKind::PartTest, 1),
            // The resupply draw is body-scoped to rask — unreachable
            // to latch in this sim tier, so the leg proves the sum
            // does NOT count terrin deliveries (documented in
            // `coverage_run`).
            (CoverageKind::ColonyResupply, 0),
            (CoverageKind::CrewTransport, 1),
            (CoverageKind::TouristFlight, 1),
        ] {
            let report = coverage_run(&root, kind, 5).unwrap();
            assert_eq!(
                report.completed, want_completed,
                "{kind:?} → {:?}",
                report.params
            );
            if matches!(kind, CoverageKind::ColonyResupply) {
                assert!(
                    report.delivered_units >= 150.0,
                    "depot got its delivery (delivered {})",
                    report.delivered_units
                );
            }
            // Determinism: a second run hashes identically.
            let again = coverage_run(&root, kind, 5).unwrap();
            assert_eq!(report.final_hash, again.final_hash);
        }
    }
}
