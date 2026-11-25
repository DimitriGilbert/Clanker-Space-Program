//! The `data/balance.ron` schema — global balance constants (PLAN §8.3:
//! "ALL tuning lives here"). Named fields only — positional tuples drift
//! silently under reordering, same convention as the body/system files.

use serde::{Deserialize, Serialize};

use crate::ids::ResourceId;

/// The `schema_version` this loader understands for `balance.ron` — its
/// own namespace (balance files are not body/system files, though they
/// share the §3.1 first-field convention).
///
/// Version 2 (M6-B): the §2.4 docking block landed —
/// `capture_window_ticks`, the capture predicate tunables
/// (range/angle/relative speed), the soft-capture joint compliance
/// (pull rate + correction cap), and the per-bubble live-joint cap.
///
/// Version 3 (M6-C): the latch settle tunables —
/// `dock_latch_separation_m`/`dock_latch_rel_speed_mps`, the "settled"
/// gates that consume a soft-capture inside the window (§2.4's
/// "docking rigidizes" step 2).
///
/// Version 4 (M6-D): the §3.5 transfer block — `transfer_radius_m`,
/// `transfer_rate_units_per_s`, and `crew_transfer_ticks`, the
/// resource/crew transfer process tunables.
///
/// Version 5 (M7-A): the §3.2 science block —
/// `science_softcap_k`, `research_data_epsilon`,
/// `lab_science_per_data`, and `transmit_ec_per_data_unit`.
///
/// Version 6 (M7-C): the §3.5 economy block — `starting_funds_micros`,
/// `salary_period_days`, `crew_hire_cost_funds`,
/// `science_funds_per_point`, `recovery_efficiency`, and
/// `abandon_recovery`.
///
/// Version 7 (M7-D): the §3.6 contract block — `contract_offer_slots`,
/// the offer-board size the generator maintains.
///
/// Version 8 (M8-B): `manufacture_rate_kg_per_s` — the §3.5 build
/// order's throughput, the sim-side mirror
/// `csp_sim::economy::DEFAULT_MANUFACTURE_KG_PER_S`.
///
/// Version 9 (M8-C): the §3.5 life-support block —
/// `life_support_check_period_s` and the three per-crew consumption
/// rates (`oxygen`, `food`, `electric_charge`), the sim-side mirrors
/// `csp_sim::life_support::DEFAULT_*`.
/// 10 (M9a): `script_op_budget` added — §2.10's per-save operation
/// cap for the script host.
/// 11 (M9b): the §3.4 reliability block — dormant-hazard master
/// switch, ignition window, failure-mode effect magnitudes,
/// infant-mortality decay, notification cap, flight-qualification
/// rates, and the test-stand economics (funds per unit, the consumed
/// `reliability_bench_resource` + its rate, seconds per unit).
/// Mirrors `csp_sim::reliability::default_config`.
/// 12 (C08-F2/F3): the §3.5 insolvency-consequence block —
/// `crew_salary_per_s` (the UI's authored hire default, replacing the
/// view-only hardcode that put one crew at 5.18%/month of starting
/// funds — over PLAN:1196-1200's 2–5% staffed-base band),
/// `site_upkeep_funds_per_s` (the "facility upkeep" half of the
/// recurring drain, billed per installed site), and
/// `attrition_grace_periods` (the consecutive unpaid salary periods
/// before `StaffAttrition` starts retiring crew). Mirrors
/// `csp_sim::economy::{DEFAULT_HIRE_SALARY_PER_S,
/// DEFAULT_SITE_UPKEEP_FUNDS_PER_S, DEFAULT_ATTRITION_GRACE_PERIODS}`.
/// 13 (C21-F8): `colony_founding_cost_funds` — §3.5's "funds ─►
/// facilities" arrow for `FoundColony` (founding was free while
/// `AbandonColony` salvaged 25% — an asymmetric mint). Mirror:
/// `csp_sim::economy::DEFAULT_COLONY_FOUNDING_FUNDS`.
/// 14 (C14-F3): `convective_heat_fraction` — the M4b thermal
/// model's η, the fraction of the kinetic energy flux `½ρv³` a part's
/// exposed area absorbs as skin heat. Its own doc always named this
/// file as the tunable's home.
/// Mirror: `csp_phys::DEFAULT_CONVECTIVE_HEAT_FRACTION`.
/// 15 (C14-F3/C09-F6 remainders): `colony_module_repair_cost_funds` —
/// §3.5.1's `RepairColonyModule` debit (impact damage gained the
/// doc-promised repair path; a free repair would make the damage
/// class decorative). Mirror:
/// `csp_sim::economy::DEFAULT_COLONY_MODULE_REPAIR_FUNDS`. Same
/// version ships the calibrated η — 0.005 made M4b lethality
/// unreachable (measured entry peaks ~200–260 K against 800–1600 K
/// part limits); 0.02 burns the small low-margin parts on an
/// aggressive entry while the pod class lands.
/// 16 (water physics): `water_density_kg_m3` and `water_drag_cd` —
/// the ocean's buoyancy and drag tunables. Both live here rather than
/// on the body: every ocean in a data set is the same water.
/// Mirrors: `csp_phys::DEFAULT_WATER_DENSITY_KG_M3`,
/// `csp_phys::DEFAULT_WATER_DRAG_CD`.
pub const BALANCE_SCHEMA_VERSION: u32 = 16;

/// `data/balance.ron` — the tunables every later milestone consumes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BalanceDef {
    /// First field of every data file (§3.1). Must equal
    /// [`BALANCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Physics bubble radius in metres (PLAN §2.3: 20 km; vessels go on
    /// rails at the radius and rejoin at 90% — the hysteresis is sim
    /// logic, not a second constant).
    pub bubble_radius_m: f64,
    /// PLAN §2.6.5 / balance.ron's own comment: `advance_to` processes at
    /// most this many events per frame; overflow returns early at the
    /// tick reached with `partial: true`. The sim-side mirror for
    /// no-data worlds is `csp_sim::DEFAULT_MAX_EVENTS_PER_FRAME` (2000) —
    /// a `csp-bench` test pins the two together.
    pub advance_to_max_events_per_frame: u64,
    /// PLAN §3.5's object caps: `max_tracked_vessels = 512` — the
    /// vessel-arena cap; spawns beyond it reject `Recoverable`. The
    /// sim-side mirror is `csp_sim::DEFAULT_MAX_VESSELS` (512 — the
    /// authored file and the compiled default agree, so data-less
    /// worlds run the same bound; a `csp-bench` test pins them).
    pub max_tracked_vessels: u64,
    /// PLAN §3.5: `max_debris` — breakup debris cap; the sim evicts
    /// oldest-first by `(debris_since, id)`. Mirror:
    /// `csp_sim::DEFAULT_MAX_DEBRIS` (64).
    pub max_debris: u64,
    /// PLAN §2.4: hard cap on live transient joints per bubble — joint
    /// creation beyond it degrades to an immediate clean separation.
    /// Mirror: `csp_sim::DEFAULT_MAX_LIVE_JOINTS` (16).
    pub max_live_joints_per_bubble: u64,
    /// PLAN §2.4: `capture_window_ticks` — the soft-capture joint's
    /// maximum life, ticks (default 192 = 3 s at 64 Hz). Mirror:
    /// `csp_sim::DEFAULT_CAPTURE_WINDOW_TICKS`.
    pub capture_window_ticks: u64,
    /// Docking capture predicate: two port faces within this distance
    /// (m) may soft-capture.
    pub dock_capture_range_m: f64,
    /// Docking capture predicate: maximum port-facing misalignment
    /// (degrees). The sim stores it as a cosine — capture requires
    /// `dir_a · dir_b ≤ −cos(angle)`.
    pub dock_capture_angle_deg: f64,
    /// Docking capture predicate: maximum relative speed at the port
    /// faces (m/s) — a fast fly-through cannot capture.
    pub dock_capture_rel_speed_mps: f64,
    /// Soft-capture compliance (§2.4 "a few seconds"): exponential
    /// anchor reel-in rate (1/s) — separation decays at `e^{−rate·t}`.
    pub dock_soft_pull_per_s: f64,
    /// Soft-capture compliance: the joint's anchor-closing speed cap
    /// (m/s) — the "soft": approach energy damps rather than welding.
    pub dock_soft_max_correction_mps: f64,
    /// §2.4's latch merge (M6-C): the joint's anchors within this
    /// distance (m) count as mated — the first half of the settle
    /// predicate that rigidizes the pair inside the capture window.
    /// Mirror: `csp_sim::DEFAULT_LATCH_SEPARATION_M` (0.1).
    pub dock_latch_separation_m: f64,
    /// §2.4's latch merge (M6-C): relative point speed at the anchors
    /// at or below this (m/s) — the second half of the settle
    /// predicate. Mirror: `csp_sim::DEFAULT_LATCH_REL_SPEED_MPS` (0.5).
    pub dock_latch_rel_speed_mps: f64,
    /// §3.5's `transfer_radius_m` (default 100 m) — the undocked
    /// transfer precondition: two vessels whose centres are within
    /// this distance may run a `ResourceTransfer`/crew transfer
    /// without docking. Mirror: `csp_sim::DEFAULT_TRANSFER_RADIUS_M`.
    pub transfer_radius_m: f64,
    /// §3.5's transfer rate — the plan's `min(source_outflow,
    /// dest_inflow)` per connected port pair. v1 has no per-port flow
    /// stats, so a single rate (units/s) stands in for the pair —
    /// documented M6-D simplification. Mirror:
    /// `csp_sim::DEFAULT_TRANSFER_RATE_PER_S`.
    pub transfer_rate_units_per_s: f64,
    /// The crew-transfer process duration in ticks (§3.5's
    /// `ResourceTransfer` process applied to crew — the plan names a
    /// rate for resources only; a fixed traversing delay stands in).
    /// Mirror: `csp_sim::DEFAULT_CREW_TRANSFER_TICKS`.
    pub crew_transfer_ticks: u64,
    /// §3.2's diminishing-returns constant `k` (≈ 1.0): the soft cap on
    /// cumulative data per `(experiment, body, biome, situation)` key is
    /// `yield = (cap − collected)·(1 − exp(−k·run/cap))` — the exact
    /// integral of the plan's differential form, which reproduces its
    /// quoted 63%/86%/95% repeat-run sequence for a run of `cap/k`
    /// units. Mirror: `csp_sim::DEFAULT_SOFTCAP_K`.
    pub science_softcap_k: f64,
    /// §3.2's `data_epsilon` (1e-6 data units): a lab project's data is
    /// exhausted when it drops below this — the finite-time bound that
    /// makes the scarce-regime exponential ever fire its event.
    /// Mirror: `csp_sim::DEFAULT_DATA_EPSILON`.
    pub research_data_epsilon: f64,
    /// §3.2's `η` — science points produced per data unit a lab
    /// consumes (`D(t) = D0 − R·t/η` in the abundant regime). Mirror:
    /// `csp_sim::DEFAULT_SCIENCE_PER_DATA`.
    pub lab_science_per_data: f64,
    /// `ElectricCharge` units consumed per data unit transmitted
    /// (§3.2: "transmitting … consumes `ElectricCharge`" — the plan
    /// authors no number; this is the tunable). Mirror:
    /// `csp_sim::DEFAULT_TRANSMIT_EC_PER_UNIT`.
    pub transmit_ec_per_data_unit: f64,
    /// §3.5's starting program funds, in micro-funds (1e6 micros = 1
    /// fund — the sim's integer fund unit; see `csp_sim::economy`'s
    /// "integer micro-units" note for why funds are not `f64`).
    /// Mirror: `csp_sim::DEFAULT_STARTING_FUNDS_MICROS`.
    pub starting_funds_micros: u64,
    /// §3.5's salary cadence in sim-days — the recurring drain fires
    /// once per this many sim-days (30 = one sim-month per §3.5's
    /// "a sim-month is 30 sim-days" for the drain band). Mirror:
    /// `csp_sim::DEFAULT_SALARY_PERIOD_DAYS`.
    pub salary_period_days: u64,
    /// §3.5's hire cost per crew member (funds) — `SimCommand::
    /// HireCrew`'s debit. Mirror: `csp_sim::DEFAULT_HIRE_COST_FUNDS`.
    pub crew_hire_cost_funds: f64,
    /// §3.5's science-milestone payout — funds credited per banked
    /// science point (the "science milestones ─► funds" edge of the
    /// economy loop). Mirror:
    /// `csp_sim::DEFAULT_SCIENCE_FUNDS_PER_POINT`.
    pub science_funds_per_point: f64,
    /// §3.5's `recovery_efficiency` — the non-launch site's share of
    /// a recovered vessel's contents and part value credited to its
    /// inventory/funds (default 0.9; a launch site always gets 1.0,
    /// the sim derives it from `SiteDef::launch`). Mirror:
    /// `csp_sim::DEFAULT_RECOVERY_EFFICIENCY`.
    pub recovery_efficiency: f64,
    /// §3.5's `abandon_recovery` (default 0.25, PLAN:1274): the
    /// share credited when a vessel is recovered with no named site
    /// (a remote salvage — contents are scrapped in place, only the
    /// fractional part value pays out). Mirror:
    /// `csp_sim::DEFAULT_ABANDON_RECOVERY`.
    pub abandon_recovery: f64,
    /// §3.6's offer-board size — the number of live contract offers
    /// the generator maintains (drawing at successive `offer_index`
    /// counters when the board is short). Mirror:
    /// `csp_sim::DEFAULT_CONTRACT_BOARD_SIZE`.
    pub contract_offer_slots: u32,
    /// M8-B §3.5 manufacture: the build order's throughput — a craft's
    /// construction takes `mass_kg / manufacture_rate_kg_per_s` sim
    /// seconds at the launch site that ordered it. Mirror:
    /// `csp_sim::economy::DEFAULT_MANUFACTURE_KG_PER_S`.
    pub manufacture_rate_kg_per_s: f64,
    /// M8-C §3.5 life support: the periodic vessel check's cadence in
    /// sim-seconds — one `LifeSupportCheck` fires per this interval on
    /// the absolute grid, debiting `rate · period` per crew member
    /// aboard each crewed vessel. Mirror:
    /// `csp_sim::life_support::DEFAULT_CHECK_PERIOD_S`.
    pub life_support_check_period_s: f64,
    /// M8-C §3.5 life support: `oxygen` units consumed per crew
    /// member per sim-second while life support is enabled. Mirror:
    /// `csp_sim::life_support::DEFAULT_OXYGEN_PER_CREW_S`.
    pub life_support_oxygen_per_crew_s: f64,
    /// M8-C §3.5 life support: `food` units per crew member per
    /// sim-second. Mirror:
    /// `csp_sim::life_support::DEFAULT_FOOD_PER_CREW_S`.
    pub life_support_food_per_crew_s: f64,
    /// M8-C §3.5 life support: `electric_charge` units per crew
    /// member per sim-second. Mirror:
    /// `csp_sim::life_support::DEFAULT_EC_PER_CREW_S`.
    pub life_support_ec_per_crew_s: f64,
    /// M9a §2.10's per-save script op budget — "default 20 000,
    /// configurable per save". Mirror:
    /// `csp_sim::DEFAULT_SCRIPT_OP_BUDGET`.
    pub script_op_budget: u64,
    /// M9b §3.4: the global dormant-hazard multiplier — `0` disables
    /// the mechanic (§3.4's `dormant_stress = 0` off-switch).
    /// Default `1.0`.
    pub reliability_dormant_stress_scale: f64,
    /// Characteristic ignition hazard window (s) — the Bernoulli
    /// ignition-failure probability is
    /// `1 − exp(−λ_ign · ignition_hazard_s)`. Default `1.0`.
    pub reliability_ignition_hazard_s: f64,
    /// Thrust multiplier a `DegradedThrust` failure applies.
    /// Default `0.5`.
    pub reliability_degraded_thrust_factor: f64,
    /// `Leak` mode drain rate (tank units/s). Default `0.001`.
    pub reliability_leak_units_per_s: f64,
    /// Infant-mortality decay constant (s). Default `3600`.
    pub reliability_infant_tau_s: f64,
    /// Notification-log bound. Default `256`.
    pub reliability_notification_cap: u64,
    /// Flight qualification units per burn second in vacuum.
    /// Default `0.01`.
    pub reliability_flight_units_per_s_vacuum: f64,
    /// Flight qualification units per burn second in atmosphere —
    /// §3.4's regime weighting. Default `0.02`.
    pub reliability_flight_units_per_s_atmosphere: f64,
    /// Test-stand funds charge per bench unit. Default `50`.
    pub reliability_bench_funds_per_unit: f64,
    /// The site-inventory resource a bench run consumes — authored,
    /// not hardcoded (§8.3: ALL tuning lives here). Default `"metal"`;
    /// the sim-side mirror is `csp_sim::reliability::default_config`'s
    /// `bench_resource`.
    pub reliability_bench_resource: ResourceId,
    /// Test-stand resource units per bench unit (of
    /// `reliability_bench_resource`). Default `1.0`.
    pub reliability_bench_resource_per_unit: f64,
    /// Sim-seconds one bench unit takes. Default `60`.
    pub reliability_bench_seconds_per_unit: f64,
    /// C08-F2 §3.5: the hire default the UI pays (funds/sim-second) —
    /// `SimCommand::HireCrew` carries its rate verbatim, so this is
    /// the authored default the presentation layer reads through
    /// `csp_sim::economy::EconomyConfig`. Authored inside the
    /// PLAN:1196-1200 band: 20 crew at this rate plus the shipping
    /// sites' upkeep drain ≈3.1% of the post-hire balance per
    /// sim-month. Mirror: `csp_sim::economy::DEFAULT_HIRE_SALARY_PER_S`.
    pub crew_salary_per_s: f64,
    /// C08-F3 §3.5's "facility upkeep" — the recurring drain's
    /// second half: funds/sim-second billed per installed site (the
    /// `economy.sites` arena IS the facility surface — launch pads
    /// and test stands) at the same monthly boundary as salaries.
    /// Mirror: `csp_sim::economy::DEFAULT_SITE_UPKEEP_FUNDS_PER_S`.
    pub site_upkeep_funds_per_s: f64,
    /// C08-F3 §3.5: consecutive unpaid salary periods granted before
    /// `StaffAttrition` retires a roster member — 1 means the first
    /// missed payroll only warns (`Insolvent` + `FacilityDormant`)
    /// and each further consecutive miss retires one member. Mirror:
    /// `csp_sim::economy::DEFAULT_ATTRITION_GRACE_PERIODS`.
    pub attrition_grace_periods: u64,
    /// C21-F8 §3.5's colony founding charge (funds) — `FoundColony`'s
    /// debit at apply, the "funds ─► facilities" arrow of the economy
    /// diagram. Authored starting colonies bypass it (world state,
    /// not a purchase); `AbandonColony`'s 25% store salvage makes a
    /// free founding a net mint, which is why the charge exists.
    /// Mirror: `csp_sim::economy::DEFAULT_COLONY_FOUNDING_FUNDS`.
    pub colony_founding_cost_funds: f64,
    /// C14-F3 §2.4's convective-heating coefficient η — the
    /// dimensionless fraction of `½ρv³` on a part's exposed area
    /// absorbed as skin heat in the M4b thermal tick. Authored 0.02 —
    /// the M4b "survivable AND lethal in the right places"
    /// calibration: 0.005 peaked ~200–260 K on a steep 2.3 km/s
    /// entry against authored `max_temp_k` 800–1600 K (lethality
    /// unreachable); 0.02 burns the 20–50 kJ/K, 800–1000 K parts on
    /// that profile while the 400 kJ/K, 1200 K pod class peaks ~360 K
    /// and lands.
    /// Mirror: `csp_phys::DEFAULT_CONVECTIVE_HEAT_FRACTION`.
    pub convective_heat_fraction: f64,
    /// C09-F6 §3.5/§3.5.1's `RepairColonyModule` debit (funds) — the
    /// per-module charge for clearing `damaged` on a live colony (the
    /// doc-promised post-M8-A repair path landing with the M8-C
    /// impact seam already in play). Authored free is legal — the
    /// validator only bounds finite-≥0 — but a zero cost makes the
    /// §3.5.1 crash-damage class decorative.
    /// Mirror: `csp_sim::economy::DEFAULT_COLONY_MODULE_REPAIR_FUNDS`.
    pub colony_module_repair_cost_funds: f64,
    /// The density of every body's ocean water (kg/m³) — the mass a
    /// part displaces per submerged cubic metre of its collider. 1025
    /// is standard seawater; a part floats iff its own density is
    /// below this number.
    /// Mirror: `csp_phys::DEFAULT_WATER_DENSITY_KG_M3`.
    pub water_density_kg_m3: f64,
    /// The quadratic water-drag coefficient every part is slowed by:
    /// drag = `½ · water_density · water_drag_cd · area · speed²` on
    /// the part's submerged share of its projected area. Bluff-body
    /// order of magnitude; the same coefficient serves every part.
    /// Mirror: `csp_phys::DEFAULT_WATER_DRAG_CD`.
    pub water_drag_cd: f64,
}
