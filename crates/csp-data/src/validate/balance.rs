//! The §8.3 `balance.ron` rules: `schema_version` and every numeric
//! tunable inside the domain its `csp-sim` `*Config::is_valid` mirror
//! gates at restore.

use crate::resource::ResourceFile;

use super::ValidationError;

use super::common::{check_id, part_positive};

/// §8.3's `balance.ron` checks: `schema_version` current; every numeric
/// tunable finite and inside the domain its `csp-sim` `*Config::is_valid`
/// mirror gates at restore (`DockingConfig`, `TransferConfig`,
/// `ScienceConfig`, `EconomyConfig`, `LifeSupportConfig`,
/// `ReliabilityConfig` — the authored file must never produce a config
/// the sim itself would reject); and `reliability_bench_resource`
/// resolvable against `resources.ron` — the bench run reads the site
/// inventory through it, so an unresolved id silently charges nothing.
///
/// Sign conventions: strictly-positive where a zero is not a tunable
/// (radii, rates, cadences — `part_positive`'s rule), non-negative for
/// magnitudes that may be authored off (`dormant_stress_scale: 0`
/// disables the mechanic). Ranges beyond the sign report as
/// [`ValidationError::BadBalance`]. Ungated counters — `max_debris`,
/// `max_live_joints_per_bubble`, `capture_window_ticks`,
/// `crew_transfer_ticks`, `starting_funds_micros`,
/// `script_op_budget`, `reliability_notification_cap`, and
/// `max_tracked_vessels` — all admit `0` as a legal degenerate (no
/// debris, no joints, an empty arena) and carry no finite domain to
/// check.
///
/// The `dock_soft_pull_per_s < 128` bound is the explicit-Euler
/// stability limit of the soft-capture map `c_{n+1} ≈ (1 − pull·dt)·c_n`
/// at the pinned `1/64` s tick (`pull·dt < 2`); `csp-sim`
/// `DockingConfig::is_valid` is its canonical home (constant
/// `joints::SOFT_PULL_LIMIT_PER_S` = `2/PHYSICS_DT`) — `csp-data`
/// cannot call it (the dependency runs the other way), so the bound
/// is mirrored here with the constant named.
#[allow(clippy::too_many_lines)] // lint-allow (§8.6): one line per field reads as a table
pub fn validate_balance(
    b: &crate::balance::BalanceDef,
    res: &ResourceFile,
) -> Vec<ValidationError> {
    /// The `part` slot balance errors stamp — the `resources.ron`
    /// convention above.
    const WHO: &str = "balance.ron";
    /// Finite and `> 0`.
    fn pos(field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
        part_positive(WHO, field, v, out);
    }
    /// Finite and `≥ 0`.
    fn nonneg(field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
        if !v.is_finite() {
            out.push(ValidationError::PartNonFinite {
                part: WHO.to_string(),
                field,
            });
        } else if v < 0.0 {
            out.push(ValidationError::PartNonPositive {
                part: WHO.to_string(),
                field,
                value: v,
            });
        }
    }
    /// Finite, then the authored band — a band failure is a
    /// [`ValidationError::BadBalance`], not a sign failure.
    fn band(
        field: &'static str,
        v: f64,
        in_band: impl FnOnce(f64) -> bool,
        reason: &'static str,
        out: &mut Vec<ValidationError>,
    ) {
        if !v.is_finite() {
            out.push(ValidationError::PartNonFinite {
                part: WHO.to_string(),
                field,
            });
        } else if !in_band(v) {
            out.push(ValidationError::BadBalance { field, reason });
        }
    }

    let mut out = Vec::new();
    if b.schema_version != crate::balance::BALANCE_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: WHO.to_string(),
            found: b.schema_version,
            expected: crate::balance::BALANCE_SCHEMA_VERSION,
        });
    }
    // §2.3/§2.6.5/§2.9 world-shape tunables.
    pos("bubble_radius_m", b.bubble_radius_m, &mut out);
    if b.advance_to_max_events_per_frame == 0 {
        out.push(ValidationError::BadBalance {
            field: "advance_to_max_events_per_frame",
            reason: "zero — every advance_to call would overflow on its first event",
        });
    }
    // §2.4 docking block — `DockingConfig::is_valid`'s finite-≥0 domain,
    // plus the authored refinements `from_balance` documents.
    nonneg("dock_capture_range_m", b.dock_capture_range_m, &mut out);
    band(
        "dock_capture_angle_deg",
        b.dock_capture_angle_deg,
        |v| v > 0.0 && v <= 90.0,
        "outside the authored (0, 90] capture cone — from_balance clamps into it",
        &mut out,
    );
    nonneg(
        "dock_capture_rel_speed_mps",
        b.dock_capture_rel_speed_mps,
        &mut out,
    );
    nonneg("dock_soft_pull_per_s", b.dock_soft_pull_per_s, &mut out);
    if b.dock_soft_pull_per_s >= 128.0 {
        out.push(ValidationError::BadBalance {
            field: "dock_soft_pull_per_s",
            reason: ">= 128 — the soft-capture Euler step diverges (pull*dt >= 2 at dt = 1/64 s)",
        });
    }
    nonneg(
        "dock_soft_max_correction_mps",
        b.dock_soft_max_correction_mps,
        &mut out,
    );
    nonneg(
        "dock_latch_separation_m",
        b.dock_latch_separation_m,
        &mut out,
    );
    nonneg(
        "dock_latch_rel_speed_mps",
        b.dock_latch_rel_speed_mps,
        &mut out,
    );
    // §3.5 transfers — `TransferConfig::is_valid`: radius ≥ 0, rate > 0.
    nonneg("transfer_radius_m", b.transfer_radius_m, &mut out);
    pos(
        "transfer_rate_units_per_s",
        b.transfer_rate_units_per_s,
        &mut out,
    );
    // §3.2 science — `ScienceConfig::is_valid`: k, epsilon, η > 0; the
    // transmit cost ≥ 0.
    pos("science_softcap_k", b.science_softcap_k, &mut out);
    pos("research_data_epsilon", b.research_data_epsilon, &mut out);
    pos("lab_science_per_data", b.lab_science_per_data, &mut out);
    nonneg(
        "transmit_ec_per_data_unit",
        b.transmit_ec_per_data_unit,
        &mut out,
    );
    // §3.5 economy — `EconomyConfig::is_valid`: salary cadence nonzero,
    // payout ≥ 0, shares in [0,1], salary/upkeep rates ≥ 0, manufacture
    // rate > 0.
    if b.salary_period_days == 0 {
        out.push(ValidationError::BadBalance {
            field: "salary_period_days",
            reason: "zero — the salary cadence must be nonzero",
        });
    }
    nonneg("crew_hire_cost_funds", b.crew_hire_cost_funds, &mut out);
    // C21-F8: founding may be authored free (colony bootstrap worlds)
    // but never negative/non-finite — `to_micros` is the mirror.
    nonneg(
        "colony_founding_cost_funds",
        b.colony_founding_cost_funds,
        &mut out,
    );
    // `EconomyConfig::is_valid`'s finite-≥0 mirror — a NaN or negative
    // salary/upkeep passes the loader but `set_economy_config`/
    // `World::restore` rejects it.
    nonneg("crew_salary_per_s", b.crew_salary_per_s, &mut out);
    nonneg(
        "site_upkeep_funds_per_s",
        b.site_upkeep_funds_per_s,
        &mut out,
    );
    nonneg(
        "science_funds_per_point",
        b.science_funds_per_point,
        &mut out,
    );
    band(
        "recovery_efficiency",
        b.recovery_efficiency,
        |v| (0.0..=1.0).contains(&v),
        "outside [0, 1] — it is a share of recovered value",
        &mut out,
    );
    band(
        "abandon_recovery",
        b.abandon_recovery,
        |v| (0.0..=1.0).contains(&v),
        "outside [0, 1] — it is a share of recovered value",
        &mut out,
    );
    // §3.6 board — `ContractConfig::is_valid`: board size nonzero.
    if b.contract_offer_slots == 0 {
        out.push(ValidationError::BadBalance {
            field: "contract_offer_slots",
            reason: "zero — the offer board would never stock",
        });
    }
    // §3.5 manufacture + life support — rates ≥ 0, cadence > 0.
    pos(
        "manufacture_rate_kg_per_s",
        b.manufacture_rate_kg_per_s,
        &mut out,
    );
    pos(
        "life_support_check_period_s",
        b.life_support_check_period_s,
        &mut out,
    );
    nonneg(
        "life_support_oxygen_per_crew_s",
        b.life_support_oxygen_per_crew_s,
        &mut out,
    );
    nonneg(
        "life_support_food_per_crew_s",
        b.life_support_food_per_crew_s,
        &mut out,
    );
    nonneg(
        "life_support_ec_per_crew_s",
        b.life_support_ec_per_crew_s,
        &mut out,
    );
    // §3.4 reliability — `ReliabilityConfig::is_valid` domains.
    nonneg(
        "reliability_dormant_stress_scale",
        b.reliability_dormant_stress_scale,
        &mut out,
    );
    nonneg(
        "reliability_ignition_hazard_s",
        b.reliability_ignition_hazard_s,
        &mut out,
    );
    band(
        "reliability_degraded_thrust_factor",
        b.reliability_degraded_thrust_factor,
        |v| (0.0..=1.0).contains(&v),
        "outside [0, 1] — a degraded-thrust multiplier may not exceed full thrust",
        &mut out,
    );
    nonneg(
        "reliability_leak_units_per_s",
        b.reliability_leak_units_per_s,
        &mut out,
    );
    pos(
        "reliability_infant_tau_s",
        b.reliability_infant_tau_s,
        &mut out,
    );
    nonneg(
        "reliability_flight_units_per_s_vacuum",
        b.reliability_flight_units_per_s_vacuum,
        &mut out,
    );
    nonneg(
        "reliability_flight_units_per_s_atmosphere",
        b.reliability_flight_units_per_s_atmosphere,
        &mut out,
    );
    nonneg(
        "reliability_bench_funds_per_unit",
        b.reliability_bench_funds_per_unit,
        &mut out,
    );
    check_id(b.reliability_bench_resource.as_str(), &mut out);
    if !res.contains(&b.reliability_bench_resource) {
        out.push(ValidationError::UnknownResource {
            part: WHO.to_string(),
            resource: b.reliability_bench_resource.as_str().to_string(),
        });
    }
    nonneg(
        "reliability_bench_resource_per_unit",
        b.reliability_bench_resource_per_unit,
        &mut out,
    );
    nonneg(
        "reliability_bench_seconds_per_unit",
        b.reliability_bench_seconds_per_unit,
        &mut out,
    );
    out
}
