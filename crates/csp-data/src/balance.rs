//! The `data/balance.ron` schema — global balance constants (PLAN §8.3:
//! "ALL tuning lives here"). Named fields only — positional tuples drift
//! silently under reordering, same convention as the body/system files.

use serde::{Deserialize, Serialize};

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
pub const BALANCE_SCHEMA_VERSION: u32 = 10;

/// `data/balance.ron` — the tunables every later milestone consumes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// PLAN §2.9: `max_tracked_vessels` — vessel-arena cap; spawns
    /// beyond it reject `Recoverable`. The sim-side mirror is
    /// `csp_sim::DEFAULT_MAX_VESSELS` (512).
    pub max_tracked_vessels: u64,
    /// PLAN §2.9: `max_debris` — breakup debris cap; the sim evicts
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
}
