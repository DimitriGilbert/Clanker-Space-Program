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
