//! M7-C §8.3 — the economy harness: a "fully staffed base with no
//! active missions" probe that fast-forwards a world over sim-months
//! and sim-years, measures the liquid-funds drain against the plan's
//! **2–5% per 30-sim-day month** band, checks the serialized save
//! stays under the **≤32 MB at 100 sim-years** bound, and watches
//! the insolvency transition fire on schedule.
//!
//! The "fully staffed base" is the crew roster plus the program's
//! site facilities — C08-F3 landed the §3.5 "salaries + facility
//! upkeep" pair: `site_upkeep_funds_per_s` drains per installed site
//! at the same monthly boundary. The staff's `salary_per_s` total is
//! *solved* for a target drain rate of the starting balance — the
//! harness proves the recurring-drain machinery lands the authored
//! rate on the money, deterministically, month after month. C08-F2
//! adds the SHIPPED-rate gate: `shipped_salary_lands_in_band` pays
//! `balance.ron`'s authored `crew_salary_per_s` verbatim (never the
//! solved rate) so an out-of-band default fails the §8.3 check it
//! was written to enforce.
//!
//! Everything runs through the public `World` interface — enqueue
//! `HireCrew`/`AdvanceTo`, `step_frame`, read `funds_micros` and the
//! fired-events stream — so the harness exercises the same path the
//! replay driver does.

use std::collections::BTreeMap;
use std::path::Path;

use csp_core::{SimError, TICK_HZ};
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
    /// this exactly. Unused when `salary_per_s` is `Some`.
    pub drain_fraction: f64,
    /// C08-F2: `Some` pays this verbatim per-hire salary (the probe
    /// for the SHIPPED `balance.crew_salary_per_s`); `None` keeps the
    /// solved-from-`drain_fraction` behaviour.
    pub salary_per_s: Option<f64>,
    /// C08-F2: install the shipping `sites.ron` so the per-site
    /// facility upkeep joins the drain — the plan's "fully staffed
    /// base" includes its facilities.
    pub install_sites: bool,
    /// Months to measure the band over.
    pub band_months: u64,
    /// Total sim-years to fast-forward (the run continues at zero
    /// funds once insolvent — §3.5's bankruptcy floor).
    pub years: u64,
}

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
    w.set_economy_config(cfg)?;
    if probe.install_sites {
        // The shipping `sites.ron` — the staffed base's facility
        // surface; the per-site upkeep drains at the salary boundary.
        let sites = csp_data::load_sites(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/systems/home/sites.ron"),
        )
        .map_err(|_| SimError::Fatal("economy: shipping sites.ron failed to load"))?;
        w.set_sites(
            sites
                .sites
                .iter()
                .map(|s| (s.id.clone(), s.clone()))
                .collect(),
        );
    }
    let period_ticks = cfg.salary_period_ticks;
    let period_s = period_ticks as f64 / TICK_HZ as f64;
    // The monthly charge is a pure function of the roster's salary
    // total — solve it so the drain lands on `drain_fraction` of the
    // balance that survives the hire costs, split evenly per member.
    let funds_after_hire = w
        .funds_micros()
        .saturating_sub(cfg.hire_cost_micros.saturating_mul(probe.staff as u64));
    let total_salary =
        probe.drain_fraction * funds_after_hire as f64 / economy::MICROS_PER_FUND / period_s;
    let salary = probe
        .salary_per_s
        .unwrap_or(total_salary / probe.staff.max(1) as f64);
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
                skill: 1,
                salary_per_s: salary,
            },
        );
    }
    // Drain the hires one boundary (re-issuing the effective target
    // while §2.6.5 splits the frame) — and surface rejections: a
    // refused `HireCrew` must fail the probe, never silently shrink
    // the roster it claims to measure.
    let mut target = w.clock().ticks() + 1;
    loop {
        let r = w.step_frame(target)?;
        if let Some(first) = r.rejected.first() {
            return Err(first.error);
        }
        if !r.partial {
            break;
        }
        target = r.effective_target;
    }
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
    /// Live store levels `{resource: units}` at the run's end — the
    /// production side (drill output buffered, converter inputs
    /// drained, converter outputs accrued, EC sustained).
    pub store_levels: BTreeMap<String, f64>,
    /// The colony's state at the run's end — `Nominal` says the
    /// authored power budget held across the whole fast-forward
    /// (a `Degraded`/`PowerLost` episode would read otherwise).
    pub final_state: String,
    /// The site inventory the `AbandonColony` salvage landed
    /// `{resource: units}` — the economy side: the authored
    /// `abandon_recovery` fraction of every store.
    pub salvaged: BTreeMap<String, f64>,
    /// Serialized `WorldSnapshot` size at the run's end (bytes).
    pub final_save_bytes: u64,
}

/// M8-C §8.3's colony/economy probe — the AUTHORED colony, not a
/// synthetic fixture (C30-F5): the world is `View::new`'s own
/// construction (`csp_view::load_sim_world` over `data/`), which
/// founds `data/systems/home/colonies.ron`'s rask mining outpost at
/// world build. The probe fast-forwards `months` sim-months — the
/// drill/converter/power solves run over the authored day-night
/// cycle and the real site yields — then `AbandonColony` salvages
/// the stores into the `rask-mines` site inventory the economy
/// reads. Production is piecewise-linear, event-bounded — one
/// recompute per boundary, never a per-tick integrator.
#[allow(clippy::too_many_lines)] // the run reads as one pass
pub fn colony_production_run(months: u64) -> Result<ColonyEconomyReport, SimError> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let mut w = csp_view::load_sim_world(&root, 0)
        .map_err(|_| SimError::Fatal("economy: data-world build failed"))?;
    // The authored seed — `colonies.ron`'s rask-mines outpost is
    // the first (and only) founding at world build.
    let colony = *w
        .colonies()
        .keys()
        .next()
        .ok_or(SimError::Fatal("economy: authored colony not founded"))?;
    let month_ticks = (30.0 * 86_400.0 * TICK_HZ as f64) as u64;
    let target = months * month_ticks;
    while w.clock().ticks() < target {
        w.step_frame(w.clock().ticks() + month_ticks)?;
    }
    let store_levels = w.colonies()[&colony]
        .stores
        .iter()
        .map(|(r, s)| (r.as_str().to_string(), s.level_at(w.clock().ticks())))
        .collect();
    let final_state = format!("{:?}", w.colonies()[&colony].state);
    w.enqueue(
        csp_sim::SourceClass::Ui,
        0,
        SimCommand::AbandonColony { colony },
    );
    w.step_frame(w.clock().ticks() + 1)?;
    let site = w
        .economy()
        .sites
        .get(&csp_data::SiteId::from("rask-mines"))
        .ok_or(SimError::Fatal("economy: abandon left no site inventory"))?;
    let salvaged = site
        .inventory
        .iter()
        .map(|(r, q)| (r.as_str().to_string(), *q))
        .collect();
    let snapshot = w.snapshot()?;
    Ok(ColonyEconomyReport {
        store_levels,
        final_state,
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
        // C08-F2/F3's insolvency block — same pinning contract.
        assert_eq!(b.crew_salary_per_s, economy::DEFAULT_HIRE_SALARY_PER_S);
        assert_eq!(
            b.site_upkeep_funds_per_s,
            economy::DEFAULT_SITE_UPKEEP_FUNDS_PER_S
        );
        assert_eq!(
            b.attrition_grace_periods,
            economy::DEFAULT_ATTRITION_GRACE_PERIODS
        );
    }

    /// C08-F2: the SHIPPED hire default — `balance.ron`'s authored
    /// `crew_salary_per_s` paid verbatim, never the solved rate —
    /// lands the fully-staffed base inside the plan's **2–5% per
    /// sim-month** band: 20 crew plus the three shipping sites'
    /// authored upkeep (the facilities the plan's "base" includes).
    /// An out-of-band default now fails the §8.3 gate it was written
    /// to enforce — the solved probe could never see it (the report
    /// verified: the retired view hardcode paid 5.18%/month for ONE
    /// crew and nothing caught it).
    #[test]
    fn shipped_salary_lands_in_band() {
        let authored = shipping_balance().unwrap();
        let probe = EconomyProbe {
            staff: 20,
            drain_fraction: 0.0,
            salary_per_s: Some(authored.crew_salary_per_s),
            install_sites: true,
            band_months: 6,
            years: 1,
        };
        let report = staffed_base_run(&probe).unwrap();
        assert_eq!(report.crew_count, probe.staff);
        assert!(report.months.len() >= probe.band_months as usize);
        for (tick, start, end) in report.months.iter().take(probe.band_months as usize) {
            assert!(
                *start > 0 && *end <= *start,
                "month at {tick}: {start} → {end}"
            );
            let ratio = (*start - *end) as f64 / *start as f64;
            assert!(
                (0.02..=0.05).contains(&ratio),
                "month at {tick}: shipped-rate drain {ratio:.4} outside 2–5% ({start} → {end})"
            );
        }
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
            salary_per_s: None,
            install_sites: false,
            band_months: 24,
            years: 4,
        };
        let report = staffed_base_run(&probe).unwrap();
        // The roster actually hired — a rejected `HireCrew` that
        // slipped through would otherwise silently shrink the staff
        // the band is measured against.
        assert_eq!(report.crew_count, probe.staff);
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
            salary_per_s: None,
            install_sites: false,
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

    /// M8-C §8.3's colony/economy seam on the AUTHORED outpost
    /// (C30-F5): `data/systems/home/colonies.ron`'s rask mines run
    /// three sim-months — the drill buffers ore (bounded by its
    /// authored 100-unit store), the smelter produces metal, the
    /// sinter produces composite — the recipe data the file was
    /// written to exercise — and the power budget holds Nominal.
    /// `AbandonColony` then feeds the stores back into the site
    /// inventory at the authored `abandon_recovery` fraction.
    #[test]
    fn colony_production_feeds_the_economy() {
        let report = colony_production_run(3).unwrap();
        let levels = &report.store_levels;
        // The drill ran: ore sits at/above its 10-unit founding
        // seed (the store's cap is 100 — production saturates, the
        // converter draw decides the level).
        assert!(
            levels.get("ore").copied().unwrap_or(0.0) >= 10.0,
            "ore level {levels:?}"
        );
        // The authored recipes RAN — metal and composite accrued:
        // the §3.5.1 converter path on shipped recipes, not a
        // synthetic fixture.
        assert!(
            levels.get("metal").copied().unwrap_or(0.0) > 0.0,
            "smelter produced {levels:?}"
        );
        assert!(
            levels.get("composite").copied().unwrap_or(0.0) > 0.0,
            "sinter produced {levels:?}"
        );
        // The authored 340 000-EC battery held the night — the
        // colonies.ron header's own bound.
        assert_eq!(report.final_state, "Nominal");
        // The abandon salvaged every store into the site inventory
        // at the authored `abandon_recovery` fraction — colony
        // production feeding the economy.
        let cfg = economy::EconomyConfig::from_balance(&shipping_balance().unwrap());
        for (r, level) in levels {
            let got = report.salvaged.get(r).copied().unwrap_or(0.0);
            assert!(
                (got - level * cfg.abandon_recovery).abs() < level.max(1.0) * 1e-6,
                "salvage {r}: {got} vs {level}·{}",
                cfg.abandon_recovery
            );
        }
        assert!(report.final_save_bytes <= 32 * 1024 * 1024);
    }

    /// The colony probe is deterministic — two runs produce
    /// identical books (the §8.3 replayable-economy reading).
    #[test]
    fn colony_probe_is_deterministic() {
        let a = colony_production_run(1).unwrap();
        let b = colony_production_run(1).unwrap();
        assert_eq!(a.store_levels, b.store_levels);
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
            salary_per_s: None,
            install_sites: false,
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
