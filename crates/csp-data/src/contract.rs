//! The §3.6 contract schema — `data/contracts/<id>.ron`.
//!
//! A [`ContractTemplate`] is everything the plan names: parameters
//! (the generation space the offer draws concretize), generator
//! weights, prerequisites, rewards, deadline, and failure penalty.
//! The template itself is data; the *offer* it spawns is sim state —
//! the sim's deterministic generator ([`csp_sim::contracts`]) draws a
//! template by `weight` and then draws concrete
//! [`ContractParams`] from the authored space.
//!
//! # The v1 kinds
//!
//! §3.6 names seven: satellite placement, crew transport, rescue,
//! survey data run, part test at regime, base resupply, tourist
//! flight. M7-D shipped the four whose predicates had honest
//! sim-state machinery, M8-C added the resupply, and fix-plan 7.3
//! (C34-F5) landed the two crewed kinds once the crew machinery
//! (`TransferCargo::Crew`, `land_crew`, `CrewLocation`) could prove
//! their facts. Completion is a predicate over sim state checked on
//! relevant events:
//!
//! - [`ContractKind::SatellitePlacement`] — a vessel reaches a
//!   matching orbit (checked on orbit-achieved triggers).
//! - [`ContractKind::SurveyRun`] — data of an experiment kind at a
//!   body is returned home (checked on science-banking transitions).
//! - [`ContractKind::PartTest`] — a vessel carrying a matching part
//!   module reaches a regime (checked on orbit-achieved and
//!   surface-latch triggers).
//! - [`ContractKind::Rescue`] — a vessel latched at a body is
//!   recovered (checked on `RecoverVessel`).
//! - [`ContractKind::ColonyResupply`] — M8-C's base resupply: resource
//!   units delivered into the stores of colonies on a body (checked on
//!   `TransferComplete` landings — the vessel↔colony transfer is the
//!   only producer of the delivery fact).
//! - [`ContractKind::CrewTransport`] — deliver N crew to a named site:
//!   roster members posted at the colony occupying the site, via
//!   `AssignCrewToColony` or the vessel↔colony crew transfer (checked
//!   on the crew-posting transitions).
//! - [`ContractKind::TouristFlight`] — fly a `CrewKind::Tourist`
//!   through a drawn regime and return them alive: leg one stamps the
//!   rider on the orbit/surface triggers, leg two completes when the
//!   flown member is home (`Unassigned`) and `Active` — the recovery
//!   transition's crew walk.
//!
//! All seven §3.6 v1 kinds are authored (C34-F5 / fix-plan 7.3).
//!
//! # Prerequisites — facts, not reputation
//!
//! §3.6: there is no reputation currency. [`ContractPrereq`] is a
//! predicate over *concrete serialized facts* — tech researched, a
//! body's orbit/latch record, contracts completed by kind, crew lost.
//! The sim evaluates every entry (logical AND) at generation AND at
//! acceptance, and `standing` for display weighting is derived
//! elsewhere — never stored.

use serde::{Deserialize, Serialize};

use crate::ids::{BodyId, ContractTypeId, ExperimentId, LocKey, ResourceId, SiteId, TechNodeId};

/// The `schema_version` this loader understands for
/// `data/contracts/*.ron` — its own namespace (§3.1 first-field
/// convention).
pub const CONTRACT_SCHEMA_VERSION: u32 = 1;

/// The contract kind — what the completion predicate evaluates.
/// Discriminants are explicit, stable `u16`s (§7.1 rule 7):
/// append-only, never reordered.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ContractKind {
    /// A vessel reaches a matching orbit — the satellite contract.
    SatellitePlacement = 0,
    /// Science data of an experiment kind at a body is returned —
    /// the survey data run.
    SurveyRun = 1,
    /// A vessel carrying a matching part module reaches a regime —
    /// the part test.
    PartTest = 2,
    /// A vessel surface-latched at a body is recovered — the rescue.
    Rescue = 3,
    /// Resource units are delivered into colony stores on a body —
    /// the base resupply (M8-C).
    ColonyResupply = 4,
    /// Crew posted at the colony occupying a named site — the crew
    /// transport.
    CrewTransport = 5,
    /// A tourist rides a vessel through a regime and returns alive —
    /// the tourist flight.
    TouristFlight = 6,
}

impl csp_core::StableDiscriminant for ContractKind {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// `data/contracts/<id>.ron` — one contract template (§3.6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractTemplate {
    /// First field of every data file (§3.1). Must equal
    /// [`CONTRACT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable contract-type ID (`contract.satellite`) — also the
    /// file's stem (the validator checks `id` ↔ filename agreement).
    /// Tech nodes' `unlocks.contract_types` name these.
    pub id: ContractTypeId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// Loc key for the description blurb.
    pub description: LocKey,
    /// Which completion predicate this template instantiates.
    pub kind: ContractKind,
    /// The offer-generation space: `weight` is this template's share
    /// of the weighted pick among prereq-eligible templates; `space`
    /// carries the candidate sets/ranges the concrete params draw
    /// from. `space`'s variant must match `kind` (validator rule).
    pub generate: ContractGenerator,
    /// §3.6 fact-based prerequisites — every entry must hold for the
    /// template to be offerable. Checked at generation AND at
    /// `AcceptContract` (facts may have moved since the offer drew).
    pub prerequisites: Vec<ContractPrereq>,
    /// The funds lines — §3.5's "contracts (advance + completion)"
    /// plus the §3.6 failure penalty.
    pub rewards: ContractRewards,
    /// Days after acceptance the contract expires at (`None` = no
    /// deadline). The accept apply converts to the
    /// `ContractDeadline` event's tick — `Clamping`, §2.6.5.
    pub deadline_days: Option<f64>,
}

/// The offer-generation space for one template — the weighted pick's
/// weight plus the per-kind candidate sets the concrete
/// [`ContractParams`] draw from, in the documented draw order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractGenerator {
    /// Weighted-pick share (validator: finite, `> 0`). A template with
    /// weight `w` among eligible templates totaling `W` draws with
    /// probability `w/W`.
    pub weight: f64,
    /// The candidate space the offer's params draw from — the variant
    /// mirrors [`ContractTemplate::kind`] exactly.
    pub space: ContractParamSpace,
}

/// One template's param-generation candidates, per kind. Draw order
/// is the field order written here — the KDF stream's fixed arity
/// (§2.6.1: a stored `offer_index` reproduces the exact draw).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ContractParamSpace {
    /// `SatellitePlacement`: draw `body` from `bodies`, then the
    /// orbit's periapsis altitude and apoapsis altitude, each uniform
    /// in its `(lo, hi)` range. The bands may overlap partially
    /// (`apoapsis_alt_m.0 < periapsis_alt_m.0` is legal): when a
    /// drawn apoapsis cap lands under the drawn periapsis floor the
    /// sim orders the pair, so the published
    /// `[min_periapsis, max_apoapsis]` window is never empty and the
    /// offer stays satisfiable. A *fully* inverted space is not legal:
    /// the validator rejects `apoapsis_alt_m.1 < periapsis_alt_m.0`,
    /// where every draw would come out of the swap and the authored
    /// bands would carry none of their stated meaning. Altitudes are
    /// above the body's datum radius.
    SatellitePlacement {
        /// Candidate bodies — non-empty.
        bodies: Vec<BodyId>,
        /// `(lo, hi)` draw range for the required minimum periapsis
        /// altitude, metres.
        periapsis_alt_m: (f64, f64),
        /// `(lo, hi)` draw range for the maximum allowed apoapsis
        /// altitude, metres.
        apoapsis_alt_m: (f64, f64),
    },
    /// `SurveyRun`: draw `experiment` then `body`.
    SurveyRun {
        /// Candidate experiment kinds — non-empty.
        experiments: Vec<ExperimentId>,
        /// Candidate bodies — non-empty.
        bodies: Vec<BodyId>,
        /// Data units that must be *returned* at matching keys —
        /// fixed, not drawn (see [`ContractParams::SurveyData`] for
        /// the accounting).
        min_units: f64,
    },
    /// `PartTest`: draw the module requirement, then the regime.
    PartTest {
        /// Candidate module requirements — non-empty.
        modules: Vec<ModuleReq>,
        /// The regime space (orbit with an altitude draw, or landed).
        regime: RegimeSpace,
    },
    /// `Rescue`: draw `body`.
    Rescue {
        /// Candidate bodies — non-empty.
        bodies: Vec<BodyId>,
    },
    /// `ColonyResupply` (M8-C): draw `body`, then `resource`, then the
    /// delivery amount uniform in `amount`'s `(lo, hi)` range.
    ColonyResupply {
        /// Candidate bodies whose colonies resupply — non-empty.
        bodies: Vec<BodyId>,
        /// Candidate resources to deliver — non-empty.
        resources: Vec<ResourceId>,
        /// `(lo, hi)` draw range for the required delivered units.
        amount: (f64, f64),
    },
    /// `CrewTransport`: draw `site`, then the headcount uniform over
    /// `count`'s INCLUSIVE `(lo, hi)` integer range. The sim filters
    /// `sites` to those hosting a live colony that can take crew
    /// before the pick (the resupply draw's completability gate —
    /// a transport offer against nowhere to post could never
    /// complete).
    CrewTransport {
        /// Candidate sites whose colonies receive the crew —
        /// non-empty; every entry must resolve in `sites.ron`
        /// (colonies exist only at authored sites).
        sites: Vec<SiteId>,
        /// `(lo, hi)` draw range for the required headcount —
        /// `1 <= lo <= hi` (validator).
        count: (u32, u32),
    },
    /// `TouristFlight`: draw the regime the tourist must ride
    /// through before coming home — the same `RegimeSpace` the part
    /// test draws (orbit with an altitude draw, or landed).
    TouristFlight {
        /// The flight-profile regime space.
        regime: RegimeSpace,
    },
}

/// The regime a [`ContractKind::PartTest`] or
/// [`ContractKind::TouristFlight`] offer demands — the
/// generation-space form (with candidate bodies and draw ranges).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RegimeSpace {
    /// A bound orbit at a drawn body with periapsis altitude ≥ the
    /// drawn minimum.
    Orbit {
        /// Candidate bodies — non-empty.
        bodies: Vec<BodyId>,
        /// `(lo, hi)` draw range for the minimum periapsis altitude.
        min_alt_m: (f64, f64),
    },
    /// Surface-latched at a drawn body.
    Landed {
        /// Candidate bodies — non-empty.
        bodies: Vec<BodyId>,
    },
}

/// What a vessel's craft must carry for a [`ContractKind::PartTest`]
/// — a module *capability*, not a `PartId` (the weld record carries
/// resolved `WeldPart`s, which hold no part-id back-reference; the
/// honest sim-side check is on the resolved module presence).
/// Discriminants append-only.
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ModuleReq {
    /// A part with a `Lab` module.
    Lab = 0,
    /// A part with ≥1 docking port.
    DockPort = 1,
    /// A part with a `CrewSeat` module (≥1 seat).
    CrewSeat = 2,
    /// A part with an `Experiment` module of this kind.
    Experiment(ExperimentId) = 3,
    /// A part with ≥1 tank (any resource).
    Tank = 4,
    /// A part with ≥1 thrust `ForceSite`.
    ThrustSite = 5,
}

/// A concrete generated regime — the sim-state form of
/// [`RegimeSpace`] after the draw (shared by `PartTest` and
/// `TouristFlight` params).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ContractRegime {
    /// Bound orbit at `body`, periapsis altitude ≥ `min_alt_m` above
    /// the body's datum.
    Orbit {
        /// The orbit's parent body.
        body: BodyId,
        /// Minimum periapsis altitude (m above datum).
        min_alt_m: f64,
    },
    /// Surface-latched at `body`.
    Landed {
        /// The latched body.
        body: BodyId,
    },
}

/// The concrete parameters an offer carries — what the completion
/// predicate is evaluated against. Discriminants append-only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ContractParams {
    /// `SatellitePlacement`: a bound orbit at `body` with
    /// `min_periapsis_alt_m ≤ r_p − radius` and
    /// `r_a − radius ≤ max_apoapsis_alt_m`.
    SatelliteOrbit {
        /// The orbit's parent body.
        body: BodyId,
        /// Required minimum periapsis altitude (m above datum).
        min_periapsis_alt_m: f64,
        /// Maximum allowed apoapsis altitude (m above datum).
        max_apoapsis_alt_m: f64,
    },
    /// `SurveyRun`: cumulative *returned* data at keys matching
    /// `(experiment, body)` reaches `min_units`. The sim's per-key
    /// ledger tracks cumulative `collected`, not returns, so the
    /// predicate counts `collected` minus what is still outstanding
    /// (pooled aboard any vessel, escrowed inside a live lab
    /// project) at keys that have produced a return — transmitted at
    /// least once or closed by recovery. Units lost with a vessel
    /// are not distinguished; the ledger keeps no per-key loss
    /// record.
    SurveyData {
        /// The experiment kind.
        experiment: ExperimentId,
        /// The body the data came from.
        body: BodyId,
        /// Required returned units.
        min_units: f64,
    },
    /// `PartTest`: a vessel whose craft carries `module` reaches
    /// `regime`.
    PartTest {
        /// The module capability required aboard.
        module: ModuleReq,
        /// The regime the test must happen in.
        regime: ContractRegime,
    },
    /// `Rescue`: a vessel surface-latched at `body` is recovered.
    Rescue {
        /// The body the stranded vessel sits on.
        body: BodyId,
    },
    /// `ColonyResupply` (M8-C): cumulative resource units delivered
    /// into the stores of colonies on `body` reach `amount`.
    ColonyResupply {
        /// The body whose colonies take the delivery.
        body: BodyId,
        /// The resource delivered.
        resource: ResourceId,
        /// Required delivered units — cumulative, across every colony
        /// on the body.
        amount: f64,
    },
    /// `CrewTransport`: at least `count` `Active` roster members are
    /// posted at colonies occupying `site`. The accept re-bases the
    /// ask to `posted_at_accept + drawn` — crew already stationed
    /// never satisfies a fresh signature (the resupply re-base
    /// precedent for cumulative asks).
    CrewTransport {
        /// The site whose colony(ies) receive the crew.
        site: SiteId,
        /// Required posted headcount — post-re-base an absolute
        /// level, not a delta.
        count: u32,
    },
    /// `TouristFlight`: a `CrewKind::Tourist` roster member rode a
    /// vessel satisfying `regime` while the contract was active
    /// (recorded in the sim-side progress, not here — the params
    /// stay the frozen offer draw), then reads `Active` +
    /// `Unassigned`: home, alive.
    TouristFlight {
        /// The regime the flight profile had to demonstrate.
        regime: ContractRegime,
    },
}

impl ContractParams {
    /// The [`ContractKind`] these params instantiate — the completed
    /// bookkeeping keys on it.
    pub fn kind(&self) -> ContractKind {
        match self {
            Self::SatelliteOrbit { .. } => ContractKind::SatellitePlacement,
            Self::SurveyData { .. } => ContractKind::SurveyRun,
            Self::PartTest { .. } => ContractKind::PartTest,
            Self::Rescue { .. } => ContractKind::Rescue,
            Self::ColonyResupply { .. } => ContractKind::ColonyResupply,
            Self::CrewTransport { .. } => ContractKind::CrewTransport,
            Self::TouristFlight { .. } => ContractKind::TouristFlight,
        }
    }
}

/// §3.6's fact-based prerequisite — one predicate over serialized
/// program facts (module doc). All listed entries must hold.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ContractPrereq {
    /// A tech-tree node is researched (`TechState::researched`).
    TechResearched(TechNodeId),
    /// At least `count` contracts of `kind` have completed — §3.6's
    /// "contracts completed by type".
    ContractsCompleted {
        /// The completed-contract kind counted.
        kind: ContractKind,
        /// Required completion count.
        count: u32,
    },
    /// A vessel has orbited or surface-latched at `body` — §3.6's
    /// "bodies visited" (the contract facts record's visit set).
    BodyVisited(BodyId),
    /// At most `count` roster members are `Lost` — §3.6's "crew
    /// lost" (a program with a death toll past the line gets no
    /// risky work).
    CrewLostAtMost(u32),
}

/// §3.5's funds lines plus §3.6's failure penalty — authored in
/// funds (the sim converts to micros at apply, the `to_micros`
/// boundary).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractRewards {
    /// Paid on `AcceptContract` — §3.5's advance.
    pub advance_funds: f64,
    /// Paid when the completion predicate lands.
    pub completion_funds: f64,
    /// Charged when the `ContractDeadline` expires the contract —
    /// a *drain*, so it runs the funds clamp like salary, never a
    /// rejection (§3.5's bankruptcy rule).
    pub failure_penalty_funds: f64,
}
