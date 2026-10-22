//! M7-C §8.3 — the economy harness: a "fully staffed base with no
//! active missions" probe that fast-forwards a world over sim-months
//! and sim-years, measures the liquid-funds drain against the plan's
//! **2–5% per 30-sim-day month** band, checks the serialized save
//! stays under the **≤32 MB at 100 sim-years** bound, and watches
//! the insolvency transition fire on schedule.
//!
//! At M7-C the "fully staffed base" is the crew roster itself — the
//! salary drain is the economy's recurring cost (facilities and
//! their upkeep land with M8's colony/economy block; the harness's
//! contract is the same: fund the program, staff it, watch the
//! monthly ratio). The staff's `salary_per_s` total is *solved* for
//! a target drain rate of the starting balance — the harness proves
//! the recurring-drain machinery lands the authored rate on the
//! money, deterministically, month after month.
//!
//! Everything runs through the public `World` interface — enqueue
//! `HireCrew`/`AdvanceTo`, `step_frame`, read `funds_micros` and the
//! fired-events stream — so the harness exercises the same path the
//! replay driver does.

use std::collections::BTreeMap;
use std::path::Path;

use csp_core::SimError;
use csp_sim::{EventKind, SimCommand, World, economy};

/// The report a [`staffed_base_run`] produces — the harness's
/// measurement surface.
#[derive(Clone, Debug)]
pub struct EconomyReport {
    /// `(month_end_tick, start_micros, end_micros)` per sim-month —
    /// the drain curve in raw micros.
    pub months: Vec<(u64, u64, u64)>,
    /// The tick the first `Insolvent` event fired, if one did.
    pub insolvent_tick: Option<u64>,
    /// Serialized `WorldSnapshot` size at the run's end (bytes).
    pub final_save_bytes: u64,
    /// Roster size and the ledger's length at the end — the
    /// boundedness witnesses.
    pub crew_count: usize,
    pub ledger_len: usize,
}

/// The harness's configurable knobs — a test picks the staff count,
/// the target monthly drain fraction of the *starting* balance, how
/// many months to measure the band over, and the total years to
/// fast-forward.
pub struct EconomyProbe {
    /// Roster size ("fully staffed").
    pub staff: usize,
    /// Target monthly drain as a fraction of the post-hire balance
    /// — the harness solves `salary_per_s` so the total charge hits
    /// this exactly.
    pub drain_fraction: f64,
    /// Months to measure the band over.
    pub band_months: u64,
    /// Total sim-years to fast-forward (the run continues at zero
    /// funds once insolvent — §3.5's bankruptcy floor).
    pub years: u64,
}

const TICKS_PER_SECOND: u64 = 64;
const SIM_YEAR_S: f64 = 365.0 * 86_400.0;

/// Load `data/balance.ron` from the workspace — the §8.3 "shipping
/// data" the harness runs under (the `flight-launch` fixture's
/// balance is a frozen variant of the same file).
fn shipping_balance() -> Result<csp_data::BalanceDef, csp_data::LoadError> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/balance.ron");
    csp_data::load_balance(&path)
}

/// Run the staffed-base probe: build a fresh world on the shipping
/// `balance.ron`, hire `probe.staff` members with salaries solved to
/// drain `drain_fraction` of the post-hire balance per sim-month,
/// then fast-forward `years` sim-years measuring every month.
#[allow(clippy::too_many_lines)] // the probe's build/staff/measure loop is one readable pass
pub fn staffed_base_run(probe: &EconomyProbe) -> Result<EconomyReport, SimError> {
    let balance = shipping_balance()
        .map_err(|_| SimError::Fatal("economy: shipping balance.ron failed to load"))?;
    let cfg = economy::EconomyConfig::from_balance(&balance);
    let mut w = World::with_config(
        csp_sim::orbit::PatchedConics::new(BTreeMap::new()),
        balance.advance_to_max_events_per_frame,
    );
    w.set_economy_config(cfg);
    let period_ticks = cfg.salary_period_ticks;
    let period_s = period_ticks as f64 / TICKS_PER_SECOND as f64;
    // The monthly charge is a pure function of the roster's salary
    // total — solve it so the drain lands on `drain_fraction` of the
    // balance that survives the hire costs, split evenly per member.
    let funds_after_hire = w
        .funds_micros()
        .saturating_sub(cfg.hire_cost_micros.saturating_mul(probe.staff as u64));
    let total_salary =
        probe.drain_fraction * funds_after_hire as f64 / economy::MICROS_PER_FUND / period_s;
    let salary = total_salary / probe.staff.max(1) as f64;
    for i in 0..probe.staff {
        w.enqueue(
            csp_sim::SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: format!("staff-{i}"),
                kind: match i % 3 {
                    0 => economy::CrewKind::Pilot,
                    1 => economy::CrewKind::Engineer,
                    _ => economy::CrewKind::Scientist,
                },
                salary_per_s: salary,
            },
        );
    }
    w.step_frame(w.clock().ticks() + 1)?;
    let mut report = EconomyReport {
        months: Vec::new(),
        insolvent_tick: None,
        final_save_bytes: 0,
        crew_count: 0,
        ledger_len: 0,
    };
    // The drain band is measured *post-hire* — the harness's ratio is
    // (start − end)/start for each month, which the plan reads as
    // "the fully staffed base's liquid-funds drain per sim-month".
    // One sim-month IS the salary period (30 sim-days per §3.5).
    let total_months = (probe.years as f64 * SIM_YEAR_S / period_s).ceil() as u64;
    let target = total_months * period_ticks;
    let mut month = 0u64;
    let mut start = w.funds_micros();
    let mut insolvent_seen = false;
    while w.clock().ticks() < target {
        let r = w.step_frame(w.clock().ticks() + period_ticks)?;
        if !insolvent_seen {
            insolvent_seen = r
                .fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::Insolvent { .. }));
            if insolvent_seen {
                report.insolvent_tick = Some(w.clock().ticks());
            }
        }
        if w.clock().ticks() >= (month + 1) * period_ticks {
            month += 1;
            let end = w.funds_micros();
            report.months.push((w.clock().ticks(), start, end));
            start = end;
        }
        if r.partial {
            // §2.6.5's event budget can halt the call mid-target —
            // re-enter until the frame lands (the driver's contract).
            continue;
        }
    }
    report.crew_count = w.economy().crew.len();
    report.ledger_len = w.economy().ledger.len();
    let snapshot = w.snapshot()?;
    report.final_save_bytes = bincode::serde::encode_to_vec(&snapshot, bincode::config::standard())
        .map_err(|_| SimError::Fatal("economy: snapshot encode failed"))?
        .len() as u64;
    Ok(report)
}

/// What a [`colony_production_run`] observed — the M8-C extension
/// of the §8.3 harness: colony production feeding the economy.
#[derive(Clone, Debug)]
pub struct ColonyEconomyReport {
    /// Ore units the colony's drill produced into its store by the
    /// end of the run (the production side).
    pub ore_produced: f64,
    /// Ore units the `AbandonColony` salvage landed in the site's
    /// inventory (the economy side — the inventory the program's
    /// recovery/build machinery draws on) — the authored
    /// `abandon_recovery` fraction of the produced level.
    pub salvaged: f64,
    /// Serialized `WorldSnapshot` size at the run's end (bytes).
    pub final_save_bytes: u64,
}

/// M8-C §8.3's colony/economy probe: a mining colony runs `months`
/// sim-months — the drill's production accrues into the colony's
/// store (piecewise-linear, event-bounded — one recompute per
/// boundary, never a per-tick integrator), then `AbandonColony`
/// salvages the stores into the site inventory the rest of the
/// economy reads. The world carries no physics/orbit model: the
/// colony's light environment is synthetic (`colony_env` is
/// construction config), so the run is a pure production book.
#[allow(clippy::too_many_lines)] // site/env fixture + run read as one pass
pub fn colony_production_run(months: u64) -> Result<ColonyEconomyReport, SimError> {
    use csp_data::{BodyId, ColonyModuleDef, ResourceId, SiteId};
    let balance = shipping_balance()
        .map_err(|_| SimError::Fatal("economy: shipping balance.ron failed to load"))?;
    let mut bodies = BTreeMap::new();
    bodies.insert(
        BodyId::from("terra"),
        csp_sim::orbit::BodyOrbit {
            mu_m3_per_s2: 4.0e14,
            soi_m: None,
            parent: None,
            elements: None,
        },
    );
    let mut w = World::with_config(
        csp_sim::orbit::PatchedConics::new(bodies),
        balance.advance_to_max_events_per_frame,
    );
    w.set_economy_config(economy::EconomyConfig::from_balance(&balance));
    w.set_life_support_config(csp_sim::life_support::LifeSupportConfig::from_balance(
        &balance,
    ));
    w.set_colony_env(BTreeMap::from([(
        BodyId::from("terra"),
        csp_sim::ColonyBodyEnv {
            rotation_period_s: Some(21_600.0),
            insolation_w_m2: Some(1361.0),
            radius_m: 6.0e6,
        },
    )]));
    let ore = ResourceId::from("ore");
    w.set_sites(BTreeMap::from([(
        SiteId::from("pad"),
        csp_data::SiteDef {
            schema_version: 1,
            id: SiteId::from("pad"),
            display_name: csp_data::LocKey::from("site.pad"),
            body: BodyId::from("terra"),
            latitude_rad: 0.0,
            longitude_rad: 0.0,
            altitude_m: 0.0,
            launch: false,
            resource_yields: vec![(ore.clone(), 1.0)],
        },
    )]));
    // The authored shape: a constant-power generator, a drill
    // pulling ore at the site's full concentration, and the store
    // it fills. 50 days of production at 0.5 u/s ≈ 2.16 M units —
    // the store capacity reads as unbounded for the probe.
    w.enqueue(
        csp_sim::SourceClass::Ui,
        0,
        SimCommand::FoundColony {
            def: csp_data::ColonyDef {
                site: SiteId::from("pad"),
                display_name: csp_data::LocKey::from("colony.probe"),
                modules: vec![
                    ColonyModuleDef::Generator {
                        fuel: ore.clone(),
                        fuel_rate_per_s: 0.0,
                        rate_per_s: 10.0,
                        priority: 0,
                    },
                    ColonyModuleDef::Drill {
                        resource: ore.clone(),
                        base_rate_per_s: 0.5,
                        min_concentration: 1.0,
                        power_ec_per_s: 1.0,
                    },
                    ColonyModuleDef::Store {
                        resource: ore.clone(),
                        capacity: 1.0e12,
                    },
                ],
                stores: vec![],
            },
        },
    );
    w.step_frame(w.clock().ticks() + 1)?;
    let colony = *w
        .colonies()
        .keys()
        .next()
        .ok_or(SimError::Fatal("economy: colony did not found"))?;
    let month_ticks = (30.0 * 86_400.0 * TICKS_PER_SECOND as f64) as u64;
    let target = months * month_ticks;
    while w.clock().ticks() < target {
        w.step_frame(w.clock().ticks() + month_ticks)?;
    }
    let ore_produced = w.colonies()[&colony].stores[&ore].level_at(w.clock().ticks());
    w.enqueue(
        csp_sim::SourceClass::Ui,
        0,
        SimCommand::AbandonColony { colony },
    );
    w.step_frame(w.clock().ticks() + 1)?;
    let site = w
        .economy()
        .sites
        .get(&SiteId::from("pad"))
        .ok_or(SimError::Fatal("economy: abandon left no site inventory"))?;
    let salvaged = site.inventory.get(&ore).copied().unwrap_or(0.0);
    let snapshot = w.snapshot()?;
    Ok(ColonyEconomyReport {
        ore_produced,
        salvaged,
        final_save_bytes: bincode::serde::encode_to_vec(&snapshot, bincode::config::standard())
            .map_err(|_| SimError::Fatal("economy: snapshot encode failed"))?
            .len() as u64,
    })
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// §8.3's shipping-data mirror pin — the economy block of
    /// `balance.ron` must agree with `csp_sim`'s `DEFAULT_*` mirrors,
    /// the same pinning contract `balance_ron_mirrors_sim_event_budget`
    /// holds the other blocks to.
    #[test]
    fn balance_ron_mirrors_economy_defaults() {
        let b = shipping_balance().unwrap();
        assert_eq!(
            b.starting_funds_micros,
            economy::DEFAULT_STARTING_FUNDS_MICROS
        );
        assert_eq!(b.salary_period_days, economy::DEFAULT_SALARY_PERIOD_DAYS);
        assert_eq!(b.crew_hire_cost_funds, economy::DEFAULT_HIRE_COST_FUNDS);
        assert_eq!(
            b.science_funds_per_point,
            economy::DEFAULT_SCIENCE_FUNDS_PER_POINT
        );
        assert_eq!(b.recovery_efficiency, economy::DEFAULT_RECOVERY_EFFICIENCY);
        assert_eq!(b.abandon_recovery, economy::DEFAULT_ABANDON_RECOVERY);
        assert_eq!(
            b.manufacture_rate_kg_per_s,
            economy::DEFAULT_MANUFACTURE_KG_PER_S
        );
        // M8-C's life-support block — the same pinning contract:
        // `csp_sim::life_support::DEFAULT_*` are what data-less
        // worlds run, so they must agree with the authored file
        // (the wiring `View::new`/`scenarios` now perform through
        // `LifeSupportConfig::from_balance`).
        assert_eq!(
            b.life_support_check_period_s,
            csp_sim::life_support::DEFAULT_CHECK_PERIOD_S
        );
        assert_eq!(
            b.life_support_oxygen_per_crew_s,
            csp_sim::life_support::DEFAULT_OXYGEN_PER_CREW_S
        );
        assert_eq!(
            b.life_support_food_per_crew_s,
            csp_sim::life_support::DEFAULT_FOOD_PER_CREW_S
        );
        assert_eq!(
            b.life_support_ec_per_crew_s,
            csp_sim::life_support::DEFAULT_EC_PER_CREW_S
        );
    }

    /// The core §8.3 check: a fully staffed base's recurring drain
    /// lands inside the plan's **2–5% of liquid funds per sim-month**
    /// band for the whole measured window — the recurring-cost
    /// machinery works, deterministic and bounded.
    #[test]
    fn staffed_base_drains_in_band() {
        let probe = EconomyProbe {
            staff: 20,
            drain_fraction: 0.022, // solved just above the floor
            band_months: 24,
            years: 4,
        };
        let report = staffed_base_run(&probe).unwrap();
        assert!(report.months.len() >= probe.band_months as usize);
        for (tick, start, end) in report.months.iter().take(probe.band_months as usize) {
            assert!(
                end <= start,
                "month at {tick} gained funds ({start} → {end})"
            );
            if *start == 0 {
                // The base went insolvent — out of band scope (the
                // plan's band describes a *solvent* program).
                break;
            }
            let ratio = (start - end) as f64 / *start as f64;
            assert!(
                (0.02..=0.05).contains(&ratio),
                "month at {tick}: drain {ratio:.4} outside 2–5% ({start} → {end})"
            );
        }
    }

    /// §8.3's save-size bound: a 100-sim-year run on shipping data
    /// keeps the serialized `WorldSnapshot` under **32 MB** and the
    /// ledger inside its cap — the bounded-history guarantees hold.
    /// The run deliberately outspends: `Insolvent` must have fired
    /// somewhere along the warp, proving the clamp/notify path works
    /// across a long fast-forward.
    #[test]
    fn hundred_year_save_size_and_insolvency() {
        let probe = EconomyProbe {
            staff: 20,
            drain_fraction: 0.03,
            band_months: 0,
            years: 100,
        };
        let report = staffed_base_run(&probe).unwrap();
        assert!(report.final_save_bytes <= 32 * 1024 * 1024);
        assert!(report.ledger_len <= economy::LEDGER_CAP);
        assert_eq!(report.crew_count, 20);
        assert!(
            report.insolvent_tick.is_some(),
            "a 100-year staffed run never went insolvent — the drain \
             or the clamp is broken"
        );
    }

    /// M8-C §8.3's colony/economy seam: a mining colony's production
    /// accrues into its store across sim-months, and `AbandonColony`
    /// feeds the produced units back into the site inventory the
    /// economy's recovery/build machinery draws on — the same
    /// bounded ledger/save-size contract holds through it.
    #[test]
    fn colony_production_feeds_the_economy() {
        let report = colony_production_run(3).unwrap();
        // Three months at 0.5 u/s ≈ 3.9 M units — production accrued.
        assert!(report.ore_produced > 1.0e6, "ore {report:?}");
        // The abandon salvaged the produced units into the site
        // inventory at the authored `abandon_recovery` fraction —
        // colony production feeding the economy.
        let cfg = economy::EconomyConfig::from_balance(&shipping_balance().unwrap());
        assert!(
            (report.salvaged - report.ore_produced * cfg.abandon_recovery).abs()
                < report.ore_produced * 1e-9,
            "salvage {report:?} vs recovery {}·ore",
            cfg.abandon_recovery
        );
        assert!(report.final_save_bytes <= 32 * 1024 * 1024);
    }

    /// The colony probe is deterministic — two runs produce
    /// identical books (the §8.3 replayable-economy reading).
    #[test]
    fn colony_probe_is_deterministic() {
        let a = colony_production_run(1).unwrap();
        let b = colony_production_run(1).unwrap();
        assert_eq!(a.ore_produced, b.ore_produced);
        assert_eq!(a.salvaged, b.salvaged);
        assert_eq!(a.final_save_bytes, b.final_save_bytes);
    }

    /// Determinism: the probe run twice produces identical reports
    /// (the same monthly curve, the same insolvency tick, the same
    /// serialized size — a deterministic economy is a replayable one).
    #[test]
    fn probe_is_deterministic() {
        let probe = EconomyProbe {
            staff: 8,
            drain_fraction: 0.03,
            band_months: 0,
            years: 2,
        };
        let a = staffed_base_run(&probe).unwrap();
        let b = staffed_base_run(&probe).unwrap();
        assert_eq!(a.months, b.months);
        assert_eq!(a.insolvent_tick, b.insolvent_tick);
        assert_eq!(a.final_save_bytes, b.final_save_bytes);
    }
}
