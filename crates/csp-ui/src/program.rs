//! The progression surface (PLAN §3.2–§3.6): one "Program" window that
//! makes the whole fly → science → research → unlock loop visible and
//! playable — program economy (funds / banked science / contracts), the
//! tech tree with spend-science buttons, the focused vessel's experiment
//! modules with run/transmit/recover actions, and per-part reliability
//! state. Pure egui over plain-data mirrors populated by `csp-view`'s
//! `stage_program` — this crate never sees `csp-sim` (§2.1).

/// A tech-tree node row (the `ResearchTech` target).
#[derive(Clone, Debug)]
pub struct TechRow {
    /// `TechNodeId` as a string.
    pub id: String,
    /// Loc-resolved display name (or the id).
    pub name: String,
    /// `science_cost`.
    pub cost: f64,
    /// Already bought.
    pub researched: bool,
    /// Every `requires` edge researched.
    pub prereqs_met: bool,
    /// `banked >= cost`.
    pub affordable: bool,
}

/// An upgrade-track row (the `ResearchUpgradeLevel` target).
#[derive(Clone, Debug)]
pub struct TrackRow {
    /// `UpgradeTrackId` as a string.
    pub id: String,
    /// Loc-resolved display name (or the id).
    pub name: String,
    /// Levels already purchased.
    pub level: u8,
    /// Cap the researched nodes grant (`0` = not purchasable yet).
    pub cap: u8,
    /// Authored level count — `level == levels` is maxed.
    pub levels: u8,
    /// Cost of the next level, when one is purchasable.
    pub next_cost: Option<f64>,
    /// `banked >= next_cost`.
    pub affordable: bool,
}

/// A contract-board offer (the `AcceptContract` target). W1.5-(d):
/// rows carry the drawn params + reward + deadline — accepting was
/// blind when the row was name-only. W2.1 adds the description blurb
/// and the §3.6 prerequisite check so an offer the sim would refuse
/// at accept renders disabled-with-reason instead of dead-clicking.
#[derive(Clone, Debug)]
pub struct OfferRow {
    /// The offer index `AcceptContract` names.
    pub index: u64,
    /// `#index` + loc-resolved template name.
    pub label: String,
    /// The template's loc-resolved description blurb ("" when none).
    pub description: String,
    /// Kind-specific param summary ("orbit terrin: Pe ≥ 100 km …").
    pub detail: String,
    /// The funds lines ("+₣N sign · +₣N done · −₣N expiry").
    pub reward: String,
    /// The authored deadline in days, "" when the template has none.
    pub deadline: String,
    /// The template's §3.6 prerequisites — `(met, text)` pairs.
    pub prereqs: Vec<(bool, String)>,
    /// Every prerequisite holds — the accept predicate's own reading
    /// (W2.1: gated rows say why instead of failing at drain).
    pub accept_ok: bool,
}

/// An accepted contract — the active row with its countdown and a
/// live progress line (W2.1: "delivered 4/20 ore", "band satisfied").
#[derive(Clone, Debug)]
pub struct ActiveRow {
    /// `#id` + loc-resolved template name.
    pub label: String,
    /// Params + completion reward + days-remaining.
    pub detail: String,
    /// The completion predicate's current reading — per-kind progress
    /// ("" when the kind has no measurable intermediate state).
    pub progress: String,
}

/// One experiment module aboard the focused vessel.
#[derive(Clone, Debug)]
pub struct ExperimentRow {
    /// The `PartNodeId` carrying the module.
    pub node: u64,
    /// Display name of the part (loc-resolved or part id).
    pub part: String,
    /// `ExperimentId` as a string.
    pub experiment: String,
    /// `data_units` per run.
    pub data_units: f64,
    /// `reusable` — a one-shot already run reads "spent".
    pub reusable: bool,
    /// Already consumed (one-shot markers).
    pub spent: bool,
    /// The module's `SituationMask` contains the current situation AND
    /// a cap is authored for `(body, situation)` — i.e. a
    /// `RunExperiment` would apply right now.
    pub runnable: bool,
    /// Why it cannot run ("" when runnable).
    pub blocked: String,
}

/// One pooled data record aboard the focused vessel (a `DataKey`).
#[derive(Clone, Debug)]
pub struct DataRow {
    /// Opaque row index — the command references the staged key by it.
    pub index: usize,
    /// `exp @ biome / body · situation`.
    pub label: String,
    /// Units pooled.
    pub units: f64,
    /// Science a transmit would bank (`units · transmit_ratio`).
    pub transmit_yield: f64,
    /// `ElectricCharge` a transmit drains (`units ·
    /// transmit_ec_per_data_unit`) — the sim's gate, staged so the
    /// button can say "needs N EC (have M)" (W1.5-e).
    pub transmit_ec_cost: f64,
    /// Science a surface recovery would bank (1.0 or the `1−ratio`
    /// remainder for a transmitted key).
    pub recover_yield: f64,
    /// The key was already transmitted (recover banks the remainder).
    pub transmitted: bool,
}

/// One part's reliability state on the focused vessel.
#[derive(Clone, Debug)]
pub struct ReliabilityRow {
    /// `PartNodeId` — display only.
    pub node: u64,
    /// Display name of the part.
    pub part: String,
    /// Applied `FailureMode` names.
    pub failures: Vec<String>,
    /// Ignition misfire latched (no thrust until throttle returns to 0).
    pub misfire: bool,
    /// Live thrust multiplier (`< 1.0` = degraded/dead).
    pub thrust_factor: f64,
    /// Λ/θ fraction toward the next failure, `None` when θ is infinite.
    pub wear: Option<f64>,
    /// Dormant-class accrual is in force.
    pub dormant: bool,
    /// §3.4's live stress table — `(rule, active)` per authored
    /// `regime_stress` rule, evaluated at staging on the same
    /// regime metrics the sim's per-tick hazard pass reads
    /// (C10-F2/C22-F5: "displayed to the player in editor and
    /// flight"). Empty when the record resolves no spec. Dormant
    /// rows do not render it — the dormant λ ignores the rules.
    pub stress: Vec<(csp_data::StressRule, bool)>,
    /// The live `Π(stress_i)` over `stress` — the multiplier folded
    /// into the part's `lambda_per_s` (`1.0` while no rule fires).
    pub stress_product: f64,
}

/// A recent reliability/lab event line from the sim's notification log.
#[derive(Clone, Debug)]
pub struct NoticeRow {
    /// Sim tick the notice was recorded at.
    pub tick: u64,
    /// Formatted line ("vessel 2 part 4 — PART FAILED SensorFault").
    pub text: String,
}

// ---------------------------------------------------------------------------
// W2.0/W2.3 — the program-level mirror rows: vessels, roster, colonies,
// sites, labs, agenda. All plain data (§2.1): u64 arena ids cross instead
// of the sim's id newtypes.
// ---------------------------------------------------------------------------

/// Where a roster member sits — the plain-data mirror of the sim's
/// `CrewLocation`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrewWhere {
    /// On the roster, nowhere posted (the hire/recovery landing).
    Unassigned,
    /// Aboard the vessel id.
    Vessel(u64),
    /// Posted at the colony id.
    Colony(u64),
    /// At a facility (id string — forward-declared sim-side).
    Facility(String),
}

/// One roster member (the economy book's `CrewMember`).
#[derive(Clone, Debug)]
pub struct CrewRow {
    /// The `CrewId` the crew commands name.
    pub id: u64,
    /// Display name.
    pub name: String,
    /// Kind label ("pilot"/"engineer"/"scientist"/"tourist").
    pub kind: String,
    /// §3.5 skill, 0..=5.
    pub skill: u8,
    /// `status == Active` — the member draws salary and takes orders.
    pub active: bool,
    /// Status label ("active"/"lost"/"retired").
    pub status: String,
    /// Payroll rate, funds per sim-second.
    pub salary_per_s: f64,
    /// Where the member is posted.
    pub location: CrewWhere,
}

/// One vessel in the program arena — the vessels-in-program row.
#[derive(Clone, Debug)]
pub struct VesselRow {
    /// The `VesselId` the commands name.
    pub id: u64,
    /// Residence label ("in flight"/"on rails"/"landed").
    pub residence: String,
    /// The body it orbits or sits on ("" when none applies).
    pub body: String,
    /// Debris-flagged (§3.5 object caps).
    pub debris: bool,
    /// This is the focused vessel (the science/crew/transfer target).
    pub focused: bool,
    /// Crew ids aboard (roster + id-only members alike).
    pub crew: Vec<u64>,
    /// Free `CrewSeat` slots aboard (net of aboard + inbound).
    pub free_seats: u64,
    /// The colony inventory this vessel joined (`JoinColonyInventory`).
    pub joined_colony: Option<u64>,
    /// The `TransferResource`/`TransferCrew` link precondition holds
    /// against the focused vessel right now.
    pub linked_to_focus: bool,
}

/// One installed colony module row.
#[derive(Clone, Debug)]
pub struct ColonyModuleRow {
    /// Module label ("solar 40 m²" / "drill ore" / "store metal").
    pub label: String,
    /// Inert via `DamageColonyModule`.
    pub damaged: bool,
    /// The allocation pass's granted operating fraction (0..=1).
    pub fraction: f64,
}

/// One colony store row — the epoch-anchored linear reservoir read
/// at the staging tick.
#[derive(Clone, Debug)]
pub struct ColonyStoreRow {
    /// Resource id.
    pub resource: String,
    /// Level at the staged tick (units).
    pub level: f64,
    /// Aggregate capacity (units).
    pub capacity: f64,
    /// Net fill rate (units/s — negative drains).
    pub rate_per_s: f64,
}

/// One colony — the W2.3 panel row that makes the colony sim visible.
#[derive(Clone, Debug)]
pub struct ColonyRow {
    /// The `ColonyId` the colony commands name.
    pub id: u64,
    /// Display name (site's loc-resolved name / site id).
    pub name: String,
    /// Body it sits on.
    pub body: String,
    /// Its site id.
    pub site: String,
    /// State label ("nominal"/"degraded: power lost"/"dormant"/"abandoned").
    pub state: String,
    /// `Nominal`/`Degraded` — the network still runs.
    pub live: bool,
    /// "day"/"night" — the insolation step.
    pub daylight: String,
    /// Crew ids posted here.
    pub crew: Vec<u64>,
    /// C09-F4: the sim's `colony_accepts_crew` mirrored — the colony
    /// can take crew (life support disabled, or an undamaged
    /// `LifeSupport` module stands). Posting buttons dim on false.
    pub has_ls: bool,
    /// Module rows (index = `ModuleIndex`).
    pub modules: Vec<ColonyModuleRow>,
    /// Store rows.
    pub stores: Vec<ColonyStoreRow>,
    /// Held steady-state temperature (K), `None` = no radiating area.
    pub t_eq_k: Option<f64>,
    /// Days until the scheduled inspection, when set.
    pub inspection_days: Option<f64>,
    /// The focused vessel could `JoinColonyInventory` here right now
    /// (latched on this body within the transfer radius, colony live).
    pub joinable: bool,
    /// The focused vessel's `joined_colony` names this colony.
    pub joined: bool,
}

/// One surface site — geography + stockpile + a `FoundColony` slot.
#[derive(Clone, Debug)]
pub struct SiteRow {
    /// The `SiteId` the commands name.
    pub id: String,
    /// Loc-resolved display name.
    pub name: String,
    /// Body it sits on.
    pub body: String,
    /// A launch site (recovery_efficiency 1.0).
    pub launch: bool,
    /// A LIVE colony already occupies the site (C09-F9 — an
    /// `Abandoned` wreck frees the slot for re-founding).
    pub has_colony: bool,
    /// The site's stockpiled inventory (`(resource, units)`).
    pub inventory: Vec<(String, f64)>,
}

/// A lab module aboard the focused vessel.
#[derive(Clone, Debug)]
pub struct LabRow {
    /// The `PartNodeId` carrying the `Lab` module.
    pub node: u64,
    /// Display name of the part.
    pub part: String,
    /// `researcher_slots` — the `StartResearch` crew cap.
    pub slots: u32,
    /// A project already runs on this node.
    pub running: bool,
}

/// A live lab project (the `science.projects` arena row).
#[derive(Clone, Debug)]
pub struct ProjectRow {
    /// The `ResearchId` `StopResearch` names.
    pub id: u64,
    /// The vessel the lab rides.
    pub vessel: u64,
    /// The key label ("exp@biome/body·situation").
    pub label: String,
    /// Frozen conversion rate (science/s).
    pub rate_per_s: f64,
    /// Science accrued at the staged tick.
    pub accrued: f64,
    /// Escrowed data remaining at the staged tick.
    pub data_left: f64,
    /// Seconds to the predicted exhaustion event.
    pub eta_s: f64,
}

/// A parachute/decelerator node aboard the focused vessel — the
/// `DeployPart` target (W1.4's verb had no producer).
#[derive(Clone, Debug)]
pub struct ChuteRow {
    /// The `PartNodeId` carrying the decelerator.
    pub node: u64,
    /// Display name of the part.
    pub part: String,
    /// Already deployed.
    pub deployed: bool,
}

/// One part node aboard the focused vessel — the part-action
/// surfaces' name table: the flight right-click pick names a
/// `PartNodeId`, this resolves it to the loc-resolved display name
/// (and gives the whole-vessel menu its per-part list).
#[derive(Clone, Debug)]
pub struct PartRow {
    /// `PartNodeId` — the pick/commands key.
    pub node: u64,
    /// Display name of the part.
    pub part: String,
}

/// A pending player-facing event (`PlayerAlarm`/`TransferWindow`) —
/// the AGENDA section's row.
#[derive(Clone, Debug)]
pub struct AlarmRow {
    /// The sim tick it fires at.
    pub tick: u64,
    /// Seconds until it fires (staging-side countdown).
    pub in_s: f64,
    /// The label ("manual alarm" / "transfer window terrin→duva").
    pub label: String,
}

/// One in-flight transfer — the pending `TransferComplete` row.
#[derive(Clone, Debug)]
pub struct TransferRow {
    /// The label ("ore 10.0: vessel 1 → colony 0").
    pub label: String,
    /// Seconds until the completion lands.
    pub eta_s: f64,
}

/// An in-flight build order (`sim.builds`) — the W2.6 "craft under
/// construction" row: the `ManufactureCraft` order spent its BOM and
/// funds at apply and delivers a `SurfaceLatched` vessel when
/// `ConstructionComplete` fires.
#[derive(Clone, Debug)]
pub struct BuildRow {
    /// The `BuildId` the order carries.
    pub id: u64,
    /// The delivery site's label.
    pub site: String,
    /// The craft's label ("5-part craft").
    pub label: String,
    /// Seconds until `ConstructionComplete` delivers (staged).
    pub eta_s: f64,
}

/// One part family's §3.4 qualification row — the FACILITIES
/// section's bench target (W2.6). Units pool flight + bench credit;
/// the hazard factor is the family's live `qual_factor` reading
/// (evaluated on a representative catalog part's authored curve —
/// per-part curves differ, the row says which).
#[derive(Clone, Debug)]
pub struct FamilyRow {
    /// `FamilyId` as a string — the command's `family`.
    pub id: String,
    /// Display label — the family id (families carry no loc key).
    pub label: String,
    /// `flight_units + bench_units` accumulated.
    pub units: f64,
    /// The live hazard multiplier at `units` (1.0 = unqualified).
    pub hazard_factor: f64,
    /// The next cheaper knot's reading ("→ ×0.45 @ 100 u on sparc"),
    /// "" at the curve's last knot.
    pub next_knot: String,
    /// The family's credited observed failures.
    pub failures: u32,
    /// ETA (s) of the family's in-flight bench run, when one exists.
    pub bench_eta_s: Option<f64>,
}

/// One script bound to the focused vessel (W2.5 — the script
/// console's row: state, strikes, and the armed wake).
#[derive(Clone, Debug)]
pub struct ScriptRow {
    /// `ScriptId` — the commands' target.
    pub id: u64,
    /// A display name — the source's first non-empty line.
    pub label: String,
    /// `enabled` — a parked script shows its resume toggle.
    pub enabled: bool,
    /// Consecutive faults (the §2.10 strike counter).
    pub strikes: u32,
    /// Why the host parked it (the strike-disable reason).
    pub disabled_reason: Option<String>,
    /// The armed wake tick — `None` = no wake (bubble-ticked or
    /// inert).
    pub wake_tick: Option<u64>,
}

/// Everything `program_panel` needs — all plain data (§2.1).
#[derive(Clone, Debug, Default)]
pub struct ProgramInput {
    /// Program funds (whole funds, not micros — `csp-view` converts).
    pub funds: f64,
    /// Live science total (banked + in-flight project accrual).
    pub science: f64,
    /// Spendable banked science.
    pub banked: f64,
    /// Contract offers on the board.
    pub offers: Vec<OfferRow>,
    /// A board-level honesty note (e.g. kinds gated off the board
    /// because their completion verbs are unreachable).
    pub offers_note: Option<String>,
    /// Active contracts.
    pub active: Vec<ActiveRow>,
    /// The tech tree — `None` for a world with no tree installed.
    pub tech: Option<Vec<TechRow>>,
    /// Upgrade tracks (empty without a tree).
    pub tracks: Vec<TrackRow>,
    /// The focused vessel's science context, when one is staged:
    /// `(vessel label, situation name, biome label or "", body label)`.
    pub context: Option<(String, String, String, String)>,
    /// Experiment modules aboard the focused vessel.
    pub experiments: Vec<ExperimentRow>,
    /// Pooled data records aboard.
    pub pooled: Vec<DataRow>,
    /// The focused vessel is `SurfaceLatched` — gates the Recover
    /// buttons (`RecoverData` requires it) and the vessel
    /// recover/terminate actions (`RecoverVessel` requires it).
    pub latched: bool,
    /// Crew aboard the focused vessel — the LABS `StartResearch`
    /// gate (C22-F3: the resolver drops a crewless start with "NO
    /// CREW ABOARD", so the buttons disable with the reason on
    /// hover instead of dead-clicking).
    pub crew_aboard: bool,
    /// Display name of the site a vessel recovery credits to —
    /// `None` = the remote-salvage leg (no site on the latched
    /// body). Label only; the command resolves the `SiteId`
    /// view-side.
    pub recover_site: Option<String>,
    /// ElectricCharge aboard the focused vessel (`None` with no
    /// vessel) — the transmit gate's "have" side (W1.5-e).
    pub vessel_ec: Option<f64>,
    /// Per-node reliability rows for the focused vessel.
    pub reliability: Vec<ReliabilityRow>,
    /// Recent notifications (newest last), already capped.
    pub notices: Vec<NoticeRow>,
    /// W2.0: every vessel in the program arena — the
    /// vessels-in-program list the logistics verbs hang off.
    pub vessels: Vec<VesselRow>,
    /// The crew roster (economy book's named individuals).
    pub roster: Vec<CrewRow>,
    /// The colony arena — W2.3's colony section.
    pub colonies: Vec<ColonyRow>,
    /// The site table — stockpiles + the `FoundColony` slots.
    pub sites: Vec<SiteRow>,
    /// Live research projects (`science.projects`).
    pub projects: Vec<ProjectRow>,
    /// Lab modules aboard the focused vessel.
    pub labs: Vec<LabRow>,
    /// Decelerator nodes aboard the focused vessel.
    pub chutes: Vec<ChuteRow>,
    /// Every part node aboard the focused vessel — the right-click
    /// part menu's name table (the pick resolves a `PartNodeId`; the
    /// action verbs come from `experiments`/`chutes`/`pooled`).
    pub parts: Vec<PartRow>,
    /// Pending player alarms / watched transfer windows.
    pub alarms: Vec<AlarmRow>,
    /// In-flight transfers (pending `TransferComplete`s).
    pub transfers: Vec<TransferRow>,
    /// The focused vessel's aboard resource units —
    /// `(resource, units)` aggregated over its tanks; the transfer
    /// buttons' source side.
    pub cargo: Vec<(String, f64)>,
    /// Free `CrewSeat` slots aboard the focused vessel.
    pub focus_free_seats: u64,
    /// The colony the focused vessel joined, if any.
    pub focus_joined: Option<u64>,
    /// Funds charged per `HireCrew` (`crew_hire_cost_funds`).
    pub hire_cost: f64,
    /// The salary/s a UI hire pays — the panel's authored default
    /// (the sim has no salary table; `HireCrew` carries it verbatim).
    pub hire_salary_per_s: f64,
    /// C21-F8: `FoundColony`'s authored debit
    /// (`colony_founding_cost_funds`, whole funds) — the sites
    /// section's "found outpost" gate/label.
    pub colony_founding_cost: f64,
    /// W2.5 — the automation surface. Scripts bound to the focused
    /// vessel.
    pub scripts: Vec<ScriptRow>,
    /// The engaged autopilot on the focused vessel — a display label
    /// ("ascent → 80.0 km · paused"); `None` = none engaged.
    pub autopilot: Option<String>,
    /// The focused vessel's residence for the engage gate —
    /// `"bubble"`, `"rails"`, `"latched"`, or `""` when no vessel is
    /// focused. Only `"bubble"` can take an `EngageAutopilot` (the
    /// sim refuses the rest).
    pub residence: String,
    /// The `script_log` tail — `log`/`print`/diagnostic lines the
    /// scripts produced (newest last), already capped.
    pub script_log: Vec<NoticeRow>,
    /// W2.6 — the FACILITIES surface: in-flight `ManufactureCraft`
    /// build orders (`sim.builds`), per-family qualification rows,
    /// and the test-stand economics the bench buttons gate on.
    pub builds: Vec<BuildRow>,
    /// Part-family qualification rows (catalog ∪ live records).
    pub families: Vec<FamilyRow>,
    /// The stand's site — the dominant body's launch site
    /// (`RunBenchTest` requires `launch: true`); `None` disables the
    /// bench buttons.
    pub bench_site: Option<String>,
    /// Display name of the stand's site.
    pub bench_site_name: String,
    /// Funds charged per bench unit (`bench_funds_per_unit`).
    pub bench_funds_per_unit: f64,
    /// The stockpile resource a bench unit consumes.
    pub bench_resource: String,
    /// Units of `bench_resource` per bench unit.
    pub bench_resource_per_unit: f64,
    /// Sim-seconds per bench unit.
    pub bench_seconds_per_unit: f64,
    /// `bench_resource` units in the stand's stockpile right now.
    pub bench_stock: f64,
}

/// A request the panel emitted — translated into `SimCommand`s by
/// `csp-view` (the §2.1 rule: this crate cannot name sim types; the
/// context fields the sim commands need — biome, situation, the
/// `DataKey` — are resolved view-side from the staged mirror).
#[derive(Clone, Debug)]
pub enum ProgramCommand {
    /// Spend banked science on a tech node → `ResearchTech`.
    ResearchTech {
        /// The node id.
        node: String,
    },
    /// Buy the next upgrade-track level → `ResearchUpgradeLevel`.
    ResearchTrack {
        /// The track id.
        track: String,
    },
    /// Take a contract off the board → `AcceptContract`.
    AcceptContract {
        /// The offer index.
        offer: u64,
    },
    /// Refuse a board offer without accepting → `DeclineContract`
    /// (C08-F5's un-jam verb: the offer leaves the board and nothing
    /// pays in either direction). Offer-scope only — an accepted
    /// contract is NOT a decline target; its only exits are
    /// completion and the deadline, so the active rows render no
    /// button.
    DeclineContract {
        /// The offer index.
        offer: u64,
    },
    /// Run an experiment module in the current context →
    /// `RunExperiment` (view supplies vessel/biome/situation).
    RunExperiment {
        /// The module's graph node.
        node: u64,
        /// The experiment kind.
        experiment: String,
    },
    /// Bank a pooled record remotely → `TransmitData`.
    Transmit {
        /// The pooled-row index.
        row: usize,
    },
    /// Bank a pooled record by physical recovery → `RecoverData`.
    Recover {
        /// The pooled-row index.
        row: usize,
    },
    /// Recover the whole latched vessel → `RecoverVessel` (site
    /// credit + part payout + crew home + pooled science banks).
    RecoverVessel,
    /// Delete the latched vessel outright → `TerminateVessel` (the
    /// hulk/pad-trap cleanup — no payout).
    TerminateVessel,

    // W2.0/W2.3 — the progression-surface verbs. Ids are the plain-data
    // arena keys the mirror rows carry; view-side `program_command`
    // resolves the sim-side context (vessel, tick, ids, names).
    /// Hire a roster member → `HireCrew` (view authors the name +
    /// salary). `kind`: "pilot"/"engineer"/"scientist"/"tourist".
    /// `skill` is the purchased 1..=5 — each point bills the hire
    /// fee (C08-F7).
    HireCrew {
        /// The crew kind id.
        kind: String,
        /// The purchased skill (the roster record's seed).
        skill: u8,
    },
    /// Seat a fresh crew member aboard the focused vessel →
    /// `BoardCrew` (the id-only local hire; roster members reach
    /// vessels through a colony posting + transfer).
    BoardCrew,
    /// Move an aboard member to another linked vessel →
    /// `TransferCrew` (from = the focused vessel).
    TransferCrew {
        /// The crew member moving.
        crew: u64,
        /// The destination vessel.
        to_vessel: u64,
    },
    /// Move resource units focused-vessel → a linked vessel →
    /// `TransferResource`.
    TransferResource {
        /// The destination vessel.
        to_vessel: u64,
        /// The resource moving.
        resource: String,
        /// Units to move.
        amount: f64,
    },
    /// Post an `Unassigned` roster member to a colony →
    /// `AssignCrewToColony`.
    AssignCrewToColony {
        /// The roster member.
        crew: u64,
        /// The colony receiving them.
        colony: u64,
    },
    /// Bring every colony crew member home → `EvacuateColony`.
    EvacuateColony {
        /// The colony to evacuate.
        colony: u64,
    },
    /// Salvage + abandon a colony → `AbandonColony`.
    AbandonColony {
        /// The colony to abandon.
        colony: u64,
    },
    /// Schedule the colony's next inspection →
    /// `ScheduleColonyInspection` (the view resolves the tick).
    ScheduleColonyInspection {
        /// The colony to inspect.
        colony: u64,
    },
    /// Found a starter outpost at a site → `FoundColony` (the view
    /// builds the carried `ColonyDef`).
    FoundColony {
        /// The site to settle.
        site: String,
    },
    /// Opt the focused latched vessel into a colony's store
    /// inventory → `JoinColonyInventory`.
    JoinColony {
        /// The colony to join.
        colony: u64,
    },
    /// Clear the focused vessel's colony opt-in →
    /// `LeaveColonyInventory`.
    LeaveColony,
    /// Move resource units vessel↔colony → `TransferColonyResource`.
    TransferColonyResource {
        /// The colony endpoint.
        colony: u64,
        /// The resource moving.
        resource: String,
        /// Units in flight.
        amount: f64,
        /// `true` = vessel → colony; `false` = colony → vessel.
        to_colony: bool,
    },
    /// Move a crew member vessel↔colony → `TransferColonyCrew`.
    TransferColonyCrew {
        /// The colony endpoint.
        colony: u64,
        /// The crew member moving.
        crew: u64,
        /// `true` = vessel → colony; `false` = colony → vessel seat.
        to_colony: bool,
    },
    /// Deploy a parachute node aboard the focused vessel →
    /// `DeployPart`.
    DeployPart {
        /// The graph node carrying the decelerator.
        node: u64,
    },
    /// Start a lab project converting a pooled record →
    /// `StartResearch` (the view supplies the `DataKey` via `row`
    /// and the researcher list).
    StartResearch {
        /// The lab part's graph node.
        lab_node: u64,
        /// The pooled-row index.
        row: usize,
    },
    /// Stop a running lab project → `StopResearch`.
    StopResearch {
        /// The project to stop.
        project: u64,
    },
    /// Schedule a player alarm `in_seconds` out → `SetAlarm` (the
    /// view resolves the tick against the staged clock).
    SetAlarm {
        /// Sim-seconds from now the alarm fires at.
        in_seconds: f64,
        /// The label the fired event carries.
        label: String,
    },
    /// Load a script bound to the focused vessel → `LoadScript`.
    LoadScript {
        /// The guest-language source, verbatim.
        source: String,
    },
    /// Unload a script → `UnloadScript`.
    UnloadScript {
        /// The script to remove.
        script: u64,
    },
    /// Enable/disable a loaded script → `SetScriptEnabled`.
    SetScriptEnabled {
        /// The script to toggle.
        script: u64,
        /// `true` = run `on_tick`; `false` = parked.
        enabled: bool,
    },
    /// Engage the ascent autopilot on the focused vessel →
    /// `EngageAutopilot` (`AutopilotProgram::Ascent`).
    EngageAutopilot {
        /// Target apoapsis altitude above the datum, m.
        apo_m: f64,
        /// Target periapsis (circularization) altitude, m.
        pe_m: f64,
    },
    /// Release the focused vessel's autopilot → `DisengageAutopilot`.
    DisengageAutopilot,
    /// Queue a bench-test run at a launch site → `RunBenchTest`.
    RunBenchTest {
        /// The launch site running the stand.
        site: String,
        /// The part family under test.
        family: String,
        /// Bench units to accrue.
        units: f64,
    },
    /// Watch a transfer window → `WatchTransferWindow` (the tick
    /// comes from a planner call the emitter ran).
    WatchTransferWindow {
        /// The body the transfer departs.
        origin: String,
        /// The body it targets.
        destination: String,
        /// The predicted optimal departure tick.
        tick: u64,
    },
    /// Cancel an in-flight manufacture order → `CancelBuild` (the
    /// C11-F3 exit: the order's BOM restocks the ordering site's
    /// inventory and its `craft_cost_micros` refund through the
    /// ledger). Only in-flight orders render the button — a
    /// delivered build holds no live record to name.
    CancelBuild {
        /// The `BuildId` the build row carries.
        build: u64,
    },
}

const MONO: egui::FontId = egui::FontId::monospace(12.0);

/// The stock script library (W2.5) — canned `.rhai` sources the
/// console loads verbatim with `LoadScript`. Each is a tick-entry
/// `on_tick(ctx)` program against the guest surface: the wait verbs
/// register `ScriptWake`s, `engage_autopilot` hands the stick to the
/// installed Rust driver, `add_node` schedules a burn.
pub const STOCK_SCRIPTS: &[(&str, &str)] = &[
    (
        "telemetry logger",
        r#"// Prints a heartbeat line every ~10 sim-seconds (64 ticks/s).
fn on_tick(ctx) {
    if ctx.tick % 640 == 0 {
        let t = ctx.vessel;
        print(`t${ctx.tick} alt ${t.altitude}m spd ${t.speed}m/s Apo ${t.apoapsis}m`);
    }
}
"#,
    ),
    (
        "apoapsis announcer",
        r#"// Arms the apoapsis wake every tick. A rails-bound script
// only runs when its ScriptWake fires — so on rails the wake IS
// the tick and t_apo ≈ 0 there. In-bubble the script ticks every
// tick and the armed wake survives the auto-rails edge.
fn on_tick(ctx) {
    wait_apoapsis();
    if ctx.vessel.residence == "rails" && ctx.vessel.time_to_apoapsis < 30.0 {
        print(`at apoapsis — alt ${ctx.vessel.altitude} m`);
    }
}
"#,
    ),
    (
        "circularize at apoapsis",
        r#"// Circularize at apoapsis: arms the apsis wake; when it
// fires on rails (time_to_apoapsis ≈ 0) it schedules the prograde
// burn that raises Pe to Apo (vc = sqrt(mu/r) − v_horizontal).
fn on_tick(ctx) {
    let t = ctx.vessel;
    if t.residence == "rails" && "armed" in ctx.persist && t.time_to_apoapsis < 30.0 {
        let r = sqrt(t.pos.x*t.pos.x + t.pos.y*t.pos.y + t.pos.z*t.pos.z);
        let dv = sqrt(t.mu / r) - t.horizontal_speed;
        if dv > 1.0 {
            add_node(ctx.tick + 2, dv, 0.0, 0.0);
            print(`circularize: +${dv} m/s node at tick ${ctx.tick + 2}`);
        } else {
            print(`circularize: already circular (dv ${dv})`);
        }
        ctx.persist["armed"] = 0;
    } else {
        wait_apoapsis();
        ctx.persist["armed"] = 1;
    }
}
"#,
    ),
    (
        "ascent hand-off",
        r#"// Hand the stick to the built-in ascent autopilot (80 km
// target) — the same EngageAutopilot the player verb issues. Needs
// an in-bubble vessel: unlatch first.
fn on_tick(ctx) {
    if !("done" in ctx.persist) {
        ctx.persist["done"] = true;
        engage_autopilot(80000.0, 80000.0);
        print("ascent autopilot engaged — target 80 km");
    }
}
"#,
    ),
];

fn sect(ui: &mut egui::Ui, name: &str) {
    ui.label(
        egui::RichText::new(format!("── {name} ──────────────────────────────"))
            .font(MONO)
            .color(egui::Color32::from_gray(115)),
    );
}

/// The always-visible program readout — funds + science + active
/// contracts, top-right, drawn in every mode so the program state is
/// never hidden behind a view switch (§3.7's global-channel rule).
/// The glyphs label themselves on hover, and the strip is now a real
/// affordance: **clicking it opens the Program window** — the status
/// was a dead end before (players could read the numbers but had no
/// path to the loop behind them). Returns `true` on that click —
/// `csp-view` folds it into the panel flag.
pub fn program_status(ctx: &egui::Context, funds: f64, science: f64, contracts: usize) -> bool {
    let rect = ctx.content_rect();
    let mut clicked = false;
    egui::Area::new(egui::Id::unique("program_status"))
        .order(egui::Order::Foreground)
        .movable(true)
        .default_pos(egui::pos2(rect.right() - 12.0, rect.top() + 4.0))
        .pivot(egui::Align2::RIGHT_TOP)
        .show(ctx, |ui| {
            // Shared chip — the strip used to float on the scene.
            crate::hud_frame().show(ui, |ui| {
                clicked = ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(format!(
                                "₣ {funds:.0}   ⚛ {science:.1}   ✉ {contracts}"
                            ))
                            .font(egui::FontId::monospace(13.5))
                            .color(egui::Color32::from_rgb(160, 225, 165)),
                        )
                        .frame(false),
                    )
                    .on_hover_text(format!(
                        "₣ {funds:.0} funds · ⚛ {science:.1} science · ✉ {contracts} active contract(s)\n\
                         click for the Program window — contracts, research, vessel science\n\
                         in flight: the Actions strip (bottom right) + right-click any part"
                    ))
                    .clicked();
            });
        });
    clicked
}

/// The CONTRACTS section — active rows with countdowns, offers with
/// the drawn params/reward/deadline (W1.5-d: no blind accepts), and
/// the board-level honesty note (W1.5-i). Extracted from
/// `program_panel` under the complexity budget.
fn contracts_section(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut Vec<ProgramCommand>,
    dim: egui::Color32,
) {
    if input.active.is_empty() && input.offers.is_empty() {
        ui.label(egui::RichText::new("no contracts on the board").color(dim));
    }
    for a in &input.active {
        ui.label(
            egui::RichText::new(format!("▸ {}", a.label))
                .font(MONO)
                .color(dim),
        );
        ui.label(
            egui::RichText::new(format!("    {}", a.detail))
                .font(MONO)
                .color(dim)
                .small(),
        );
        if !a.progress.is_empty() {
            ui.label(
                egui::RichText::new(format!("    {}", a.progress))
                    .font(MONO)
                    .color(egui::Color32::from_rgb(140, 200, 140))
                    .small(),
            );
        }
    }
    for offer in &input.offers {
        offer_row(ui, offer, out, dim);
    }
    if let Some(note) = &input.offers_note {
        ui.label(egui::RichText::new(note).color(dim).small());
    }
}

/// One contract-board offer — the button row (accept gated on the
/// §3.6 prereq read, decline as the C08-F5 un-jam exit) plus the
/// drawn params/reward/deadline lines. Extracted from
/// `contracts_section` under the same complexity budget it was.
fn offer_row(
    ui: &mut egui::Ui,
    offer: &OfferRow,
    out: &mut Vec<ProgramCommand>,
    dim: egui::Color32,
) {
    ui.horizontal(|ui| {
        // W2.1: the accept predicate's prereqs gate the button —
        // a refused accept used to be a silent drain rejection.
        let hover = if offer.accept_ok {
            "accept the contract — the advance credits now".to_string()
        } else {
            let missing: Vec<&str> = offer
                .prereqs
                .iter()
                .filter(|(met, _)| !*met)
                .map(|(_, t)| t.as_str())
                .collect();
            format!("prerequisites unmet: {}", missing.join("; "))
        };
        if ui
            .add_enabled(offer.accept_ok, egui::Button::new("accept").small())
            .on_hover_text(hover)
            .clicked()
        {
            out.push(ProgramCommand::AcceptContract { offer: offer.index });
        }
        // C08-F5's board un-jam — offered on every offer row,
        // enabled even when the accept button isn't: the offers
        // decline exists FOR are exactly the unsatisfiable ones.
        // Active rows carry no decline (their exits are
        // completion/deadline — a free abandon would reopen the
        // advance faucet the verb closes).
        if ui
            .small_button("decline")
            .on_hover_text("refuse the offer — it leaves the board, nothing pays either way")
            .clicked()
        {
            out.push(ProgramCommand::DeclineContract { offer: offer.index });
        }
        ui.label(egui::RichText::new(&offer.label).font(MONO).color(dim));
    });
    if !offer.description.is_empty() {
        ui.label(
            egui::RichText::new(format!("    {}", offer.description))
                .font(MONO)
                .color(dim)
                .small(),
        );
    }
    if !offer.detail.is_empty() {
        ui.label(
            egui::RichText::new(format!("    {}", offer.detail))
                .font(MONO)
                .color(dim)
                .small(),
        );
    }
    for (met, text) in &offer.prereqs {
        ui.label(
            egui::RichText::new(format!("    {} {}", if *met { "✓" } else { "✗" }, text))
                .font(MONO)
                .color(if *met {
                    egui::Color32::from_rgb(140, 200, 140)
                } else {
                    egui::Color32::from_rgb(235, 150, 80)
                })
                .small(),
        );
    }
    if !offer.reward.is_empty() || !offer.deadline.is_empty() {
        ui.label(
            egui::RichText::new(format!(
                "    {}{}{}",
                offer.reward,
                if offer.reward.is_empty() || offer.deadline.is_empty() {
                    ""
                } else {
                    " · "
                },
                offer.deadline
            ))
            .font(MONO)
            .color(dim)
            .small(),
        );
    }
}

/// The VESSELS section — every arena vessel with its residence, and
/// the logistics verbs against the focused vessel: linked vessels get
/// crew/resource transfer buttons, the focused latched vessel the
/// colony join/leave affordance (W2.0/W2.3).
fn vessels_section(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut Vec<ProgramCommand>,
    dim: egui::Color32,
) {
    if input.vessels.is_empty() {
        ui.label(egui::RichText::new("no vessels").color(dim));
        return;
    }
    for v in &input.vessels {
        let mark = if v.focused { "▸" } else { " " };
        ui.label(
            egui::RichText::new(format!(
                "{mark}vessel {} · {}{}{} · crew {}/{}",
                v.id,
                v.residence,
                if v.body.is_empty() {
                    String::new()
                } else {
                    format!(" @ {}", v.body)
                },
                if v.debris { " · debris" } else { "" },
                v.crew.len(),
                v.crew.len() as u64 + v.free_seats,
            ))
            .font(MONO)
            .color(if v.focused {
                egui::Color32::from_gray(220)
            } else {
                dim
            }),
        );
        if v.linked_to_focus && !v.focused {
            // The linked vessel row's transfer affordances: crew
            // moves ride the member rows in CREW; resource sends live
            // here — one button per aboard resource per linked vessel.
            for (resource, units) in &input.cargo {
                if *units <= 0.0 {
                    continue;
                }
                for (label, amount) in [
                    // C22-F7: the button names exactly what it
                    // sends — under a light stock "10" sent the
                    // residue and silently duplicated "all".
                    (
                        format!("→v{} {}", v.id, fmt_amount(10.0_f64.min(*units))),
                        10.0_f64.min(*units),
                    ),
                    (format!("→v{} all", v.id), *units),
                ] {
                    if ui
                        .small_button(format!("  {resource} {label}"))
                        .on_hover_text(format!("transfer {resource} to vessel {}", v.id))
                        .clicked()
                    {
                        out.push(ProgramCommand::TransferResource {
                            to_vessel: v.id,
                            resource: resource.clone(),
                            amount,
                        });
                    }
                }
            }
        }
    }
}

/// The CREW section — hire buttons (one per kind, gated on funds),
/// the roster with per-member posting/transfer verbs, and the
/// focused vessel's `BoardCrew` affordance (W2.0/W2.3).
#[allow(clippy::too_many_lines)] // one member row = one button pass
fn crew_section(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut Vec<ProgramCommand>,
    dim: egui::Color32,
) {
    // C08-F7's hire band — the purchased skill, persisted across
    // frames; each point bills `hire_cost` (the sim rejects 0 and
    // >5, the widget cannot emit them).
    let skill_id = ui.scope_id().with("program_hire_skill");
    let mut skill: u8 = ui
        .data_mut(|d| d.get_persisted::<u8>(skill_id))
        .unwrap_or(1);
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut skill)
                .range(1..=5)
                .prefix("skill "),
        )
        .on_hover_text("skill of the next hire — each point costs the hire fee");
    });
    ui.data_mut(|d| d.insert_persisted(skill_id, skill));
    let cost = input.hire_cost * f64::from(skill);
    let afford = input.funds >= cost;
    ui.horizontal(|ui| {
        for kind in ["pilot", "engineer", "scientist", "tourist"] {
            if ui
                .add_enabled(afford, egui::Button::new(format!("+{kind}")).small())
                .on_hover_text(if afford {
                    format!(
                        "hire a {kind} — ₣{cost:.0} now (skill {skill}), ₣{:.3}/s salary",
                        input.hire_salary_per_s
                    )
                } else {
                    format!("hire costs ₣{cost:.0} — not enough funds")
                })
                .clicked()
            {
                out.push(ProgramCommand::HireCrew {
                    kind: kind.to_string(),
                    skill,
                });
            }
        }
        let focus = input.vessels.iter().find(|v| v.focused);
        let board_ok = focus.is_some_and(|v| v.free_seats > 0);
        if ui
            .add_enabled(board_ok, egui::Button::new("board").small())
            .on_hover_text(if board_ok {
                "seat a fresh crew member on the focused vessel (id-only — no roster record)"
                    .to_string()
            } else {
                "needs a focused vessel with a free crew seat".to_string()
            })
            .clicked()
        {
            out.push(ProgramCommand::BoardCrew);
        }
    });
    if input.roster.is_empty() {
        ui.label(egui::RichText::new("no crew hired").color(dim));
    }
    for r in &input.roster {
        let where_label = match &r.location {
            CrewWhere::Unassigned => "unassigned".to_string(),
            CrewWhere::Vessel(v) => format!("vessel {v}"),
            CrewWhere::Colony(c) => format!("colony {c}"),
            CrewWhere::Facility(f) => format!("facility {f}"),
        };
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} · skill {} · {} · ₣{:.3}/s",
                    r.name, r.kind, r.skill, where_label, r.salary_per_s
                ))
                .font(MONO)
                .color(if r.active {
                    egui::Color32::from_gray(200)
                } else {
                    dim
                }),
            );
            if r.active {
                match &r.location {
                    // Unassigned members post to a live colony —
                    // C09-F4: dim on `has_ls` (the sim refuses a
                    // posting to an LS-less colony outright; the
                    // baseline draw would starve the member).
                    CrewWhere::Unassigned => {
                        for c in input.colonies.iter().filter(|c| c.live) {
                            if ui
                                .add_enabled(
                                    c.has_ls,
                                    egui::Button::new(format!("→col{}", c.id)).small(),
                                )
                                .on_hover_text(if c.has_ls {
                                    format!("post to {}", c.name)
                                } else {
                                    format!("{} has no working life-support module", c.name)
                                })
                                .clicked()
                            {
                                out.push(ProgramCommand::AssignCrewToColony {
                                    crew: r.id,
                                    colony: c.id,
                                });
                            }
                        }
                    }
                    // Colony-posted members board a vessel joined to
                    // that colony's inventory (the colony→vessel arm).
                    CrewWhere::Colony(c) => {
                        if input.focus_joined == Some(*c)
                            && input.focus_free_seats > 0
                            && ui
                                .small_button("→vessel")
                                .on_hover_text("transfer aboard the joined focused vessel")
                                .clicked()
                        {
                            out.push(ProgramCommand::TransferColonyCrew {
                                colony: *c,
                                crew: r.id,
                                to_colony: false,
                            });
                        }
                    }
                    // Aboard members move to the joined colony or a
                    // linked vessel.
                    CrewWhere::Vessel(v) => {
                        let focused = input.vessels.iter().find(|x| x.focused);
                        if focused.is_some_and(|f| f.id == *v) {
                            if let Some(c) = input.focus_joined
                                && ui
                                    .small_button("→colony")
                                    .on_hover_text("transfer to the joined colony")
                                    .clicked()
                            {
                                out.push(ProgramCommand::TransferColonyCrew {
                                    colony: c,
                                    crew: r.id,
                                    to_colony: true,
                                });
                            }
                            for other in input.vessels.iter().filter(|o| o.linked_to_focus) {
                                if ui
                                    .small_button(format!("→v{}", other.id))
                                    .on_hover_text(format!("transfer to vessel {}", other.id))
                                    .clicked()
                                {
                                    out.push(ProgramCommand::TransferCrew {
                                        crew: r.id,
                                        to_vessel: other.id,
                                    });
                                }
                            }
                        }
                    }
                    CrewWhere::Facility(_) => {}
                }
            }
        });
    }
}

/// The COLONIES + SITES section — W2.3's RTS layer: per-colony
/// state/modules/stores/crew, the join/transfer verbs for a latched
/// focused vessel, site stockpiles, and the `FoundColony` slots.
#[allow(clippy::too_many_lines)] // one colony card = one pass
fn colonies_section(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut Vec<ProgramCommand>,
    dim: egui::Color32,
) {
    for c in &input.colonies {
        ui.label(
            egui::RichText::new(format!(
                "▸ {} @ {} — {}{}{}",
                c.name,
                c.body,
                c.state,
                if c.live {
                    format!(" · {}", c.daylight)
                } else {
                    String::new()
                },
                c.t_eq_k.map(|t| format!(" · {t:.0} K")).unwrap_or_default(),
            ))
            .font(MONO)
            .color(if c.live {
                egui::Color32::from_gray(200)
            } else {
                dim
            }),
        );
        if !c.crew.is_empty() || !c.modules.is_empty() {
            let mods: Vec<String> = c
                .modules
                .iter()
                .map(|m| {
                    format!(
                        "{}{}{}",
                        m.label,
                        if m.damaged { " (DAMAGED)" } else { "" },
                        if (m.fraction - 1.0).abs() > 0.01 {
                            format!(" @{:.0}%", m.fraction * 100.0)
                        } else {
                            String::new()
                        }
                    )
                })
                .collect();
            ui.label(
                egui::RichText::new(format!("    modules: {}", mods.join(" · ")))
                    .font(MONO)
                    .color(dim)
                    .small(),
            );
        }
        for s in &c.stores {
            ui.label(
                egui::RichText::new(format!(
                    "    {} {:.1}/{:.0} ({:+.2}/s)",
                    s.resource, s.level, s.capacity, s.rate_per_s
                ))
                .font(MONO)
                .color(dim)
                .small(),
            );
        }
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "    crew {}{}",
                    c.crew.len(),
                    c.inspection_days
                        .map(|d| format!(" · inspection in {d:.1} d"))
                        .unwrap_or_default()
                ))
                .font(MONO)
                .color(dim)
                .small(),
            );
            if c.live {
                if !c.crew.is_empty()
                    && ui
                        .small_button("evacuate")
                        .on_hover_text("bring every posted member home (Unassigned)")
                        .clicked()
                {
                    out.push(ProgramCommand::EvacuateColony { colony: c.id });
                }
                if ui
                    .small_button("inspect +1d")
                    .on_hover_text("schedule the colony's next inspection")
                    .clicked()
                {
                    out.push(ProgramCommand::ScheduleColonyInspection { colony: c.id });
                }
                if ui
                    .small_button("abandon")
                    .on_hover_text("salvage stores to the site inventory and abandon the colony")
                    .clicked()
                {
                    out.push(ProgramCommand::AbandonColony { colony: c.id });
                }
            }
            if c.joinable
                && ui
                    .small_button("join")
                    .on_hover_text("opt the focused vessel into this colony's store inventory")
                    .clicked()
            {
                out.push(ProgramCommand::JoinColony { colony: c.id });
            }
        });
        // The joined vessel's logistics verbs — only the colony the
        // focused vessel joined offers transfers (the sim's join gate).
        if c.joined {
            ui.horizontal(|ui| {
                if ui
                    .small_button("leave")
                    .on_hover_text("clear the colony-inventory opt-in")
                    .clicked()
                {
                    out.push(ProgramCommand::LeaveColony);
                }
                for (resource, units) in &input.cargo {
                    if *units <= 0.0 {
                        continue;
                    }
                    for (label, amount) in [
                        (
                            format!("deliver {}", fmt_amount(10.0_f64.min(*units))),
                            10.0_f64.min(*units),
                        ),
                        ("deliver all".to_string(), *units),
                    ] {
                        if ui
                            .small_button(format!("{resource} {label}"))
                            .on_hover_text(format!("vessel → colony: {resource}"))
                            .clicked()
                        {
                            out.push(ProgramCommand::TransferColonyResource {
                                colony: c.id,
                                resource: resource.clone(),
                                amount,
                                to_colony: true,
                            });
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                for s in &c.stores {
                    if s.level <= 0.0 {
                        continue;
                    }
                    // C22-F7: the label and hover name the sent
                    // amount — `min(10, level)` on a light store.
                    let amount = 10.0_f64.min(s.level);
                    let label = format!("withdraw {} {}", s.resource, fmt_amount(amount));
                    if ui
                        .small_button(label)
                        .on_hover_text(format!(
                            "colony → vessel: {} {} (store holds {:.1})",
                            fmt_amount(amount),
                            s.resource,
                            s.level
                        ))
                        .clicked()
                    {
                        out.push(ProgramCommand::TransferColonyResource {
                            colony: c.id,
                            resource: s.resource.clone(),
                            amount,
                            to_colony: false,
                        });
                    }
                }
            });
        }
    }
    if input.colonies.is_empty() {
        ui.label(egui::RichText::new("no colonies founded").color(dim));
    }
    if !input.sites.is_empty() {
        sect(ui, "SITES");
        for s in &input.sites {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} @ {}{}",
                        s.name,
                        s.body,
                        if s.launch { " (launch)" } else { "" }
                    ))
                    .font(MONO)
                    .color(dim),
                );
                // C21-F8/C09-F9: founding costs the authored charge —
                // "prototype, no cost" is gone — and the button only
                // offers itself on a free site with the funds to pay
                // (`has_colony` is live-only, so a salvaged wreck's
                // site can be re-founded).
                let can_found = !s.has_colony && input.funds >= input.colony_founding_cost;
                if ui
                    .add_enabled(can_found, egui::Button::new("found outpost").small())
                    .on_hover_text(if can_found {
                        format!(
                            "found a starter colony here (solar + battery + life support — ₣{:.0})",
                            input.colony_founding_cost
                        )
                    } else if s.has_colony {
                        "a live colony already occupies this site".to_string()
                    } else {
                        format!(
                            "founding costs ₣{:.0} — not enough funds",
                            input.colony_founding_cost
                        )
                    })
                    .clicked()
                {
                    out.push(ProgramCommand::FoundColony { site: s.id.clone() });
                }
            });
            if !s.inventory.is_empty() {
                let stock: Vec<String> = s
                    .inventory
                    .iter()
                    .map(|(r, u)| format!("{r} {u:.1}"))
                    .collect();
                ui.label(
                    egui::RichText::new(format!("    stockpile: {}", stock.join(" · ")))
                        .font(MONO)
                        .color(dim)
                        .small(),
                );
            }
        }
    }
    if !input.transfers.is_empty() {
        for t in &input.transfers {
            ui.label(
                egui::RichText::new(format!(
                    "    in flight: {} (eta {})",
                    t.label,
                    fmt_eta(t.eta_s)
                ))
                .font(MONO)
                .color(dim)
                .small(),
            );
        }
    }
}

/// The AGENDA section — pending `PlayerAlarm`/`TransferWindow`
/// entries plus the `SetAlarm` quick-set buttons (W2.0's alarm
/// plumbing; the full transfer planner lands with W2.4).
fn agenda_section(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut Vec<ProgramCommand>,
    dim: egui::Color32,
) {
    ui.horizontal(|ui| {
        for (label, secs) in [("+1 h", 3600.0), ("+1 d", 86_400.0)] {
            if ui
                .small_button(format!("alarm {label}"))
                .on_hover_text(format!("schedule a player alarm {label} out"))
                .clicked()
            {
                out.push(ProgramCommand::SetAlarm {
                    in_seconds: secs,
                    label: format!("manual alarm {label}"),
                });
            }
        }
    });
    for a in &input.alarms {
        ui.label(
            egui::RichText::new(format!("{} — in {}", a.label, fmt_eta(a.in_s)))
                .font(MONO)
                .color(dim),
        );
    }
    if input.alarms.is_empty() {
        ui.label(egui::RichText::new("nothing scheduled").color(dim));
    }
}

/// Seconds → a coarse countdown ("12:34", "2.3 d") for agenda/transfer rows.
fn fmt_eta(s: f64) -> String {
    crate::fmt_period(s.max(0.0))
}

/// A resource amount for a transfer button's label — the button must
/// name exactly what it sends (C22-F7: a "10" that sends `min(10,
/// stock)` mislabels the verb under a light stock and silently
/// duplicates "all"). Whole amounts print without the fraction.
fn fmt_amount(u: f64) -> String {
    if (u - u.round()).abs() < 1e-9 {
        format!("{u:.0}")
    } else {
        format!("{u:.1}")
    }
}

/// The FACILITIES section (W2.6) — §3.5's manufacture + §3.4's test
/// stand made player-reachable: in-flight build orders with their
/// delivery countdowns, and one qualification row per part family
/// with `RunBenchTest` buttons gated on the stand's real economics
/// (funds + site stockpile + time — the apply refuses shortfalls, so
/// the buttons disclose them up front like the EC-gated transmit).
#[allow(clippy::too_many_lines)] // section size, the agenda precedent
fn facilities_section(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut Vec<ProgramCommand>,
    dim: egui::Color32,
) {
    for b in &input.builds {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "building #{} {} @ {} — delivers in {}",
                    b.id,
                    b.label,
                    b.site,
                    fmt_eta(b.eta_s)
                ))
                .font(MONO)
                .color(egui::Color32::from_rgb(140, 200, 140)),
            );
            // C11-F3's mis-order exit. Every staged row is an
            // in-flight order (a delivered build holds no `builds`
            // record, so no row names one) — the refund hover is
            // the honest read of what the click does.
            if ui
                .small_button("cancel")
                .on_hover_text("cancel the order — the BOM restocks the site and the funds refund")
                .clicked()
            {
                out.push(ProgramCommand::CancelBuild { build: b.id });
            }
        });
    }
    if input.families.is_empty() {
        ui.label(egui::RichText::new("no part families").color(dim));
        return;
    }
    // The stand's economics, once — the buttons' hover repeats the
    // per-run arithmetic.
    match &input.bench_site {
        Some(_) => {
            ui.label(
                egui::RichText::new(format!(
                    "test stand @ {} — ₣{:.0} + {:.1} {} per unit, {:.0} s/u (stock {:.0})",
                    input.bench_site_name,
                    input.bench_funds_per_unit,
                    input.bench_resource_per_unit,
                    input.bench_resource,
                    input.bench_seconds_per_unit,
                    input.bench_stock
                ))
                .font(MONO)
                .color(dim)
                .small(),
            );
        }
        None => {
            ui.label(
                egui::RichText::new("no launch site — test stand unavailable")
                    .font(MONO)
                    .color(dim)
                    .small(),
            );
        }
    }
    for f in &input.families {
        ui.label(
            egui::RichText::new(format!(
                "{} — {:.0} qual units · hazard ×{:.2}{}{}",
                f.label,
                f.units,
                f.hazard_factor,
                if f.next_knot.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", f.next_knot)
                },
                if f.failures > 0 {
                    format!(" · {} failures", f.failures)
                } else {
                    String::new()
                },
            ))
            .font(MONO)
            .color(if f.hazard_factor < 1.0 {
                egui::Color32::from_rgb(140, 220, 140)
            } else {
                dim
            }),
        );
        ui.horizontal(|ui| {
            if let Some(eta) = f.bench_eta_s {
                ui.label(
                    egui::RichText::new(format!("    bench running — eta {}", fmt_eta(eta)))
                        .font(MONO)
                        .color(egui::Color32::from_rgb(140, 200, 140))
                        .small(),
                );
            }
            if let Some(site) = &input.bench_site {
                for units in [5.0_f64, 25.0] {
                    let funds = units * input.bench_funds_per_unit;
                    let stock = units * input.bench_resource_per_unit;
                    let afford = input.funds >= funds && input.bench_stock >= stock;
                    if ui
                        .add_enabled(
                            afford,
                            egui::Button::new(format!("test +{units:.0}")).small(),
                        )
                        .on_hover_text(if afford {
                            format!(
                                "bench-test {}: {} units — ₣{:.0} + {:.1} {}, {} sim-time",
                                f.label,
                                units,
                                funds,
                                stock,
                                input.bench_resource,
                                fmt_eta(units * input.bench_seconds_per_unit)
                            )
                        } else {
                            format!(
                                "needs ₣{:.0} + {:.1} {} at the stand (have ₣{:.0}, {:.1})",
                                funds, stock, input.bench_resource, input.funds, input.bench_stock
                            )
                        })
                        .clicked()
                    {
                        out.push(ProgramCommand::RunBenchTest {
                            site: site.clone(),
                            family: f.id.clone(),
                            units,
                        });
                    }
                }
            }
        });
    }
}

/// The AUTOMATION section (W2.5, pillar 4): the ascent autopilot
/// engage control + the script console — loaded scripts with their
/// strike/wake state, the stock library, a paste-in source buffer,
/// and the `script_log` tail. The target-apo field and the source
/// buffer live in egui's persisted `Id`-keyed data (session state —
/// the mirror is restaged every frame, so panel-owned edits can't
/// ride `ProgramInput`).
#[allow(clippy::too_many_lines)] // one automation card = one pass
fn automation_section(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut Vec<ProgramCommand>,
    dim: egui::Color32,
) {
    let can_engage = input.residence == "bubble";
    match &input.autopilot {
        Some(label) => {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("autopilot — {label}"))
                        .font(MONO)
                        .color(egui::Color32::from_rgb(140, 220, 140)),
                );
                if ui
                    .small_button("disengage")
                    .on_hover_text("release the vessel back to manual control")
                    .clicked()
                {
                    out.push(ProgramCommand::DisengageAutopilot);
                }
            });
        }
        None => {
            ui.horizontal(|ui| {
                let apo_id = ui.scope_id().with("autopilot_apo_km");
                let mut apo_km = ui.data_mut(|d| d.get_persisted::<f64>(apo_id).unwrap_or(80.0));
                ui.add(
                    egui::DragValue::new(&mut apo_km)
                        .range(10.0..=10_000.0)
                        .speed(1.0)
                        .prefix("target ")
                        .suffix(" km"),
                );
                ui.data_mut(|d| d.insert_persisted(apo_id, apo_km));
                if ui
                    .add_enabled(can_engage, egui::Button::new("engage ascent").small())
                    .on_hover_text(if can_engage {
                        format!("hand the vessel to the ascent autopilot — {apo_km:.0} km orbit")
                    } else {
                        match input.residence.as_str() {
                            "latched" => "unlatch the vessel first".to_string(),
                            "rails" => "a vessel on rails has no control ticks — it must be flying"
                                .to_string(),
                            _ => "no vessel in focus".to_string(),
                        }
                    })
                    .clicked()
                {
                    out.push(ProgramCommand::EngageAutopilot {
                        apo_m: apo_km * 1000.0,
                        pe_m: apo_km * 1000.0,
                    });
                }
            });
        }
    }
    // The script console — every script bound to the focused vessel,
    // its run state and its armed wake.
    for s in &input.scripts {
        ui.horizontal(|ui| {
            let (mark, col) = if s.enabled {
                ("▶", egui::Color32::from_rgb(140, 220, 140))
            } else {
                ("■", dim)
            };
            ui.label(
                egui::RichText::new(format!("{mark} script {} — {}", s.id, s.label))
                    .font(MONO)
                    .color(col),
            );
            if ui
                .small_button(if s.enabled { "pause" } else { "resume" })
                .on_hover_text(if s.enabled {
                    "park it — keeps its persist state".to_string()
                } else {
                    "resume on_tick — clears the strike ledger".to_string()
                })
                .clicked()
            {
                out.push(ProgramCommand::SetScriptEnabled {
                    script: s.id,
                    enabled: !s.enabled,
                });
            }
            if ui
                .small_button("unload")
                .on_hover_text("remove the script — drops its state and wake")
                .clicked()
            {
                out.push(ProgramCommand::UnloadScript { script: s.id });
            }
        });
        // The status line — strike count, the disable reason, and
        // the armed wake (the in-bubble case shows "tick: every").
        let mut status = Vec::new();
        if s.strikes > 0 {
            status.push(format!("strikes {}/5", s.strikes));
        }
        if let Some(r) = &s.disabled_reason {
            status.push(format!("disabled: {r}"));
        }
        status.push(match s.wake_tick {
            Some(t) => format!("wake t{t}"),
            None => "wake: none".to_string(),
        });
        ui.label(
            egui::RichText::new(format!("    {}", status.join(" · ")))
                .font(MONO)
                .color(dim)
                .small(),
        );
    }
    // The loaders — the stock library, then a paste-in buffer.
    if input.context.is_some() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("load:").color(dim).small());
            for (name, source) in STOCK_SCRIPTS {
                if ui
                    .small_button(*name)
                    .on_hover_text(format!("load the '{name}' stock script"))
                    .clicked()
                {
                    out.push(ProgramCommand::LoadScript {
                        source: (*source).to_string(),
                    });
                }
            }
        });
        let src_id = ui.scope_id().with("script_source");
        let mut source = ui.data_mut(|d| d.get_persisted::<String>(src_id).unwrap_or_default());
        ui.add(
            egui::TextEdit::multiline(&mut source)
                .hint_text("fn on_tick(ctx) { … } — or pick a stock script")
                .font(MONO)
                .desired_rows(3),
        );
        ui.data_mut(|d| d.insert_persisted(src_id, source.clone()));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !source.trim().is_empty(),
                    egui::Button::new("load source").small(),
                )
                .on_hover_text("compile + bind this script to the focused vessel")
                .clicked()
            {
                out.push(ProgramCommand::LoadScript { source });
            }
            if input.scripts.is_empty() && input.context.is_some() {
                ui.label(egui::RichText::new("no scripts loaded").color(dim).small());
            }
        });
    } else if input.scripts.is_empty() {
        ui.label(egui::RichText::new("no vessel in focus").color(dim));
    }
    if !input.script_log.is_empty() {
        ui.label(egui::RichText::new("script log:").color(dim).small());
        for n in input.script_log.iter().rev().take(12) {
            ui.label(
                egui::RichText::new(format!("t{} {}", n.tick, n.text))
                    .font(MONO)
                    .color(dim)
                    .small(),
            );
        }
    }
}

/// The window's armed-chute deploy button — emits a [`ProgramCommand`]
/// on click. C22-F2: gated on `residence == "bubble"` like the part
/// menu and action strip copies — the sim refuses a deploy on a
/// latched/rails vessel, so the ungated window row dead-clicked onto
/// the generic refusal. Split out so the gate is testable without
/// opening the whole (scrollable) window.
fn chute_deploy_button(
    ui: &mut egui::Ui,
    residence: &str,
    node: u64,
    out: &mut Vec<ProgramCommand>,
) {
    let ok = residence == "bubble";
    if ui
        .add_enabled(ok, egui::Button::new("deploy").small())
        .on_hover_text(if ok {
            "deploy the decelerator (armed → deployed)".to_string()
        } else {
            "deployable only while flying — not on the surface".to_string()
        })
        .clicked()
    {
        out.push(ProgramCommand::DeployPart { node });
    }
}

/// The Program window — overview, contracts, research, vessel science,
/// reliability. `open` mirrors the close button into the session flag.
/// Emits [`ProgramCommand`]s; `csp-view` resolves them into
/// `SimCommand`s on the outbox. Returns the window rect so the host
/// masks VAB pointer input against it (C14-F2).
#[allow(clippy::too_many_lines)] // one window = one pass
#[allow(clippy::cognitive_complexity)] // the scroll closure is section dispatch
pub fn program_panel(
    ctx: &egui::Context,
    input: &ProgramInput,
    open: &mut bool,
    out: &mut Vec<ProgramCommand>,
) -> egui::Rect {
    let mut o = *open;
    let dim = egui::Color32::from_gray(160);
    // Parks left-top on first open; the title bar drags it anywhere
    // after that (and the resize grip still holds the lower-right).
    let screen = ctx.content_rect();
    let rect = egui::Window::new("Program")
        .open(&mut o)
        .default_pos(screen.left_top() + egui::vec2(12.0, 34.0))
        .default_size([420.0, 560.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // Overview — the program ledger the player otherwise
                // never sees (funds/science were sim-only mirrors).
                sect(ui, "OVERVIEW");
                ui.label(
                    egui::RichText::new(format!(
                        "funds ₣ {:.0}   science ⚛ {:.1} (banked {:.1})",
                        input.funds, input.science, input.banked
                    ))
                    .font(MONO)
                    .color(egui::Color32::from_gray(220)),
                );

                // Contracts — offers are acceptable from here;
                // acceptance rides `AcceptContract`.
                sect(ui, "CONTRACTS");
                contracts_section(ui, input, out, dim);

                // W2.0/W2.3 — the program arena: vessels, roster,
                // colonies/sites/logistics, the alarm agenda.
                sect(ui, "VESSELS");
                vessels_section(ui, input, out, dim);
                sect(ui, "CREW");
                crew_section(ui, input, out, dim);
                sect(ui, "COLONIES");
                colonies_section(ui, input, out, dim);
                sect(ui, "AGENDA");
                agenda_section(ui, input, out, dim);
                // W2.5 — pillar 4's player surface: the ascent
                // autopilot + the script console.
                sect(ui, "AUTOMATION");
                automation_section(ui, input, out, dim);

                // Research — the spend side of the loop.
                sect(ui, "RESEARCH");
                match &input.tech {
                    None => {
                        ui.label(egui::RichText::new("no tech tree installed").color(dim));
                    }
                    Some(nodes) => {
                        for n in nodes {
                            let (mark, col) = if n.researched {
                                ("✓", egui::Color32::from_rgb(140, 220, 140))
                            } else {
                                ("○", egui::Color32::from_gray(200))
                            };
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(mark).color(col));
                                ui.label(
                                    egui::RichText::new(format!("{:<28} {:>5.0}", n.name, n.cost))
                                        .font(MONO)
                                        .color(col),
                                );
                                if !n.researched {
                                    let can = n.prereqs_met && n.affordable;
                                    if ui
                                        .add_enabled(can, egui::Button::new("research").small())
                                        .clicked()
                                    {
                                        out.push(ProgramCommand::ResearchTech {
                                            node: n.id.clone(),
                                        });
                                    }
                                    if !n.prereqs_met {
                                        ui.label(egui::RichText::new("prereqs").color(dim).small());
                                    } else if !n.affordable {
                                        // C22-F8: the tag fires on
                                        // BANKED science while the
                                        // headline number folds in
                                        // live lab accrual — the
                                        // hover keeps the bigger
                                        // figure from reading as a
                                        // gate bug.
                                        ui.label(egui::RichText::new("need ⚛").color(dim).small())
                                            .on_hover_text(format!(
                                                "spending uses banked science only — ⚛{:.1} banked \
                                                 of {:.0} needed; the headline ⚛{:.1} counts live \
                                                 lab accrual, which cannot be spent",
                                                input.banked, n.cost, input.science
                                            ));
                                    }
                                }
                            });
                        }
                    }
                }
                if !input.tracks.is_empty() {
                    ui.add_space(4.0);
                    for t in &input.tracks {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{:<28} {}/{}",
                                    t.name, t.level, t.levels
                                ))
                                .font(MONO)
                                .color(egui::Color32::from_gray(200)),
                            );
                            match t.next_cost {
                                Some(cost) => {
                                    let can = t.affordable && t.level < t.cap;
                                    if ui
                                        .add_enabled(
                                            can,
                                            egui::Button::new(format!("+{cost:.0}")).small(),
                                        )
                                        .clicked()
                                    {
                                        out.push(ProgramCommand::ResearchTrack {
                                            track: t.id.clone(),
                                        });
                                    }
                                    if t.level >= t.cap {
                                        ui.label(egui::RichText::new("capped").color(dim).small());
                                    }
                                }
                                None => {
                                    ui.label(
                                        egui::RichText::new(if t.cap == 0 {
                                            "locked"
                                        } else {
                                            "maxed"
                                        })
                                        .color(dim)
                                        .small(),
                                    );
                                }
                            }
                        });
                    }
                }

                // Vessel science — run instruments, bank data.
                sect(ui, "SCIENCE");
                match &input.context {
                    None => {
                        ui.label(egui::RichText::new("no vessel in focus").color(dim));
                    }
                    Some((vessel, situation, biome, body)) => {
                        ui.label(
                            egui::RichText::new(format!(
                                "{vessel} · {situation}{} @ {body}",
                                if biome.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {biome}")
                                }
                            ))
                            .font(MONO)
                            .color(dim),
                        );
                    }
                }
                for e in &input.experiments {
                    ui.horizontal(|ui| {
                        let status = if e.spent {
                            "spent".to_string()
                        } else {
                            format!("{:.0} data", e.data_units)
                        };
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {} ({})",
                                e.part, e.experiment, status
                            ))
                            .font(MONO)
                            .color(egui::Color32::from_gray(200)),
                        );
                        if ui
                            .add_enabled(e.runnable, egui::Button::new("run").small())
                            .on_hover_text(if e.runnable {
                                format!("collect {:.0} data units", e.data_units)
                            } else {
                                e.blocked.clone()
                            })
                            .clicked()
                        {
                            out.push(ProgramCommand::RunExperiment {
                                node: e.node,
                                experiment: e.experiment.clone(),
                            });
                        }
                    });
                }
                if input.experiments.is_empty() && input.context.is_some() {
                    ui.label(egui::RichText::new("no experiment modules aboard").color(dim));
                }
                // Vessel actions — §3.5's recovery verbs: a latched
                // hull recovers into funds/science/crew or terminates
                // to free its berth. Gated on `latched` (the sim's
                // `RecoverVessel` requires `SurfaceLatched`).
                if input.context.is_some() {
                    ui.horizontal(|ui| {
                        let recover_label = match &input.recover_site {
                            Some(site) => format!("recover vessel → {site}"),
                            None => "recover vessel (remote salvage)".to_string(),
                        };
                        if ui
                            .add_enabled(input.latched, egui::Button::new(recover_label).small())
                            .on_hover_text(if input.latched {
                                "recover the vessel: funds, science, crew home".to_string()
                            } else {
                                "vessel must be on the surface".to_string()
                            })
                            .clicked()
                        {
                            out.push(ProgramCommand::RecoverVessel);
                        }
                        if ui
                            .add_enabled(input.latched, egui::Button::new("terminate").small())
                            .on_hover_text(if input.latched {
                                "delete the vessel — no payout, frees its berth".to_string()
                            } else {
                                "vessel must be on the surface".to_string()
                            })
                            .clicked()
                        {
                            out.push(ProgramCommand::TerminateVessel);
                        }
                    });
                }
                for d in &input.pooled {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} · {:.1} data", d.label, d.units))
                                .font(MONO)
                                .color(egui::Color32::from_gray(200)),
                        );
                        // W1.5-(e): the EC gate disclosed — disabled
                        // with the cost/availability on hover instead
                        // of a silent drain-time refusal.
                        let have_ec = input.vessel_ec.unwrap_or(0.0);
                        let ec_ok = have_ec >= d.transmit_ec_cost;
                        if ui
                            .add_enabled(
                                ec_ok,
                                egui::Button::new(format!("transmit +{:.1}⚛", d.transmit_yield))
                                    .small(),
                            )
                            .on_hover_text(if ec_ok {
                                format!("drains {:.0} EC (have {:.0})", d.transmit_ec_cost, have_ec)
                            } else {
                                format!("needs {:.0} EC (have {:.0})", d.transmit_ec_cost, have_ec)
                            })
                            .clicked()
                        {
                            out.push(ProgramCommand::Transmit { row: d.index });
                        }
                        if ui
                            .add_enabled(
                                input.latched,
                                egui::Button::new(format!("recover +{:.1}⚛", d.recover_yield))
                                    .small(),
                            )
                            .on_hover_text(if input.latched {
                                "bank and close this record".to_string()
                            } else {
                                "vessel must be on the surface".to_string()
                            })
                            .clicked()
                        {
                            out.push(ProgramCommand::Recover { row: d.index });
                        }
                        if d.transmitted {
                            ui.label(egui::RichText::new("tx'd").color(dim).small());
                        }
                    });
                }
                // W2.0: decelerator deploy — `DeployPart` had no
                // producer; an armed chute shows its button.
                for ch in &input.chutes {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} · parachute", ch.part))
                                .font(MONO)
                                .color(egui::Color32::from_gray(200)),
                        );
                        if ch.deployed {
                            ui.label(egui::RichText::new("deployed").color(dim).small());
                        } else {
                            chute_deploy_button(ui, input.residence.as_str(), ch.node, out);
                        }
                    });
                }
                // W2.0: labs + live projects — the data→science
                // conversion the sim runs fully (`StartResearch`
                // escrows a pooled key; `StopResearch` banks accrual).
                if !input.labs.is_empty() || !input.projects.is_empty() {
                    sect(ui, "LABS");
                    for l in &input.labs {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · lab ({} slot{}){}",
                                    l.part,
                                    l.slots,
                                    if l.slots == 1 { "" } else { "s" },
                                    if l.running { " — running" } else { "" }
                                ))
                                .font(MONO)
                                .color(egui::Color32::from_gray(200)),
                            );
                        });
                        // An idle lab converts any pooled record —
                        // one button per pooled row. C22-F3: gated on
                        // crew aboard — the resolver drops a crewless
                        // start with "NO CREW ABOARD", so the button
                        // says why on hover instead of dead-clicking.
                        if !l.running {
                            for d in &input.pooled {
                                if ui
                                    .add_enabled(
                                        input.crew_aboard,
                                        egui::Button::new(format!("  research {}", d.label))
                                            .small(),
                                    )
                                    .on_hover_text(if input.crew_aboard {
                                        format!(
                                            "start the lab on {:.1} data (crew aboard staff it)",
                                            d.units
                                        )
                                    } else {
                                        "no crew aboard — a lab needs staff to run".to_string()
                                    })
                                    .clicked()
                                {
                                    out.push(ProgramCommand::StartResearch {
                                        lab_node: l.node,
                                        row: d.index,
                                    });
                                }
                            }
                        }
                    }
                    for p in &input.projects {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "v{} {} · {:.1}⚛ acc · {:.1} data left · eta {}",
                                    p.vessel,
                                    p.label,
                                    p.accrued,
                                    p.data_left,
                                    fmt_eta(p.eta_s)
                                ))
                                .font(MONO)
                                .color(egui::Color32::from_gray(200)),
                            );
                            if ui
                                .small_button("stop")
                                .on_hover_text("bank the accrued science and release the rest")
                                .clicked()
                            {
                                out.push(ProgramCommand::StopResearch { project: p.id });
                            }
                        });
                    }
                }

                // W2.6 — the manufacture + qualification surface:
                // build orders in flight, per-family qualification,
                // and the bench-test buttons (`RunBenchTest` had a
                // complete apply but no producer).
                sect(ui, "FACILITIES");
                facilities_section(ui, input, out, dim);

                // Reliability — failures actually fire in the sim
                // (dormant hazard, ignition misfires, Λ crossings);
                // this is where the player sees them.
                if !input.reliability.is_empty() {
                    sect(ui, "RELIABILITY");
                    for r in &input.reliability {
                        let (text, col) = if !r.failures.is_empty() {
                            (
                                format!("{} — FAILED: {}", r.part, r.failures.join(", ")),
                                egui::Color32::from_rgb(235, 120, 90),
                            )
                        } else if r.misfire {
                            (
                                format!("{} — ignition misfire", r.part),
                                egui::Color32::from_rgb(235, 180, 80),
                            )
                        } else if r.thrust_factor < 1.0 {
                            (
                                format!("{} — thrust degraded", r.part),
                                egui::Color32::from_rgb(235, 180, 80),
                            )
                        } else {
                            let w = r.wear.unwrap_or(0.0) * 100.0;
                            (
                                format!(
                                    "{} — ok (wear {:.0}%{})",
                                    r.part,
                                    w,
                                    if r.dormant { ", dormant" } else { "" }
                                ),
                                egui::Color32::from_gray(160),
                            )
                        };
                        ui.label(egui::RichText::new(text).font(MONO).color(col));
                        // §3.4's flight half (C10-F2/C22-F5): the
                        // authored stress rules marked against the
                        // live regime the staging mirror evaluated —
                        // the same `▶`-active/weak idiom the VAB
                        // inspector uses. A dormant row carries no
                        // product: the dormant λ ignores the rules,
                        // so showing them would claim stress the sim
                        // is not accruing.
                        if !r.dormant {
                            for (rule, active) in &r.stress {
                                let text = crate::vab::stress_rule_text(rule);
                                if *active {
                                    ui.label(
                                        egui::RichText::new(format!("    ▶ {text}"))
                                            .font(MONO)
                                            .color(egui::Color32::YELLOW)
                                            .small(),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(format!("      {text}"))
                                            .font(MONO)
                                            .color(dim)
                                            .small(),
                                    );
                                }
                            }
                            if !r.stress.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "    Π stress ×{:.3}",
                                        r.stress_product
                                    ))
                                    .font(MONO)
                                    .color(if r.stress_product > 1.0 {
                                        egui::Color32::from_rgb(235, 180, 80)
                                    } else {
                                        dim
                                    })
                                    .small(),
                                );
                            }
                        }
                    }
                }
                if !input.notices.is_empty() {
                    sect(ui, "LOG");
                    for n in &input.notices {
                        ui.label(
                            egui::RichText::new(format!("t{} {}", n.tick, n.text))
                                .font(MONO)
                                .color(dim),
                        );
                    }
                }
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING);
    *open = o;
    rect
}

// ---------------------------------------------------------------------------
// The in-flight action surfaces — the science/recovery loop reachable
// where the player actually looks: a right-click part menu (the
// KSP-style per-part action list) and an always-visible Actions strip.
// Both emit the same [`ProgramCommand`]s the Program window does —
// `csp-view` resolves them against the same `ProgramMirror`.
// ---------------------------------------------------------------------------

/// What the in-flight surfaces emit beside [`ProgramCommand`]s — the
/// session-state "open the Program window" affordance is a panel
/// flag, not a sim command, so it rides its own channel.
#[derive(Clone, Debug, Default)]
pub struct FlightActionOutput {
    /// Emitted commands — `build_ui` resolves them against the
    /// `ProgramMirror` exactly like the window's own clicks.
    pub commands: Vec<ProgramCommand>,
    /// Open the Program window (the "more" affordance).
    pub open_program: bool,
}

/// The right-click part menu's session model — `csp-view` owns it as
/// a resource (§2.2-C); this module owns the shape. `node = None` is
/// the whole-vessel menu: a hull click (or a part the mirror cannot
/// name) lists every part's actions instead.
#[derive(Clone, Debug)]
pub struct PartMenuState {
    /// The menu is on screen.
    pub open: bool,
    /// The picked `PartNodeId` — `None` for the whole-vessel list.
    pub node: Option<u64>,
    /// The vessel the menu opened on (`VesselId.0`, `u64::MAX` when
    /// none) — `build_ui` closes the menu when the program mirror's
    /// focus moves off it: a stale menu's `node` would silently fire
    /// the new focus's same-numbered part.
    pub vessel: u64,
    /// Anchor position (egui points) — the click's own pixel.
    pub pos: egui::Pos2,
}

impl Default for PartMenuState {
    fn default() -> Self {
        Self {
            open: false,
            node: None,
            vessel: u64::MAX,
            pos: egui::Pos2::ZERO,
        }
    }
}

/// One part-action row set — the experiments and decelerators on
/// node `n`, shared by the single-part and whole-vessel menus.
fn part_node_actions(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    n: u64,
    out: &mut FlightActionOutput,
    dim: egui::Color32,
) {
    for e in input.experiments.iter().filter(|e| e.node == n) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    e.runnable,
                    egui::Button::new(format!("⚛ run {}", e.experiment)).small(),
                )
                .on_hover_text(if e.runnable {
                    format!("collect {:.0} data units in this context", e.data_units)
                } else {
                    e.blocked.clone()
                })
                .clicked()
            {
                out.commands.push(ProgramCommand::RunExperiment {
                    node: e.node,
                    experiment: e.experiment.clone(),
                });
            }
            if e.spent {
                ui.label(egui::RichText::new("spent").color(dim).small());
            }
        });
    }
    for ch in input.chutes.iter().filter(|c| c.node == n) {
        ui.horizontal(|ui| {
            if ch.deployed {
                ui.label(
                    egui::RichText::new("parachute — deployed")
                        .font(MONO)
                        .color(dim),
                );
            } else {
                let ok = input.residence == "bubble";
                if ui
                    .add_enabled(ok, egui::Button::new("deploy parachute").small())
                    .on_hover_text(if ok {
                        "deploy the decelerator (armed → deployed)".to_string()
                    } else {
                        "deployable only while flying — not on the surface".to_string()
                    })
                    .clicked()
                {
                    out.commands
                        .push(ProgramCommand::DeployPart { node: ch.node });
                }
            }
        });
    }
}

/// The pooled-data rows — vessel-level (the pool merges by `DataKey`,
/// not per part): transmit for the fraction now, recover for the
/// full yield on the surface. Shared by both menus and the action
/// strip.
fn pooled_rows(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut FlightActionOutput,
    dim: egui::Color32,
    full_label: bool,
) {
    for d in &input.pooled {
        if full_label {
            ui.label(
                egui::RichText::new(format!("{} · {:.1} data", d.label, d.units))
                    .font(MONO)
                    .color(dim)
                    .small(),
            );
        }
        ui.horizontal(|ui| {
            if !full_label {
                ui.label(
                    egui::RichText::new(format!("{:.1} data", d.units))
                        .font(MONO)
                        .color(dim)
                        .small(),
                );
            }
            let have_ec = input.vessel_ec.unwrap_or(0.0);
            let ec_ok = have_ec >= d.transmit_ec_cost;
            if ui
                .add_enabled(
                    ec_ok,
                    egui::Button::new(format!("transmit +{:.1}⚛", d.transmit_yield)).small(),
                )
                .on_hover_text(if ec_ok {
                    format!(
                        "{} — banks now, drains {:.0} EC (have {:.0})",
                        d.label, d.transmit_ec_cost, have_ec
                    )
                } else {
                    format!(
                        "{} — needs {:.0} EC (have {:.0})",
                        d.label, d.transmit_ec_cost, have_ec
                    )
                })
                .clicked()
            {
                out.commands.push(ProgramCommand::Transmit { row: d.index });
            }
            if ui
                .add_enabled(
                    input.latched,
                    egui::Button::new(format!("recover +{:.1}⚛", d.recover_yield)).small(),
                )
                .on_hover_text(if input.latched {
                    format!("{} — bank and close on the surface", d.label)
                } else {
                    format!("{} — land and settle first to recover", d.label)
                })
                .clicked()
            {
                out.commands.push(ProgramCommand::Recover { row: d.index });
            }
            if d.transmitted {
                ui.label(egui::RichText::new("tx'd").color(dim).small());
            }
        });
    }
}

/// The vessel-level recovery verbs — the whole-hull `RecoverVessel`
/// the "how do I get the craft back" question names, gated on the
/// same `SurfaceLatched` mirror the panel's row uses.
fn vessel_recovery_row(
    ui: &mut egui::Ui,
    input: &ProgramInput,
    out: &mut FlightActionOutput,
    dim: egui::Color32,
) {
    ui.horizontal(|ui| {
        let recover_label = match &input.recover_site {
            Some(site) => format!("recover vessel → {site}"),
            None => "recover vessel (remote salvage)".to_string(),
        };
        if ui
            .add_enabled(input.latched, egui::Button::new(recover_label).small())
            .on_hover_text(if input.latched {
                "recover the vessel: funds, pooled science, and crew come home".to_string()
            } else {
                "land and settle first — the vessel must be on the surface".to_string()
            })
            .clicked()
        {
            out.commands.push(ProgramCommand::RecoverVessel);
        }
        if ui
            .add_enabled(input.latched, egui::Button::new("terminate").small())
            .on_hover_text(if input.latched {
                "delete the vessel — no payout, frees its berth".to_string()
            } else {
                "vessel must be on the surface".to_string()
            })
            .clicked()
        {
            out.commands.push(ProgramCommand::TerminateVessel);
        }
        let _ = dim;
    });
}

/// The right-click part menu — the KSP-style in-context action list,
/// anchored at the click's own pixel. A picked node shows *that
/// part's* experiments/chute plus the vessel-level rows (pooled data
/// is vessel-pooled, not per-part); `state.node = None` lists every
/// actionable part instead. Emits [`ProgramCommand`]s — `csp-view`
/// resolves them against the same `ProgramMirror` the window uses.
/// `state.open` flips `false` on the title ✕.
pub fn part_menu(
    ctx: &egui::Context,
    input: &ProgramInput,
    state: &mut PartMenuState,
    out: &mut FlightActionOutput,
) -> egui::Rect {
    if !state.open {
        return egui::Rect::NOTHING;
    }
    let dim = egui::Color32::from_gray(170);
    let title = match state.node {
        Some(n) => input
            .parts
            .iter()
            .find(|p| p.node == n)
            .map(|p| p.part.clone())
            .unwrap_or_else(|| format!("part {n}")),
        None => input
            .context
            .as_ref()
            .map(|c| format!("{} — all parts", c.0))
            .unwrap_or_else(|| "vessel".to_string()),
    };
    let mut open = true;
    let rect = egui::Window::new(title)
        .open(&mut open)
        .fixed_pos(state.pos)
        .constrain_to(ctx.content_rect())
        .resizable(false)
        .collapsible(false)
        .max_width(340.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    // THIS PART — experiments and the decelerator.
                    // `None` = the whole-vessel list (one group per
                    // actionable part).
                    let nodes: Vec<u64> = match state.node {
                        Some(n) => vec![n],
                        None => input.parts.iter().map(|p| p.node).collect(),
                    };
                    let mut any = false;
                    for n in &nodes {
                        let has = input.experiments.iter().any(|e| e.node == *n)
                            || input.chutes.iter().any(|c| c.node == *n);
                        if !has {
                            continue;
                        }
                        any = true;
                        if state.node.is_none() {
                            let name = input
                                .parts
                                .iter()
                                .find(|p| p.node == *n)
                                .map(|p| p.part.as_str())
                                .unwrap_or("part");
                            ui.label(
                                egui::RichText::new(name)
                                    .font(MONO)
                                    .color(egui::Color32::from_gray(220)),
                            );
                        }
                        part_node_actions(ui, input, *n, out, dim);
                    }
                    if !any {
                        ui.label(egui::RichText::new("no actions on this part").color(dim));
                    }
                    // THE VESSEL — pooled science and the recovery
                    // verbs live on the hull, not on one part.
                    if !input.pooled.is_empty() {
                        sect(ui, "DATA ABOARD");
                        pooled_rows(ui, input, out, dim, true);
                    }
                    sect(ui, "VESSEL");
                    vessel_recovery_row(ui, input, out, dim);
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("⚛ Program window")
                            .on_hover_text("contracts · research · experiments · reliability")
                            .clicked()
                        {
                            out.open_program = true;
                        }
                        ui.label(
                            egui::RichText::new("right-click any part for its actions")
                                .color(dim)
                                .small(),
                        );
                    });
                });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING);
    if !open {
        state.open = false;
    }
    rect
}

/// The flight-mode Actions strip — the always-visible surface for
/// the science/recovery loop, parked bottom-right of the flight view
/// so the loop's verbs sit where the player is already looking (the
/// right-click menu's sibling: nothing here needs a secret gesture).
/// One click runs every runnable experiment aboard; pooled records
/// get their transmit/recover buttons; armed decelerators a deploy;
/// a landed vessel its big RECOVER row; and the Program window is
/// one click further. Emits [`ProgramCommand`]s via `out` — `csp-view`
/// resolves them against the `ProgramMirror`.
#[allow(clippy::too_many_lines)] // one action strip = one pass
pub fn flight_action_bar(
    ctx: &egui::Context,
    input: &ProgramInput,
    keys: &crate::settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
    out: &mut FlightActionOutput,
) -> egui::Rect {
    let screen = ctx.content_rect();
    let dim = egui::Color32::from_gray(170);
    egui::Window::new("Actions")
        .default_pos(egui::pos2(screen.right() - 300.0, screen.bottom() - 260.0))
        .collapsible(true)
        .resizable(false)
        .show(ctx, |ui| {
            match &input.context {
                None => {
                    ui.label(egui::RichText::new("no vessel in focus").color(dim));
                }
                Some((vessel, situation, biome, body)) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "{vessel} · {situation}{} @ {body}",
                            if biome.is_empty() {
                                String::new()
                            } else {
                                format!(" · {biome}")
                            }
                        ))
                        .font(MONO)
                        .color(dim),
                    );
                }
            }
            // Science — one click runs every runnable module aboard;
            // a blocked set says why instead of dead-clicking.
            if !input.experiments.is_empty() {
                let runnable: Vec<&ExperimentRow> =
                    input.experiments.iter().filter(|e| e.runnable).collect();
                let (label, hover) = if runnable.is_empty() {
                    (
                        "⚛ run science".to_string(),
                        input
                            .experiments
                            .iter()
                            .find(|e| !e.blocked.is_empty())
                            .map(|e| e.blocked.clone())
                            .unwrap_or_else(|| "no experiment can run in this context".to_string()),
                    )
                } else {
                    (
                        format!("⚛ run science ({})", runnable.len()),
                        "run every runnable experiment aboard".to_string(),
                    )
                };
                if ui
                    .add_enabled(!runnable.is_empty(), egui::Button::new(label).small())
                    .on_hover_text(hover)
                    .clicked()
                {
                    for e in runnable {
                        out.commands.push(ProgramCommand::RunExperiment {
                            node: e.node,
                            experiment: e.experiment.clone(),
                        });
                    }
                }
            }
            // Pooled data — the two ways science leaves the hull.
            if !input.pooled.is_empty() {
                pooled_rows(ui, input, out, dim, false);
            }
            // Armed decelerators — `DeployPart` (in-flight only).
            for ch in input.chutes.iter().filter(|c| !c.deployed) {
                let ok = input.residence == "bubble";
                if ui
                    .add_enabled(ok, egui::Button::new(format!("deploy {}", ch.part)).small())
                    .on_hover_text(if ok {
                        "deploy the decelerator".to_string()
                    } else {
                        "deployable only while flying".to_string()
                    })
                    .clicked()
                {
                    out.commands
                        .push(ProgramCommand::DeployPart { node: ch.node });
                }
            }
            // The landed-recovery verb — surfaced loud, not buried:
            // this is the button the "how do I recover the craft"
            // question looks for.
            if input.context.is_some() {
                let label = match &input.recover_site {
                    Some(site) => format!("⏏ RECOVER VESSEL → {site}"),
                    None => "⏏ RECOVER VESSEL (remote salvage)".to_string(),
                };
                if ui
                    .add_enabled(
                        input.latched,
                        egui::Button::new(
                            egui::RichText::new(label)
                                .color(egui::Color32::from_rgb(150, 230, 150)),
                        ),
                    )
                    .on_hover_text(if input.latched {
                        "recover the vessel: funds, pooled science, and crew come home"
                    } else {
                        "land and settle first — the vessel must be on the surface"
                    })
                    .clicked()
                {
                    out.commands.push(ProgramCommand::RecoverVessel);
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .small_button(format!(
                        "⚛ Program [{}]",
                        keys.hint_labeled(crate::settings::KeyAction::OpenProgram, key_labels)
                    ))
                    .on_hover_text("contracts · research · experiments · reliability")
                    .clicked()
                {
                    out.open_program = true;
                }
                ui.label(
                    egui::RichText::new("right-click a part for its actions")
                        .color(dim)
                        .small(),
                );
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING)
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A minimal latched-vessel mirror: one experiment (node 3,
    /// runnable), one pooled record, one armed chute (node 0).
    fn demo_input() -> ProgramInput {
        ProgramInput {
            context: Some((
                "vessel 1".to_string(),
                "landed".to_string(),
                "grass".to_string(),
                "terrin".to_string(),
            )),
            latched: true,
            recover_site: Some("terrin-launch".to_string()),
            vessel_ec: Some(50.0),
            residence: "latched".to_string(),
            parts: vec![
                PartRow {
                    node: 0,
                    part: "pod".to_string(),
                },
                PartRow {
                    node: 3,
                    part: "canister".to_string(),
                },
            ],
            experiments: vec![ExperimentRow {
                node: 3,
                part: "canister".to_string(),
                experiment: "goo".to_string(),
                data_units: 10.0,
                reusable: true,
                spent: false,
                runnable: true,
                blocked: String::new(),
            }],
            pooled: vec![DataRow {
                index: 0,
                label: "goo@grass/terrin·landed".to_string(),
                units: 10.0,
                transmit_yield: 4.0,
                transmit_ec_cost: 2.0,
                recover_yield: 10.0,
                transmitted: false,
            }],
            chutes: vec![ChuteRow {
                node: 0,
                part: "pod".to_string(),
                deployed: false,
            }],
            ..Default::default()
        }
    }

    fn egui_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            focused: true,
            events,
            ..Default::default()
        }
    }

    fn primary_click(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    /// The menu draws only while open, and a click on its "run" row
    /// emits the picked node's `RunExperiment` — the in-context path
    /// into the same command vocabulary the Program window uses.
    #[test]
    fn part_menu_runs_the_clicked_parts_experiment() {
        let ctx = egui::Context::default();
        let input = demo_input();
        let mut state = PartMenuState::default();
        let mut out = FlightActionOutput::default();
        // Closed → draws nothing.
        assert_eq!(
            part_menu(&ctx, &input, &mut state, &mut out),
            egui::Rect::NOTHING
        );
        // Open on node 3 (the canister) at a fixed anchor.
        state = PartMenuState {
            open: true,
            node: Some(3),
            vessel: 1,
            pos: egui::pos2(200.0, 200.0),
        };
        let mut rect = egui::Rect::NOTHING;
        ctx.run_pass(egui_input(Vec::new()), |ctx| {
            rect = part_menu(ctx, &input, &mut state, &mut out);
        })
        .drop_without_applying_deltas();
        assert!(rect.is_positive(), "an open part menu must draw");
        // Scan the menu's upper rows for the "run" button — a click
        // on it must emit RunExperiment for node 3.
        let mut fired = false;
        'scan: for iy in 0..8 {
            for ix in 0..10 {
                let pos = egui::pos2(210.0 + ix as f32 * 12.0, 225.0 + iy as f32 * 10.0);
                let mut pass_out = FlightActionOutput::default();
                ctx.run_pass(egui_input(primary_click(pos)), |ctx| {
                    part_menu(ctx, &input, &mut state, &mut pass_out);
                })
                .drop_without_applying_deltas();
                if pass_out
                    .commands
                    .iter()
                    .any(|c| matches!(c, ProgramCommand::RunExperiment { node: 3, .. }))
                {
                    fired = true;
                    break 'scan;
                }
            }
        }
        assert!(fired, "no click inside the part menu ran the experiment");
    }

    /// The Actions strip draws a real window and its Program button
    /// asks for the window — the discoverable door into the loop.
    #[test]
    fn action_bar_draws_and_offers_the_program_window() {
        let ctx = egui::Context::default();
        let input = demo_input();
        let keys = crate::settings::Keymap::default();
        let labels = std::collections::BTreeMap::new();
        let mut out = FlightActionOutput::default();
        let mut rect = egui::Rect::NOTHING;
        ctx.run_pass(egui_input(Vec::new()), |ctx| {
            rect = flight_action_bar(ctx, &input, &keys, &labels, &mut out);
        })
        .drop_without_applying_deltas();
        assert!(rect.is_positive(), "the Actions strip must draw");
        // Click-scan the drawn window below its title bar — the
        // collapse arrow lives in the title row and folding the
        // strip would hide every button under the scan.
        let mut opened = false;
        'scan: for iy in 0..40 {
            for ix in 0..24 {
                let pos = egui::pos2(
                    rect.left() + 8.0 + ix as f32 * 12.0,
                    rect.top() + 30.0 + iy as f32 * 10.0,
                );
                if !rect.contains(pos) {
                    continue;
                }
                let mut pass_out = FlightActionOutput::default();
                ctx.run_pass(egui_input(primary_click(pos)), |ctx| {
                    flight_action_bar(ctx, &input, &keys, &labels, &mut pass_out);
                })
                .drop_without_applying_deltas();
                if pass_out.open_program {
                    opened = true;
                    break 'scan;
                }
            }
        }
        assert!(opened, "the Actions strip's Program button never clicked");
    }

    /// C22-F7: the transfer labels name the amount the button
    /// actually sends — whole units print bare, fractions keep one
    /// decimal, so a "10" that would send 3.4 reads "3.4".
    #[test]
    fn fmt_amount_names_the_sent_units() {
        assert_eq!(fmt_amount(10.0_f64.min(25.0)), "10");
        assert_eq!(fmt_amount(10.0_f64.min(3.4)), "3.4");
        assert_eq!(fmt_amount(7.0), "7");
        assert_eq!(fmt_amount(0.25), "0.2");
    }

    /// C22-F2: the window's deploy button carries the same
    /// `residence == "bubble"` gate as the part menu and action
    /// strip — enabled in flight, dead-click-proof on the pad.
    #[test]
    fn chute_deploy_button_gates_on_bubble() {
        let ctx = egui::Context::default();
        let draw = |residence: &'static str, out: &mut Vec<ProgramCommand>| {
            ctx.run_pass(egui_input(Vec::new()), |ctx| {
                egui::Area::new(egui::Id::unique("chute_test")).show(ctx, |ui| {
                    chute_deploy_button(ui, residence, 7, out);
                });
            })
            .drop_without_applying_deltas();
        };
        // Bubble residence → the button draws and clicks.
        let mut out = Vec::new();
        draw("bubble", &mut out);
        'scan: for iy in 0..4 {
            for ix in 0..8 {
                let pos = egui::pos2(10.0 + ix as f32 * 10.0, 15.0 + iy as f32 * 10.0);
                ctx.run_pass(egui_input(primary_click(pos)), |ctx| {
                    egui::Area::new(egui::Id::unique("chute_test")).show(ctx, |ui| {
                        chute_deploy_button(ui, "bubble", 7, &mut out);
                    });
                })
                .drop_without_applying_deltas();
                if out
                    .iter()
                    .any(|c| matches!(c, ProgramCommand::DeployPart { node: 7 }))
                {
                    break 'scan;
                }
            }
        }
        assert!(
            out.iter()
                .any(|c| matches!(c, ProgramCommand::DeployPart { node: 7 })),
            "an in-flight deploy must emit DeployPart"
        );
        // Latched residence → disabled: no click can emit the verb.
        let mut out = Vec::new();
        draw("latched", &mut out);
        for iy in 0..4 {
            for ix in 0..8 {
                let pos = egui::pos2(10.0 + ix as f32 * 10.0, 15.0 + iy as f32 * 10.0);
                ctx.run_pass(egui_input(primary_click(pos)), |ctx| {
                    egui::Area::new(egui::Id::unique("chute_test")).show(ctx, |ui| {
                        chute_deploy_button(ui, "latched", 7, &mut out);
                    });
                })
                .drop_without_applying_deltas();
            }
        }
        assert!(
            out.is_empty(),
            "a parked vessel's deploy button cannot emit: {out:?}"
        );
    }

    /// C08-F5's board un-jam is clickable: an offer row's decline
    /// button emits `DeclineContract` with the row's offer index —
    /// enabled even when the offer's own accept gate is closed (the
    /// unsatisfiable draw is exactly what decline exists for).
    #[test]
    fn offer_row_decline_emits_on_prereq_unmet_offer() {
        let ctx = egui::Context::default();
        let input = ProgramInput {
            offers: vec![OfferRow {
                index: 4,
                label: "#4 sat orbit".to_string(),
                description: String::new(),
                detail: String::new(),
                reward: String::new(),
                deadline: String::new(),
                prereqs: vec![(false, "needs a tracking station".to_string())],
                accept_ok: false,
            }],
            ..Default::default()
        };
        let mut out = Vec::new();
        'scan: for iy in 0..6 {
            for ix in 0..24 {
                let pos = egui::pos2(10.0 + ix as f32 * 12.0, 15.0 + iy as f32 * 10.0);
                ctx.run_pass(egui_input(primary_click(pos)), |ctx| {
                    egui::Area::new(egui::Id::unique("contract_test")).show(ctx, |ui| {
                        contracts_section(ui, &input, &mut out, egui::Color32::from_gray(170));
                    });
                })
                .drop_without_applying_deltas();
                if out
                    .iter()
                    .any(|c| matches!(c, ProgramCommand::DeclineContract { offer: 4 }))
                {
                    break 'scan;
                }
            }
        }
        assert!(
            out.iter()
                .any(|c| matches!(c, ProgramCommand::DeclineContract { offer: 4 })),
            "the offer row's decline button must emit DeclineContract: {out:?}"
        );
        // The same scan crossed the accept button — its gate held,
        // so nothing accepted.
        assert!(
            out.iter()
                .all(|c| !matches!(c, ProgramCommand::AcceptContract { .. })),
            "a prereq-unmet offer must not emit AcceptContract: {out:?}"
        );
    }

    /// An accepted contract renders no decline affordance — its
    /// only exits are completion and the deadline (the free-abandon
    /// faucet the sim deliberately keeps closed). A full click-scan
    /// of a section holding only active rows emits nothing.
    #[test]
    fn active_contract_row_offers_no_verb() {
        let ctx = egui::Context::default();
        let input = ProgramInput {
            active: vec![ActiveRow {
                label: "#7 ore run".to_string(),
                detail: "deliver 10 ore · 3.2 d left".to_string(),
                progress: "delivered 4/10".to_string(),
            }],
            ..Default::default()
        };
        let mut out = Vec::new();
        for iy in 0..6 {
            for ix in 0..24 {
                let pos = egui::pos2(10.0 + ix as f32 * 12.0, 15.0 + iy as f32 * 10.0);
                ctx.run_pass(egui_input(primary_click(pos)), |ctx| {
                    egui::Area::new(egui::Id::unique("contract_active")).show(ctx, |ui| {
                        contracts_section(ui, &input, &mut out, egui::Color32::from_gray(170));
                    });
                })
                .drop_without_applying_deltas();
            }
        }
        assert!(
            out.is_empty(),
            "an accepted-contract row must carry no verb: {out:?}"
        );
    }

    /// C11-F3's mis-order exit is clickable: an in-flight build
    /// row's cancel button emits `CancelBuild` with the row's
    /// `BuildId`.
    #[test]
    fn build_row_cancel_emits_cancel_build() {
        let ctx = egui::Context::default();
        let input = ProgramInput {
            builds: vec![BuildRow {
                id: 9,
                site: "terrin-launch".to_string(),
                label: "5-part craft".to_string(),
                eta_s: 120.0,
            }],
            ..Default::default()
        };
        let mut out = Vec::new();
        'scan: for iy in 0..6 {
            for ix in 0..40 {
                let pos = egui::pos2(10.0 + ix as f32 * 12.0, 15.0 + iy as f32 * 10.0);
                ctx.run_pass(egui_input(primary_click(pos)), |ctx| {
                    egui::Area::new(egui::Id::unique("facilities_test")).show(ctx, |ui| {
                        facilities_section(ui, &input, &mut out, egui::Color32::from_gray(170));
                    });
                })
                .drop_without_applying_deltas();
                if out
                    .iter()
                    .any(|c| matches!(c, ProgramCommand::CancelBuild { build: 9 }))
                {
                    break 'scan;
                }
            }
        }
        assert!(
            out.iter()
                .any(|c| matches!(c, ProgramCommand::CancelBuild { build: 9 })),
            "the build row's cancel button must emit CancelBuild: {out:?}"
        );
    }
}
