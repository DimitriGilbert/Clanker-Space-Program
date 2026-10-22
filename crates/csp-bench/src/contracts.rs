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
            .experiments
            .iter()
            .map(|e| (e.def.id.clone(), e.def.clone()))
            .collect(),
    );
    world.set_tech_tree(
        csp_data::load_techtree(&root.join("techtree"))
            .map_err(ReplayError::Load)?
            .merged(),
    );
    world.set_economy_config(csp_sim::economy::EconomyConfig::from_balance(&balance));
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
    // the world's data tables afterward (the config() capture).
    let cfg = w.config();
    let mut snap = w.snapshot().map_err(ReplayError::Sim)?;
    snap.tech.researched.extend(nodes.iter().cloned());
    for (k, n) in completed {
        *snap.contracts.facts.completed.entry(*k).or_insert(0) += n;
    }
    *w = World::restore(w.orbits().clone(), None, snap).map_err(ReplayError::Sim)?;
    w.set_experiments(cfg.experiments);
    w.set_tech_tree(
        cfg.tech_tree
            .ok_or(SimError::Fatal("probe: no tree"))
            .map_err(ReplayError::Sim)?,
    );
    w.set_sites(cfg.sites);
    w.set_contracts(cfg.contracts);
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
}
