//! M7-C: the §3.5 economy core — program funds, the ledger, the crew
//! roster's salary drain, recovery sites, and the `Insolvent`
//! bankruptcy path. Built as the `Contracts` slot of §2.6.5's
//! subsystem order (the program-economy slot: M7-D's contract system
//! joins it there and publishes after the salary bookkeeping inside
//! the slot).
//!
//! # The funds model — integer micro-units
//!
//! `funds_micros: u64` — 1e6 micros to the fund — is deliberate over
//! `f64`: charges and credits are exact integer arithmetic, identical
//! bitwise under every §2.6.1 partition and every replay, with no
//! float-associativity question to document. The one float boundary
//! is conversion at the edge: a `f64` cost (resolved part cost,
//! `salary_per_s · period`) becomes micros once, by [`to_micros`]
//! (nearest-micro rounding — a fixed rule over fixed inputs, so the
//! boundary is deterministic), and everything downstream is integer.
//! Negative funds never exist (§3.5): a debit pays `min(amount,
//! funds)` and the shortfall is what `Insolvent` reports.
//!
//! # The ledger
//!
//! [`EconomyState::ledger`] is a bounded `VecDeque` (cap
//! [`LEDGER_CAP`]) of every funds movement — `tick`, signed
//! `delta_micros`, [`FundsReason`]. It feeds the §8.3 economy harness
//! and any UI transaction view. Like `command_log` it is *recorded
//! history*: serialized (§7.2's notification-log class), **not
//! hashed** — it can never change the next tick's evolution, so it
//! stays out of §7.1's forward-looking walk. It IS shadowed: a
//! rejected command or rolled-back iteration truncates its entries
//! with the rest of the capture.
//!
//! # Salaries — a scheduled event, not an interval
//!
//! The recurring drain is `EventKind::SalaryDue` (`Silent`): it fires
//! once per `salary_period_ticks` (balance.ron `salary_period_days`,
//! 30 sim-days = one sim-month), debits `Σ salary_per_s · period_s`
//! over every `Active` roster member, and reschedules itself one
//! period further along the absolute period grid. Mutation happens
//! only at the absolute-tick event boundary — the same
//! partition-independence argument the labs make; an
//! interval-integrated drain would be `fl(fl(x − r·dt₁) − r·dt₂)`,
//! which is exactly the last-ulp drift §2.6.1 bans.
//!
//! `SalaryDue` is lazily *published*, like the Orbits subsystem's
//! rescan: the `Contracts` slot's interval advance schedules the
//! next-grid event whenever the heap lacks one — a pure function of
//! `(clock, heap)`, so the first publish lands identically in every
//! partition and restore can't lose the cadence.
//!
//! # Recovery
//!
//! `SimCommand::RecoverVessel { vessel, site }`: the vessel must be
//! `SurfaceLatched`, and a named site must sit on the latched body.
//! Efficiency is `1.0` at the body's launch site (`SiteDef::launch`),
//! `recovery_efficiency` (0.9) elsewhere; `site: None` is the remote
//! salvage — `abandon_recovery` (0.25) and no inventory credit at
//! all. Effects: resource contents move to the site's inventory at
//! `efficiency`, the live part graph's resolved `part_costs` pay out
//! at `efficiency` to funds, roster crew aboard come home
//! (`location: Unassigned`), pooled science banks at the §3.2
//! recover fraction — and the vessel leaves the sim (the
//! `remove_vessel` scrub).
//!
//! # Insolvency
//!
//! §3.5: *"funds clamp at zero; unpaid salaries trigger
//! `StaffAttrition` and `FacilityDormant` events; an `Insolvent`
//! state is a `Clamping` event that auto-demotes warp to ×1 and
//! notifies."* A [`charge_funds`](World::charge_funds) shortfall sets
//! `insolvent` and schedules `EventKind::Insolvent` AND
//! `EventKind::FacilityDormant` (both `Clamping`) at `clock + 1` —
//! once per insolvency: the flag clears when a credit lifts funds
//! above zero, and a second clamp while already insolvent schedules
//! no duplicates. Dormant IS the flag: while `insolvent` holds, the
//! program's facility verbs (`ManufactureCraft`, `RunBenchTest` —
//! gated once in `apply_command`) reject `Recoverable`, and any
//! credit lifting funds above zero restores them. *Actions* that
//! cost funds (hire, manufacture) instead reject `Recoverable` —
//! you cannot buy what you cannot afford; only the *drains*
//! (salary, upkeep, contract penalties) clamp.
//!
//! `StaffAttrition` is the salary firing's own bookkeeping:
//! [`fire_salary_due`](World::fire_salary_due) counts consecutive
//! salary shortfalls in `unpaid_salary_streak`; past the authored
//! `attrition_grace_periods` each further missed payroll retires one
//! member — the highest-salary `Active` member first (attrition
//! relieves the drain fastest), lowest `CrewId` on ties — scheduled
//! as the `Clamping` event plus a notification-log entry. A fully
//! paid period resets the streak.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use csp_core::{SimError, StableDiscriminant, StateHasher, TICK_HZ};
use csp_data::{BalanceDef, ResourceId, SiteDef, SiteId};

use crate::transfers::CrewId;
use crate::{EventKind, TickShadow, VesselId, World};

/// Micro-units per fund — the sim's integer currency quantum
/// (module doc). Conversion to `f64` funds happens only at the
/// presentation surface; the sim never stores a float fund amount.
pub const MICROS_PER_FUND: f64 = 1e6;

/// `f64` funds → integer micros: nearest-micro rounding, clamped to
/// `[0, u64::MAX]` — non-finite and negative inputs convert to 0 (the
/// callers gate their own error paths; this is the total, documented
/// edge rule so a NaN authored cost can never mint).
pub fn to_micros(funds: f64) -> u64 {
    if !(funds.is_finite() && funds > 0.0) {
        return 0;
    }
    (funds * MICROS_PER_FUND).round().min(u64::MAX as f64) as u64
}

/// The ledger's capacity — a bounded recent-transaction window so the
/// serialized notification history can never grow the save without
/// bound over a 100-sim-year run (§3.5's object-cap discipline
/// applied to its own bookkeeping). 1024 entries of ~40 bytes is a
/// rounding error next to the ≤32 MB bound.
pub const LEDGER_CAP: usize = 1024;

// ---------------------------------------------------------------------------
// Tunables (balance.ron → construction config)
// ---------------------------------------------------------------------------

/// `balance.ron: starting_funds_micros` mirror for data-less worlds
/// — 1e6 funds.
pub const DEFAULT_STARTING_FUNDS_MICROS: u64 = 1_000_000_000_000;
/// `balance.ron: salary_period_days` mirror — 30 sim-days, the §3.5
/// sim-month the drain band is measured in.
pub const DEFAULT_SALARY_PERIOD_DAYS: u64 = 30;
/// `balance.ron: crew_hire_cost_funds` mirror — 5 000 funds.
pub const DEFAULT_HIRE_COST_FUNDS: f64 = 5_000.0;
/// `balance.ron: science_funds_per_point` mirror — 10 funds per
/// banked science point.
pub const DEFAULT_SCIENCE_FUNDS_PER_POINT: f64 = 10.0;
/// `balance.ron: recovery_efficiency` mirror — §3.5's 0.9.
pub const DEFAULT_RECOVERY_EFFICIENCY: f64 = 0.9;
/// `balance.ron: abandon_recovery` mirror — §3.5/PLAN:1274's 0.25.
pub const DEFAULT_ABANDON_RECOVERY: f64 = 0.25;
/// `balance.ron: manufacture_rate_kg_per_s` mirror — M8-B's §3.5
/// build-order throughput: a craft takes
/// `mass_kg / rate` sim-seconds at its launch site.
pub const DEFAULT_MANUFACTURE_KG_PER_S: f64 = 50.0;
/// `balance.ron: crew_salary_per_s` mirror — C08-F2's authored hire
/// default (funds/sim-second): the rate the UI's hire buttons pay.
/// 0.0005 ≈ ₣1 296/crew/sim-month — 20 crew ≈ 2.9% of the post-hire
/// balance, inside PLAN:1196-1200's 2–5% band (the view's retired
/// `HIRE_SALARY_PER_S = 0.02` put one crew alone at 5.18%).
pub const DEFAULT_HIRE_SALARY_PER_S: f64 = 0.0005;
/// `balance.ron: site_upkeep_funds_per_s` mirror — C08-F3's §3.5
/// "facility upkeep": funds/sim-second billed per installed site at
/// the salary boundary. 0.0002 ≈ ₣518/site/sim-month.
pub const DEFAULT_SITE_UPKEEP_FUNDS_PER_S: f64 = 0.0002;
/// `balance.ron: attrition_grace_periods` mirror — C08-F3: the
/// consecutive unpaid salary periods before `StaffAttrition` starts
/// retiring crew (1 = the first miss only warns; the second and each
/// further miss retires a member).
pub const DEFAULT_ATTRITION_GRACE_PERIODS: u64 = 1;
/// `balance.ron: colony_founding_cost_funds` mirror — C21-F8's §3.5
/// "funds ─► facilities" arrow: `FoundColony`'s debit (authored
/// starting colonies bypass it — they are world state, not a
/// purchase). 25 000 funds ≈ 2.5% of the starting balance, a mid-tier
/// purchase next to the authored contract payouts.
pub const DEFAULT_COLONY_FOUNDING_FUNDS: f64 = 25_000.0;

/// Sim-day in seconds (86400) — the salary cadence conversion.
const DAY_S: f64 = 86_400.0;

/// The §3.5 economy tunables — construction config (loaded from
/// `balance.ron` in real worlds; the `DEFAULT_*` mirrors otherwise),
/// **serialized and hashed** like `transfer_cfg`/`science_cfg`: every
/// field changes state evolution (the salary cadence, the charge
/// amounts, the recovery share).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EconomyConfig {
    /// Fresh-world funds (micros). Consumed at world construction and
    /// by [`World::set_economy_config`] — a construction-time seed,
    /// serialized so a restored world enforces the same value set.
    pub starting_funds_micros: u64,
    /// The salary cadence, ticks (`salary_period_days · 86400 · 64`
    /// at load). `SalaryDue` events fire on this absolute grid.
    pub salary_period_ticks: u64,
    /// `SimCommand::HireCrew`'s debit (micros).
    pub hire_cost_micros: u64,
    /// Funds credited per banked science point (the milestone edge).
    pub science_funds_per_point: f64,
    /// The non-launch site's recovery share (§3.5's 0.9).
    pub recovery_efficiency: f64,
    /// The site-less recovery share (PLAN:1274's 0.25).
    pub abandon_recovery: f64,
    /// M8-B: the launch-site build-order throughput — a craft's
    /// construction takes `mass_kg / manufacture_rate_kg_per_s` sim
    /// seconds before `ConstructionComplete` delivers it. Hashed like
    /// every tunable: it moves the event grid.
    pub manufacture_rate_kg_per_s: f64,
    /// C08-F2: the authored hire default (funds/sim-second) — the
    /// presentation layer's salary when `HireCrew` doesn't name its
    /// own. The sim never consumes it (`apply_hire_crew` keeps its
    /// parameterized shape), but it is serialized + hashed with the
    /// rest of the config: a restored world enforces the value set it
    /// was built under.
    pub crew_salary_per_s: f64,
    /// C08-F3: the per-site facility upkeep (funds/sim-second) —
    /// debited `× period_s × sites.len()` at each `SalaryDue`
    /// boundary under `FundsReason::SiteUpkeep`. Hashed: it moves
    /// the funds trajectory.
    pub site_upkeep_funds_per_s: f64,
    /// C08-F3: consecutive unpaid salary periods granted before
    /// `StaffAttrition` retires a member (`fire_salary_due` retires
    /// when `unpaid_salary_streak` exceeds this). Hashed: it moves
    /// the roster evolution.
    pub attrition_grace_periods: u64,
    /// C21-F8 §3.5: `FoundColony`'s funds debit (micros) — the
    /// command path's founding charge. Hashed: a restored world
    /// enforces the founding economics it was built under.
    pub colony_founding_cost_micros: u64,
}

/// The `DEFAULT_*` config — `World::new()`'s no-data fallback.
pub fn default_config() -> EconomyConfig {
    EconomyConfig {
        starting_funds_micros: DEFAULT_STARTING_FUNDS_MICROS,
        salary_period_ticks: (DEFAULT_SALARY_PERIOD_DAYS as f64 * DAY_S * TICK_HZ as f64) as u64,
        hire_cost_micros: to_micros(DEFAULT_HIRE_COST_FUNDS),
        science_funds_per_point: DEFAULT_SCIENCE_FUNDS_PER_POINT,
        recovery_efficiency: DEFAULT_RECOVERY_EFFICIENCY,
        abandon_recovery: DEFAULT_ABANDON_RECOVERY,
        manufacture_rate_kg_per_s: DEFAULT_MANUFACTURE_KG_PER_S,
        crew_salary_per_s: DEFAULT_HIRE_SALARY_PER_S,
        site_upkeep_funds_per_s: DEFAULT_SITE_UPKEEP_FUNDS_PER_S,
        attrition_grace_periods: DEFAULT_ATTRITION_GRACE_PERIODS,
        colony_founding_cost_micros: to_micros(DEFAULT_COLONY_FOUNDING_FUNDS),
    }
}

impl EconomyConfig {
    /// The `balance.ron` → config conversion (§8.3).
    pub fn from_balance(b: &BalanceDef) -> Self {
        Self {
            starting_funds_micros: b.starting_funds_micros,
            salary_period_ticks: (b.salary_period_days as f64 * DAY_S * TICK_HZ as f64) as u64,
            hire_cost_micros: to_micros(b.crew_hire_cost_funds),
            science_funds_per_point: b.science_funds_per_point,
            recovery_efficiency: b.recovery_efficiency,
            abandon_recovery: b.abandon_recovery,
            manufacture_rate_kg_per_s: b.manufacture_rate_kg_per_s,
            crew_salary_per_s: b.crew_salary_per_s,
            site_upkeep_funds_per_s: b.site_upkeep_funds_per_s,
            attrition_grace_periods: b.attrition_grace_periods,
            colony_founding_cost_micros: to_micros(b.colony_founding_cost_funds),
        }
    }

    /// The snapshot-restore gate — corrupt config fails typed rather
    /// than silently diverging (a zero period would re-arm the salary
    /// grid at the clock; a non-finite rate would mint or eat funds).
    pub fn is_valid(&self) -> bool {
        self.salary_period_ticks > 0
            && self.science_funds_per_point.is_finite()
            && self.science_funds_per_point >= 0.0
            && self.recovery_efficiency.is_finite()
            && (0.0..=1.0).contains(&self.recovery_efficiency)
            && self.abandon_recovery.is_finite()
            && (0.0..=1.0).contains(&self.abandon_recovery)
            && self.manufacture_rate_kg_per_s.is_finite()
            && self.manufacture_rate_kg_per_s > 0.0
            && self.crew_salary_per_s.is_finite()
            && self.crew_salary_per_s >= 0.0
            && self.site_upkeep_funds_per_s.is_finite()
            && self.site_upkeep_funds_per_s >= 0.0
    }
}

// ---------------------------------------------------------------------------
// The crew roster (§3.5's named-individual model)
// ---------------------------------------------------------------------------

/// §2.2 sim-side colony identity — the `CrewLocation::Colony` slot's
/// key type. The colony arena lands with M8's flow network; the id
/// type lands with the roster that references it (the `FacilityId`
/// precedent — an id type before its def).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ColonyId(pub u64);

/// §3.5's crew kinds — discriminants are the §7.1 rule-7 encoding:
/// explicit, stable, append-only.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrewKind {
    /// Pilot.
    Pilot = 0,
    /// Engineer.
    Engineer = 1,
    /// Scientist.
    Scientist = 2,
    /// Tourist — contract cargo; draws no duties (§3.5).
    Tourist = 3,
}

impl StableDiscriminant for CrewKind {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// §3.5's crew status — discriminants append-only.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrewStatus {
    /// On the payroll — draws `salary_per_s`.
    Active = 0,
    /// Dead (per the save's deaths setting; the `Lost` status the
    /// seat-destruction paths will write when the crew model binds).
    Lost = 1,
    /// Off the payroll — draws nothing.
    Retired = 2,
}

impl StableDiscriminant for CrewStatus {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// §3.5's crew location — discriminants append-only. `Colony` is live:
/// M8-A's `AssignCrewToColony`/`EvacuateColony` write it (the roster↔
/// crew-set restore pairing resolves it against the colony arena), and
/// M8-B's vessel↔colony crew transfer lands members here. `Facility`
/// stays forward-declared — no producer can write it yet (restore
/// rejects it — see the restore gate).
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrewLocation {
    /// At a program facility (data id — `FacilityId` precedes its
    /// arena, same convention as `ColonyId`).
    Facility(csp_data::FacilityId) = 0,
    /// Aboard a vessel.
    Vessel(VesselId) = 1,
    /// At a colony.
    Colony(ColonyId) = 2,
    /// On the roster, nowhere assigned — the hire/recovery landing.
    Unassigned = 3,
}

impl StableDiscriminant for CrewLocation {
    fn discriminant(&self) -> u16 {
        match self {
            Self::Facility(_) => 0,
            Self::Vessel(_) => 1,
            Self::Colony(_) => 2,
            Self::Unassigned => 3,
        }
    }
}

/// §3.5's `CrewMember` — one named individual. Serialized sim state
/// (the roster is the crew arena §7.2 names); hashed — a member's
/// salary and status change the next `SalaryDue` firing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CrewMember {
    /// Display name — carried on `HireCrew` (logged like a
    /// `SetAlarm` label, so the roster is replayable).
    pub name: String,
    /// The member's kind.
    pub kind: CrewKind,
    /// §3.5's `skill 0..=5` — no XP in v1.
    pub skill: u8,
    /// Where they are — `Unassigned` after hire or recovery.
    pub location: CrewLocation,
    /// Payroll rate, funds/sim-second — §3.5's `salary_per_s`.
    pub salary_per_s: f64,
    /// `Active` draws salary; `Lost`/`Retired` draw nothing.
    pub status: CrewStatus,
}

// ---------------------------------------------------------------------------
// The ledger + site inventory + economy state
// ---------------------------------------------------------------------------

/// Why funds moved — the ledger's reason set (append-only
/// discriminants; M7-D added the contract lifecycle reasons:
/// `ContractAdvance`, `ContractCompletion`, `ContractPenalty` —
/// the `ContractReward` stub stays as the debug command's reason).
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FundsReason {
    /// `SpawnPhysicalVessel`'s build charge (`part manufacture`).
    Manufacture = 0,
    /// `RecoverVessel`'s part-value payout.
    Recovery = 1,
    /// Science banked → funds at `science_funds_per_point`.
    ScienceMilestone = 2,
    /// `HireCrew`'s charge.
    CrewHire = 3,
    /// The `SalaryDue` drain.
    Salary = 4,
    /// `ContractReward` — the M7-D stub interface (contracts land
    /// in D; the payout channel exists now so the ledger/harness
    /// exercise the funds path they will drive).
    ContractReward = 5,
    /// §3.5's contract advance — `AcceptContract`'s credit (M7-D).
    ContractAdvance = 6,
    /// §3.5's completion reward — a contract's predicate landed.
    ContractCompletion = 7,
    /// §3.6's failure penalty — a `ContractDeadline` expiry's
    /// charge (a drain: clamps, never rejects).
    ContractPenalty = 8,
    /// M9b §3.4's test-stand charge — `RunBenchTest`'s spend
    /// (reservation: charged at apply, delivered at
    /// `BenchTestComplete`).
    BenchTest = 9,
    /// C08-F3 §3.5's facility upkeep — the per-site drain billed at
    /// the `SalaryDue` boundary (`site_upkeep_funds_per_s · period_s`
    /// per installed site).
    SiteUpkeep = 10,
    /// C11-F3's `CancelBuild` refund — the order's funds spend
    /// credited back (the `Manufacture` entry's inverse).
    BuildRefund = 11,
    /// C21-F8 §3.5's `FoundColony` founding charge — the
    /// `colony_founding_cost_funds` debit (facilities spend).
    ColonyFounding = 12,
}

impl StableDiscriminant for FundsReason {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// One funds movement — the ledger entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// The tick the movement stamped at (drain boundary or event
    /// tick — the sim clock, always).
    pub tick: u64,
    /// Signed micros: positive = credited, negative = debited (the
    /// *paid* amount — a clamped salary's shortfall is not a
    /// transaction; it rides the `Insolvent` event's payload).
    pub delta_micros: i64,
    /// What moved the money.
    pub reason: FundsReason,
}

/// A recovery site's sim state — the §7.2 "launch-site inventory":
/// resource units credited by `RecoverVessel` at efficiency. The
/// site's identity/geography is construction config (`SiteDef`
/// defs); this record is the per-site *mutable* state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SiteState {
    /// Its arena key — kept inline per §2.2's idiom.
    pub id: SiteId,
    /// Stockpiled units per resource — the recovery credit target.
    pub inventory: BTreeMap<ResourceId, f64>,
}

/// The world's §3.5 economy book — everything the economy owns:
/// funds, the insolvency flag, the crew roster, the site inventory
/// arena, and the (bounded, unhashed) transaction ledger.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EconomyState {
    /// Liquid funds in micro-units — `u64`, so "clamp at zero" is
    /// the type's own floor (module doc). Hashed + serialized.
    pub funds_micros: u64,
    /// §3.5's "`Insolvent` state": set by the first shortfall a
    /// charge clamps into (and the one `Insolvent` + `FacilityDormant`
    /// event pair it schedules); cleared when a credit lifts funds
    /// above zero — a new insolvency then notifies afresh. The flag
    /// IS the facility-dormancy state: `apply_command` rejects the
    /// facility verbs while it holds. Hashed + serialized.
    pub insolvent: bool,
    /// C08-F3's `StaffAttrition` input — consecutive `SalaryDue`
    /// firings whose salary charge shortfell. Past
    /// `attrition_grace_periods` each further miss retires one
    /// member; a fully paid period resets it to zero. Hashed +
    /// serialized (it decides the next firing's retirement).
    pub unpaid_salary_streak: u64,
    /// The §3.5 crew roster, keyed by `CrewId` (`next_crew_id`
    /// allocates — the same counter `BoardCrew` draws from; a
    /// roster member is a full `CrewMember`, a `BoardCrew` seat is
    /// an id-only member not in this map). Hashed + serialized.
    pub crew: BTreeMap<CrewId, CrewMember>,
    /// The site inventory arena, keyed by `SiteId` — one state per
    /// installed `SiteDef` (the `set_sites` reconciliation).
    /// Hashed + serialized.
    pub sites: BTreeMap<SiteId, SiteState>,
    /// The bounded transaction ledger — serialized (notification
    /// class) but **not hashed** and shadow-restored by truncation,
    /// like `command_log`. Newest entries evict oldest past
    /// [`LEDGER_CAP`].
    pub ledger: VecDeque<LedgerEntry>,
}

// ---------------------------------------------------------------------------
// Hash walks (§7.1) — hand-written, ordering is the schema.
// ---------------------------------------------------------------------------

fn hash_location(h: &mut StateHasher, l: &CrewLocation) {
    h.stable_enum(l);
    match l {
        CrewLocation::Facility(f) => {
            h.str(f.as_str());
        }
        CrewLocation::Vessel(v) => {
            h.u64(v.0);
        }
        CrewLocation::Colony(c) => {
            h.u64(c.0);
        }
        CrewLocation::Unassigned => {}
    }
}

/// The world-level [`EconomyState`] walk — appended to `state_hash`.
/// The ledger is deliberately absent: recorded history, like
/// `command_log` (module doc).
pub fn hash_economy(h: &mut StateHasher, e: &EconomyState) {
    h.u64(e.funds_micros);
    h.bool(e.insolvent);
    h.u64(e.unpaid_salary_streak);
    h.arena(e.crew.iter().map(|(id, c)| (id.0, c)), |h, c| {
        h.str(&c.name);
        h.stable_enum(&c.kind);
        h.u8(c.skill);
        hash_location(h, &c.location);
        h.f64(c.salary_per_s);
        h.stable_enum(&c.status);
    });
    h.u32(e.sites.len() as u32);
    for s in e.sites.values() {
        h.str(s.id.as_str());
        h.btree_map(
            &s.inventory,
            |h, r| {
                h.str(r.as_str());
            },
            |h, u| {
                h.f64(*u);
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Ledger + funds primitives — free functions so `apply_breakup`'s
// field-split callers can bank through them without `&mut World`.
// ---------------------------------------------------------------------------

/// Append a ledger entry under the cap — deterministic eviction
/// (oldest first), the `max_debris` rule for bookkeeping.
fn ledger_push(e: &mut EconomyState, tick: u64, delta_micros: i64, reason: FundsReason) {
    if delta_micros == 0 {
        return;
    }
    e.ledger.push_back(LedgerEntry {
        tick,
        delta_micros,
        reason,
    });
    while e.ledger.len() > LEDGER_CAP {
        e.ledger.pop_front();
    }
}

/// Credit `micros` — returns the amount actually credited (always
/// `micros`, saturating on overflow; the `u64::MAX` corner is far
/// beyond any run's reach). Clears `insolvent` when funds land
/// positive: a recovered program re-notifies on its next insolvency.
pub(crate) fn credit(e: &mut EconomyState, tick: u64, micros: u64, reason: FundsReason) {
    if micros == 0 {
        return;
    }
    e.funds_micros = e.funds_micros.saturating_add(micros);
    if e.funds_micros > 0 {
        e.insolvent = false;
    }
    ledger_push(e, tick, micros.min(i64::MAX as u64) as i64, reason);
}

/// Debit up to `micros` — pays `min(micros, funds)`, never negative
/// (§3.5's clamp). Returns `(paid, shortfall)`; the caller decides
/// whether the shortfall schedules `Insolvent` (drains do — the
/// flag check lives in `World::charge_funds`).
pub(crate) fn charge(
    e: &mut EconomyState,
    tick: u64,
    micros: u64,
    reason: FundsReason,
) -> (u64, u64) {
    let paid = micros.min(e.funds_micros);
    if paid > 0 {
        e.funds_micros -= paid;
        ledger_push(e, tick, -(paid.min(i64::MAX as u64) as i64), reason);
    }
    (paid, micros - paid)
}

/// One craft's resolved build cost in micros — `Σ part_costs` over
/// the live graph's effective indices (the `part_costs` table is
/// parallel to `craft.parts`; a node removed by surgery simply stops
/// referencing its index). Non-finite/negative entries count as 0 —
/// the spawn-time `check_part_costs` gate rejects them upstream.
pub fn craft_cost_micros(craft: &crate::CraftDef) -> u64 {
    let mut total: u128 = 0;
    for node in craft.graph.nodes.values() {
        let idx = node.effective_index as usize;
        let c = craft.part_costs.get(idx).copied().unwrap_or(0.0);
        if c.is_finite() && c > 0.0 {
            total = total.saturating_add(to_micros(c) as u128);
        }
    }
    total.min(u64::MAX as u128) as u64
}

/// The vessel's *live-graph* resource recovery credit: `(resource,
/// units)` sums over `tank_contents` at `efficiency`.
pub(crate) fn recovery_resources(
    vessel: &crate::Vessel,
    efficiency: f64,
) -> Vec<(ResourceId, f64)> {
    let mut out: BTreeMap<ResourceId, f64> = BTreeMap::new();
    for stores in vessel.tank_contents.values() {
        for (res, units) in stores {
            if *units > 0.0 {
                *out.entry(res.clone()).or_insert(0.0) += units * efficiency;
            }
        }
    }
    out.into_iter().collect()
}

/// Lazy-shadow capture for the economy book (the `shadow_science`
/// precedent — one clone per tick that mutates it).
pub(crate) fn shadow_economy(world_economy: &EconomyState, slot: &mut Option<EconomyState>) {
    if slot.is_none() {
        *slot = Some(world_economy.clone());
    }
}

/// Roster bookkeeping for seats that change vessels: every member
/// still recording `CrewLocation::Vessel(from)` re-points at `to`
/// — the roster↔seat pairing the restore gate enforces in both
/// directions (`location == Vessel(carrier)`), so every split/merge
/// writer must keep the roster side true or the next snapshot
/// `Fatal`s. Id-only `BoardCrew` members (no roster record) and
/// members whose record already disagrees are skipped — the
/// `location == Vessel(from)` gate is the same one the recovery
/// path uses before writing `Unassigned`.
pub(crate) fn reseat_roster(
    economy: &mut EconomyState,
    from: VesselId,
    to: VesselId,
    crew: &[crate::transfers::VesselCrew],
) {
    for c in crew {
        if let Some(m) = economy.crew.get_mut(&c.id)
            && m.location == CrewLocation::Vessel(from)
        {
            m.location = CrewLocation::Vessel(to);
        }
    }
}

/// Roster bookkeeping for seats that leave the simulation: every
/// member still recording `CrewLocation::Vessel(vessel)` goes `Lost`
/// + `Unassigned` — the `fire_vessel_ls_depleted` precedent.
///
/// Called by every path that destroys or removes seat-carrying mass
/// (jettison, breakup, vessel removal) with the crew list as it was
/// *before* the removal. Callers shadow the economy book first.
pub(crate) fn lose_aboard_crew(
    economy: &mut EconomyState,
    vessel: VesselId,
    crew: &[crate::transfers::VesselCrew],
) {
    for c in crew {
        if let Some(m) = economy.crew.get_mut(&c.id)
            && m.location == CrewLocation::Vessel(vessel)
        {
            m.status = CrewStatus::Lost;
            m.location = CrewLocation::Unassigned;
        }
    }
}

// ---------------------------------------------------------------------------
// World impl — commands, the salary event, observation.
// ---------------------------------------------------------------------------

impl World {
    /// The economy book — observation only (tests, the harness, UI).
    pub fn economy(&self) -> &EconomyState {
        &self.economy
    }

    /// Liquid funds in micro-units (`MICROS_PER_FUND` per fund).
    pub fn funds_micros(&self) -> u64 {
        self.economy.funds_micros
    }

    /// The configured economy tunables.
    pub fn economy_config(&self) -> EconomyConfig {
        self.economy_cfg
    }

    /// The economy tunables — post-construction setter for worlds
    /// built from `balance.ron` (the `set_transfer_config` pattern —
    /// a construction-config write: call before the first
    /// `step_frame`). Also (re)seeds `funds_micros` from
    /// `starting_funds_micros`, since the starting balance is part of
    /// the same authored economy a data-built world runs under.
    /// An invalid config (the `is_valid` gate every apply runs) is
    /// rejected `Recoverable` rather than installed: the config lands
    /// in hashed state, and a bad value in it would otherwise poison
    /// every downstream check with no honest surface.
    pub fn set_economy_config(&mut self, cfg: EconomyConfig) -> Result<(), SimError> {
        if !cfg.is_valid() {
            return Err(SimError::Recoverable("economy: invalid config"));
        }
        self.economy_cfg = cfg;
        self.economy.funds_micros = cfg.starting_funds_micros;
        Ok(())
    }

    /// Install the site's def table — construction config (the
    /// `set_experiments` precedent: data-derived, handed in, never
    /// serialized or hashed; `WorldConfig` carries it so §7.6
    /// replays rebuild it). Reconciles the `economy.sites` arena to
    /// the def set — a pre-run write like every `set_*`; an existing
    /// state's inventory survives a same-id re-install. A fresh row
    /// seeds from the def's `starting_stock` (§3.5 v3): the launch
    /// site's authored stockpile is what `ManufactureCraft` BOMs and
    /// `RunBenchTest` stands spend.
    pub fn set_sites(&mut self, defs: BTreeMap<SiteId, SiteDef>) {
        let states = &mut self.economy.sites;
        states.retain(|id, _| defs.contains_key(id));
        for (id, def) in &defs {
            states.entry(id.clone()).or_insert_with(|| SiteState {
                id: id.clone(),
                // The arena's seed — `starting_stock` is a def-list
                // (duplicate resources would need aggregation; the
                // validator forbids them, but aggregate anyway so a
                // hand-built def can never mint a corrupt row).
                inventory: def
                    .starting_stock
                    .iter()
                    .fold(BTreeMap::new(), |mut m, (r, u)| {
                        *m.entry(r.clone()).or_insert(0.0) += u;
                        m
                    }),
            });
        }
        self.site_defs = defs;
    }

    /// The installed site defs (construction config).
    pub fn site_defs(&self) -> &BTreeMap<SiteId, SiteDef> {
        &self.site_defs
    }

    /// Credit `micros` under a reason — the world-level wrapper:
    /// shadows the economy book, credits, appends the ledger entry.
    /// Used by every income path (recovery, milestone, contract
    /// payout) and by `credit_science_payout` below.
    pub(crate) fn credit_funds(
        &mut self,
        micros: u64,
        reason: FundsReason,
        shadow: &mut TickShadow,
    ) {
        if micros == 0 {
            return;
        }
        self.shadow_economy(shadow);
        credit(&mut self.economy, self.clock.ticks(), micros, reason);
    }

    /// Debit `micros` — §3.5's clamp: pays `min(amount, funds)`; a
    /// shortfall marks `insolvent` and schedules the `Clamping`
    /// `Insolvent` event at `clock + 1` (module doc). Returns the
    /// paid amount.
    pub(crate) fn charge_funds(
        &mut self,
        micros: u64,
        reason: FundsReason,
        shadow: &mut TickShadow,
    ) -> Result<u64, SimError> {
        if micros == 0 {
            return Ok(0);
        }
        self.shadow_economy(shadow);
        let (paid, shortfall) = charge(&mut self.economy, self.clock.ticks(), micros, reason);
        if shortfall > 0 && !self.economy.insolvent {
            self.economy.insolvent = true;
            let at = self.clock.ticks().saturating_add(1);
            self.schedule_event(
                EventKind::Insolvent {
                    shortfall_micros: shortfall,
                },
                at,
                shadow,
            )?;
            // C08-F3 §3.5: the same clamp suspends the program's
            // facility surfaces — dormant IS the `insolvent` flag
            // (any credit lifting funds above zero restores them),
            // so the announcement rides the same once-per-insolvency
            // gate: one `Clamping` event + one notification-log entry
            // per insolvency.
            self.schedule_event(EventKind::FacilityDormant, at, shadow)?;
            self.notify(
                crate::reliability::Notification {
                    tick: self.clock.ticks(),
                    vessel: crate::reliability::PROGRAM_VESSEL,
                    node: None,
                    kind: crate::reliability::NotificationKind::FacilityDormant,
                },
                shadow,
            );
        }
        Ok(paid)
    }

    /// The science-milestone payout — `banked` science just banked
    /// credits `science_funds_per_point` funds/point. Every site that
    /// adds to `science.banked` calls this next (module doc: the
    /// "science milestones ─► funds" edge of the §3.5 loop).
    pub(crate) fn credit_science_payout(&mut self, banked: f64, shadow: &mut TickShadow) {
        let micros = to_micros(banked * self.economy_cfg.science_funds_per_point);
        self.credit_funds(micros, FundsReason::ScienceMilestone, shadow);
    }

    /// The `Contracts` slot's publish step (§2.6.5's "publish its
    /// next event time"): ensure exactly one `SalaryDue` sits on the
    /// absolute period grid — the lazily-published cadence (module
    /// doc). The publish is gated on a non-empty `Active` roster — a
    /// program with no payroll has no salary to drain and gets no
    /// perpetual no-op event (a pure function of `(clock, heap,
    /// roster)`, so the first publish lands identically in every
    /// §2.6.1 partition, and a crew-less world's heap stays empty).
    pub(crate) fn publish_economy(&mut self, shadow: &mut TickShadow) -> Result<(), SimError> {
        let period = self.economy_cfg.salary_period_ticks;
        if period == 0
            || !self
                .economy
                .crew
                .values()
                .any(|c| c.status == CrewStatus::Active)
        {
            return Ok(());
        }
        if self
            .event_heap
            .iter()
            .any(|e| matches!(e.kind, EventKind::SalaryDue))
        {
            return Ok(());
        }
        // The `fire_salary_due` re-arm convention: saturating math,
        // strictly ahead of the clock — a `clock` near `u64::MAX`
        // saturates to a far-future grid point rather than panicking
        // on the unchecked `+`/`*`.
        let next = (self.clock.ticks() / period)
            .saturating_add(1)
            .saturating_mul(period)
            .max(self.clock.ticks().saturating_add(1));
        self.schedule_event(EventKind::SalaryDue, next, shadow)
    }

    /// `SalaryDue` firing: debit `Σ salary_per_s · period_s` over the
    /// `Active` roster plus `site_upkeep_funds_per_s · period_s` per
    /// installed site (the §3.5 "salaries + facility upkeep"
    /// recurring drain — the site arena IS the facility surface:
    /// launch pads and test stands), then re-arm one period further
    /// along the absolute grid. Both charges clamp into `Insolvent`
    /// at zero — §3.5's warp-to-the-window bankruptcy — and the
    /// salary shortfall is one unpaid period on
    /// `unpaid_salary_streak`: past the authored
    /// `attrition_grace_periods` each further missed payroll retires
    /// one `Active` member — the highest-salary first (attrition
    /// relieves the drain fastest), lowest `CrewId` on ties — with a
    /// `StaffAttrition` event + log entry (module doc). A fully paid
    /// period resets the streak.
    pub(crate) fn fire_salary_due(
        &mut self,
        event_tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let period = self.economy_cfg.salary_period_ticks;
        let period_s = period as f64 / TICK_HZ as f64;
        let due: f64 = self
            .economy
            .crew
            .values()
            .filter(|c| c.status == CrewStatus::Active)
            .map(|c| c.salary_per_s)
            .sum();
        let salary_micros = to_micros(due * period_s);
        let upkeep_micros = to_micros(
            self.economy_cfg.site_upkeep_funds_per_s * period_s * self.economy.sites.len() as f64,
        );
        let paid_salary = self.charge_funds(salary_micros, FundsReason::Salary, shadow)?;
        self.charge_funds(upkeep_micros, FundsReason::SiteUpkeep, shadow)?;
        // C08-F3 §3.5's `StaffAttrition` — sustained insolvency makes
        // staff leave, never silently: `Retired` members stay on the
        // roster (the §7.2 crew arena) but draw nothing.
        if paid_salary < salary_micros {
            self.shadow_economy(shadow);
            self.economy.unpaid_salary_streak = self.economy.unpaid_salary_streak.saturating_add(1);
            if self.economy.unpaid_salary_streak > self.economy_cfg.attrition_grace_periods {
                // Highest-salary `Active` member first, lowest id on
                // ties — the ascending-BTreeMap first-wins scan is the
                // monotonic-id order mutation elsewhere requires.
                let leaver = self
                    .economy
                    .crew
                    .iter()
                    .filter(|(_, c)| c.status == CrewStatus::Active)
                    .fold(None, |best, (id, c)| match best {
                        Some((_, s)) if c.salary_per_s <= s => best,
                        _ => Some((*id, c.salary_per_s)),
                    })
                    .map(|(id, _)| id);
                if let Some(crew) = leaver {
                    if let Some(m) = self.economy.crew.get_mut(&crew) {
                        m.status = CrewStatus::Retired;
                    }
                    self.schedule_event(
                        EventKind::StaffAttrition { crew },
                        self.clock.ticks().saturating_add(1),
                        shadow,
                    )?;
                    self.notify(
                        crate::reliability::Notification {
                            tick: self.clock.ticks(),
                            vessel: crate::reliability::PROGRAM_VESSEL,
                            node: None,
                            kind: crate::reliability::NotificationKind::StaffAttrition {
                                crew: crew.0,
                            },
                        },
                        shadow,
                    );
                }
            }
        } else if self.economy.unpaid_salary_streak != 0 {
            self.shadow_economy(shadow);
            self.economy.unpaid_salary_streak = 0;
        }
        // Re-arm at the next grid tick — `event_tick + period` keeps
        // the cadence absolute even when a deferred batch fires the
        // event at `clock` (`.max(clock+1)` covers the same corner).
        // An emptied roster lets the event die — the `Contracts`
        // slot's publish re-creates it if anyone returns to Active.
        if !self
            .economy
            .crew
            .values()
            .any(|c| c.status == CrewStatus::Active)
        {
            return Ok(());
        }
        let next = event_tick
            .saturating_add(period)
            .max(self.clock.ticks().saturating_add(1));
        self.schedule_event(EventKind::SalaryDue, next, shadow)
    }

    /// `HireCrew` — §3.5's "hire → `Facility`" first step: charge the
    /// hire cost and add the member to the roster `Unassigned`
    /// (facilities land with M8's economy; `Unassigned` is the
    /// roster's honest "on the books, nowhere posted" slot). The hire
    /// is an *action* — unaffordable rejects `Recoverable`, it never
    /// clamps (module doc).
    pub(crate) fn apply_hire_crew(
        &mut self,
        name: &str,
        kind: CrewKind,
        skill: u8,
        salary_per_s: f64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !(salary_per_s.is_finite() && salary_per_s >= 0.0) {
            return Err(SimError::Recoverable("hire: non-finite or negative salary"));
        }
        // C08-F7: skill rides the command now — the roster record is
        // the truth `StartResearch` cross-checks against (the
        // hardcoded `skill: 0` made every hire identical). `1..=5`
        // is the hire band: 0 is §3.5's type-level floor, not a
        // purchase — a skill-0 member draws salary while its lab
        // contribution (`Σ skill`) is provably zero.
        if !(1..=5).contains(&skill) {
            return Err(SimError::Recoverable("hire: skill outside 1..=5"));
        }
        // Skilled labour costs more: each point of skill bills the
        // authored `crew_hire_cost_funds`, so the hire band is a real
        // price/quality choice rather than always-max-skill.
        let cost = self
            .economy_cfg
            .hire_cost_micros
            .saturating_mul(u64::from(skill));
        if cost > self.economy.funds_micros {
            return Err(SimError::Recoverable("hire: insufficient funds"));
        }
        self.shadow_economy(shadow);
        self.shadow_transfers(shadow);
        let id = CrewId(self.next_crew_id);
        self.next_crew_id = self
            .next_crew_id
            .checked_add(1)
            .ok_or(SimError::Fatal("crew id counter overflow"))?;
        self.economy.crew.insert(
            id,
            CrewMember {
                name: name.to_string(),
                kind,
                skill,
                location: CrewLocation::Unassigned,
                salary_per_s,
                status: CrewStatus::Active,
            },
        );
        charge(
            &mut self.economy,
            self.clock.ticks(),
            cost,
            FundsReason::CrewHire,
        );
        Ok(())
    }

    /// `ContractReward` — the M7-D stub interface (module doc): a
    /// signed micro amount applied to funds with a `ContractReward`
    /// ledger reason. Negative amounts run the clamp path like every
    /// drain — a penalty you cannot pay is an insolvency, not a
    /// rejection.
    pub(crate) fn apply_contract_reward(
        &mut self,
        micros: i64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if micros >= 0 {
            self.credit_funds(micros as u64, FundsReason::ContractReward, shadow);
        } else {
            self.charge_funds(micros.unsigned_abs(), FundsReason::ContractReward, shadow)?;
        }
        Ok(())
    }

    /// `RecoverVessel` — §3.5's flight recovery (module doc). The
    /// vessel must be `SurfaceLatched`; a named `site` must sit on
    /// the latched body and takes the resource credit at its
    /// efficiency (1.0 launch, `recovery_efficiency` otherwise);
    /// `site: None` is the 0.25 remote salvage with no inventory
    /// credit. Resource contents, part value, crew, and pooled
    /// science all resolve before the `remove_vessel` scrub.
    #[allow(clippy::too_many_lines)] // the compute-then-commit list grew with the science close
    pub(crate) fn apply_recover_vessel(
        &mut self,
        vessel: VesselId,
        site: &Option<SiteId>,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.economy_cfg.is_valid() {
            return Err(SimError::Recoverable("recover: invalid economy config"));
        }
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("recover: unknown vessel"));
        };
        let Some(craft) = &v.craft else {
            return Err(SimError::Recoverable("recover: vessel has no craft"));
        };
        let latched_body = match &v.state {
            crate::VesselState::SurfaceLatched(p) => p.body.clone(),
            _ => return Err(SimError::Recoverable("recover: vessel not on the surface")),
        };
        let efficiency = match site {
            Some(id) => {
                let Some(def) = self.site_defs.get(id) else {
                    return Err(SimError::Recoverable("recover: unknown site"));
                };
                if def.body != latched_body {
                    return Err(SimError::Recoverable("recover: site on a different body"));
                }
                if def.launch {
                    1.0
                } else {
                    self.economy_cfg.recovery_efficiency
                }
            }
            None => self.economy_cfg.abandon_recovery,
        };
        // Compute every effect first, then commit — the apply stays
        // one atomic mutation under its command shadow.
        let resource_credit = match site {
            Some(id) if self.economy.sites.contains_key(id) => recovery_resources(v, efficiency),
            Some(_) => return Err(SimError::Recoverable("recover: site has no state")),
            None => Vec::new(),
        };
        let part_micros = ((craft_cost_micros(craft) as u128)
            .saturating_mul((efficiency * MICROS_PER_FUND) as u128)
            / (MICROS_PER_FUND as u128))
            .min(u64::MAX as u128) as u64;
        let crew_home: Vec<CrewId> = v.crew.iter().map(|c| c.id).collect();
        self.shadow_vessels(shadow);
        self.shadow_economy(shadow);
        self.shadow_science(shadow);
        // C08-F1: a running lab project holds its data in escrow —
        // invisible to the pooled loop below, so dropping it with the
        // vessel (`drop_vessel_projects`'s "no accrual" rule) silently
        // destroys both the accrued science AND the unconverted
        // remainder on a recovery the UI frames as banking. Finalize
        // the vessel's projects at the recovery tick instead — the
        // `apply_stop_research` contract: accrual banks, the remnant
        // returns to the pool, where it prices like any pooled unit
        // (fraction + close rules below). The pending
        // Exhausted/Complete pair is scrubbed inside
        // `finalize_projects_on_nodes`, so the heap shadows first; a
        // lab-less recovery sees an empty set and skips everything.
        self.shadow_events_whole(shadow);
        let lab_nodes: std::collections::BTreeSet<csp_phys::PartNodeId> = self
            .science
            .projects
            .values()
            .filter(|p| p.vessel == vessel)
            .map(|p| p.lab_node)
            .collect();
        if !lab_nodes.is_empty() {
            let (remnants, lab_banked) =
                self.finalize_projects_on_nodes(vessel, &lab_nodes, self.clock.ticks());
            if let Some(v) = self.vessels.get_mut(&vessel) {
                for (node, key, units) in remnants {
                    *v.science
                        .data
                        .entry(node)
                        .or_default()
                        .entry(key)
                        .or_insert(0.0) += units;
                }
            }
            // The banked accrual pays out at the milestone edge — the
            // same `credit_science_payout` + ScienceBanked pairing the
            // pooled loop and the Exhausted firing use.
            self.credit_science_payout(lab_banked, shadow);
            if lab_banked > 0.0 {
                self.notify(
                    crate::reliability::Notification {
                        tick: self.clock.ticks(),
                        vessel: crate::reliability::PROGRAM_VESSEL,
                        node: None,
                        kind: crate::reliability::NotificationKind::ScienceBanked {
                            micros: (lab_banked * 1.0e6) as u64,
                        },
                    },
                    shadow,
                );
            }
        }
        // Pooled science banks at the §3.2 recover fraction per key —
        // closed only where the closed-key invariant is safe (no
        // other vessel pools the key, no live project holds it — the
        // vessel's own projects finalized above, so their keys are
        // closeable again). The `1 − transmit_ratio` remainder is the
        // per-key blanket
        // [`crate::research::ScienceKeyState::transmitted`] names: it
        // applies to all units pooled at recovery, including any
        // collected after the transmit and the lab remnants just
        // returned above. Computed here, after the finalize: the
        // remnant return mutates the pool this loop prices.
        let mut science_bank: Vec<(crate::research::DataKey, f64, bool)> = Vec::new();
        if let Some(v) = self.vessels.get(&vessel) {
            for m in v.science.data.values() {
                for (key, units) in m {
                    if *units <= 0.0 || !units.is_finite() {
                        continue;
                    }
                    let prior = self.science.keys.get(key).copied().unwrap_or_default();
                    if prior.closed {
                        continue;
                    }
                    let fraction = if prior.transmitted {
                        self.experiments
                            .get(&key.experiment)
                            .map_or(0.0, |d| (1.0 - d.transmit_ratio).max(0.0))
                    } else {
                        1.0
                    };
                    let closable = !self
                        .vessels
                        .iter()
                        .any(|(id, o)| *id != vessel && o.science.units_at(key) > 0.0)
                        && !self.science.projects.values().any(|p| p.key == *key);
                    science_bank.push((key.clone(), units * fraction, closable));
                }
            }
        }
        if let Some(id) = site
            && let Some(st) = self.economy.sites.get_mut(id)
        {
            for (res, units) in resource_credit {
                *st.inventory.entry(res).or_insert(0.0) += units;
            }
        }
        for (key, banked, closable) in science_bank {
            self.science.banked += banked;
            self.credit_science_payout(banked, shadow);
            self.notify(
                crate::reliability::Notification {
                    tick: self.clock.ticks(),
                    vessel: crate::reliability::PROGRAM_VESSEL,
                    node: None,
                    kind: crate::reliability::NotificationKind::ScienceBanked {
                        micros: (banked * 1.0e6) as u64,
                    },
                },
                shadow,
            );
            if closable {
                self.science.keys.entry(key).or_default().closed = true;
            }
        }
        for id in crew_home {
            if let Some(m) = self.economy.crew.get_mut(&id)
                && m.location == CrewLocation::Vessel(vessel)
            {
                m.location = CrewLocation::Unassigned;
            }
        }
        credit(
            &mut self.economy,
            self.clock.ticks(),
            part_micros,
            FundsReason::Recovery,
        );
        self.remove_vessel(vessel, shadow);
        // M7-D: the recovery is a contract trigger — the rescue
        // predicate needs the body the vessel sat on (gone now —
        // the event carries it) and the survey predicate needs the
        // banking that just landed.
        self.contract_notify(
            &crate::contracts::ContractTrigger::VesselRecovered(latched_body),
            shadow,
        )?;
        self.contract_notify(&crate::contracts::ContractTrigger::DataBanked, shadow)
    }
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{
        CraftDef, Lease, SimCommand, SourceClass, SurfacePose, Vessel, VesselScience, VesselState,
        VesselThermal,
    };
    use csp_core::DQuat;
    use csp_data::BodyId;

    /// A one-node craft with `cost` — recovery's part-value input.
    fn cost_craft(cost: f64) -> CraftDef {
        let mut graph = csp_phys::PartGraph::new(csp_phys::PartNodeId(0));
        graph.nodes.insert(
            csp_phys::PartNodeId(0),
            csp_phys::PartNode::new(None, csp_phys::PartTransform::IDENTITY, 0),
        );
        CraftDef {
            hash_memo: Default::default(),
            part_ids: vec![],
            graph,
            parts: vec![csp_phys::WeldPart {
                experiments: Vec::new(),
                lab: None,
                engine: None,
                parachute: None,
                dock_ports: vec![],
                tanks: vec![csp_phys::TankSpec {
                    resource: ResourceId::from("liquid_fuel"),
                    capacity: 100.0,
                    mass_per_unit_kg: 1.0,
                }],
                crew_seats: 0,
                structural: csp_data::StructuralDef::unlimited(),
                mass_kg: 100.0,
                com: csp_core::DVec3::ZERO,
                inertia: csp_phys::SymMat3::solid_box(100.0, csp_core::DVec3::new(0.5, 0.5, 0.5)),
                collider: csp_phys::ColliderShape::Box {
                    half_extents: csp_core::DVec3::new(0.5, 0.5, 0.5),
                },
                force_sites: vec![],
                aero: csp_data::AeroDef::constant(0.0, 0.0, 1.0),
                thermal: csp_data::ThermalDef {
                    heat_capacity_j_per_k: 100_000.0,
                    max_temp_k: 1_000.0,
                    emissivity: 0.8,
                    conduction_w_per_k: 10.0,
                },
            }],
            part_costs: vec![cost],
            reliability: vec![],
        }
    }

    /// A latched vessel on `body` with `cost`-funds of parts and
    /// `fuel` units of liquid_fuel aboard.
    fn latched_vessel(w: &mut World, cost: f64, fuel: f64) -> VesselId {
        let id = VesselId(w.next_vessel_id);
        w.next_vessel_id += 1;
        let mut tanks = BTreeMap::new();
        let mut stores = BTreeMap::new();
        stores.insert(ResourceId::from("liquid_fuel"), fuel);
        tanks.insert(csp_phys::PartNodeId(0), stores);
        w.vessels.insert(
            id,
            Vessel {
                id,
                craft: Some(cost_craft(cost)),
                state: VesselState::SurfaceLatched(SurfacePose {
                    body: BodyId::from("terrin"),
                    latitude_rad: 0.0,
                    longitude_rad: 0.0,
                    altitude_m: 0.0,
                    orientation: DQuat::IDENTITY,
                }),
                aero_mask: csp_phys::ConfigMask::ALL,
                thermal: VesselThermal::default(),
                debris_since: None,
                maneuver: None,
                seams: Vec::new(),
                tank_contents: tanks,
                crew: Vec::new(),
                science: VesselScience::default(),
                joined_colony: None,
                reliability: BTreeMap::new(),
                control_lease: Lease::Player,
                deployed: BTreeMap::new(),
            },
        );
        id
    }

    fn site(id: &str, launch: bool) -> (SiteId, SiteDef) {
        (
            SiteId::from(id),
            SiteDef {
                schema_version: 1,
                id: SiteId::from(id),
                display_name: csp_data::LocKey::from(format!("site.{id}")),
                body: BodyId::from("terrin"),
                latitude_rad: 0.0,
                longitude_rad: 0.0,
                altitude_m: 0.0,
                launch,
                resource_yields: vec![],
                starting_stock: vec![],
            },
        )
    }

    /// A test world: 1 000 funds seeded, a 1-fund hire cost, and a
    /// 100-s salary period (6 400 ticks) so the grid is cheap to walk.
    fn world() -> World {
        let mut w = World::new();
        w.set_economy_config(EconomyConfig {
            starting_funds_micros: 1_000_000_000_000,
            hire_cost_micros: 1_000_000,
            salary_period_ticks: 6_400,
            ..default_config()
        })
        .unwrap();
        w
    }

    /// Funds transactions: hire debits exactly the hire cost; the
    /// ledger records it; a second hire clamps nothing.
    #[test]
    fn hire_debits_and_ledgers() {
        let mut w = world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: CrewKind::Pilot,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(1).unwrap();
        let hire = w.economy_config().hire_cost_micros;
        assert_eq!(w.funds_micros(), 1_000_000_000_000 - hire);
        assert_eq!(w.economy().crew.len(), 1);
        assert_eq!(w.economy().ledger.len(), 1);
        assert_eq!(w.economy().ledger[0].delta_micros, -(hire as i64));
        assert_eq!(w.economy().ledger[0].reason, FundsReason::CrewHire);
    }

    /// An unaffordable hire is a Recoverable rejection — funds and
    /// roster untouched, no clamp, no Insolvent.
    #[test]
    fn unaffordable_hire_rejects() {
        let mut w = world();
        w.economy.funds_micros = 2_500_000; // 2.5 funds — two hires fit
        for i in 0..4 {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::HireCrew {
                    name: format!("s{i}"),
                    kind: CrewKind::Scientist,
                    skill: 1,
                    salary_per_s: 0.0,
                },
            );
        }
        let r = w.step_frame(1).unwrap();
        assert_eq!(r.rejected.len(), 2, "the world afforded all four hires");
        assert_eq!(w.economy().crew.len(), 2);
        assert!(!w.economy().insolvent);
    }

    /// C08-F7: `skill` is a hire parameter — the roster record
    /// carries it verbatim (the `StartResearch` cross-check reads
    /// it), the fee scales per point (`hire_cost · skill` so the
    /// 1..=5 band is a real price/quality choice), and the band
    /// itself is enforced — 0 is the type-level floor for legacy
    /// records, not a purchase.
    #[test]
    fn hire_records_skill_and_scales_cost() {
        let mut w = world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "ace".into(),
                kind: CrewKind::Pilot,
                skill: 3,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(1).unwrap();
        let hire = w.economy_config().hire_cost_micros;
        assert_eq!(w.funds_micros(), 1_000_000_000_000 - 3 * hire);
        assert_eq!(w.economy().crew[&CrewId(0)].skill, 3);
        // The band refuses outright — skill 0 and skill 6 both.
        for skill in [0u8, 6] {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::HireCrew {
                    name: "x".into(),
                    kind: CrewKind::Pilot,
                    skill,
                    salary_per_s: 0.0,
                },
            );
        }
        let r = w.step_frame(2).unwrap();
        assert_eq!(r.rejected.len(), 2, "{:?}", r.rejected);
        assert_eq!(w.economy().crew.len(), 1);
    }

    /// The salary drain: `Σ salary_per_s · period_s` debited exactly
    /// once per `salary_period_ticks` on the absolute grid — the
    /// periodicity check the §3.5 recurring-cost contract needs.
    #[test]
    fn salary_drains_on_the_grid() {
        let mut w = world();
        let period = w.economy_config().salary_period_ticks;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: CrewKind::Engineer,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(1).unwrap();
        let before = w.funds_micros();
        w.step_frame(period + 1).unwrap();
        let period_s = period as f64 / 64.0;
        let expected = to_micros(1.0 * period_s);
        assert_eq!(before - w.funds_micros(), expected);
        // Exactly one Salary ledger entry per firing.
        assert_eq!(
            w.economy()
                .ledger
                .iter()
                .filter(|e| e.reason == FundsReason::Salary)
                .count(),
            1
        );
        w.step_frame(2 * period + 1).unwrap();
        assert_eq!(before - w.funds_micros(), 2 * expected);
    }

    /// Insolvency: a salary the funds can't cover pays what's left,
    /// clamps at zero, marks `insolvent`, and fires the `Clamping`
    /// `Insolvent` event exactly once — a second firing while still
    /// broke does not re-announce.
    #[test]
    fn salary_shortfall_clamps_and_announces_once() {
        let mut w = world();
        let period = w.economy_config().salary_period_ticks;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: CrewKind::Pilot,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(1).unwrap();
        w.economy.funds_micros = 1_000; // nearly broke
        let r = w.step_frame(period + 1).unwrap();
        assert_eq!(w.funds_micros(), 0);
        assert!(w.economy().insolvent);
        // The Insolvent event lands at clock+1 = period+1 — inside
        // this same step (the clamp scheduled it while the clock was
        // still at `period`).
        assert!(
            r.fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::Insolvent { .. })),
            "insolvency never fired: {r:?}"
        );
        // A second salary firing while insolvent announces nothing new.
        w.step_frame(w.clock().ticks() + period).unwrap();
        let mut again = 0usize;
        for _ in 0..period + 4 {
            let r3 = w.step_frame(w.clock().ticks() + 1).unwrap();
            again += r3
                .fired_events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::Insolvent { .. }))
                .count();
        }
        assert_eq!(again, 0);
    }

    /// Recovery at the launch site: efficiency 1.0 — full contents to
    /// inventory, full part value to funds, vessel gone.
    #[test]
    fn recovery_at_launch_site() {
        let mut w = world();
        w.set_sites([site("terrin-launch", true)].into_iter().collect());
        w.economy.funds_micros = 0;
        let v = latched_vessel(&mut w, 400.0, 50.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RecoverVessel {
                vessel: v,
                site: Some(SiteId::from("terrin-launch")),
            },
        );
        w.step_frame(1).unwrap();
        assert!(!w.vessels().contains_key(&v));
        assert_eq!(w.funds_micros(), to_micros(400.0));
        let inv = &w.economy().sites[&SiteId::from("terrin-launch")].inventory;
        assert!((inv[&ResourceId::from("liquid_fuel")] - 50.0).abs() < 1e-9);
    }

    /// Recovery at an ordinary site: `recovery_efficiency` 0.9 — the
    /// same vessel credits 90% of contents and 90% of part value.
    #[test]
    fn recovery_at_ordinary_site() {
        let mut w = world();
        w.set_sites([site("terrin-downrange", false)].into_iter().collect());
        w.economy.funds_micros = 0;
        let v = latched_vessel(&mut w, 400.0, 50.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RecoverVessel {
                vessel: v,
                site: Some(SiteId::from("terrin-downrange")),
            },
        );
        w.step_frame(1).unwrap();
        assert_eq!(w.funds_micros(), to_micros(400.0 * 0.9));
        let inv = &w.economy().sites[&SiteId::from("terrin-downrange")].inventory;
        assert!((inv[&ResourceId::from("liquid_fuel")] - 45.0).abs() < 1e-9);
    }

    /// Abandon recovery (site None): `abandon_recovery` 0.25 — funds
    /// get a quarter of the part value; nothing lands in inventory.
    #[test]
    fn abandon_recovery_pays_quarter() {
        let mut w = world();
        w.economy.funds_micros = 0;
        let v = latched_vessel(&mut w, 400.0, 50.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RecoverVessel {
                vessel: v,
                site: None,
            },
        );
        w.step_frame(1).unwrap();
        assert_eq!(w.funds_micros(), to_micros(400.0 * 0.25));
        assert!(w.economy().sites.is_empty());
    }

    /// Recovery refusals stay typed: unknown vessel, not-latched
    /// vessel, unknown site, wrong-body site — all `Recoverable`.
    #[test]
    fn recovery_rejects_cleanly() {
        let mut w = world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RecoverVessel {
                vessel: VesselId(99),
                site: None,
            },
        );
        let r = w.step_frame(1).unwrap();
        assert_eq!(r.rejected.len(), 1);
        assert!(matches!(r.rejected[0].error, SimError::Recoverable(_)));
    }

    /// The contract-reward stub: positive credits, negative runs the
    /// clamp path (and can produce Insolvent) — the interface M7-D
    /// drives is already the real funds path.
    #[test]
    fn contract_reward_stub_moves_funds() {
        let mut w = world();
        w.economy.funds_micros = 1_000;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ContractReward { micros: 500 },
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ContractReward { micros: -3_000 },
        );
        w.step_frame(1).unwrap();
        assert_eq!(w.funds_micros(), 0);
        assert!(w.economy().insolvent);
        assert_eq!(
            w.economy()
                .ledger
                .iter()
                .map(|e| e.delta_micros)
                .collect::<Vec<_>>(),
            vec![500, -1_500]
        );
    }

    /// Serialization + hashing: the economy book survives
    /// snapshot→restore byte-for-byte, and funds sensitivity shows up
    /// in `state_hash` — two worlds that differ only in balance hash
    /// differently.
    #[test]
    fn economy_serializes_and_hashes() {
        let mut a = world();
        let mut b = world();
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
        b.economy.funds_micros += 1;
        assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());
        // Snapshot → restore preserves the whole book — the C08-F3
        // streak counter included.
        b.economy.insolvent = false; // keep the flag coherent
        b.economy.unpaid_salary_streak = 2;
        let snap = b.snapshot().unwrap();
        let r = World::restore(a.orbits().clone(), None, snap).unwrap();
        assert_eq!(r.economy(), b.economy());
        assert_eq!(r.state_hash().unwrap(), b.state_hash().unwrap());
        // Ledger survives (serialized, unhashed).
        a.enqueue(SourceClass::Ui, 0, SimCommand::ContractReward { micros: 7 });
        a.step_frame(1).unwrap();
        let snap2 = a.snapshot().unwrap();
        let r2 = World::restore(a.orbits().clone(), None, snap2).unwrap();
        assert_eq!(r2.economy().ledger.len(), 1);
    }

    /// Partition independence: the same world advanced ×1 vs ×many
    /// produces identical state and hash — the salary grid's absolute
    /// ticks make the drain immune to step partitioning.
    #[test]
    fn salary_is_partition_independent() {
        let period = world().economy_config().salary_period_ticks;
        let mk = || {
            let mut w = world();
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::HireCrew {
                    name: "a".into(),
                    kind: CrewKind::Pilot,
                    skill: 1,
                    salary_per_s: 0.5,
                },
            );
            w.step_frame(1).unwrap();
            w
        };
        let mut x1 = mk();
        let mut x2 = mk();
        // ×1 for two periods vs a single two-period jump.
        for _ in 0..2 * period {
            x1.step_frame(x1.clock().ticks() + 1).unwrap();
        }
        x2.step_frame(2 * period + 1).unwrap();
        assert_eq!(x1.funds_micros(), x2.funds_micros());
        assert_eq!(x1.state_hash().unwrap(), x2.state_hash().unwrap());
    }

    /// C08-F3 §3.5's facility upkeep: every installed site draws
    /// `site_upkeep_funds_per_s · period_s` at the salary boundary,
    /// ledgered under `SiteUpkeep` — the recurring drain's second
    /// half rides the same clamp.
    #[test]
    fn site_upkeep_drains_at_the_salary_boundary() {
        let mut w = world();
        w.set_sites(
            [site("terrin-launch", true), site("terrin-downrange", false)]
                .into_iter()
                .collect(),
        );
        let period = w.economy_config().salary_period_ticks;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: CrewKind::Engineer,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(1).unwrap();
        let before = w.funds_micros();
        w.step_frame(period + 1).unwrap();
        let period_s = period as f64 / 64.0;
        let upkeep = to_micros(w.economy_config().site_upkeep_funds_per_s * period_s * 2.0);
        assert_eq!(before - w.funds_micros(), to_micros(period_s) + upkeep);
        assert_eq!(
            w.economy()
                .ledger
                .iter()
                .filter(|e| e.reason == FundsReason::SiteUpkeep)
                .count(),
            1
        );
    }

    /// C08-F3's `StaffAttrition`: the authored grace (1 unpaid
    /// period) means the first missed payroll only warns —
    /// `Insolvent` + `FacilityDormant` — and each further
    /// consecutive miss retires one member, highest-salary first.
    /// An emptied `Active` roster lets the salary grid die; a paid
    /// month resets the streak.
    #[test]
    fn attrition_retires_after_the_grace() {
        let mut w = world();
        let period = w.economy_config().salary_period_ticks;
        for (name, salary) in [("cheap", 0.5), ("dear", 2.0)] {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::HireCrew {
                    name: name.into(),
                    kind: CrewKind::Pilot,
                    skill: 1,
                    salary_per_s: salary,
                },
            );
        }
        w.step_frame(1).unwrap();
        let id_of = |w: &World, n: &str| {
            w.economy()
                .crew
                .iter()
                .find(|(_, c)| c.name == n)
                .map(|(id, _)| *id)
                .unwrap()
        };
        let (cheap, dear) = (id_of(&w, "cheap"), id_of(&w, "dear"));
        w.economy.funds_micros = 1_000; // nearly broke — the payroll misses
        // Month 1: streak 1 ≤ grace 1 — warns, nobody leaves.
        let r = w.step_frame(period + 1).unwrap();
        assert!(w.economy().insolvent);
        assert!(
            r.fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::FacilityDormant)),
            "no FacilityDormant on the first miss: {r:?}"
        );
        assert!(
            !r.fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::StaffAttrition { .. })),
            "grace period retired someone: {r:?}"
        );
        assert!(
            w.economy()
                .crew
                .values()
                .all(|c| c.status == CrewStatus::Active)
        );
        assert_eq!(w.economy().unpaid_salary_streak, 1);
        // Month 2: streak 2 > grace — the expensive seat goes first,
        // announced by the Clamping event + the log entry.
        let r = w.step_frame(2 * period + 1).unwrap();
        assert!(
            r.fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::StaffAttrition { crew } if crew == dear)),
            "the dear seat did not resign: {r:?}"
        );
        assert_eq!(w.economy().crew[&dear].status, CrewStatus::Retired);
        assert_eq!(w.economy().crew[&cheap].status, CrewStatus::Active);
        assert!(
            w.notifications().iter().any(|n| matches!(
                n.kind,
                crate::reliability::NotificationKind::StaffAttrition { crew } if crew == dear.0
            )),
            "attrition never reached the notification log"
        );
        // Month 3: the cheap seat follows — the emptied Active
        // roster lets the grid die (no further SalaryDue pending).
        let r = w.step_frame(3 * period + 1).unwrap();
        assert!(
            r.fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::StaffAttrition { crew } if crew == cheap)),
            "the cheap seat did not resign: {r:?}"
        );
        assert_eq!(w.economy().crew[&cheap].status, CrewStatus::Retired);
        assert!(
            !w.event_heap
                .iter()
                .any(|e| matches!(e.kind, EventKind::SalaryDue)),
            "the salary grid should die with the Active roster"
        );
        // A paid month after re-solvency resets the streak — the
        // `ContractReward` credit clears `insolvent` and covers the
        // re-hire cost, so the re-armed grid pays clean.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ContractReward { micros: 2_000_000 },
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "new".into(),
                kind: CrewKind::Scientist,
                skill: 1,
                salary_per_s: 0.001,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(!w.economy().insolvent);
        w.step_frame(4 * period + 1).unwrap();
        assert_eq!(w.economy().unpaid_salary_streak, 0);
    }

    /// C08-F3's `FacilityDormant`: while `insolvent` holds, the
    /// facility verbs (`RunBenchTest`, `ManufactureCraft`) reject
    /// dormant — the stands and pads are shut — and a credit lifting
    /// funds above zero restores them.
    #[test]
    fn insolvency_suspends_facilities_until_solvency() {
        let mut w = world();
        let period = w.economy_config().salary_period_ticks;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: CrewKind::Pilot,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(1).unwrap();
        w.economy.funds_micros = 1_000;
        let r = w.step_frame(period + 1).unwrap();
        assert!(w.economy().insolvent);
        assert!(
            r.fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::FacilityDormant))
        );
        assert!(w.notifications().iter().any(|n| matches!(
            n.kind,
            crate::reliability::NotificationKind::FacilityDormant
        )));
        // The dormant gate: a stand order rejects with the dormancy
        // diagnostic — never reaching the stand's own checks.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RunBenchTest {
                site: SiteId::from("terrin-launch"),
                family: csp_data::FamilyId::from("f"),
                units: 1.0,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(r.rejected.len(), 1);
        match &r.rejected[0].error {
            SimError::Recoverable(m) => {
                assert!(m.contains("dormant"), "not the dormant gate: {m}")
            }
            other => panic!("expected a Recoverable dormant rejection, got {other:?}"),
        }
        // Solvency restores: a credit clears `insolvent`, and the
        // same order now fails on its own merits (unknown site) —
        // the dormant gate is gone.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ContractReward { micros: 10_000 },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(!w.economy().insolvent);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RunBenchTest {
                site: SiteId::from("terrin-launch"),
                family: csp_data::FamilyId::from("f"),
                units: 1.0,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(r.rejected.len(), 1);
        match &r.rejected[0].error {
            SimError::Recoverable(m) => {
                assert!(!m.contains("dormant"), "still dormant after solvency: {m}")
            }
            other => panic!("expected a Recoverable rejection, got {other:?}"),
        }
    }
}
